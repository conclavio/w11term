//! Per-profile normal window placement and reversible borderless fullscreen.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Write};
use windows::Win32::{Foundation::*, Graphics::Gdi::*, UI::WindowsAndMessaging::*};
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    pub profiles: BTreeMap<String, Placement>,
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
        let profiles = crate::w11term_data_path()
            .and_then(|p| std::fs::read(p.join("windows.json")).map_err(Into::into))
            .and_then(|b| serde_json::from_slice(&b).map_err(Into::into))
            .unwrap_or_default();
        Self {
            profiles,
            fullscreen: None,
            grid: None,
        }
    }
    pub fn fullscreen(&self) -> bool {
        self.fullscreen.is_some()
    }
    pub fn remember(&mut self, name: &str) -> Result<()> {
        let hwnd = crate::input::window().context("window is not ready")?;
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
        let mut f = tempfile::NamedTempFile::new_in(&dir)?;
        serde_json::to_writer_pretty(&mut f, &self.profiles)?;
        f.write_all(b"\n")?;
        f.persist(dir.join("windows.json"))?;
        Ok(())
    }
    pub fn restore(&self, name: &str) -> Result<()> {
        if self.fullscreen() {
            return Ok(());
        }
        let Some(p) = self.profiles.get(name) else {
            return Ok(());
        };
        let hwnd = crate::input::window().context("window is not ready")?;
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
            let width = p.width.clamp(320, (work.right - work.left).max(320));
            let height = p.height.clamp(240, (work.bottom - work.top).max(240));
            let x = p.x.clamp(work.left, work.right - width);
            let y = p.y.clamp(work.top, work.bottom - height);
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
        let hwnd = crate::input::window().context("window is not ready")?;
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
