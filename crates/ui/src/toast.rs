//! Desktop notifications as Windows toasts under schwätz's own name and icon.
//!
//! An unpackaged program has no identity the notification center can show, so tray balloons end
//! up attributed to some other app. Toasts are sent for an AppUserModelID registered under
//! `HKCU\Software\Classes\AppUserModelId` with a display name and icon instead.

use std::collections::VecDeque;
use std::path::PathBuf;

use schwaetz_core::BufferId;
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager, ToastNotifier};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW,
    RegSetValueExW,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{HSTRING, IInspectable, Ref};

const AUMID: &str = "bogenpirat.schwaetz";
const ICON_PNG: &[u8] = include_bytes!("../res/icon.png");
/// Toasts kept alive so that clicking them still reaches the handler.
const KEEP: usize = 16;

/// Posted when a toast is clicked; `wparam` is the buffer id.
pub const WM_APP_TOAST: u32 = WM_APP + 5;

pub struct Toasts {
    hwnd: HWND,
    icon: PathBuf,
    notifier: Option<ToastNotifier>,
    shown: VecDeque<ToastNotification>,
}

impl Toasts {
    /// `icon` is where the icon file is written for the notification center to read.
    pub fn new(hwnd: HWND, icon: PathBuf) -> Toasts {
        Toasts { hwnd, icon, notifier: None, shown: VecDeque::new() }
    }

    pub fn show(&mut self, title: &str, body: &str, buffer: BufferId) -> windows::core::Result<()> {
        let notifier = match &self.notifier {
            Some(n) => n,
            None => {
                unsafe {
                    // WinRT needs COM on this thread; initializing it again is harmless.
                    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                }
                if std::fs::read(&self.icon).ok().as_deref() != Some(ICON_PNG) {
                    let _ = std::fs::write(&self.icon, ICON_PNG);
                }
                register(&self.icon);
                self.notifier.insert(ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?)
            }
        };
        let xml = format!(
            "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
            escape(title),
            escape(body)
        );
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(xml))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        let hwnd = self.hwnd.0 as isize;
        toast.Activated(&TypedEventHandler::new(move |_: Ref<ToastNotification>, _: Ref<IInspectable>| {
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_TOAST, WPARAM(buffer.0 as usize), LPARAM(0));
            }
            Ok(())
        }))?;
        notifier.Show(&toast)?;
        if self.shown.len() == KEEP {
            self.shown.pop_front();
        }
        self.shown.push_back(toast);
        Ok(())
    }
}

/// Gives the AppUserModelID the name and icon the notification center shows.
fn register(icon: &std::path::Path) {
    let key = HSTRING::from(format!("Software\\Classes\\AppUserModelId\\{AUMID}"));
    unsafe {
        let mut h = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &key,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut h,
            None,
        )
        .is_err()
        {
            return;
        }
        for (name, value) in [("DisplayName", "schwätz".to_owned()), ("IconUri", icon.display().to_string())] {
            let data: Vec<u8> = crate::win::wide(&value).iter().flat_map(|u| u.to_le_bytes()).collect();
            let _ = RegSetValueExW(h, &HSTRING::from(name), None, REG_SZ, Some(&data));
        }
        let _ = RegCloseKey(h);
    }
}

/// XML-escapes text, dropping the control characters XML cannot hold.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn escape_markup_and_controls() {
        assert_eq!(super::escape("a<b> & \"c\"\x02\x0f"), "a&lt;b&gt; &amp; &quot;c&quot;");
    }
}
