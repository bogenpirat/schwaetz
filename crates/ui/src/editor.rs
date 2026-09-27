//! Multi-line text editor used for the input box and dialog text fields.

use crate::text::{self, Text, U16Map};
use windows::Win32::Graphics::DirectWrite::{IDWriteTextFormat, IDWriteTextLayout};

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditKind {
    None,
    Typing,
    Deleting,
}

pub struct Editor {
    text: String,
    pub cursor: usize,
    pub anchor: usize,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
    last_kind: EditKind,
    preferred_x: Option<f32>,
    layout: Option<(IDWriteTextLayout, U16Map, f32, u64)>,
    version: u64,
    pub single_line: bool,
    /// Render as bullets (password fields).
    pub masked: bool,
}

/// mIRC control codes are shown as visible control pictures (one UTF-16 unit each, so offsets
/// don't change).
fn display_char(c: char) -> char {
    match c {
        '\x02' => '␂',
        '\x03' => '␃',
        '\x04' => '␄',
        '\x0f' => '␏',
        '\x11' => '␑',
        '\x16' => '␖',
        '\x1d' => '␝',
        '\x1e' => '␞',
        '\x1f' => '␟',
        c => c,
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Default for Editor {
    fn default() -> Self {
        Editor {
            text: String::new(),
            cursor: 0,
            anchor: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            last_kind: EditKind::None,
            preferred_x: None,
            layout: None,
            version: 0,
            single_line: false,
            masked: false,
        }
    }
}

impl Editor {
    pub fn single_line() -> Editor {
        Editor { single_line: true, ..Default::default() }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn changed(&mut self) {
        self.version += 1;
        self.preferred_x = None;
    }

    fn snapshot(&mut self, kind: EditKind) {
        if kind == EditKind::None || kind != self.last_kind {
            self.undo.push((self.text.clone(), self.cursor));
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        self.last_kind = kind;
    }

    /// Replaces the whole text (history navigation, completion) as one undo step.
    pub fn set_text(&mut self, s: &str, cursor: Option<usize>) {
        if s == self.text {
            return;
        }
        self.snapshot(EditKind::None);
        self.text = s.to_owned();
        let c = cursor.unwrap_or(s.len()).min(s.len());
        self.cursor = c;
        self.anchor = c;
        self.changed();
    }

    pub fn clear(&mut self) {
        self.set_text("", None);
    }

    pub fn has_selection(&self) -> bool {
        self.cursor != self.anchor
    }

    pub fn selection(&self) -> (usize, usize) {
        (self.cursor.min(self.anchor), self.cursor.max(self.anchor))
    }

    pub fn selected_text(&self) -> &str {
        let (a, b) = self.selection();
        &self.text[a..b]
    }

    fn delete_selection(&mut self) -> bool {
        if !self.has_selection() {
            return false;
        }
        let (a, b) = self.selection();
        self.text.replace_range(a..b, "");
        self.cursor = a;
        self.anchor = a;
        true
    }

    pub fn insert(&mut self, s: &str) {
        let s: String =
            if self.single_line { s.replace(['\r', '\n'], " ") } else { s.replace("\r\n", "\n").replace('\r', "\n") };
        let kind = if s.chars().count() == 1 && !s.contains([' ', '\n']) { EditKind::Typing } else { EditKind::None };
        self.snapshot(kind);
        self.delete_selection();
        self.text.insert_str(self.cursor, &s);
        self.cursor += s.len();
        self.anchor = self.cursor;
        self.changed();
    }

    fn prev_boundary(&self, from: usize, word: bool) -> usize {
        let before = &self.text[..from];
        if !word {
            return before.char_indices().next_back().map_or(0, |(i, _)| i);
        }
        let mut it = before.char_indices().rev().peekable();
        // Skip non-word characters, then the word.
        while let Some(&(_, c)) = it.peek() {
            if is_word(c) {
                break;
            }
            it.next();
        }
        let mut pos = it.peek().map_or(0, |&(i, c)| i + c.len_utf8());
        for (i, c) in it {
            if !is_word(c) {
                break;
            }
            pos = i;
        }
        pos
    }

    fn next_boundary(&self, from: usize, word: bool) -> usize {
        let after = &self.text[from..];
        if !word {
            return after.chars().next().map_or(from, |c| from + c.len_utf8());
        }
        let mut pos = from;
        let mut seen_word = false;
        for (i, c) in after.char_indices() {
            if is_word(c) {
                seen_word = true;
            } else if seen_word {
                return from + i;
            }
            pos = from + i + c.len_utf8();
        }
        pos
    }

    pub fn backspace(&mut self, word: bool) {
        if self.has_selection() {
            self.snapshot(EditKind::None);
            self.delete_selection();
        } else if self.cursor > 0 {
            self.snapshot(EditKind::Deleting);
            let p = self.prev_boundary(self.cursor, word);
            self.text.replace_range(p..self.cursor, "");
            self.cursor = p;
            self.anchor = p;
        }
        self.changed();
    }

    pub fn delete(&mut self, word: bool) {
        if self.has_selection() {
            self.snapshot(EditKind::None);
            self.delete_selection();
        } else if self.cursor < self.text.len() {
            self.snapshot(EditKind::Deleting);
            let n = self.next_boundary(self.cursor, word);
            self.text.replace_range(self.cursor..n, "");
        }
        self.changed();
    }

    pub fn move_h(&mut self, right: bool, word: bool, select: bool) {
        let target = if !select && self.has_selection() {
            let (a, b) = self.selection();
            if right { b } else { a }
        } else if right {
            self.next_boundary(self.cursor, word)
        } else {
            self.prev_boundary(self.cursor, word)
        };
        self.cursor = target;
        if !select {
            self.anchor = target;
        }
        self.preferred_x = None;
        self.last_kind = EditKind::None;
    }

    pub fn home(&mut self, select: bool, whole: bool) {
        let target = if whole { 0 } else { self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1) };
        self.cursor = target;
        if !select {
            self.anchor = target;
        }
        self.last_kind = EditKind::None;
    }

    pub fn end(&mut self, select: bool, whole: bool) {
        let target = if whole {
            self.text.len()
        } else {
            self.text[self.cursor..].find('\n').map_or(self.text.len(), |i| self.cursor + i)
        };
        self.cursor = target;
        if !select {
            self.anchor = target;
        }
        self.last_kind = EditKind::None;
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.text.len();
    }

    pub fn undo(&mut self) {
        if let Some((t, c)) = self.undo.pop() {
            self.redo.push((std::mem::replace(&mut self.text, t), self.cursor));
            self.cursor = c.min(self.text.len());
            self.anchor = self.cursor;
            self.last_kind = EditKind::None;
            self.changed();
        }
    }

    pub fn redo(&mut self) {
        if let Some((t, c)) = self.redo.pop() {
            self.undo.push((std::mem::replace(&mut self.text, t), self.cursor));
            self.cursor = c.min(self.text.len());
            self.anchor = self.cursor;
            self.last_kind = EditKind::None;
            self.changed();
        }
    }

    /// Builds (or reuses) the layout for the current text at `width`.
    pub fn layout(&mut self, text: &Text, fmt: &IDWriteTextFormat, width: f32) -> &IDWriteTextLayout {
        let stale = match &self.layout {
            Some((_, _, w, v)) => *v != self.version || (*w - width).abs() > 0.5,
            None => true,
        };
        if stale {
            let display: String = if self.masked {
                self.text.chars().map(|_| '•').collect()
            } else {
                self.text.chars().map(display_char).collect()
            };
            // Keep a trailing newline visible as an empty last line.
            let wide: Vec<u16> = display.encode_utf16().collect();
            let w = if self.single_line { 100_000.0 } else { width };
            let l = text.layout_wide(&wide, fmt, w, 100_000.0);
            if self.single_line {
                unsafe {
                    let _ = l.SetWordWrapping(windows::Win32::Graphics::DirectWrite::DWRITE_WORD_WRAPPING_NO_WRAP);
                }
            }
            let map = if self.masked { U16Map::new(&display) } else { U16Map::new(&self.text) };
            self.layout = Some((l, map, width, self.version));
        }
        &self.layout.as_ref().unwrap().0
    }

    fn map(&self) -> Option<&U16Map> {
        self.layout.as_ref().map(|l| &l.1)
    }

    fn to_u16(&self, byte: usize) -> u32 {
        if self.masked {
            return self.text[..byte].chars().count() as u32;
        }
        self.map().map_or(0, |m| m.to_u16(byte as u32))
    }

    fn to_byte(&self, u: u32) -> usize {
        if self.masked {
            return self.text.char_indices().nth(u as usize).map_or(self.text.len(), |(i, _)| i);
        }
        self.map().map_or(0, |m| m.to_byte(u))
    }

    /// Caret rectangle relative to the layout origin: (x, y, height).
    pub fn caret(&self) -> (f32, f32, f32) {
        match &self.layout {
            Some((l, ..)) => text::caret_pos(l, self.to_u16(self.cursor)),
            None => (0.0, 0.0, 16.0),
        }
    }

    /// Selection rectangles relative to the layout origin.
    pub fn selection_rects(&self) -> Vec<(f32, f32, f32, f32)> {
        let Some((l, ..)) = &self.layout else { return Vec::new() };
        if !self.has_selection() {
            return Vec::new();
        }
        let (a, b) = self.selection();
        let (ua, ub) = (self.to_u16(a), self.to_u16(b));
        text::range_rects(l, ua, ub - ua)
    }

    /// Places the cursor at a point (relative to the layout origin).
    pub fn click(&mut self, x: f32, y: f32, select: bool) {
        let Some((l, ..)) = &self.layout else { return };
        let (u, _) = text::hit_point(l, x, y);
        self.cursor = self.to_byte(u);
        if !select {
            self.anchor = self.cursor;
        }
        self.preferred_x = None;
        self.last_kind = EditKind::None;
    }

    pub fn select_word_at_cursor(&mut self) {
        self.anchor = self.prev_boundary(self.cursor.min(self.text.len()), true);
        let start = if self.text[self.anchor..].starts_with(|c: char| is_word(c)) { self.anchor } else { self.cursor };
        self.anchor = start;
        self.cursor = self.next_boundary(self.cursor, true);
    }

    /// Moves the caret one visual line up/down. Returns false when already on the first/last line.
    pub fn move_v(&mut self, down: bool, select: bool) -> bool {
        let Some((l, ..)) = &self.layout else { return false };
        let (x, y, h) = text::caret_pos(l, self.to_u16(self.cursor));
        let total = text::metrics(l).height;
        let px = *self.preferred_x.get_or_insert(x);
        let ty = if down { y + h * 1.5 } else { y - h * 0.5 };
        if ty < 0.0 || ty > total {
            return false;
        }
        let (u, _) = text::hit_point(l, px, ty);
        self.cursor = self.to_byte(u);
        if !select {
            self.anchor = self.cursor;
        }
        self.last_kind = EditKind::None;
        true
    }

    /// Wraps the selection (or inserts at the caret) with a formatting code.
    pub fn toggle_format(&mut self, code: char) {
        if self.has_selection() {
            let (a, b) = self.selection();
            self.snapshot(EditKind::None);
            let mut s = String::new();
            s.push(code);
            self.text.insert(b, code);
            self.text.insert_str(a, &s);
            self.cursor = b + 2 * code.len_utf8();
            self.anchor = self.cursor;
            self.changed();
        } else {
            self.insert(&code.to_string());
        }
    }

    pub fn content_height(&self) -> f32 {
        self.layout.as_ref().map_or(0.0, |(l, ..)| text::metrics(l).height)
    }

    pub fn content_width(&self) -> f32 {
        self.layout.as_ref().map_or(0.0, |(l, ..)| text::metrics(l).widthIncludingTrailingWhitespace)
    }
}

/// The standard Windows editing keys, shared by every single-line text box (dialog fields,
/// quick switcher and filter boxes). Returns `false` for keys it does not handle. Password
/// boxes (`masked`) never put their text on the clipboard.
pub fn edit_key(
    ed: &mut Editor,
    v: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY,
    ctrl: bool,
    shift: bool,
    hwnd: windows::Win32::Foundation::HWND,
) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let letter = |c: u8| ctrl && v.0 == c as u16;
    let copyable = ed.has_selection() && !ed.masked;
    match v {
        _ if letter(b'A') => ed.select_all(),
        _ if letter(b'C') || (ctrl && v == VK_INSERT) => {
            if copyable {
                crate::win::set_clipboard(hwnd, ed.selected_text());
            }
        }
        _ if letter(b'X') || (shift && v == VK_DELETE) => {
            if copyable {
                crate::win::set_clipboard(hwnd, ed.selected_text());
                ed.backspace(false);
            }
        }
        _ if letter(b'V') || (shift && v == VK_INSERT) => {
            if let Some(t) = crate::win::get_clipboard(hwnd) {
                ed.insert(&t);
            }
        }
        _ if letter(b'Z') && shift => ed.redo(),
        _ if letter(b'Z') => ed.undo(),
        _ if letter(b'Y') => ed.redo(),
        VK_LEFT => ed.move_h(false, ctrl, shift),
        VK_RIGHT => ed.move_h(true, ctrl, shift),
        VK_HOME => ed.home(shift, true),
        VK_END => ed.end(shift, true),
        VK_BACK => ed.backspace(ctrl),
        VK_DELETE => ed.delete(ctrl),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_editing_keys() {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::Input::KeyboardAndMouse::*;
        let key = |c: u8| VIRTUAL_KEY(c as u16);
        let mut ed = Editor::single_line();
        ed.insert("hello");
        ed.insert(" world");
        assert!(edit_key(&mut ed, key(b'Z'), true, false, HWND::default()));
        assert_eq!(ed.text(), "hello");
        assert!(edit_key(&mut ed, key(b'Y'), true, false, HWND::default()));
        assert_eq!(ed.text(), "hello world");
        assert!(edit_key(&mut ed, key(b'Z'), true, false, HWND::default()));
        assert!(edit_key(&mut ed, key(b'Z'), true, true, HWND::default()), "Ctrl+Shift+Z redoes");
        assert_eq!(ed.text(), "hello world");
        assert!(edit_key(&mut ed, key(b'A'), true, false, HWND::default()));
        assert_eq!(ed.selected_text(), "hello world");
        assert!(edit_key(&mut ed, VK_BACK, false, false, HWND::default()));
        assert_eq!(ed.text(), "");
        assert!(!edit_key(&mut ed, VK_F5, false, false, HWND::default()), "other keys are left alone");
    }

    #[test]
    fn editing_and_undo() {
        let mut e = Editor::default();
        for c in "hello world".chars() {
            e.insert(&c.to_string());
        }
        assert_eq!(e.text(), "hello world");
        e.backspace(true);
        assert_eq!(e.text(), "hello ");
        e.undo();
        assert_eq!(e.text(), "hello world");
        // Typing is undone word by word.
        e.undo();
        assert_eq!(e.text(), "hello ");
        e.undo();
        assert_eq!(e.text(), "hello");
        e.redo();
        e.redo();
        assert_eq!(e.text(), "hello world");
    }

    #[test]
    fn word_moves() {
        let mut e = Editor::default();
        e.set_text("foo bar_baz  qux", None);
        e.move_h(false, true, false);
        assert_eq!(e.cursor, 13);
        e.move_h(false, true, false);
        assert_eq!(e.cursor, 4);
        e.move_h(true, true, true);
        assert_eq!(e.selected_text(), "bar_baz");
    }

    #[test]
    fn format_wrap_and_unicode() {
        let mut e = Editor::default();
        e.set_text("aé😀", None);
        e.backspace(false);
        assert_eq!(e.text(), "aé");
        e.select_all();
        e.toggle_format('\x02');
        assert_eq!(e.text(), "\x02aé\x02");
    }
}
