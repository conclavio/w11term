//! Per-profile normal window placement and reversible borderless fullscreen.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Write};
use windows::Win32::{Foundation::*, Graphics::Gdi::*, UI::WindowsAndMessaging::*};
/// A requested cell box is best-effort: displays can clamp native geometry.
#[derive(Default)]
pub struct RequestedGrid {
    target: std::cell::Cell<Option<(usize, usize)>>,
    attempts: std::cell::Cell<u8>,
    epoch: std::cell::Cell<u64>,
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
        self.attempts.set(0);
        self.epoch.set(self.epoch.get().wrapping_add(1));
    }
    pub fn epoch(&self) -> u64 {
        self.epoch.get()
    }
    pub fn correction(&self, measured: (usize, usize)) -> Option<((usize, usize), bool)> {
        let target = self.get()?;
        if target == measured || self.attempts.get() >= 2 {
            self.set(None);
            return None;
        }
        let first = self.attempts.get() == 0;
        self.attempts.set(self.attempts.get() + 1);
        Some((target, first))
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
        assert_eq!(request.correction((80, 24)), Some(((170, 48), true)));
        assert_eq!(request.correction((100, 30)), Some(((170, 48), false)));
        assert_eq!(request.correction((100, 30)), None);
        assert_eq!(request.get(), None);
        request.set(Some((80, 24)));
        assert_eq!(request.correction((80, 24)), None);
        assert_eq!(request.get(), None);
        request.set(Some((170, 48)));
        assert_eq!(request.correction((80, 24)), Some(((170, 48), true)));
        request.set(None); // user resizing cancels the pending request
        assert_eq!(request.correction((90, 25)), None);
    }
    #[test]
    fn placement_fits_tiny_negative_and_normal_work_areas() {
        assert_eq!(fit_axis(999, 640, 0, 200, 320), (0, 200));
        assert_eq!(fit_axis(-9999, 400, -1920, -640, 320), (-1920, 400));
        assert_eq!(fit_axis(800, 400, 0, 1000, 320), (600, 400));
    }
}
