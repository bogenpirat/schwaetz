//! Window/session state persisted between runs (`session.toml` in the data folder).

use serde::{Deserialize, Serialize};
use std::path::Path;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowPlacement, SW_SHOWMAXIMIZED, SW_SHOWNORMAL, SetWindowPlacement, WINDOWPLACEMENT,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Session {
    /// Normal (restored) window rectangle in screen pixels: left, top, right, bottom.
    pub window: Option<[i32; 4]>,
    pub maximized: bool,
    pub sidebar_width: Option<f32>,
    /// Width of the member list, as dragged at its left edge.
    pub nicklist_width: Option<f32>,
    /// Network display name and buffer name of the buffer that was active on exit.
    pub active: Option<(String, String)>,
}

impl Session {
    pub fn load(path: &Path) -> Session {
        std::fs::read_to_string(path).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        if let Ok(t) = toml::to_string(self) {
            let tmp = path.with_extension("toml.tmp");
            if std::fs::write(&tmp, t).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }

    pub fn capture_window(&mut self, hwnd: HWND) {
        let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        if unsafe { GetWindowPlacement(hwnd, &mut wp) }.is_ok() {
            let r = wp.rcNormalPosition;
            self.window = Some([r.left, r.top, r.right, r.bottom]);
            self.maximized = wp.showCmd == SW_SHOWMAXIMIZED.0 as u32;
        }
    }

    /// Applies the saved placement. Returns the show command to use.
    pub fn apply_window(&self, hwnd: HWND) -> bool {
        let Some([l, t, r, b]) = self.window else { return false };
        if r - l < 200 || b - t < 150 {
            return false;
        }
        let wp = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            showCmd: if self.maximized { SW_SHOWMAXIMIZED.0 as u32 } else { SW_SHOWNORMAL.0 as u32 },
            ptMinPosition: POINT { x: -1, y: -1 },
            ptMaxPosition: POINT { x: -1, y: -1 },
            // SetWindowPlacement keeps the window on a visible monitor if that one is gone.
            rcNormalPosition: RECT { left: l, top: t, right: r, bottom: b },
            ..Default::default()
        };
        unsafe { SetWindowPlacement(hwnd, &wp).is_ok() }
    }
}
