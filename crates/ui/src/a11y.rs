//! UI Automation (screen reader) support.
//!
//! The UI is custom-drawn, so it exposes its own automation tree:
//!
//! ```text
//! window (root)
//! ├── Buffers   (list)  → one selectable item per sidebar entry
//! ├── Messages  (list)  → the most recent lines of the active buffer
//! └── Message input (edit, Value pattern)
//! ```
//!
//! Providers hold only an element identity; every query takes a fresh snapshot of the UI, and
//! actions go through the same guarded access as the window procedure. New messages in the
//! active buffer are announced with UIA notifications.

#![allow(non_upper_case_globals)] // UIA constants keep their Win32 names.

use windows::Win32::Foundation::{E_FAIL, HWND, RECT};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayPutElement};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_BSTR, VT_I4};
use windows::Win32::UI::Accessibility::*;
use windows_core::{BOOL, BSTR, Error, IUnknown, IUnknownImpl, Interface, PCWSTR, Result, implement};

/// State the automation tree is built from (screen coordinates).
pub struct Snapshot {
    pub title: String,
    pub window: RECT,
    pub sidebar: RECT,
    pub chat: RECT,
    pub input: RECT,
    pub input_text: String,
    /// (buffer id, accessible name, selected, rect)
    pub buffers: Vec<(u32, String, bool, RECT)>,
    /// Recent lines of the active buffer, oldest first.
    pub lines: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum El {
    Sidebar,
    Item(u32),
    Chat,
    Line(usize),
    Input,
}

fn snapshot() -> Option<Snapshot> {
    crate::shell::with_ui(|ui| ui.a11y_snapshot())
}

fn variant_str(s: &str) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        (*v.Anonymous.Anonymous).vt = VT_BSTR;
        (*v.Anonymous.Anonymous).Anonymous.bstrVal = std::mem::ManuallyDrop::new(BSTR::from(s));
    }
    v
}

fn variant_i32(n: i32) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        (*v.Anonymous.Anonymous).vt = VT_I4;
        (*v.Anonymous.Anonymous).Anonymous.lVal = n;
    }
    v
}

fn variant_bool(b: bool) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        (*v.Anonymous.Anonymous).vt = VT_BOOL;
        (*v.Anonymous.Anonymous).Anonymous.boolVal = windows::Win32::Foundation::VARIANT_BOOL(if b { -1 } else { 0 });
    }
    v
}

fn uia_rect(r: RECT) -> UiaRect {
    UiaRect {
        left: r.left as f64,
        top: r.top as f64,
        width: (r.right - r.left) as f64,
        height: (r.bottom - r.top) as f64,
    }
}

fn contains(r: &RECT, x: f64, y: f64) -> bool {
    x >= r.left as f64 && x < r.right as f64 && y >= r.top as f64 && y < r.bottom as f64
}

/// "No element" for methods that must return S_OK with a null interface.
fn none<T>() -> Result<T> {
    Err(Error::empty())
}

#[implement(IRawElementProviderSimple, IRawElementProviderFragment, IRawElementProviderFragmentRoot)]
pub struct Root {
    hwnd: HWND,
}

#[implement(IRawElementProviderSimple, IRawElementProviderFragment, IValueProvider, ISelectionItemProvider)]
struct Node {
    hwnd: HWND,
    el: El,
}

impl Root {
    pub fn provider(hwnd: HWND) -> IRawElementProviderSimple {
        Root { hwnd }.into()
    }
}

fn node(hwnd: HWND, el: El) -> IRawElementProviderFragment {
    Node { hwnd, el }.into()
}

fn root_fragment(hwnd: HWND) -> IRawElementProviderFragment {
    Root { hwnd }.into()
}

impl IRawElementProviderSimple_Impl for Root_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider | ProviderOptions_UseComThreading)
    }
    fn GetPatternProvider(&self, _id: UIA_PATTERN_ID) -> Result<IUnknown> {
        none()
    }
    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        Ok(match id {
            UIA_NamePropertyId => variant_str(&snapshot().map(|s| s.title).unwrap_or_else(|| "schwätz".into())),
            UIA_ControlTypePropertyId => variant_i32(UIA_WindowControlTypeId.0),
            _ => VARIANT::default(),
        })
    }
    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        unsafe { UiaHostProviderFromHwnd(self.hwnd) }
    }
}

