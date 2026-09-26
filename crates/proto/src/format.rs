//! mIRC-style formatting codes (<https://modern.ircdocs.horse/formatting>).

pub const BOLD: char = '\x02';
pub const COLOR: char = '\x03';
pub const HEX_COLOR: char = '\x04';
pub const RESET: char = '\x0f';
pub const MONOSPACE: char = '\x11';
pub const REVERSE: char = '\x16';
pub const ITALIC: char = '\x1d';
pub const STRIKETHROUGH: char = '\x1e';
pub const UNDERLINE: char = '\x1f';

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    /// mIRC palette index 0..=98 (99 means "default" and is represented as `None`).
    Index(u8),
    /// 0xRRGGBB
    Rgb(u32),
}

impl Color {
    /// Resolves to RGB using the standard palette. Themes usually override indices 0..=15.
    pub fn rgb(self) -> u32 {
        match self {
            Color::Index(i) => PALETTE.get(i as usize).copied().unwrap_or(0),
            Color::Rgb(c) => c,
        }
    }
}

/// The 99-entry mIRC palette (0..=15 classic, 16..=98 extended).
pub const PALETTE: [u32; 99] = [
    0xffffff, 0x000000, 0x00007f, 0x009300, 0xff0000, 0x7f0000, 0x9c009c, 0xfc7f00, 0xffff00, 0x00fc00, 0x009393,
    0x00ffff, 0x0000fc, 0xff00ff, 0x7f7f7f, 0xd2d2d2, //
    0x470000, 0x472100, 0x474700, 0x324700, 0x004700, 0x00472c, 0x004747, 0x002747, 0x000047, 0x2e0047, 0x470047,
    0x47002a, 0x740000, 0x743a00, 0x747400, 0x517400, 0x007400, 0x007449, 0x007474, 0x004074, 0x000074, 0x4b0074,
    0x740074, 0x740045, 0xb50000, 0xb56300, 0xb5b500, 0x7db500, 0x00b500, 0x00b571, 0x00b5b5, 0x0063b5, 0x0000b5,
    0x7500b5, 0xb500b5, 0xb5006b, 0xff0000, 0xff8c00, 0xffff00, 0xb2ff00, 0x00ff00, 0x00ffa0, 0x00ffff, 0x008cff,
    0x0000ff, 0xa500ff, 0xff00ff, 0xff0098, 0xff5959, 0xffb459, 0xffff71, 0xcfff60, 0x6fff6f, 0x65ffc9, 0x6dffff,
    0x59b4ff, 0x5959ff, 0xc459ff, 0xff66ff, 0xff59bc, 0xff9c9c, 0xffd39c, 0xffff9c, 0xe2ff9c, 0x9cff9c, 0x9cffdb,
    0x9cffff, 0x9cd3ff, 0x9c9cff, 0xdc9cff, 0xff9cff, 0xff94d3, 0x000000, 0x131313, 0x282828, 0x363636, 0x4d4d4d,
    0x656565, 0x818181, 0x9f9f9f, 0xbcbcbc, 0xe2e2e2, 0xffffff,
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub monospace: bool,
    pub reverse: bool,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
}

impl Style {
    pub fn is_plain(&self) -> bool {
        *self == Style::default()
    }
}

/// A run of text (byte range into [`Styled::text`]) sharing one style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub style: Style,
}

/// Text with formatting codes removed plus the style runs that applied to it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Styled {
    pub text: String,
    /// Non-overlapping, ordered runs. Plain runs are omitted.
    pub spans: Vec<Span>,
}

impl Styled {
    pub fn plain(text: impl Into<String>) -> Styled {
        Styled { text: text.into(), spans: Vec::new() }
    }
}

