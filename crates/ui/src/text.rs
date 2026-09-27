//! DirectWrite text formats, layouts and IRC formatting → layout styling.

use crate::gfx::Color;
use crate::theme::Theme;
use schwaetz_proto::format::{Span, Style};
use std::collections::HashMap;
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, ID2D1SolidColorBrush};
use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{HSTRING, Interface, PCWSTR, w};

pub struct Fonts {
    pub chat: IDWriteTextFormat,
    pub chat_bold: IDWriteTextFormat,
    /// Right-aligned, single line, ellipsis-trimmed (nick column).
    pub nick: IDWriteTextFormat,
    pub small: IDWriteTextFormat,
    pub ui: IDWriteTextFormat,
    pub ui_semibold: IDWriteTextFormat,
    pub ui_small: IDWriteTextFormat,
    pub title: IDWriteTextFormat,
    /// Symbol font for toolbar icons (Segoe Fluent Icons, or Segoe MDL2 Assets on Windows 10).
    pub icons: IDWriteTextFormat,
    pub mono_family: HSTRING,
    pub chat_size: f32,
    /// Height of one chat text line.
    pub line_height: f32,
    /// Average character width of the chat font (for the nick column).
    pub char_width: f32,
}

pub struct Text {
    pub dwrite: IDWriteFactory,
    pub fonts: Fonts,
}

fn format(
    dw: &IDWriteFactory,
    family: &HSTRING,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
    single_line: bool,
) -> windows::core::Result<IDWriteTextFormat> {
    let f = unsafe {
        dw.CreateTextFormat(
            PCWSTR(family.as_ptr()),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("en-us"),
        )?
    };
    if single_line {
        unsafe {
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let sign = dw.CreateEllipsisTrimmingSign(&f)?;
            let trimming =
                DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            f.SetTrimming(&trimming, &sign)?;
        }
    }
    Ok(f)
}

fn family_exists(dw: &IDWriteFactory, name: &str) -> bool {
    let mut coll = None;
    if unsafe { dw.GetSystemFontCollection(&mut coll, false) }.is_err() {
        return false;
    }
    let Some(coll) = coll else { return false };
    let mut idx = 0u32;
    let mut exists = windows::core::BOOL(0);
    let name = HSTRING::from(name);
    unsafe { coll.FindFamilyName(&name, &mut idx, &mut exists) }.is_ok() && exists.as_bool()
}

impl Text {
    pub fn new(
        dwrite: IDWriteFactory,
        chat_family: &str,
        chat_size: f32,
        ui_family: &str,
    ) -> windows::core::Result<Text> {
        let fonts = Self::make_fonts(&dwrite, chat_family, chat_size, ui_family)?;
        Ok(Text { dwrite, fonts })
    }

    pub fn reconfigure(&mut self, chat_family: &str, chat_size: f32, ui_family: &str) -> windows::core::Result<()> {
        self.fonts = Self::make_fonts(&self.dwrite, chat_family, chat_size, ui_family)?;
        Ok(())
    }

    fn make_fonts(
        dw: &IDWriteFactory,
        chat_family: &str,
        chat_size: f32,
        ui_family: &str,
    ) -> windows::core::Result<Fonts> {
        let pick = |want: &str, fallbacks: &[&str]| -> HSTRING {
            std::iter::once(want)
                .chain(fallbacks.iter().copied())
                .find(|f| family_exists(dw, f))
                .map(HSTRING::from)
                .unwrap_or_else(|| HSTRING::from("Segoe UI"))
        };
        let chat_f = pick(chat_family, &["Segoe UI Variable Text", "Segoe UI"]);
        let ui_f = pick(ui_family, &["Segoe UI Variable Text", "Segoe UI"]);
        let mono = pick("Cascadia Mono", &["Consolas", "Courier New"]);
        let size = chat_size.clamp(8.0, 40.0);
        let chat = format(dw, &chat_f, size, DWRITE_FONT_WEIGHT_NORMAL, false)?;
        let chat_bold = format(dw, &chat_f, size, DWRITE_FONT_WEIGHT_SEMI_BOLD, true)?;
        let nick = format(dw, &chat_f, size, DWRITE_FONT_WEIGHT_SEMI_BOLD, true)?;
        unsafe { nick.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)? };
        let small = format(dw, &chat_f, (size * 0.82).round(), DWRITE_FONT_WEIGHT_NORMAL, true)?;
        let ui = format(dw, &ui_f, 13.0, DWRITE_FONT_WEIGHT_NORMAL, true)?;
        let ui_semibold = format(dw, &ui_f, 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, true)?;
        let ui_small = format(dw, &ui_f, 11.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, true)?;
        let title = format(dw, &ui_f, 15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, true)?;
        let icon_f = pick("Segoe Fluent Icons", &["Segoe MDL2 Assets"]);
        let icons = format(dw, &icon_f, 15.0, DWRITE_FONT_WEIGHT_NORMAL, true)?;