impl IRawElementProviderFragment_Impl for Root_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        match direction {
            NavigateDirection_FirstChild => Ok(node(self.hwnd, El::Sidebar)),
            NavigateDirection_LastChild => Ok(node(self.hwnd, El::Input)),
            _ => none(),
        }
    }
    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }
    fn BoundingRectangle(&self) -> Result<UiaRect> {
        Ok(UiaRect::default())
    }
    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }
    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }
    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(Root { hwnd: self.hwnd }.into())
    }
}

impl IRawElementProviderFragmentRoot_Impl for Root_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        let Some(s) = snapshot() else { return none() };
        if let Some(b) = s.buffers.iter().find(|b| contains(&b.3, x, y)) {
            return Ok(node(self.hwnd, El::Item(b.0)));
        }
        let el = if contains(&s.input, x, y) {
            El::Input
        } else if contains(&s.chat, x, y) {
            El::Chat
        } else if contains(&s.sidebar, x, y) {
            El::Sidebar
        } else {
            return Ok(root_fragment(self.hwnd));
        };
        Ok(node(self.hwnd, el))
    }
    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        Ok(node(self.hwnd, El::Input))
    }
}

impl Node_Impl {
    fn name(&self, s: &Snapshot) -> String {
        match self.el {
            El::Sidebar => "Buffers".into(),
            El::Item(id) => s.buffers.iter().find(|b| b.0 == id).map(|b| b.1.clone()).unwrap_or_default(),
            El::Chat => "Messages".into(),
            El::Line(i) => s.lines.get(i).cloned().unwrap_or_default(),
            El::Input => "Message input".into(),
        }
    }

    fn rect(&self, s: &Snapshot) -> RECT {
        match self.el {
            El::Sidebar => s.sidebar,
            El::Item(id) => s.buffers.iter().find(|b| b.0 == id).map(|b| b.3).unwrap_or_default(),
            El::Chat | El::Line(_) => s.chat,
            El::Input => s.input,
        }
    }
}

impl IRawElementProviderSimple_Impl for Node_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider | ProviderOptions_UseComThreading)
    }
    fn GetPatternProvider(&self, id: UIA_PATTERN_ID) -> Result<IUnknown> {
        let this: IUnknown = self.to_interface();
        match (id, self.el) {
            (UIA_ValuePatternId, El::Input) => Ok(this),
            (UIA_SelectionItemPatternId, El::Item(_)) => Ok(this),
            _ => none(),
        }
    }
    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        let Some(s) = snapshot() else { return Ok(VARIANT::default()) };
        let control = match self.el {
            El::Sidebar | El::Chat => UIA_ListControlTypeId,
            El::Item(_) | El::Line(_) => UIA_ListItemControlTypeId,
            El::Input => UIA_EditControlTypeId,
        };
        Ok(match id {
            UIA_NamePropertyId => variant_str(&self.name(&s)),
            UIA_ControlTypePropertyId => variant_i32(control.0),
            UIA_AutomationIdPropertyId => variant_str(match self.el {
                El::Sidebar => "buffers",
                El::Item(_) => "buffer",
                El::Chat => "messages",
                El::Line(_) => "message",
                El::Input => "input",
            }),
            UIA_IsKeyboardFocusablePropertyId => variant_bool(matches!(self.el, El::Input | El::Item(_))),
            UIA_HasKeyboardFocusPropertyId => variant_bool(self.el == El::Input),
            UIA_IsControlElementPropertyId | UIA_IsContentElementPropertyId | UIA_IsEnabledPropertyId => {
                variant_bool(true)
            }
            UIA_ValueValuePropertyId if self.el == El::Input => variant_str(&s.input_text),
            _ => VARIANT::default(),
        })
    }
    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        none()
    }
}

