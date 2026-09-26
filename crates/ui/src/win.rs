//! Small Win32 helpers: clipboard, tray icon, context menus, window chrome, shell.

use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIIF_RESPECT_QUIET_TIME, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HSTRING, PCSTR, PCWSTR, w};

const CF_UNICODETEXT: u32 = 13;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn get_clipboard(hwnd: HWND) -> Option<String> {
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let result = (|| {
            let h = GetClipboardData(CF_UNICODETEXT).ok()?;
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            let max = GlobalSize(g) / 2;
            let len = (0..max).take_while(|&i| *p.add(i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            let _ = GlobalUnlock(g);
            Some(s)
        })();
        let _ = CloseClipboard();
        result
    }
}

pub fn set_clipboard(hwnd: HWND, text: &str) {
    let w = wide(text);
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
            let p = GlobalLock(g) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                let _ = GlobalUnlock(g);
                let _ = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(g.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

/// Opens a URL with the default handler. Only web/IRC/mail schemes are allowed.
pub fn open_url(url: &str) {
    let lower = url.to_ascii_lowercase();
    if !["http://", "https://", "irc://", "ircs://", "mailto:"].iter().any(|s| lower.starts_with(s)) {
        return;
    }
    let u = HSTRING::from(url);
    unsafe {
        ShellExecuteW(None, w!("open"), &u, None, None, SW_SHOWNORMAL);
    }
}

pub fn set_dark_titlebar(hwnd: HWND, dark: bool) {
    let v = BOOL::from(dark);
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(20), &v as *const _ as _, 4);
    }
}

/// Enables the Mica backdrop (Windows 11). Returns false if unsupported.
pub fn enable_mica(hwnd: HWND, on: bool) -> bool {
    let backdrop: i32 = if on { 2 } else { 1 }; // DWMSBT_MAINWINDOW / DWMSBT_NONE
    unsafe {
        let ok = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(38), &backdrop as *const _ as _, 4).is_ok();
        if ok && on {
            let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
        }
        ok && on
    }
}

/// Opts the process into dark context menus (undocumented uxtheme ordinals, Windows 10 1903+).
pub fn allow_dark_menus(dark: bool) {
    unsafe {
        let Ok(ux) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(f) = GetProcAddress(ux, PCSTR(135 as *const u8)) {
            let set_mode: extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            set_mode(if dark { 2 } else { 3 }); // ForceDark / ForceLight
        }
        if let Some(f) = GetProcAddress(ux, PCSTR(136 as *const u8)) {
            let flush: extern "system" fn() = std::mem::transmute(f);
            flush();
        }
    }
}

pub fn is_system_dark() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let mut v: u32 = 1;
    let mut size = 4u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    ok.is_ok() && v == 0
}

pub enum MenuItem<'a> {
    Item(u32, &'a str),
    Check(u32, &'a str, bool),
    Separator,
    Sub(&'a str, Vec<MenuItem<'a>>),
}

fn build_menu(items: &[MenuItem]) -> HMENU {
    unsafe {
        let m = CreatePopupMenu().expect("menu");
        for it in items {
            match it {
                MenuItem::Item(id, label) => {
                    let l = HSTRING::from(*label);
                    let _ = AppendMenuW(m, MF_STRING, *id as usize, &l);
                }
                MenuItem::Check(id, label, on) => {
                    let l = HSTRING::from(*label);
                    let flags = if *on { MF_STRING | MF_CHECKED } else { MF_STRING };
                    let _ = AppendMenuW(m, flags, *id as usize, &l);
                }
                MenuItem::Separator => {
                    let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
                }
                MenuItem::Sub(label, sub) => {
                    let s = build_menu(sub);
                    let l = HSTRING::from(*label);
                    let _ = AppendMenuW(m, MF_POPUP, s.0 as usize, &l);
                }
            }
        }
        m
    }
}

/// Shows a context menu at the cursor; returns the chosen id (0 = cancelled).
pub fn popup_menu(hwnd: HWND, items: &[MenuItem]) -> u32 {
    unsafe {
        let m = build_menu(items);
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let r = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, None, hwnd, None);
        let _ = DestroyMenu(m);
        r.0 as u32
    }
}

pub struct Tray {
    hwnd: HWND,
    added: bool,
}

pub const WM_MOUSELEAVE: u32 = 0x02A3;
pub const WM_APP_TRAY: u32 = WM_APP + 2;
pub const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;

fn copy_wide(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

impl Tray {
    pub fn new(hwnd: HWND) -> Tray {
        Tray { hwnd, added: false }
    }

    fn data(&self) -> NOTIFYICONDATAW {
        let mut d = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            ..Default::default()
        };
        d.uCallbackMessage = WM_APP_TRAY;
        d.hIcon = app_icon(true);
        copy_wide(&mut d.szTip, "schwätz");
        d
    }

    pub fn add(&mut self) {
        if self.added {
            return;
        }
        let mut d = self.data();
        d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
        unsafe {
            self.added = Shell_NotifyIconW(NIM_ADD, &d).as_bool();
            d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &d);
        }
    }

    /// Shows a notification (rendered as a toast on Windows 10/11).
    pub fn notify(&mut self, title: &str, body: &str) {
        self.add();
        let mut d = self.data();
        d.uFlags = NIF_INFO;
        copy_wide(&mut d.szInfoTitle, title);
        copy_wide(&mut d.szInfo, body);
        d.dwInfoFlags = NIIF_INFO | NIIF_RESPECT_QUIET_TIME;
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }

    pub fn remove(&mut self) {
        if self.added {
            let d = self.data();
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &d);
            }
            self.added = false;
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
    }
}

/// The application icon from resources (id 1), falling back to the stock icon.
pub fn app_icon(small: bool) -> HICON {
    unsafe {
        let module = GetModuleHandleW(None).unwrap_or_default();
        let size = if small { GetSystemMetrics(SM_CXSMICON) } else { GetSystemMetrics(SM_CXICON) };
        LoadImageW(
            Some(module.into()),
            PCWSTR(std::ptr::without_provenance(1)),
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR,
        )
        .map(|h| HICON(h.0))
        .unwrap_or_else(|_| LoadIconW(None, IDI_APPLICATION).unwrap_or_default())
    }
}

pub fn lparam_point(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32)
}

pub fn wheel_delta(wp: WPARAM) -> i16 {
    ((wp.0 >> 16) & 0xffff) as i16
}

pub fn key_down(vk: u16) -> bool {
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(vk as i32) < 0 }
}

/// Shows a blocking error dialog (used for fatal startup errors and crashes).
pub fn error_box(title: &str, text: &str) {
    let t = HSTRING::from(title);
    let m = HSTRING::from(text);
    unsafe {
        MessageBoxW(None, &m, &t, MB_OK | MB_ICONERROR);
    }
}

/// Finds a running schwätz window and forwards `text` to it. Returns false if none is running.
pub fn forward_to_running_instance(text: &str) -> bool {
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    unsafe {
        let Ok(hwnd) = FindWindowW(w!("schwaetz.main"), PCWSTR::null()) else { return false };
        if hwnd.is_invalid() {
            return false;
        }
        let payload: Vec<u16> = text.encode_utf16().collect();
        let cds = COPYDATASTRUCT {
            dwData: crate::shell::COPYDATA_MAGIC,
            cbData: (payload.len() * 2) as u32,
            lpData: payload.as_ptr() as *mut _,
        };
        let _ = SendMessageW(hwnd, WM_COPYDATA, None, Some(LPARAM(&cds as *const _ as isize)));
        let _ = SetForegroundWindow(hwnd);
        true
    }
}