/// Parses formatting codes. Other C0 control characters (except tab) are dropped.
pub fn parse(input: &str) -> Styled {
    // Fast path: no control characters at all.
    if !input.bytes().any(|b| b < 0x20 && b != b'\t') {
        return Styled::plain(input);
    }
    let mut out = Styled { text: String::with_capacity(input.len()), spans: Vec::new() };
    let mut style = Style::default();
    let mut run_start = 0usize;
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut plain_from = 0;

    macro_rules! flush_text {
        () => {
            out.text.push_str(&input[plain_from..i]);
        };
    }
    macro_rules! change_style {
        ($new:expr) => {{
            let new: Style = $new;
            if new != style {
                let end = out.text.len();
                if end > run_start && !style.is_plain() {
                    out.spans.push(Span { start: run_start as u32, end: end as u32, style });
                }
                run_start = end;
                style = new;
            }
        }};
    }

    while i < bytes.len() {
        let b = bytes[i];
        if b >= 0x20 || b == b'\t' {
            i += 1;
            continue;
        }
        flush_text!();
        i += 1;
        let mut s = style;
        match b as char {
            BOLD => s.bold = !s.bold,
            ITALIC => s.italic = !s.italic,
            UNDERLINE => s.underline = !s.underline,
            STRIKETHROUGH => s.strikethrough = !s.strikethrough,
            MONOSPACE => s.monospace = !s.monospace,
            REVERSE => s.reverse = !s.reverse,
            RESET => s = Style::default(),
            COLOR => {
                let (fg, n) = read_digits(&bytes[i..]);
                if let Some(fg) = fg {
                    i += n;
                    s.fg = palette_color(fg);
                    if bytes.get(i) == Some(&b',') {
                        let (bg, n) = read_digits(&bytes[i + 1..]);
                        if let Some(bg) = bg {
                            i += 1 + n;
                            s.bg = palette_color(bg);
                        }
                    }
                } else {
                    s.fg = None;
                    s.bg = None;
                }
            }
            HEX_COLOR => {
                if let Some(fg) = read_hex(&bytes[i..]) {
                    i += 6;
                    s.fg = Some(Color::Rgb(fg));
                    if bytes.get(i) == Some(&b',')
                        && let Some(bg) = read_hex(&bytes[i + 1..])
                    {
                        i += 7;
                        s.bg = Some(Color::Rgb(bg));
                    }
                } else {
                    s.fg = None;
                    s.bg = None;
                }
            }
            _ => {}
        }
        change_style!(s);
        plain_from = i;
    }
    flush_text!();
    let end = out.text.len();
    if end > run_start && !style.is_plain() {
        out.spans.push(Span { start: run_start as u32, end: end as u32, style });
    }
    out
}

fn palette_color(n: u8) -> Option<Color> {
    if n >= 99 { None } else { Some(Color::Index(n)) }
}

fn read_digits(b: &[u8]) -> (Option<u8>, usize) {
    let n = b.iter().take(2).take_while(|c| c.is_ascii_digit()).count();
    if n == 0 {
        return (None, 0);
    }
    let v = b[..n].iter().fold(0u8, |acc, d| acc * 10 + (d - b'0'));
    (Some(v), n)
}

fn read_hex(b: &[u8]) -> Option<u32> {
    let h = b.get(..6)?;
    if !h.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    u32::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()
}

/// Removes all formatting codes.
pub fn strip(input: &str) -> String {
    parse(input).text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_fast_path() {
        let s = parse("hello world");
        assert_eq!(s.text, "hello world");
        assert!(s.spans.is_empty());
    }

    #[test]
    fn bold_and_color() {
        let s = parse("a\x02b\x034,12c\x0fd");
        assert_eq!(s.text, "abcd");
        assert_eq!(s.spans.len(), 2);
        assert_eq!(s.spans[0], Span { start: 1, end: 2, style: Style { bold: true, ..Default::default() } });
        let st = s.spans[1].style;
        assert!(st.bold);
        assert_eq!(st.fg, Some(Color::Index(4)));
        assert_eq!(st.bg, Some(Color::Index(12)));
        assert_eq!((s.spans[1].start, s.spans[1].end), (2, 3));
    }

    #[test]
    fn color_edge_cases() {
        // Comma without a digit is literal text.
        assert_eq!(parse("\x034,x").text, ",x");
        // Three digits: only two are consumed.
        let s = parse("\x03123");
        assert_eq!(s.text, "3");
        assert_eq!(s.spans[0].style.fg, Some(Color::Index(12)));
        // Bare \x03 resets colors.
        let s = parse("\x034a\x03b");
        assert_eq!(
            s.spans,
            vec![Span { start: 0, end: 1, style: Style { fg: Some(Color::Index(4)), ..Default::default() } }]
        );
        // 99 is "default".
        assert!(parse("\x0399a").spans.is_empty());
    }

    #[test]
    fn hex_color() {
        let s = parse("\x04ff8800,000000x");
        assert_eq!(s.text, "x");
        assert_eq!(s.spans[0].style.fg, Some(Color::Rgb(0xff8800)));
        assert_eq!(s.spans[0].style.bg, Some(Color::Rgb(0)));
    }

    #[test]
    fn strip_all() {
        assert_eq!(strip("\x02\x1dhi\x1f\x16 \x11there\x0f\x07"), "hi there");
    }
}
