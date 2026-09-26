//! Colors. Built-in dark and light themes; user themes are TOML files overriding any subset.

use crate::gfx::{Color, hex, with_alpha};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Theme {
    pub dark: bool,
    /// Painted where the Mica backdrop shows through (alpha < 1).
    pub backdrop: Color,
    /// Used instead of `backdrop` when Mica is unavailable/disabled.
    pub backdrop_opaque: Color,
    pub sidebar_fg: Color,
    pub sidebar_dim: Color,
    pub sidebar_header: Color,
    pub sidebar_selected: Color,
    pub sidebar_hover: Color,
    pub chat_bg: Color,
    pub text: Color,
    pub text_dim: Color,
    pub timestamp: Color,
    pub accent: Color,
    pub accent_fg: Color,
    pub highlight_bg: Color,
    pub highlight_bar: Color,
    pub selection: Color,
    pub link: Color,
    pub error: Color,
    pub join: Color,
    pub part: Color,
    pub notice: Color,
    pub own_nick: Color,
    pub border: Color,
    pub input_bg: Color,
    pub topic_bg: Color,
    pub nicklist_bg: Color,
    pub badge_bg: Color,
    pub badge_fg: Color,
    pub badge_highlight: Color,
    pub scrollbar: Color,
    pub unread_marker: Color,
    pub overlay_scrim: Color,
    pub panel_bg: Color,
    pub online: Color,
    pub connecting: Color,
    pub offline: Color,
    pub nick_colors: Vec<Color>,
    /// mIRC colors 0..=15 as rendered in this theme.
    pub mirc: [Color; 16],
}

impl Theme {
    pub fn dark() -> Theme {
        Theme {
            dark: true,
            backdrop: with_alpha(hex(0x1b1c1f), 0.55),
            backdrop_opaque: hex(0x1b1c1f),
            sidebar_fg: hex(0xd5d7dc),
            sidebar_dim: hex(0x8a8e97),
            sidebar_header: hex(0xeceef2),
            sidebar_selected: with_alpha(hex(0xffffff), 0.09),
            sidebar_hover: with_alpha(hex(0xffffff), 0.05),
            chat_bg: hex(0x222327),
            text: hex(0xdfe1e6),
            text_dim: hex(0x8c909a),
            timestamp: hex(0x6d717b),
            accent: hex(0x7aa2f7),
            accent_fg: hex(0x10131a),
            highlight_bg: with_alpha(hex(0xe0af68), 0.12),
            highlight_bar: hex(0xe0af68),
            selection: with_alpha(hex(0x7aa2f7), 0.35),
            link: hex(0x7dcfff),
            error: hex(0xf7768e),
            join: hex(0x73b97e),
            part: hex(0xc27a7a),
            notice: hex(0xbb9af7),
            own_nick: hex(0x7aa2f7),
            border: with_alpha(hex(0xffffff), 0.07),
            input_bg: hex(0x2a2b30),
            topic_bg: hex(0x222327),
            nicklist_bg: hex(0x1f2024),
            badge_bg: with_alpha(hex(0xffffff), 0.14),
            badge_fg: hex(0xe6e8ec),
            badge_highlight: hex(0xe0af68),
            scrollbar: with_alpha(hex(0xffffff), 0.18),
            unread_marker: hex(0xf7768e),
            overlay_scrim: with_alpha(hex(0x000000), 0.45),
            panel_bg: hex(0x2b2c31),
            online: hex(0x73b97e),
            connecting: hex(0xe0af68),
            offline: hex(0x6d717b),
            nick_colors: [
                0xe06c75, 0xe5c07b, 0x98c379, 0x56b6c2, 0x61afef, 0xc678dd, 0xd19a66, 0x7ec8a9, 0xf2a2c0, 0x9aa5ff,
                0xffb86c, 0x8be9fd, 0xb8e986, 0xff79c6, 0xa3be8c, 0x88c0d0,
            ]
            .into_iter()
            .map(hex)
            .collect(),
            mirc: mirc_default(),
        }
    }