        // Measure line height and average glyph width with a sample layout.
        let sample: Vec<u16> = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ".encode_utf16().collect();
        let l = unsafe { dw.CreateTextLayout(&sample, &chat, 10_000.0, 1000.0)? };
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe { l.GetMetrics(&mut m)? };
        Ok(Fonts {
            chat,
            chat_bold,
            nick,
            small,
            ui,
            ui_semibold,
            ui_small,
            title,
            icons,
            mono_family: mono,
            chat_size: size,
            line_height: m.height.max(size * 1.2),
            char_width: m.widthIncludingTrailingWhitespace / sample.len() as f32,
        })
    }

    pub fn layout(&self, text: &str, fmt: &IDWriteTextFormat, max_w: f32, max_h: f32) -> IDWriteTextLayout {
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe { self.dwrite.CreateTextLayout(&wide, fmt, max_w.max(1.0), max_h.max(1.0)).expect("text layout") }
    }

    pub fn layout_wide(&self, wide: &[u16], fmt: &IDWriteTextFormat, max_w: f32, max_h: f32) -> IDWriteTextLayout {
        unsafe { self.dwrite.CreateTextLayout(wide, fmt, max_w.max(1.0), max_h.max(1.0)).expect("text layout") }
    }
}

pub fn metrics(l: &IDWriteTextLayout) -> DWRITE_TEXT_METRICS {
    let mut m = DWRITE_TEXT_METRICS::default();
    unsafe {
        let _ = l.GetMetrics(&mut m);
    }
    m
}

/// Maps between UTF-8 byte offsets and UTF-16 code-unit offsets for one string.
pub struct U16Map {
    /// (byte offset, utf16 offset) for every char boundary, plus the end.
    pts: Vec<(u32, u32)>,
}

impl U16Map {
    pub fn new(s: &str) -> U16Map {
        let mut pts = Vec::with_capacity(s.len() + 1);
        let mut u = 0u32;
        for (b, c) in s.char_indices() {
            pts.push((b as u32, u));
            u += c.len_utf16() as u32;
        }
        pts.push((s.len() as u32, u));
        U16Map { pts }
    }

    pub fn to_u16(&self, byte: u32) -> u32 {
        match self.pts.binary_search_by_key(&byte, |p| p.0) {
            Ok(i) => self.pts[i].1,
            Err(i) => self.pts[i.saturating_sub(1)].1,
        }
    }

    pub fn to_byte(&self, u: u32) -> usize {
        match self.pts.binary_search_by_key(&u, |p| p.1) {
            Ok(i) => self.pts[i].0 as usize,
            Err(i) => self.pts[i.saturating_sub(1)].0 as usize,
        }
    }

    pub fn len_u16(&self) -> u32 {
        self.pts.last().map_or(0, |p| p.1)
    }
}

/// Solid color brushes used as DirectWrite drawing effects, keyed by RGBA.
#[derive(Default)]
pub struct Brushes {
    map: HashMap<u32, ID2D1SolidColorBrush>,
    generation: u64,
}

fn pack(c: Color) -> u32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    q(c.r) << 24 | q(c.g) << 16 | q(c.b) << 8 | q(c.a)
}

impl Brushes {
    pub fn get(&mut self, ctx: &ID2D1DeviceContext, generation: u64, c: Color) -> ID2D1SolidColorBrush {
        if self.generation != generation {
            self.map.clear();
            self.generation = generation;
        }
        self.map
            .entry(pack(c))
            .or_insert_with(|| unsafe { ctx.CreateSolidColorBrush(&c, None).expect("brush") })
            .clone()
    }
}

/// A background run to paint behind text (mIRC background colors, reverse video).
pub struct BgRun {
    pub start: u32,
    pub len: u32,
    pub color: Color,
}

pub fn range(start: u32, len: u32) -> DWRITE_TEXT_RANGE {
    DWRITE_TEXT_RANGE { startPosition: start, length: len }
}

