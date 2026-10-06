//! Small Win32 helpers: clipboard, tray icon, context menus, window chrome, shell.

use crate::gfx::Color;
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::Shell::{
    DragFinish, DragQueryFileW, DragQueryPoint, HDROP, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP,
    NIIF_INFO, NIIF_RESPECT_QUIET_TIME, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICON_VERSION_4,
    NOTIFYICONDATAW, Shell_NotifyIconW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HSTRING, PCSTR, PCWSTR, w};

const CF_UNICODETEXT: u32 = 13;
const CF_DIB: u32 = 8;
const CF_HDROP: u32 = 15;

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

/// An image offered by the clipboard.
pub enum ImageSource {
    /// Files copied in Explorer (not necessarily images).
    Files(Vec<std::path::PathBuf>),
    /// The bytes of a PNG file.
    Png(Vec<u8>),
    /// The bytes of a BMP file.
    Bmp(Vec<u8>),
}

/// Copies the clipboard's data in `format`. The clipboard must be open.
unsafe fn clipboard_bytes(format: u32) -> Option<Vec<u8>> {
    unsafe {
        let g = HGLOBAL(GetClipboardData(format).ok()?.0);
        let p = GlobalLock(g) as *const u8;
        if p.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(p, GlobalSize(g)).to_vec();
        let _ = GlobalUnlock(g);
        Some(bytes)
    }
}

unsafe fn hdrop_files(h: HDROP) -> Vec<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    unsafe {
        (0..DragQueryFileW(h, u32::MAX, None))
            .map(|i| {
                let mut buf = vec![0u16; DragQueryFileW(h, i, None) as usize + 1];
                let n = DragQueryFileW(h, i, Some(&mut buf)) as usize;
                std::ffi::OsString::from_wide(&buf[..n]).into()
            })
            .collect()
    }
}

/// Puts a file header in front of a device-independent bitmap (`CF_DIB`), making it a BMP file.
fn dib_to_bmp(dib: &[u8]) -> Option<Vec<u8>> {
    let u32_at = |i: usize| dib.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let header = u32_at(0)?;
    if header < 40 {
        return None;
    }
    let bits = u16::from_le_bytes([*dib.get(14)?, *dib.get(15)?]);
    let (compression, used) = (u32_at(16)?, u32_at(32)?);
    // A plain BITMAPINFOHEADER is followed by its color masks; larger headers contain them.
    let masks = match compression {
        3 if header == 40 => 12,
        6 if header == 40 => 16,
        _ => 0,
    };
    let colors = if used == 0 && bits <= 8 { 1u32 << bits } else { used };
    let pixels = 14u32.checked_add(header)?.checked_add(masks)?.checked_add(colors.checked_mul(4)?)?;
    if pixels as usize > 14 + dib.len() {
        return None;
    }
    let mut bmp = Vec::with_capacity(14 + dib.len());
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(14 + dib.len() as u32).to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&pixels.to_le_bytes());
    bmp.extend_from_slice(dib);
    Some(bmp)
}

/// The image on the clipboard, if it holds one and no text (copied spreadsheet cells and the
/// like offer both; those paste as text).
pub fn clipboard_image(hwnd: HWND) -> Option<ImageSource> {
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let result = (|| {
            if let Ok(h) = GetClipboardData(CF_HDROP) {
                return Some(ImageSource::Files(hdrop_files(HDROP(h.0))));
            }
            if IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok() {
                return None;
            }
            let png = RegisterClipboardFormatW(w!("PNG"));
            if png != 0
                && IsClipboardFormatAvailable(png).is_ok()
                && let Some(bytes) = clipboard_bytes(png)
            {
                return Some(ImageSource::Png(bytes));
            }
            clipboard_bytes(CF_DIB).and_then(|dib| dib_to_bmp(&dib)).map(ImageSource::Bmp)
        })();
        let _ = CloseClipboard();
        result
    }
}

/// The files of a `WM_DROPFILES` message and the client point (pixels) they were dropped at.
pub fn dropped_files(wp: WPARAM) -> (Vec<std::path::PathBuf>, POINT) {
    let h = HDROP(wp.0 as *mut _);
    let mut pt = POINT::default();
    unsafe {
        let files = hdrop_files(h);
        let _ = DragQueryPoint(h, &mut pt);
        DragFinish(h);
        (files, pt)
    }
}

/// Opens a URL with the default handler. Only web/IRC/mail schemes are allowed.
/// Opens a folder in Explorer (only existing directories).
pub fn open_folder(path: &std::path::Path) {
    if !path.is_dir() {
        return;
    }
    let p = HSTRING::from(path);
    unsafe {
        ShellExecuteW(None, w!("explore"), &p, None, None, SW_SHOWNORMAL);
    }
}

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