    pub fn light() -> Theme {
        Theme {
            dark: false,
            backdrop: with_alpha(hex(0xf3f3f3), 0.5),
            backdrop_opaque: hex(0xefeff1),
            sidebar_fg: hex(0x2c2e33),
            sidebar_dim: hex(0x6b6f78),
            sidebar_header: hex(0x16181c),
            sidebar_selected: with_alpha(hex(0x000000), 0.07),
            sidebar_hover: with_alpha(hex(0x000000), 0.04),
            chat_bg: hex(0xfcfcfd),
            text: hex(0x1f2328),
            text_dim: hex(0x6b6f78),
            timestamp: hex(0x8b8f98),
            accent: hex(0x2f6fdf),
            accent_fg: hex(0xffffff),
            highlight_bg: with_alpha(hex(0xf2b300), 0.14),
            highlight_bar: hex(0xd49b00),
            selection: with_alpha(hex(0x2f6fdf), 0.25),
            link: hex(0x0b63c9),
            error: hex(0xc4314b),
            join: hex(0x2f8a45),
            part: hex(0xa3453c),
            notice: hex(0x7a4fc9),
            own_nick: hex(0x2f6fdf),
            border: with_alpha(hex(0x000000), 0.08),
            input_bg: hex(0xffffff),
            topic_bg: hex(0xfcfcfd),
            nicklist_bg: hex(0xf6f6f8),
            badge_bg: with_alpha(hex(0x000000), 0.10),
            badge_fg: hex(0x2c2e33),
            badge_highlight: hex(0xd49b00),
            scrollbar: with_alpha(hex(0x000000), 0.22),
            unread_marker: hex(0xd6334d),
            overlay_scrim: with_alpha(hex(0x000000), 0.25),
            panel_bg: hex(0xffffff),
            online: hex(0x2f8a45),
            connecting: hex(0xc58a00),
            offline: hex(0x9a9ea6),
            nick_colors: [
                0xb3261e, 0x9a6700, 0x2f7d32, 0x00796b, 0x1565c0, 0x7b1fa2, 0xbf5b04, 0x2e7d67, 0xad1457, 0x4550c4,
                0xa05a00, 0x00838f, 0x558b2f, 0xc2185b, 0x5d7a3a, 0x3f6f87,
            ]
            .into_iter()
            .map(hex)
            .collect(),
            mirc: mirc_default(),
        }
    }

    /// Color for a nick, stable across sessions, readable on the chat background.
    pub fn nick_color(&self, nick: &str) -> Color {
        let mut h: u32 = 0x811c9dc5;
        for b in nick.to_lowercase().bytes() {
            h ^= b as u32;
            h = h.wrapping_mul(0x01000193);
        }
        self.nick_colors[(h as usize) % self.nick_colors.len()]
    }

    /// A formatting color as rendered in this theme, without contrast correction.
    pub fn mirc_raw(&self, c: schwaetz_proto::format::Color) -> Color {
        match c {
            schwaetz_proto::format::Color::Index(i) if (i as usize) < 16 => self.mirc[i as usize],
            other => hex(other.rgb()),
        }
    }

    /// Resolves a foreground formatting color, correcting contrast against the chat background.
    pub fn mirc_color(&self, c: schwaetz_proto::format::Color) -> Color {
        ensure_contrast(self.mirc_raw(c), self.chat_bg, 3.0)
    }