/// Applies IRC formatting spans to a layout. Returns background runs (UTF-16 ranges).
#[allow(clippy::too_many_arguments)]
pub fn apply_spans(
    layout: &IDWriteTextLayout,
    spans: &[Span],
    map: &U16Map,
    theme: &Theme,
    brushes: &mut Brushes,
    ctx: &ID2D1DeviceContext,
    generation: u64,
    fonts: &Fonts,
    colors: bool,
) -> Vec<BgRun> {
    let mut bgs = Vec::new();
    for sp in spans {
        let a = map.to_u16(sp.start);
        let b = map.to_u16(sp.end);
        if b <= a {
            continue;
        }
        let r = range(a, b - a);
        let st: Style = sp.style;
        unsafe {
            if st.bold {
                let _ = layout.SetFontWeight(DWRITE_FONT_WEIGHT_BOLD, r);
            }
            if st.italic {
                let _ = layout.SetFontStyle(DWRITE_FONT_STYLE_ITALIC, r);
            }
            if st.underline {
                let _ = layout.SetUnderline(true, r);
            }
            if st.strikethrough {
                let _ = layout.SetStrikethrough(true, r);
            }
            if st.monospace {
                let _ = layout.SetFontFamilyName(PCWSTR(fonts.mono_family.as_ptr()), r);
            }
        }
        if !colors {
            continue;
        }
        let mut bg = st.bg.map(|c| theme.mirc_raw(c));
        // Foregrounds are made readable against their actual background.
        let mut fg = st.fg.map(|c| crate::theme::ensure_contrast(theme.mirc_raw(c), bg.unwrap_or(theme.chat_bg), 3.0));
        if st.reverse {
            let f = fg.unwrap_or(theme.text);
            let b = bg.unwrap_or(theme.chat_bg);
            fg = Some(b);
            bg = Some(f);
        }
        if let Some(fg) = fg {
            let brush = brushes.get(ctx, generation, fg);
            unsafe {
                let _ = layout.SetDrawingEffect(&brush.cast::<windows::core::IUnknown>().unwrap(), r);
            }
        }
        if let Some(bg) = bg {
            bgs.push(BgRun { start: a, len: b - a, color: bg });
        }
    }
    bgs
}

pub fn set_color(layout: &IDWriteTextLayout, brush: &ID2D1SolidColorBrush, start: u32, len: u32) {
    unsafe {
        let _ = layout.SetDrawingEffect(&brush.cast::<windows::core::IUnknown>().unwrap(), range(start, len));
    }
}

/// Rectangles covering a UTF-16 range of a layout (relative to the layout origin).
pub fn range_rects(layout: &IDWriteTextLayout, start: u32, len: u32) -> Vec<(f32, f32, f32, f32)> {
    let mut count = 0u32;
    unsafe {
        let _ = layout.HitTestTextRange(start, len, 0.0, 0.0, None, &mut count);
    }
    if count == 0 {
        return Vec::new();
    }
    let mut buf = vec![DWRITE_HIT_TEST_METRICS::default(); count as usize];
    unsafe {
        if layout.HitTestTextRange(start, len, 0.0, 0.0, Some(&mut buf), &mut count).is_err() {
            return Vec::new();
        }
    }
    buf.truncate(count as usize);
    buf.iter().map(|m| (m.left, m.top, m.width, m.height)).collect()
}

/// UTF-16 position under a point, and whether the point is actually inside the text.
pub fn hit_point(layout: &IDWriteTextLayout, x: f32, y: f32) -> (u32, bool) {
    let mut trailing = windows::core::BOOL(0);
    let mut inside = windows::core::BOOL(0);
    let mut m = DWRITE_HIT_TEST_METRICS::default();
    unsafe {
        let _ = layout.HitTestPoint(x, y, &mut trailing, &mut inside, &mut m);
    }
    (m.textPosition + if trailing.as_bool() { m.length } else { 0 }, inside.as_bool())
}

/// Caret position (x, y, height) for a UTF-16 index.
pub fn caret_pos(layout: &IDWriteTextLayout, pos: u32) -> (f32, f32, f32) {
    let (mut x, mut y) = (0.0f32, 0.0f32);
    let mut m = DWRITE_HIT_TEST_METRICS::default();
    unsafe {
        let _ = layout.HitTestTextPosition(pos, false, &mut x, &mut y, &mut m);
    }
    (x, m.top, m.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u16_map() {
        let s = "a😀bé";
        let m = U16Map::new(s);
        assert_eq!(m.to_u16(0), 0);
        assert_eq!(m.to_u16(1), 1);
        assert_eq!(m.to_u16(5), 3); // after the emoji (2 code units)
        assert_eq!(m.to_byte(3), 5);
        assert_eq!(m.len_u16(), 5);
    }
}
