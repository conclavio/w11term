//! Per-profile normal window placement and reversible borderless fullscreen.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Write};
use windows::Win32::{Foundation::*, Graphics::Gdi::*, UI::WindowsAndMessaging::*};
/// A requested cell box is best-effort: displays can clamp native geometry.
#[derive(Default)]
pub struct RequestedGrid {
    target: std::cell::Cell<Option<(usize, usize)>>,
    pane: std::cell::Cell<Option<w11term::tabs::TabId>>,
    attempts: std::cell::Cell<u8>,
    last_measurement: std::cell::Cell<Option<GridMeasurement>>,
    epoch: std::cell::Cell<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct GridMeasurement {
    cells: (usize, usize),
    pixels: (u32, u32, u32),
}

#[derive(Debug, PartialEq, Eq)]
pub enum GridCorrection {
    Publish,
    Wait,
    Resize {
        cols: usize,
        rows: usize,
        first: bool,
    },
}

pub fn client_extent(hwnd: HWND, scale: f32) -> Option<(f64, f64)> {
    let mut rect = RECT::default();
    unsafe { GetClientRect(hwnd, &mut rect) }.ok()?;
    let scale = f64::from(scale);
    Some((
        f64::from(rect.right - rect.left) / scale,
        f64::from(rect.bottom - rect.top) / scale,
    ))
}

impl RequestedGrid {
    pub fn new(target: Option<(usize, usize)>) -> Self {
        let value = Self::default();
        value.set(target);
        value
    }
    pub fn get(&self) -> Option<(usize, usize)> {
        self.target.get()
    }
    pub fn set(&self, target: Option<(usize, usize)>) {
        self.target.set(target);
        self.pane.set(None);
        self.attempts.set(0);
        self.last_measurement.set(None);
        self.epoch.set(self.epoch.get().wrapping_add(1));
    }
    pub fn epoch(&self) -> u64 {
        self.epoch.get()
    }
    pub fn set_for_pane(&self, pane: w11term::tabs::TabId, target: (usize, usize)) -> u64 {
        self.set(Some(target));
        self.pane.set(Some(pane));
        self.epoch()
    }
    pub fn pane(&self) -> Option<w11term::tabs::TabId> {
        self.pane.get()
    }
    pub fn expire(&self, epoch: u64) -> bool {
        self.epoch() == epoch && self.target.take().is_some()
    }
    pub fn correction(&self, measured: (usize, usize), pixels: (u32, u32, u32)) -> GridCorrection {
        let Some((cols, rows)) = self.get() else {
            return GridCorrection::Publish;
        };
        if (cols, rows) == measured {
            self.target.set(None);
            return GridCorrection::Publish;
        }
        // A caret blink or an SSH frame can redraw the same surface before WinUI
        // has applied the queued window size. Wait for a different measurement.
        let measurement = GridMeasurement {
            cells: measured,
            pixels,
        };
        if self.last_measurement.get() == Some(measurement) {
            return GridCorrection::Wait;
        }
        let attempts = self.attempts.get();
        if attempts >= 2 {
            self.target.set(None);
            return GridCorrection::Publish;
        }
        self.last_measurement.set(Some(measurement));
        self.attempts.set(attempts + 1);
        GridCorrection::Resize {
            cols,
            rows,
            first: attempts == 0,
        }
    }
}
fn fit_axis(position: i32, size: i32, start: i32, end: i32, minimum: i32) -> (i32, i32) {
    let available = end.saturating_sub(start).max(1);
    let size = size.clamp(minimum.min(available), available);
    (
        position.clamp(start, end.saturating_sub(size).max(start)),
        size,
    )
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
    #[serde(default)]
    pub cols: Option<usize>,
    #[serde(default)]
    pub rows: Option<usize>,
}
#[derive(Default)]
pub struct Windows {
    scope: crate::input::WindowScope,
    pub profiles: BTreeMap<String, Placement>,
    saved_profiles: BTreeMap<String, Placement>,
    pub grid: Option<(usize, usize)>,
    fullscreen: Option<NormalWindow>,
}
struct NormalWindow {
    placement: WINDOWPLACEMENT,
    style: isize,
    grid: Option<(usize, usize)>,
}
impl Windows {
    pub fn load() -> Self {
        let profiles: BTreeMap<String, Placement> = crate::w11term_data_path()
            .and_then(|p| std::fs::read(p.join("windows.json")).map_err(Into::into))
            .and_then(|b| serde_json::from_slice(&b).map_err(Into::into))
            .unwrap_or_default();
        Self {
            scope: crate::input::WindowScope::current(),
            saved_profiles: profiles.clone(),
            profiles,
            fullscreen: None,
            grid: None,
        }
    }
    pub fn fullscreen(&self) -> bool {
        self.fullscreen.is_some()
    }
    pub fn remember(&mut self, name: &str) -> Result<()> {
        let hwnd = self.scope.hwnd().context("window is not ready")?;
        let grid = self
            .fullscreen
            .as_ref()
            .map_or(self.grid, |saved| saved.grid);
        let placement = if let Some(saved) = &self.fullscreen {
            saved.placement
        } else {
            let mut placement = WINDOWPLACEMENT {
                length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            unsafe {
                GetWindowPlacement(hwnd, &mut placement)?;
            }
            placement
        };
        let r = placement.rcNormalPosition;
        self.profiles.insert(
            name.into(),
            Placement {
                x: r.left,
                y: r.top,
                width: r.right - r.left,
                height: r.bottom - r.top,
                maximized: placement.showCmd == SW_SHOWMAXIMIZED.0 as u32,
                cols: grid.map(|g| g.0),
                rows: grid.map(|g| g.1),
            },
        );
        let dir = crate::w11term_data_path()?;
        std::fs::create_dir_all(&dir)?;
        self.save_placements(&dir, name)
    }
    fn save_placements(&mut self, dir: &std::path::Path, current: &str) -> Result<()> {
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("windows.lock"))?;
        lock.try_lock()
            .context("Another window is saving its placement")?;
        let path = dir.join("windows.json");
        let mut latest: BTreeMap<String, Placement> = match std::fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).unwrap_or_else(|_| self.saved_profiles.clone())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error.into()),
        };
        // Apply this window's profile renames and newly measured placement to
        // the latest map. A sibling window may have saved other profiles.
        for removed in self
            .saved_profiles
            .keys()
            .filter(|key| !self.profiles.contains_key(*key))
        {
            latest.remove(removed);
        }
        for (name, placement) in &self.profiles {
            if name == current || self.saved_profiles.get(name) != Some(placement) {
                latest.insert(name.clone(), placement.clone());
            }
        }
        let mut f = tempfile::NamedTempFile::new_in(dir)?;
        serde_json::to_writer_pretty(&mut f, &latest)?;
        f.write_all(b"\n")?;
        f.persist(path)?;
        self.saved_profiles = latest.clone();
        self.profiles = latest;
        Ok(())
    }
    pub fn restore(&self, name: &str) -> Result<()> {
        if self.fullscreen() {
            return Ok(());
        }
        let Some(p) = self.profiles.get(name) else {
            return Ok(());
        };
        let hwnd = self.scope.hwnd().context("window is not ready")?;
        unsafe {
            let desired = RECT {
                left: p.x,
                top: p.y,
                right: p.x.saturating_add(p.width),
                bottom: p.y.saturating_add(p.height),
            };
            let monitor = MonitorFromRect(&desired, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            GetMonitorInfoW(monitor, &mut info).ok()?;
            let work = info.rcWork;
            let (x, width) = fit_axis(p.x, p.width, work.left, work.right, 320);
            let (y, height) = fit_axis(p.y, p.height, work.top, work.bottom, 240);
            let placement = WINDOWPLACEMENT {
                length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                showCmd: SW_SHOWNOACTIVATE.0 as u32,
                rcNormalPosition: RECT {
                    left: x,
                    top: y,
                    right: x + width,
                    bottom: y + height,
                },
                ..Default::default()
            };
            // Restore size/position without activating the application.
            SetWindowPlacement(hwnd, &placement)?;
        }
        Ok(())
    }
    pub fn toggle(&mut self) -> Result<()> {
        let hwnd = self.scope.hwnd().context("window is not ready")?;
        unsafe {
            if let Some(NormalWindow {
                placement,
                style,
                grid,
            }) = self.fullscreen.take()
            {
                self.grid = grid;
                SetWindowLongPtrW(hwnd, GWL_STYLE, style);
                SetWindowPlacement(hwnd, &placement)?;
                SetWindowPos(
                    hwnd,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                )?;
            } else {
                let mut placement = WINDOWPLACEMENT {
                    length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                    ..Default::default()
                };
                GetWindowPlacement(hwnd, &mut placement)?;
                let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
                let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                let mut info = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                GetMonitorInfoW(monitor, &mut info).ok()?;
                let r = info.rcMonitor;
                SetWindowLongPtrW(hwnd, GWL_STYLE, style & !(WS_OVERLAPPEDWINDOW.0 as isize));
                SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                )?;
                self.fullscreen = Some(NormalWindow {
                    placement,
                    style,
                    grid: self.grid,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saving_after_a_sibling_preserves_its_placements_and_profile_renames() {
        let dir = tempfile::tempdir().unwrap();
        let initial = Placement {
            x: 0,
            y: 0,
            width: 600,
            height: 400,
            maximized: false,
            cols: None,
            rows: None,
        };
        let mut first = Windows::default();
        first.profiles.insert("old".into(), initial.clone());
        first.save_placements(dir.path(), "old").unwrap();
        let mut second = Windows {
            profiles: first.profiles.clone(),
            saved_profiles: first.saved_profiles.clone(),
            ..Default::default()
        };
        first.profiles.remove("old");
        first.profiles.insert("renamed".into(), initial.clone());
        first.save_placements(dir.path(), "renamed").unwrap();
        second.profiles.insert("other".into(), initial);
        second.save_placements(dir.path(), "other").unwrap();
        assert_eq!(
            second
                .profiles
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["other", "renamed"]
        );
    }
    #[test]
    fn geometry_converges_or_falls_back_and_new_requests_reset_budget() {
        let request = RequestedGrid::new(Some((170, 48)));
        let first_viewport = (800, 480, 96);
        let next_viewport = (1000, 600, 96);
        let initial_epoch = request.epoch();
        assert_eq!(
            request.correction((80, 24), first_viewport),
            GridCorrection::Resize {
                cols: 170,
                rows: 48,
                first: true
            }
        );
        assert_eq!(
            request.correction((100, 30), next_viewport),
            GridCorrection::Resize {
                cols: 170,
                rows: 48,
                first: false
            }
        );
        assert_eq!(
            request.correction((100, 30), next_viewport),
            GridCorrection::Wait
        );
        assert_eq!(
            request.correction((105, 30), (1050, 600, 96)),
            GridCorrection::Publish
        );
        assert_eq!(request.get(), None);
        assert_eq!(request.epoch(), initial_epoch);
        let epoch = request.set_for_pane(8, (80, 24));
        assert_eq!(request.pane(), Some(8));
        assert!(!request.expire(initial_epoch));
        assert_eq!(
            request.correction((80, 24), first_viewport),
            GridCorrection::Publish
        );
        assert_eq!(request.get(), None);
        assert_eq!(request.epoch(), epoch);
        request.set(Some((170, 48)));
        assert_eq!(request.pane(), None);
        assert!(matches!(
            request.correction((80, 24), first_viewport),
            GridCorrection::Resize { first: true, .. }
        ));
        request.set(None); // user resizing cancels the pending request
        assert_eq!(
            request.correction((90, 25), (900, 500, 96)),
            GridCorrection::Publish
        );
    }

    #[test]
    fn repeating_a_paint_before_the_window_resizes_keeps_the_target_pending() {
        let request = RequestedGrid::default();
        let epoch = request.set_for_pane(7, (64, 18));
        let old_viewport = (656, 395, 96);
        assert_eq!(
            request.correction((64, 19), old_viewport),
            GridCorrection::Resize {
                cols: 64,
                rows: 18,
                first: true
            }
        );
        for _ in 0..4 {
            assert_eq!(
                request.correction((64, 19), old_viewport),
                GridCorrection::Wait
            );
            assert_eq!(request.get(), Some((64, 18)));
            assert_eq!(request.epoch(), epoch);
        }
        assert_eq!(
            request.correction((64, 18), (660, 380, 96)),
            GridCorrection::Publish
        );
        assert_eq!(request.get(), None);
    }
    #[test]
    fn placement_fits_tiny_negative_and_normal_work_areas() {
        assert_eq!(fit_axis(999, 640, 0, 200, 320), (0, 200));
        assert_eq!(fit_axis(-9999, 400, -1920, -640, 320), (-1920, 400));
        assert_eq!(fit_axis(800, 400, 0, 1000, 320), (600, 400));
    }
}