    /// Loads a theme file, starting from the built-in base it declares (`base = "dark"|"light"`).
    pub fn from_toml(text: &str) -> Result<Theme, String> {
        #[derive(Deserialize)]
        struct File {
            #[serde(default)]
            base: Option<String>,
            #[serde(default)]
            colors: HashMap<String, String>,
            #[serde(default)]
            nick_colors: Vec<String>,
            #[serde(default)]
            mirc: Vec<String>,
        }
        let f: File = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut t = if f.base.as_deref() == Some("light") { Theme::light() } else { Theme::dark() };
        for (k, v) in &f.colors {
            let c = parse_color(v).ok_or_else(|| format!("invalid color {v:?} for {k}"))?;
            let slot = match k.as_str() {
                "backdrop" => &mut t.backdrop,
                "backdrop_opaque" => &mut t.backdrop_opaque,
                "sidebar_fg" => &mut t.sidebar_fg,
                "sidebar_dim" => &mut t.sidebar_dim,
                "sidebar_header" => &mut t.sidebar_header,
                "sidebar_selected" => &mut t.sidebar_selected,
                "sidebar_hover" => &mut t.sidebar_hover,
                "chat_bg" => &mut t.chat_bg,
                "text" => &mut t.text,
                "text_dim" => &mut t.text_dim,
                "timestamp" => &mut t.timestamp,
                "accent" => &mut t.accent,
                "accent_fg" => &mut t.accent_fg,
                "highlight_bg" => &mut t.highlight_bg,
                "highlight_bar" => &mut t.highlight_bar,
                "selection" => &mut t.selection,
                "link" => &mut t.link,
                "error" => &mut t.error,
                "join" => &mut t.join,
                "part" => &mut t.part,
                "notice" => &mut t.notice,
                "own_nick" => &mut t.own_nick,
                "border" => &mut t.border,
                "input_bg" => &mut t.input_bg,
                "topic_bg" => &mut t.topic_bg,
                "nicklist_bg" => &mut t.nicklist_bg,
                "badge_bg" => &mut t.badge_bg,
                "badge_fg" => &mut t.badge_fg,
                "badge_highlight" => &mut t.badge_highlight,
                "scrollbar" => &mut t.scrollbar,
                "unread_marker" => &mut t.unread_marker,
                "overlay_scrim" => &mut t.overlay_scrim,
                "panel_bg" => &mut t.panel_bg,
                "online" => &mut t.online,
                "connecting" => &mut t.connecting,
                "offline" => &mut t.offline,
                _ => return Err(format!("unknown color key {k:?}")),
            };
            *slot = c;
        }
        if !f.nick_colors.is_empty() {
            t.nick_colors = f.nick_colors.iter().filter_map(|c| parse_color(c)).collect();
            if t.nick_colors.is_empty() {
                return Err("nick_colors has no valid colors".into());
            }
        }
        for (i, c) in f.mirc.iter().take(16).enumerate() {
            if let Some(c) = parse_color(c) {
                t.mirc[i] = c;
            }
        }
        Ok(t)
    }
}

fn mirc_default() -> [Color; 16] {
    let mut out = [hex(0); 16];
    for (i, c) in schwaetz_proto::format::PALETTE[..16].iter().enumerate() {
        out[i] = hex(*c);
    }
    out
}

/// `#rrggbb` or `#rrggbbaa`.
pub fn parse_color(s: &str) -> Option<Color> {
    let h = s.trim().strip_prefix('#')?;
    match h.len() {
        6 => u32::from_str_radix(h, 16).ok().map(hex),
        8 => {
            let v = u32::from_str_radix(h, 16).ok()?;
            Some(with_alpha(hex(v >> 8), (v & 0xff) as f32 / 255.0))
        }
        _ => None,
    }
}

fn channel_lum(c: f32) -> f32 {
    if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

pub fn luminance(c: Color) -> f32 {
    0.2126 * channel_lum(c.r) + 0.7152 * channel_lum(c.g) + 0.0722 * channel_lum(c.b)
}

pub fn contrast(a: Color, b: Color) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Lightens or darkens `fg` until it reaches `min` contrast against `bg`.
pub fn ensure_contrast(fg: Color, bg: Color, min: f32) -> Color {
    if contrast(fg, bg) >= min {
        return fg;
    }
    let toward_white = luminance(bg) < 0.4;
    let mut c = fg;
    for _ in 0..20 {
        c = if toward_white {
            Color { r: c.r + (1.0 - c.r) * 0.15, g: c.g + (1.0 - c.g) * 0.15, b: c.b + (1.0 - c.b) * 0.15, a: c.a }
        } else {
            Color { r: c.r * 0.85, g: c.g * 0.85, b: c.b * 0.85, a: c.a }
        };
        if contrast(c, bg) >= min {
            break;
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_fix() {
        let t = Theme::dark();
        let navy = t.mirc_color(schwaetz_proto::format::Color::Index(2));
        assert!(contrast(navy, t.chat_bg) >= 3.0);
        for c in &t.nick_colors {
            assert!(contrast(*c, t.chat_bg) >= 3.0, "{c:?}");
        }
        let l = Theme::light();
        for c in &l.nick_colors {
            assert!(contrast(*c, l.chat_bg) >= 3.0, "{c:?}");
        }
    }

    #[test]
    fn theme_file() {
        let t =
            Theme::from_toml("base = \"light\"\n[colors]\naccent = \"#ff0000\"\nselection = \"#00ff0080\"\n").unwrap();
        assert!(!t.dark);
        assert_eq!(t.accent.r, 1.0);
        assert!((t.selection.a - 0.5).abs() < 0.01);
        assert!(Theme::from_toml("[colors]\nbogus = \"#000000\"").is_err());
    }
}