/// Gives the title bar fixed colors instead of the Windows accent color it otherwise takes while
/// the window is focused (Windows 11; older versions ignore this). Alpha is ignored.
pub fn set_titlebar_colors(hwnd: HWND, caption: Color, text: Color, border: Color) {
    let colorref = |c: Color| {
        let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        ch(c.r) | ch(c.g) << 8 | ch(c.b) << 16
    };
    // DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_BORDER_COLOR
    for (attr, c) in [(35, caption), (36, text), (34, border)] {
        let v = colorref(c);
        unsafe {
            let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(attr), &v as *const _ as _, 4);
        }
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
/// Asks for an existing file with the Windows file dialog, starting in the folder of `current`
/// (if any). `filters` are (description, patterns) pairs such as ("Text (*.txt)", "*.txt").
/// Returns the chosen path, or `None` if the dialog was cancelled.
pub fn pick_file(hwnd: HWND, title: &str, filters: &[(&str, &str)], current: &str) -> Option<String> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    };
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FileOpenDialog, IFileOpenDialog, IShellItem,
        SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
    };
    unsafe {
        // The dialog needs COM on this thread; initializing it again is harmless.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let names: Vec<(HSTRING, HSTRING)> =
            filters.iter().map(|(n, p)| (HSTRING::from(*n), HSTRING::from(*p))).collect();
        let specs: Vec<COMDLG_FILTERSPEC> = names
            .iter()
            .map(|(n, p)| COMDLG_FILTERSPEC { pszName: PCWSTR(n.as_ptr()), pszSpec: PCWSTR(p.as_ptr()) })
            .collect();
        if !specs.is_empty() {
            let _ = dlg.SetFileTypes(&specs);
        }
        let _ = dlg.SetTitle(&HSTRING::from(title));
        if let Ok(o) = dlg.GetOptions() {
            let _ = dlg.SetOptions(o | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM);
        }
        let dir = std::path::Path::new(current).parent().filter(|d| !d.as_os_str().is_empty() && d.is_dir());
        if let Some(dir) = dir
            && let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(dir.as_os_str()), None)
        {
            let _ = dlg.SetFolder(&item);
        }
        dlg.Show(Some(hwnd)).ok()?;
        let name = dlg.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        path
    }
}

pub fn popup_menu(hwnd: HWND, items: &[MenuItem]) -> u32 {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    track_menu(hwnd, items, pt, TRACK_POPUP_MENU_FLAGS(0))
}

/// Shows a popup menu for a button: its bottom-left corner at client point (`x`, `y`) in pixels,
/// so it opens upwards from there. Returns the chosen id, or 0.
pub fn popup_menu_above(hwnd: HWND, items: &[MenuItem], x: i32, y: i32) -> u32 {
    let mut pt = POINT { x, y };
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut pt);
    }
    track_menu(hwnd, items, pt, TPM_BOTTOMALIGN)
}

fn track_menu(hwnd: HWND, items: &[MenuItem], pt: POINT, align: TRACK_POPUP_MENU_FLAGS) -> u32 {
    unsafe {
        let m = build_menu(items);
        let _ = SetForegroundWindow(hwnd);
        let flags = TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY | align;
        let r = TrackPopupMenu(m, flags, pt.x, pt.y, None, hwnd, None);
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

#[cfg(test)]
mod tests {
    use super::dib_to_bmp;

    fn dib(header: u32, bits: u16, compression: u32, used: u32, rest: usize) -> Vec<u8> {
        let mut d = vec![0u8; header as usize + rest];
        d[0..4].copy_from_slice(&header.to_le_bytes());
        d[14..16].copy_from_slice(&bits.to_le_bytes());
        d[16..20].copy_from_slice(&compression.to_le_bytes());
        d[32..36].copy_from_slice(&used.to_le_bytes());
        d
    }

    #[test]
    fn bitmap_file_header() {
        let pixel_offset = |d: &[u8]| {
            let bmp = dib_to_bmp(d).unwrap();
            assert_eq!(&bmp[..2], b"BM");
            assert_eq!(u32::from_le_bytes(bmp[2..6].try_into().unwrap()) as usize, bmp.len());
            assert_eq!(&bmp[14..], d);
            u32::from_le_bytes(bmp[10..14].try_into().unwrap())
        };
        assert_eq!(pixel_offset(&dib(40, 32, 0, 0, 16)), 54, "true color: pixels follow the header");
        assert_eq!(pixel_offset(&dib(40, 32, 3, 0, 28)), 66, "BI_BITFIELDS adds three masks");
        assert_eq!(pixel_offset(&dib(124, 32, 3, 0, 16)), 138, "a V5 header contains its masks");
        assert_eq!(pixel_offset(&dib(40, 8, 0, 0, 1024 + 8)), 54 + 1024, "full palette");
        assert_eq!(pixel_offset(&dib(40, 8, 0, 2, 8 + 8)), 54 + 8, "short palette");
        assert!(dib_to_bmp(&dib(40, 8, 0, 0, 10)).is_none(), "palette runs past the end");
        assert!(dib_to_bmp(&[12, 0, 0, 0]).is_none());
    }
}