impl IRawElementProviderFragment_Impl for Node_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let Some(s) = snapshot() else { return none() };
        let top = [El::Sidebar, El::Chat, El::Input];
        let h = self.hwnd;
        let target = match (self.el, direction) {
            (El::Sidebar | El::Chat | El::Input, NavigateDirection_Parent) => return Ok(root_fragment(h)),
            (El::Item(_), NavigateDirection_Parent) => Some(El::Sidebar),
            (El::Line(_), NavigateDirection_Parent) => Some(El::Chat),
            (el @ (El::Sidebar | El::Chat | El::Input), NavigateDirection_NextSibling) => {
                top.iter().position(|e| *e == el).and_then(|i| top.get(i + 1)).copied()
            }
            (el @ (El::Sidebar | El::Chat | El::Input), NavigateDirection_PreviousSibling) => {
                top.iter().position(|e| *e == el).and_then(|i| i.checked_sub(1)).map(|i| top[i])
            }
            (El::Sidebar, NavigateDirection_FirstChild) => s.buffers.first().map(|b| El::Item(b.0)),
            (El::Sidebar, NavigateDirection_LastChild) => s.buffers.last().map(|b| El::Item(b.0)),
            (El::Chat, NavigateDirection_FirstChild) => (!s.lines.is_empty()).then_some(El::Line(0)),
            (El::Chat, NavigateDirection_LastChild) => s.lines.len().checked_sub(1).map(El::Line),
            (El::Item(id), NavigateDirection_NextSibling | NavigateDirection_PreviousSibling) => {
                let i = s.buffers.iter().position(|b| b.0 == id);
                let j = match direction {
                    NavigateDirection_NextSibling => i.map(|i| i + 1),
                    _ => i.and_then(|i| i.checked_sub(1)),
                };
                j.and_then(|j| s.buffers.get(j)).map(|b| El::Item(b.0))
            }
            (El::Line(i), NavigateDirection_NextSibling) => (i + 1 < s.lines.len()).then_some(El::Line(i + 1)),
            (El::Line(i), NavigateDirection_PreviousSibling) => i.checked_sub(1).map(El::Line),
            _ => None,
        };
        match target {
            Some(el) => Ok(node(h, el)),
            None => none(),
        }
    }
    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        let (kind, id) = match self.el {
            El::Sidebar => (1, 0),
            El::Item(b) => (2, b as i32),
            El::Chat => (3, 0),
            El::Line(i) => (4, i as i32),
            El::Input => (5, 0),
        };
        let ids = [UiaAppendRuntimeId as i32, kind, id];
        unsafe {
            let sa = SafeArrayCreateVector(VT_I4, 0, ids.len() as u32);
            if sa.is_null() {
                return Err(E_FAIL.into());
            }
            for (i, v) in ids.iter().enumerate() {
                let idx = i as i32;
                SafeArrayPutElement(sa, &idx, v as *const i32 as *const _)?;
            }
            Ok(sa)
        }
    }
    fn BoundingRectangle(&self) -> Result<UiaRect> {
        Ok(snapshot().map(|s| uia_rect(self.rect(&s))).unwrap_or_default())
    }
    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }
    fn SetFocus(&self) -> Result<()> {
        if let El::Item(id) = self.el {
            crate::shell::with_ui(|ui| ui.a11y_select(id));
        }
        Ok(())
    }
    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(Root { hwnd: self.hwnd }.into())
    }
}

impl IValueProvider_Impl for Node_Impl {
    fn SetValue(&self, val: &PCWSTR) -> Result<()> {
        let text = unsafe { val.to_string() }.unwrap_or_default();
        crate::shell::with_ui(|ui| ui.a11y_set_input(&text)).ok_or_else(|| Error::from(E_FAIL))
    }
    fn Value(&self) -> Result<BSTR> {
        Ok(BSTR::from(snapshot().map(|s| s.input_text).unwrap_or_default()))
    }
    fn IsReadOnly(&self) -> Result<BOOL> {
        Ok(false.into())
    }
}

impl ISelectionItemProvider_Impl for Node_Impl {
    fn Select(&self) -> Result<()> {
        if let El::Item(id) = self.el {
            crate::shell::with_ui(|ui| ui.a11y_select(id));
        }
        Ok(())
    }
    fn AddToSelection(&self) -> Result<()> {
        self.Select()
    }
    fn RemoveFromSelection(&self) -> Result<()> {
        Ok(())
    }
    fn IsSelected(&self) -> Result<BOOL> {
        let id = match self.el {
            El::Item(id) => id,
            _ => return Ok(false.into()),
        };
        Ok(snapshot().is_some_and(|s| s.buffers.iter().any(|b| b.0 == id && b.2)).into())
    }
    fn SelectionContainer(&self) -> Result<IRawElementProviderSimple> {
        node(self.hwnd, El::Sidebar).cast()
    }
}

/// Speaks `text` through screen readers (if any are listening).
pub fn announce(hwnd: HWND, text: &str, important: bool) {
    unsafe {
        if !UiaClientsAreListening().as_bool() {
            return;
        }
        let root = Root::provider(hwnd);
        let processing = if important { NotificationProcessing_ImportantAll } else { NotificationProcessing_All };
        let _ = UiaRaiseNotificationEvent(
            &root,
            NotificationKind_Other,
            processing,
            &BSTR::from(text),
            &BSTR::from("schwaetz.message"),
        );
    }
}

pub fn listening() -> bool {
    unsafe { UiaClientsAreListening().as_bool() }
}
