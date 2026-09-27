//! The chat view: a virtualized, bottom-anchored list of buffer lines.

use crate::gfx::{Painter, Rect, with_alpha};
use crate::text::{self, BgRun, Brushes, Text, U16Map};
use crate::theme::Theme;
use schwaetz_core::buffer::{Buffer, BufferId, Line, LineFlags, LineKind};
use schwaetz_core::time;
use std::collections::HashMap;
use windows::Win32::Graphics::Direct2D::ID2D1DeviceContext;
use windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;
use windows::core::Interface;

pub const PAD_X: f32 = 14.0;
const PAD_BOTTOM: f32 = 8.0;
const LINE_GAP: f32 = 3.0;
const DAY_SEP_H: f32 = 30.0;
const MARKER_H: f32 = 16.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    Url(String),
    Channel(String),
}

#[derive(Clone)]
struct Link {
    start: u32,
    len: u32,
    target: LinkTarget,
}

struct Cached {
    msg: IDWriteTextLayout,
    msg_h: f32,
    nick: Option<IDWriteTextLayout>,
    ts: IDWriteTextLayout,
    reply: Option<IDWriteTextLayout>,
    reactions: Option<IDWriteTextLayout>,
    links: Vec<Link>,
    bgs: Vec<BgRun>,
    /// Placed inline emotes: (utf16 start, utf16 len, code, image url).
    emotes: Vec<(u32, u32, String, String)>,
    /// Plain text actually laid out in `msg` (for copying).
    plain: String,
    map: U16Map,
    height: f32,
    card: Option<Card>,
    /// Content signature; a mismatch (deleted, decorated, folded …) forces a re-layout.
    sig: u64,
    used: u64,
}

/// A link preview below a message: either a "Show preview" chip or a card.
struct Card {
    url: String,
    w: f32,
    h: f32,
    chip: bool,
    image: Option<(String, f32, f32)>,
    title: Option<IDWriteTextLayout>,
    desc: Option<IDWriteTextLayout>,
}

/// A line as drawn in the last frame (for hit testing).
struct Drawn {
    id: u64,
    /// "Reply" button shown on the hovered line.
    reply_btn: Option<Rect>,
    y: f32,
    h: f32,
    msg_x: f32,
    msg_y: f32,
    nick_rect: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pos {
    pub line: u64,
    pub u16: u32,
}

pub struct Metrics {
    pub ts_w: f32,
    pub nick_w: f32,
    pub msg_x: f32,
}

/// What a point in the chat view refers to.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    Link(LinkTarget),
    /// The reply button of a line.
    Reply(u64),
    /// The "Show preview" chip of a link.
    LoadPreview(String),
    Nick(String),
    Text(Pos),
    Nothing,
}

pub struct ChatView {
    pub rect: Rect,
    buffer: Option<BufferId>,
    pub pinned: bool,
    anchor: Option<u64>,
    /// Bottom edge of the anchor line, relative to `rect.y`.
    anchor_y: f32,
    cache: HashMap<u64, Cached>,
    /// Chat width, nick column width, graphics and style generation the cache was built for.
    cache_key: (u32, u32, u64, u64),
    frame: u64,
    drawn: Vec<Drawn>,
    pub selection: Option<(Pos, Pos)>,
    pub selecting: bool,
    /// Unread-marker snapshot taken when the buffer was opened.
    pub marker: Option<i64>,
    pub show_filtered: bool,
    /// Set when the top of the buffer came into view (load older history).
    pub wants_older: bool,
    pub style_gen: u64,
    /// Line under the mouse (hover affordances).
    pub hover: Option<u64>,
    /// Line id → position in the buffer, refreshed while a selection exists.
    order: HashMap<u64, usize>,
    /// Line id → sender nick for drawn lines (nick-column clicks).
    nicks: HashMap<u64, String>,
    /// The nick column fitted to the buffer's names (`Ctx::nick_column_auto`).
    nick_fit: NickFit,
    /// Measured widths of nick column texts (badges, status prefix and name).
    nick_widths: HashMap<String, f32>,
    /// Scrollbar as last drawn (only while the lines don't all fit).
    pub bar: Option<crate::scrollbar::Scrollbar>,
    /// The scrollbar is hovered or dragged (drawn wider).
    pub bar_hot: bool,
    /// Lines that fit above the bottom one in the last frame (maps scrollbar positions to lines).
    per_view: usize,
}

/// Width of the widest nick column text in a buffer, kept up to date as lines arrive.
#[derive(Default)]
struct NickFit {
    /// Buffer, graphics generation, style generation and filter setting it was measured for.
    key: (Option<BufferId>, u64, u64, bool),
    /// Buffer generation last scanned.
    seen: Option<u64>,
    /// Lines up to this id were measured.
    upto: u64,
    width: f32,
}

pub struct Ctx<'a> {
    pub text: &'a Text,
    /// Hover/press transitions of the Reply and "Jump to latest" buttons.
    pub anims: &'a crate::anim::Anims,
    pub theme: &'a Theme,
    pub brushes: &'a mut Brushes,
    pub dc: &'a ID2D1DeviceContext,
    pub gfx_gen: u64,
    pub ts_format: &'a str,
    pub nick_column: bool,
    /// Fit the nick column to the names in the buffer instead of `nick_column_chars`.
    pub nick_column_auto: bool,
    pub nick_column_chars: u32,
    pub colors: bool,
    pub colored_nicks: bool,
    /// The active network shows Twitch users' own name colors.
    pub twitch_colors: bool,
    pub images: &'a std::rc::Rc<std::cell::RefCell<crate::images::ImageStore>>,
    /// Link previews enabled for this buffer.
    pub previews: bool,
    /// Load previews without a click (subject to `allow_hosts`).
    pub preview_auto: bool,
    pub allow_hosts: &'a [String],
    /// Messages in this buffer can be replied to.
    pub replies: bool,
}

impl Default for ChatView {
    fn default() -> Self {
        ChatView {
            rect: Rect::default(),
            buffer: None,
            pinned: true,
            anchor: None,
            anchor_y: 0.0,
            cache: HashMap::new(),
            cache_key: (0, 0, 0, 0),
            frame: 0,
            drawn: Vec::new(),
            selection: None,
            selecting: false,
            marker: None,
            show_filtered: false,
            wants_older: false,
            style_gen: 0,
            hover: None,
            order: HashMap::new(),
            nicks: HashMap::new(),
            nick_fit: NickFit::default(),
            nick_widths: HashMap::new(),
            bar: None,
            bar_hot: false,
            per_view: 0,
        }
    }
}

fn visible(l: &Line, show_filtered: bool) -> bool {
    show_filtered || !l.flags.has(LineFlags::FILTERED)
}

impl ChatView {
    /// Switches to another buffer, snapshotting its read marker for the "new messages" line.
    pub fn set_buffer(&mut self, id: BufferId, marker: Option<i64>) {
        if self.buffer != Some(id) {
            self.buffer = Some(id);
            self.pinned = true;
            self.anchor = None;
            self.selection = None;
            self.marker = marker;
            self.cache.clear();
        }
    }

    pub fn invalidate_styles(&mut self) {
        self.cache.clear();
    }

    pub fn metrics(&self, c: &Ctx) -> Metrics {
        let f = &c.text.fonts;
        let sample = time::format(c.ts_format, time::breakdown(0));
        let ts_w = if c.ts_format.is_empty() { 0.0 } else { sample.chars().count() as f32 * f.char_width * 0.95 + 4.0 };
        let nick_w = if !c.nick_column {
            0.0
        } else if c.nick_column_auto {
            // Room for the longest name, but never most of the pane (longer ones get an ellipsis).
            let max = (self.rect.w * 0.4).max(f.char_width * 8.0);
            (self.nick_fit.width.ceil() + 1.0).clamp(f.char_width * 3.0, max)
        } else {
            c.nick_column_chars.clamp(4, 40) as f32 * f.char_width
        };
        let msg_x = PAD_X + ts_w + if c.nick_column { nick_w + 12.0 } else { 6.0 };
        Metrics { ts_w, nick_w, msg_x }
    }

    fn layout_line(&self, c: &mut Ctx, line: &Line, m: &Metrics) -> Cached {
        let f = &c.text.fonts;
        let th = c.theme;
        let msg_w = (self.rect.w - m.msg_x - PAD_X).max(40.0);
        let deleted = line.flags.has(LineFlags::DELETED);
        let is_msg = matches!(line.kind, LineKind::Message | LineKind::Action | LineKind::Notice);
        let parsed = if is_msg || line.kind == LineKind::Motd || line.kind == LineKind::System {
            schwaetz_proto::format::parse(&line.text)
        } else {
            schwaetz_proto::format::Styled::plain(line.text.to_string())
        };
        let nick_display = line.display_nick().to_owned();
        let nick_color = self.nick_color(c, line);

        // Compose the message text; actions and inline-nick mode prefix the nick.
        let mut prefix = String::new();
        if line.kind == LineKind::Action {
            prefix = format!("{nick_display} ");
        } else if !c.nick_column && is_msg {
            prefix = match line.kind {
                LineKind::Notice => format!("-{nick_display}- "),
                _ => format!("{nick_display}: "),
            };
        }
        let body = if deleted && is_msg && parsed.text.is_empty() {
            "<message deleted>".to_owned()
        } else {
            parsed.text.clone()
        };
        let plain = format!("{prefix}{body}");
        let map = U16Map::new(&plain);
        let fmt = &f.chat;
        let msg = c.text.layout(&plain, fmt, msg_w, 100_000.0);
        let off = prefix.len() as u32;

        // Base color by line kind.
        let base = match line.kind {
            LineKind::Join => Some(with_alpha(th.join, 0.9)),
            LineKind::Part | LineKind::Quit | LineKind::Kick | LineKind::Netsplit => Some(with_alpha(th.part, 0.9)),
            LineKind::Nick
            | LineKind::Mode
            | LineKind::Topic
            | LineKind::Invite
            | LineKind::Server
            | LineKind::Ctcp => Some(th.text_dim),
            LineKind::Motd => Some(th.text_dim),
            LineKind::Error => Some(th.error),
            LineKind::Status => Some(th.text_dim),
            LineKind::System => Some(th.accent),
            LineKind::Notice if !c.nick_column => Some(th.notice),
            _ => None,
        };
        let base = if deleted { Some(with_alpha(th.text_dim, 0.7)) } else { base };
        if let Some(col) = base {
            let br = c.brushes.get(c.dc, c.gfx_gen, col);
            text::set_color(&msg, &br, 0, map.len_u16());
        }
        if deleted {
            unsafe {
                let _ = msg.SetStrikethrough(true, text::range(0, map.len_u16()));
            }
        }
        if line.kind == LineKind::Motd {
            unsafe {
                let _ =
                    msg.SetFontFamilyName(windows::core::PCWSTR(f.mono_family.as_ptr()), text::range(0, map.len_u16()));
            }
        }
        if !prefix.is_empty() {
            let br = c.brushes.get(c.dc, c.gfx_gen, nick_color);
            let nick_u16 = map.to_u16(
                nick_display.len() as u32 + if line.kind == LineKind::Notice && !c.nick_column { 2 } else { 0 },
            );
            text::set_color(&msg, &br, 0, nick_u16);
            unsafe {
                let _ = msg.SetFontWeight(
                    windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    text::range(0, nick_u16),
                );
            }
        }
        // Formatting spans (shifted by the prefix).
        let spans: Vec<_> = parsed
            .spans
            .iter()
            .map(|s| schwaetz_proto::format::Span { start: s.start + off, end: s.end + off, style: s.style })
            .collect();
        let bgs = if deleted {
            Vec::new()
        } else {
            text::apply_spans(&msg, &spans, &map, th, c.brushes, c.dc, c.gfx_gen, f, c.colors)
        };

        // Links and channel names.
        let mut links = Vec::new();
        if is_msg
            || matches!(
                line.kind,
                LineKind::Topic | LineKind::Server | LineKind::Motd | LineKind::Status | LineKind::System
            )
        {
            let finder = linkify::LinkFinder::new();
            for l in finder.links(&body) {
                if *l.kind() != linkify::LinkKind::Url {
                    continue;
                }
                let a = map.to_u16(l.start() as u32 + off);
                let b = map.to_u16(l.end() as u32 + off);
                links.push(Link { start: a, len: b - a, target: LinkTarget::Url(l.as_str().to_owned()) });
            }
            for (i, w) in body.match_indices('#') {
                let before_ok = body[..i].chars().next_back().is_none_or(|p| p.is_whitespace() || p == '(');
                if !before_ok {
                    continue;
                }
                let end =
                    body[i..].find(|ch: char| ch.is_whitespace() || ",)\"'".contains(ch)).map_or(body.len(), |e| i + e);
                let name = body[i..end].trim_end_matches(['.', '!', '?', ':', ';']);
                if name.len() > 1
                    && !links.iter().any(|l| {
                        map.to_byte(l.start) <= i + off as usize && i + (off as usize) < map.to_byte(l.start + l.len)
                    })
                {
                    let a = map.to_u16(i as u32 + off);
                    let b = map.to_u16((i + name.len()) as u32 + off);
                    links.push(Link { start: a, len: b - a, target: LinkTarget::Channel(name.to_owned()) });
                }
                let _ = w;
            }
            let br = c.brushes.get(c.dc, c.gfx_gen, th.link);
            for l in &links {
                text::set_color(&msg, &br, l.start, l.len);
                if matches!(l.target, LinkTarget::Url(_)) {
                    unsafe {
                        let _ = msg.SetUnderline(true, text::range(l.start, l.len));
                    }
                }
            }
        }

        let mut placed = Vec::new();
        // Inline emotes (Twitch tags or script decorations): byte ranges of the stripped body.
        if !deleted && let Some(emotes) = line.extra.as_ref().map(|e| &e.emotes).filter(|e| !e.is_empty()) {
            let h = (f.line_height * 1.35).round();
            for e in emotes {
                if e.end as usize > body.len() || e.start >= e.end || c.images.borrow().failed(&e.url) {
                    continue;
                }
                let a = map.to_u16(e.start + off);
                let b = map.to_u16(e.end + off);
                // Never hide part of a link behind an image.
                if links.iter().any(|l| a < l.start + l.len && l.start < b) {
                    continue;
                }
                let obj = crate::images::InlineImage::create(&e.url, h, h * 0.78, c.images, c.dc);
                unsafe {
                    let _ = msg.SetInlineObject(&obj, text::range(a, b - a));
                }
                let code = if e.name.is_empty() {
                    body.get(e.start as usize..e.end as usize).unwrap_or_default().to_owned()
                } else {
                    e.name.clone()
                };
                placed.push((a, b - a, code, e.url.clone()));
            }
        }

        // Nick column.
        let nick = if c.nick_column {
            let (full, pre16) = nick_column_text(line);
            let l = c.text.layout(&full, &f.nick, m.nick_w, 100.0);
            let color = match line.kind {
                LineKind::Message | LineKind::Action => nick_color,
                LineKind::Notice => th.notice,
                LineKind::Join => th.join,
                LineKind::Part | LineKind::Quit | LineKind::Kick | LineKind::Netsplit => th.part,
                LineKind::Error => th.error,
                LineKind::System => th.accent,
                _ => th.text_dim,
            };
            let br = c.brushes.get(c.dc, c.gfx_gen, color);
            let n16 = full.encode_utf16().count() as u32;
            text::set_color(&l, &br, 0, n16);
            if pre16 > 0 {
                let dim = c.brushes.get(c.dc, c.gfx_gen, th.text_dim);
                text::set_color(&l, &dim, 0, pre16);
            }
            Some(l)
        } else {
            None
        };

        let ts_text = time::format(c.ts_format, time::local(line.time));
        let ts = c.text.layout(&ts_text, &f.small, m.ts_w + 20.0, 100.0);

        let extra = line.extra.as_deref();
        let reply = extra.and_then(|e| e.reply_to.as_ref()).map(|(_, nick, excerpt)| {
            let t = if nick.is_empty() { format!("↪ {excerpt}") } else { format!("↪ {nick}: {excerpt}") };
            c.text.layout(&t, &f.small, msg_w, 100.0)
        });
        let reactions = extra.filter(|e| !e.reactions.is_empty()).map(|e| {
            let t: Vec<String> = e.reactions.iter().map(|(r, n)| format!("{r} {}", n.len())).collect();
            c.text.layout(&t.join("   "), &f.small, msg_w, 100.0)
        });

        let msg_h = text::metrics(&msg).height.max(f.line_height);
        let mut height = msg_h + LINE_GAP;
        if let Some(r) = &reply {
            height += text::metrics(r).height + 2.0;
        }
        if let Some(r) = &reactions {
            height += text::metrics(r).height + 6.0;
        }
        let card = if c.previews && !deleted && matches!(line.kind, LineKind::Message | LineKind::Action) {
            let url = links.iter().find_map(|l| match &l.target {
                LinkTarget::Url(u) if u.starts_with("https://") => Some(u.clone()),
                _ => None,
            });
            url.and_then(|u| self.card_for(c, u, msg_w))
        } else {
            None
        };
        if let Some(card) = &card {
            height += card.h + 6.0;
        }
        Cached {
            msg,
            msg_h,
            nick,
            ts,
            reply,
            reactions,
            links,
            bgs,
            emotes: placed,
            plain,
            map,
            height,
            card,
            sig: 0,
            used: self.frame,
        }
    }

    /// Builds the preview card (or load chip) for a message's first https link.
    fn card_for(&self, c: &mut Ctx, url: String, msg_w: f32) -> Option<Card> {
        let f = &c.text.fonts;

        let max_w = msg_w.min(420.0);
        let mut store = c.images.borrow_mut();
        let host_ok = || {
            let host = url
                .split("://")
                .nth(1)
                .and_then(|r| r.split(['/', '?', '#']).next())
                .unwrap_or("")
                .to_ascii_lowercase();
            c.allow_hosts.is_empty() || c.allow_hosts.iter().any(|h| host == *h || host.ends_with(&format!(".{h}")))
        };
        let state = store.previews.get(&url).cloned();
        match state {
            None => {
                if store.requested.contains(&url) || (c.preview_auto && host_ok()) {
                    if !store.wants.iter().any(|(u, _)| *u == url) {
                        store.wants.push((url.clone(), true));
                    }
                    return None;
                }
                Some(Card { url, w: 128.0, h: 22.0, chip: true, image: None, title: None, desc: None })
            }
            Some(crate::images::Preview::Failed) => None,
            Some(crate::images::Preview::Image) => {
                let (w, h) = store.size(&url, false)?;
                let scale = (max_w / w as f32).min(240.0 / h as f32).min(1.0);
                let (iw, ih) = (w as f32 * scale, h as f32 * scale);
                Some(Card {
                    url: url.clone(),
                    w: iw,
                    h: ih,
                    chip: false,
                    image: Some((url, iw, ih)),
                    title: None,
                    desc: None,
                })
            }
            Some(crate::images::Preview::Page(meta)) => {
                let inner = max_w - 24.0;
                let image = meta.image.as_ref().and_then(|img| {
                    let (w, h) = store.size(img, false)?;
                    let scale = (inner / w as f32).min(200.0 / h as f32).min(1.0);
                    Some((img.clone(), w as f32 * scale, h as f32 * scale))
                });
                drop(store);
                let title = meta.title.as_ref().map(|t| c.text.layout(t, &f.ui_semibold, inner, 40.0));
                let desc_text = match (&meta.site, &meta.description) {
                    (Some(s), Some(d)) => format!("{s} — {d}"),
                    (Some(s), None) => s.clone(),
                    (None, Some(d)) => d.clone(),
                    (None, None) => String::new(),
                };
                let desc = (!desc_text.is_empty()).then(|| {
                    let l = c.text.layout(&desc_text, &f.chat, inner, f.line_height * 3.2);
                    unsafe {
                        let _ = l.SetWordWrapping(windows::Win32::Graphics::DirectWrite::DWRITE_WORD_WRAPPING_WRAP);
                        let _ =
                            l.SetFontSize(f.chat_size * 0.9, text::range(0, desc_text.encode_utf16().count() as u32));
                    }
                    l
                });
                let mut h = 12.0;
                if let Some(t) = &title {
                    h += text::metrics(t).height + 4.0;
                }
                if let Some(d) = &desc {
                    h += text::metrics(d).height.min(f.line_height * 3.2) + 4.0;
                }
                if let Some((_, _, ih)) = &image {
                    h += ih + 8.0;
                }
                Some(Card { url, w: max_w, h: h + 4.0, chip: false, image, title, desc })
            }
        }
    }

    fn nick_color(&self, c: &Ctx, line: &Line) -> crate::gfx::Color {
        let own = line.flags.has(LineFlags::OWN);
        let twitch = line.extra.as_ref().and_then(|e| e.color);
        pick_nick_color(c.theme, c.colored_nicks, c.twitch_colors, own, twitch, &line.nick)
    }

    fn ensure(&mut self, c: &mut Ctx, line: &Line, m: &Metrics) -> f32 {
        let frame = self.frame;
        let sig = signature(line);
        if let Some(e) = self.cache.get_mut(&line.id)
            && e.sig == sig
        {
            e.used = frame;
            return e.height;
        }
        let mut e = self.layout_line(c, line, m);
        e.sig = sig;
        let h = e.height;
        self.cache.insert(line.id, e);
        h
    }

    /// Extra height above a line for the day separator / unread marker.
    fn decorations(&self, lines: &Buffer, vis: &[usize], vi: usize) -> (bool, bool) {
        let l = &lines.lines[vis[vi]];
        let prev = vi.checked_sub(1).map(|p| &lines.lines[vis[p]]);
        let day = prev.is_some_and(|p| time::local_day(p.time) != time::local_day(l.time));
        let marker = self
            .marker
            .is_some_and(|m| l.time > m && prev.is_some_and(|p| p.time <= m) && !l.flags.has(LineFlags::OWN));
        (day, marker)
    }

    fn block_height(&mut self, c: &mut Ctx, b: &Buffer, vis: &[usize], vi: usize, m: &Metrics) -> f32 {
        let h = self.ensure(c, &b.lines[vis[vi]], m);
        let (day, marker) = self.decorations(b, vis, vi);
        h + if day { DAY_SEP_H } else { 0.0 } + if marker { MARKER_H } else { 0.0 }
    }

    fn check_cache_key(&mut self, c: &Ctx) {
        // Message text wraps at a width that depends on the chat and nick column widths.
        let key = (self.rect.w.round() as u32, self.metrics(c).nick_w.round() as u32, c.gfx_gen, self.style_gen);
        if key != self.cache_key {
            self.cache.clear();
            self.cache_key = key;
        }
    }

    /// Scrolls by `dy` DIPs (positive = towards older lines).
    pub fn scroll(&mut self, c: &mut Ctx, b: &Buffer, dy: f32) {
        self.fit_nicks(c, b);
        self.check_cache_key(c);
        let vis: Vec<usize> = (0..b.lines.len()).filter(|&i| visible(&b.lines[i], self.show_filtered)).collect();
        if vis.is_empty() {
            return;
        }
        let m = self.metrics(c);
        let view_h = self.rect.h - PAD_BOTTOM;
        let mut ai = if self.pinned { vis.len() - 1 } else { self.anchor_index(b, &vis) };
        if self.pinned {
            self.anchor_y = view_h;
        }
        self.anchor_y += dy;
        // Move the anchor to the line crossing the bottom edge.
        loop {
            let h = self.block_height(c, b, &vis, ai, &m);
            if self.anchor_y - h > view_h && ai > 0 {
                self.anchor_y -= h;
                ai -= 1;
            } else if self.anchor_y < view_h && ai + 1 < vis.len() {
                ai += 1;
                self.anchor_y += self.block_height(c, b, &vis, ai, &m);
            } else {
                break;
            }
        }
        if ai + 1 == vis.len() && self.anchor_y <= view_h {
            self.pinned = true;
            self.anchor = None;
            return;
        }
        // Don't scroll past the oldest line.
        let mut top = self.anchor_y;
        let mut i = ai as isize;
        while i >= 0 && top > 0.0 {
            top -= self.block_height(c, b, &vis, i as usize, &m);
            i -= 1;
        }
        if i < 0 && top > 4.0 {
            // Content shorter than the view: pin to bottom; otherwise clamp at top.
            let total: f32 = (0..vis.len()).map(|v| self.block_height(c, b, &vis, v, &m)).sum();
            if total <= view_h {
                self.pinned = true;
                self.anchor = None;
                return;
            }
            self.anchor_y -= top - 4.0;
            self.wants_older = true;
        }
        self.pinned = false;
        self.anchor = Some(b.lines[vis[ai]].id);
    }

    /// Widens the fitted nick column for lines added since the last call (all lines after a
    /// buffer switch or a font change). While a buffer is shown it only grows, so the messages
    /// don't shift back and forth as lines arrive and scroll out.
    fn fit_nicks(&mut self, c: &Ctx, b: &Buffer) {
        if !c.nick_column || !c.nick_column_auto {
            return;
        }
        let key = (self.buffer, c.gfx_gen, self.style_gen, self.show_filtered);
        if key != self.nick_fit.key || b.lines.is_empty() {
            self.nick_fit = NickFit { key, ..Default::default() };
        }
        if self.nick_fit.seen == Some(b.generation) {
            return;
        }
        self.nick_fit.seen = Some(b.generation);
        if self.nick_widths.len() > 20_000 {
            self.nick_widths.clear();
        }
        let upto = self.nick_fit.upto;
        for line in b.lines.iter().filter(|l| l.id > upto) {
            self.nick_fit.upto = self.nick_fit.upto.max(line.id);
            if !visible(line, self.show_filtered) {
                continue;
            }
            let (full, _) = nick_column_text(line);
            let w = match self.nick_widths.get(&full) {
                Some(w) => *w,
                None => {
                    let l = c.text.layout(&full, &c.text.fonts.nick, 10_000.0, 100.0);
                    let w = text::metrics(&l).widthIncludingTrailingWhitespace;
                    self.nick_widths.insert(full, w);
                    w
                }
            };
            self.nick_fit.width = self.nick_fit.width.max(w);
        }
    }

    fn anchor_index(&self, b: &Buffer, vis: &[usize]) -> usize {
        match self.anchor {
            Some(id) => vis.iter().rposition(|&i| b.lines[i].id == id).unwrap_or(vis.len() - 1),
            None => vis.len() - 1,
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.pinned = true;
        self.anchor = None;
    }

    /// Scrolls to a scrollbar position: 0 shows the oldest lines, 1 the newest.
    pub fn scroll_to(&mut self, c: &mut Ctx, b: &Buffer, pos: f32) {
        let vis: Vec<usize> = (0..b.lines.len()).filter(|&i| visible(&b.lines[i], self.show_filtered)).collect();
        if vis.is_empty() {
            return;
        }
        // The position picks the line at the bottom edge, from the last of the first screenful
        // to the newest.
        let n = vis.len();
        let lo = self.per_view.min(n - 1);
        let ai = lo + ((n - 1 - lo) as f32 * pos.clamp(0.0, 1.0)).round() as usize;
        if ai + 1 >= n {
            self.scroll_to_bottom();
            return;
        }
        self.pinned = false;
        self.anchor = Some(b.lines[vis[ai]].id);
        self.anchor_y = self.rect.h - PAD_BOTTOM;
        // Settles the anchor (and stops at the oldest line).
        self.scroll(c, b, 0.0);
    }

    pub fn render(&mut self, p: &Painter, c: &mut Ctx, b: &Buffer) {
        self.frame += 1;
        self.fit_nicks(c, b);
        self.check_cache_key(c);
        let th = c.theme;
        p.fill(self.rect, th.chat_bg);
        self.drawn.clear();
        if self.nicks.len() > 2000 {
            self.nicks.clear();
        }
        let vis: Vec<usize> = (0..b.lines.len()).filter(|&i| visible(&b.lines[i], self.show_filtered)).collect();
        self.bar = None;
        if vis.is_empty() {
            return;
        }
        let m = self.metrics(c);
        let view_h = self.rect.h - PAD_BOTTOM;
        let ai = if self.pinned { vis.len() - 1 } else { self.anchor_index(b, &vis) };
        let anchor_bottom = if self.pinned { view_h } else { self.anchor_y };

        // Collect (vi, top y) for visible blocks: walk up from the anchor, then down.
        let mut blocks: Vec<(usize, f32, f32)> = Vec::new();
        let mut y = anchor_bottom;
        let mut vi = ai as isize;
        while vi >= 0 && y > -50.0 {
            let h = self.block_height(c, b, &vis, vi as usize, &m);
            y -= h;
            blocks.push((vi as usize, y, h));
            vi -= 1;
        }
        if vi < 0 && y > 0.0 && !self.pinned {
            self.wants_older = true;
        }
        if vi < 0 && y > 0.0 && b.lines.len() >= 20 && self.pinned && !b.history_exhausted {
            // The whole buffer fits: nothing older in memory, try history.
            self.wants_older = true;
        }
        // Everything from the oldest line down to the anchor is in view.
        let all_above = vi < 0 && y >= 0.0;
        let above = blocks.len();
        self.per_view = above.saturating_sub(1);
        blocks.reverse();
        let mut y = anchor_bottom;
        let mut vi = ai + 1;
        while vi < vis.len() && y < self.rect.h {
            let h = self.block_height(c, b, &vis, vi, &m);
            blocks.push((vi, y, h));
            y += h;
            vi += 1;
        }

        p.clip(self.rect);
        let sel = self.normalized_selection(b);
        for &(vi, top, h) in &blocks {
            let line = &b.lines[vis[vi]];
            let (day, marker) = self.decorations(b, &vis, vi);
            let mut ly = self.rect.y + top;
            if day {
                self.draw_day_separator(p, c, line.time, ly);
                ly += DAY_SEP_H;
            }
            if marker {
                let my = ly + MARKER_H / 2.0;
                p.line(
                    self.rect.x + PAD_X,
                    my,
                    self.rect.right() - PAD_X - 34.0,
                    my,
                    with_alpha(th.unread_marker, 0.7),
                    1.0,
                );
                let l = c.text.layout("new", &c.text.fonts.ui_small, 40.0, 20.0);
                p.text(&l, self.rect.right() - PAD_X - 28.0, my - 8.0, th.unread_marker);
                ly += MARKER_H;
            }
            let block_h = h - (ly - (self.rect.y + top));
            self.draw_line(p, c, line, ly, block_h, &m, sel);
        }
        p.unclip();

        // Scrollbar while not all lines fit; positions as in `scroll_to`.
        if !all_above || !self.pinned {
            let n = vis.len();
            let lo = self.per_view.min(n - 1);
            let pos = if self.pinned || n - 1 <= lo { 1.0 } else { ai.saturating_sub(lo) as f32 / (n - 1 - lo) as f32 };
            let bar = crate::scrollbar::Scrollbar::new(self.rect, above as f32 / n as f32, pos);
            bar.draw(p, th, self.bar_hot);
            self.bar = Some(bar);
        }
        if !self.pinned {
            // "Jump to latest" pill.
            let label = c.text.layout("↓  Jump to latest", &c.text.fonts.ui_small, 200.0, 20.0);
            let w = text::metrics(&label).width + 24.0;
            let r = Rect::new(self.rect.x + (self.rect.w - w) / 2.0, self.rect.bottom() - 34.0, w, 24.0);
            let jc = crate::anim::Control::JumpPill;
            let face = crate::anim::button_face(p, r, 12.0, th.accent, th, c.anims.hover(jc), c.anims.press(jc));
            p.text(&label, face.x + 12.0, face.y + (face.h - 14.0) / 2.0, th.accent_fg);
        }

        // Evict layouts not used recently.
        if self.cache.len() > 800 {
            let keep_after = self.frame.saturating_sub(3);
            self.cache.retain(|_, e| e.used >= keep_after);
        }
    }

    fn draw_day_separator(&self, p: &Painter, c: &Ctx, t: i64, y: f32) {
        let th = c.theme;
        let label = time::format("%A, %d %B %Y", time::local(t));
        let l = c.text.layout(&label, &c.text.fonts.ui_small, 400.0, 20.0);
        let w = text::metrics(&l).width;
        let cx = self.rect.x + self.rect.w / 2.0;
        let my = y + DAY_SEP_H / 2.0;
        p.line(self.rect.x + PAD_X, my, cx - w / 2.0 - 10.0, my, th.border, 1.0);
        p.line(cx + w / 2.0 + 10.0, my, self.rect.right() - PAD_X, my, th.border, 1.0);
        p.text(&l, cx - w / 2.0, my - 8.0, th.text_dim);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_line(
        &mut self,
        p: &Painter,
        c: &Ctx,
        line: &Line,
        y: f32,
        h: f32,
        m: &Metrics,
        sel: Option<(Pos, Pos, usize, usize)>,
    ) {
        let th = c.theme;
        let Some(e) = self.cache.get(&line.id) else { return };
        let x0 = self.rect.x;
        if self.hover == Some(line.id) && c.replies && line.kind.is_message() {
            p.fill(Rect::new(x0, y, self.rect.w, h), th.sidebar_hover);
        }
        if line.flags.has(LineFlags::HIGHLIGHT) {
            p.fill(Rect::new(x0, y, self.rect.w, h), th.highlight_bg);
            p.fill(Rect::new(x0, y, 3.0, h), th.highlight_bar);
        } else if line.kind == LineKind::System {
            p.fill(Rect::new(x0, y, self.rect.w, h), with_alpha(th.accent, 0.07));
            p.fill(Rect::new(x0, y, 3.0, h), th.accent);
        } else if line.flags.has(LineFlags::FIRST_MESSAGE) {
            p.fill(Rect::new(x0, y, 3.0, h), th.join);
        }
        let mut my = y;
        if let Some(r) = &e.reply {
            p.text(r, x0 + m.msg_x, my, th.text_dim);
            my += text::metrics(r).height + 2.0;
        }
        let baseline_off = 0.0;
        if !c.ts_format.is_empty() {
            let small_h = text::metrics(&e.ts).height;
            p.text(&e.ts, x0 + PAD_X, my + (c.text.fonts.line_height - small_h) / 2.0 + baseline_off, th.timestamp);
        }
        let nick_x = x0 + PAD_X + m.ts_w + 4.0;
        if let Some(n) = &e.nick {
            p.text(n, nick_x, my, th.text);
        }
        let msg_x = x0 + m.msg_x;
        for bg in &e.bgs {
            for (rx, ry, rw, rh) in text::range_rects(&e.msg, bg.start, bg.len) {
                p.fill(Rect::new(msg_x + rx, my + ry, rw, rh), bg.color);
            }
        }
        // Selection.
        if let Some((a, b, ai, bi)) = sel {
            let (s, t) = (a.line, b.line);
            let range = if s == line.id && t == line.id {
                Some((a.u16, b.u16))
            } else if s == line.id {
                Some((a.u16, e.map.len_u16()))
            } else if t == line.id {
                Some((0, b.u16))
            } else if self.between(line.id, ai, bi) {
                Some((0, e.map.len_u16()))
            } else {
                None
            };
            if let Some((from, to)) = range.filter(|(f, t)| t > f) {
                for (rx, ry, rw, rh) in text::range_rects(&e.msg, from, to - from) {
                    p.fill(Rect::new(msg_x + rx, my + ry, rw, rh), th.selection);
                }
            }
        }
        let fg = th.text;
        p.text(&e.msg, msg_x, my, fg);
        if let Some(r) = &e.reactions {
            let ry = my + e.msg_h + 3.0;
            let mt = text::metrics(r);
            p.fill_round(Rect::new(msg_x - 4.0, ry - 1.0, mt.width + 12.0, mt.height + 4.0), 6.0, th.badge_bg);
            p.text(r, msg_x + 2.0, ry + 1.0, th.text);
        }
        if let Some(card) = &e.card {
            let top = my + card_offset(e);
            let r = Rect::new(msg_x, top, card.w, card.h);
            if card.chip {
                p.fill_round(r, 11.0, th.badge_bg);
                let l = c.text.layout("🖼  Show preview", &c.text.fonts.ui_small, card.w, 20.0);
                p.text(&l, r.x + 12.0, r.y + 4.0, th.text_dim);
            } else if card.title.is_none() && card.desc.is_none() {
                // Direct image link.
                if let Some((url, w, h)) = &card.image
                    && let Some(bmp) = c.images.borrow().bitmap(url)
                {
                    p.bitmap(&bmp.cast().unwrap(), Rect::new(r.x, r.y, *w, *h), 1.0);
                }
            } else {
                p.fill_round(r, 8.0, th.input_bg);
                p.stroke_round(r, 8.0, th.border, 1.0);
                p.fill_round(Rect::new(r.x, r.y, 3.0, r.h), 1.5, th.accent);
                let mut y = r.y + 8.0;
                if let Some(t) = &card.title {
                    p.text(t, r.x + 14.0, y, th.link);
                    y += text::metrics(t).height + 4.0;
                }
                if let Some(d) = &card.desc {
                    p.clip(Rect::new(r.x, y, r.w, c.text.fonts.line_height * 3.2));
                    p.text(d, r.x + 14.0, y, th.text_dim);
                    p.unclip();
                    y += text::metrics(d).height.min(c.text.fonts.line_height * 3.2) + 4.0;
                }
                if let Some((url, w, h)) = &card.image
                    && let Some(bmp) = c.images.borrow().bitmap(url)
                {
                    p.bitmap(&bmp.cast().unwrap(), Rect::new(r.x + 14.0, y + 4.0, *w, *h), 1.0);
                }
            }
        }
        if matches!(line.kind, LineKind::Message | LineKind::Action | LineKind::Notice) && !line.nick.is_empty() {
            self.nicks.insert(line.id, line.nick.to_string());
        }
        let mut reply_btn = None;
        if self.hover == Some(line.id)
            && c.replies
            && line.kind.is_message()
            && line.msgid().is_some()
            && !line.flags.has(LineFlags::DELETED)
        {
            let l = c.text.layout("↩  Reply", &c.text.fonts.ui_small, 120.0, 20.0);
            let w = text::metrics(&l).width + 20.0;
            let r = Rect::new(self.rect.right() - w - 18.0, y + 1.0, w, 22.0);
            let rc = crate::anim::Control::Reply(line.id);
            let (h, pr) = (c.anims.hover(rc), c.anims.press(rc));
            let face = r.inset(pr * 1.5, pr * 1.0);
            p.fill_round(face, 11.0, th.panel_bg);
            if h > 0.0 {
                p.fill_round(face, 11.0, with_alpha(th.accent, 0.14 * h));
            }
            p.stroke_round(face, 11.0, crate::anim::mix(th.border, th.accent, h), 1.0);
            p.text(&l, face.x + 10.0, face.y + (face.h - 14.0) / 2.0, th.text);
            reply_btn = Some(r);
        }
        self.drawn.push(Drawn {
            id: line.id,
            reply_btn,
            y,
            h,
            msg_x,
            msg_y: my,
            nick_rect: Rect::new(nick_x, my, m.nick_w, c.text.fonts.line_height),
        });
    }

    /// Order of drawn lines for "between" checks: by position in `drawn`.
    fn between(&self, id: u64, a_idx: usize, b_idx: usize) -> bool {
        self.order_of(id).is_some_and(|i| i > a_idx && i < b_idx)
    }

    fn order_of(&self, id: u64) -> Option<usize> {
        self.order.get(&id).copied()
    }

    /// Selection endpoints ordered by buffer position.
    fn normalized_selection(&mut self, b: &Buffer) -> Option<(Pos, Pos, usize, usize)> {
        let (a, z) = self.selection?;
        self.order = b.lines.iter().enumerate().map(|(i, l)| (l.id, i)).collect();
        let (ai, zi) = (self.order.get(&a.line).copied()?, self.order.get(&z.line).copied()?);
        if (ai, a.u16) <= (zi, z.u16) { Some((a, z, ai, zi)) } else { Some((z, a, zi, ai)) }
    }

    pub fn hit(&self, x: f32, y: f32) -> Hit {
        for d in &self.drawn {
            if let Some(r) = d.reply_btn
                && r.contains(x, y)
            {
                return Hit::Reply(d.id);
            }
            if y < d.y || y >= d.y + d.h {
                continue;
            }
            if d.nick_rect.contains(x, y)
                && let Some(n) = self.nicks.get(&d.id)
            {
                return Hit::Nick(n.clone());
            }
            let Some(e) = self.cache.get(&d.id) else { return Hit::Nothing };
            if let Some(card) = &e.card {
                let r = Rect::new(d.msg_x, d.msg_y + card_offset(e), card.w, card.h);
                if r.contains(x, y) {
                    return if card.chip {
                        Hit::LoadPreview(card.url.clone())
                    } else {
                        Hit::Link(LinkTarget::Url(card.url.clone()))
                    };
                }
            }
            let (pos, inside) = text::hit_point(&e.msg, x - d.msg_x, y - d.msg_y);
            if inside && let Some(l) = e.links.iter().find(|l| pos >= l.start && pos < l.start + l.len) {
                return Hit::Link(l.target.clone());
            }
            let pos = if x < d.msg_x { 0 } else { pos };
            return Hit::Text(Pos { line: d.id, u16: pos });
        }
        // Above/below all lines: clamp to the first/last drawn line.
        if let Some(d) = self.drawn.first().filter(|d| y < d.y) {
            return Hit::Text(Pos { line: d.id, u16: 0 });
        }
        if let Some(d) = self.drawn.iter().max_by(|a, b| a.y.total_cmp(&b.y)).filter(|d| y >= d.y + d.h) {
            let len = self.cache.get(&d.id).map_or(0, |e| e.map.len_u16());
            return Hit::Text(Pos { line: d.id, u16: len });
        }
        Hit::Nothing
    }

    /// The inline emote under a point: its code, image url and bounds.
    pub fn emote_at(&self, x: f32, y: f32) -> Option<(String, String, Rect)> {
        let d = self.drawn.iter().find(|d| y >= d.y && y < d.y + d.h)?;
        let e = self.cache.get(&d.id).filter(|e| !e.emotes.is_empty())?;
        let (pos, inside) = text::hit_point(&e.msg, x - d.msg_x, y - d.msg_y);
        if !inside {
            return None;
        }
        let (start, len, code, url) = e.emotes.iter().find(|(s, l, ..)| pos >= *s && pos < s + l)?;
        let (rx, ry, rw, rh) = text::range_rects(&e.msg, *start, *len).into_iter().next()?;
        Some((code.clone(), url.clone(), Rect::new(d.msg_x + rx, d.msg_y + ry, rw, rh)))
    }

    pub fn nick_of(&self, id: u64) -> Option<String> {
        self.nicks.get(&id).cloned()
    }

    /// "Jump to latest" pill hit test.
    pub fn jump_pill(&self, x: f32, y: f32) -> bool {
        !self.pinned
            && y >= self.rect.bottom() - 34.0
            && y <= self.rect.bottom() - 10.0
            && (x - (self.rect.x + self.rect.w / 2.0)).abs() < 70.0
    }

    /// Text of the current selection, one line per buffer line.
    pub fn selected_text(&self, b: &Buffer, ts_format: &str) -> Option<String> {
        let (a, z) = self.selection?;
        let order: HashMap<u64, usize> = b.lines.iter().enumerate().map(|(i, l)| (l.id, i)).collect();
        let (ai, zi) = (*order.get(&a.line)?, *order.get(&z.line)?);
        let ((a, ai), (z, zi)) = if (ai, a.u16) <= (zi, z.u16) { ((a, ai), (z, zi)) } else { ((z, zi), (a, ai)) };
        let mut out = String::new();
        for i in ai..=zi {
            let line = &b.lines[i];
            if !visible(line, self.show_filtered) {
                continue;
            }
            let plain = match self.cache.get(&line.id) {
                Some(e) => {
                    let from = if i == ai { e.map.to_byte(a.u16) } else { 0 };
                    let to = if i == zi { e.map.to_byte(z.u16) } else { e.plain.len() };
                    if ai == zi {
                        return Some(e.plain.get(from..to).unwrap_or("").to_owned());
                    }
                    e.plain.get(from..to).unwrap_or("").to_owned()
                }
                None => schwaetz_proto::format::strip(&line.text),
            };
            if !out.is_empty() {
                out.push_str("\r\n");
            }
            let ts = if ts_format.is_empty() {
                String::new()
            } else {
                format!("[{}] ", time::format(ts_format, time::local(line.time)))
            };
            match line.kind {
                LineKind::Message => out.push_str(&format!("{ts}<{}> {plain}", line.display_nick())),
                LineKind::Notice => out.push_str(&format!("{ts}-{}- {plain}", line.display_nick())),
                LineKind::Action => out.push_str(&format!("{ts}* {plain}")),
                _ => out.push_str(&format!("{ts}{plain}")),
            }
        }
        Some(out)
    }

    /// Selects the whole line (double/triple click) or a word.
    pub fn select_word(&mut self, pos: Pos) {
        let Some(e) = self.cache.get(&pos.line) else { return };
        let b = e.map.to_byte(pos.u16);
        let s = &e.plain;
        let is_word = |c: char| c.is_alphanumeric() || "_-#@.:/'".contains(c);
        let start = s[..b].rfind(|c: char| !is_word(c)).map_or(0, |i| i + s[i..].chars().next().unwrap().len_utf8());
        let end = s[b..].find(|c: char| !is_word(c)).map_or(s.len(), |i| b + i);
        self.selection = Some((
            Pos { line: pos.line, u16: e.map.to_u16(start as u32) },
            Pos { line: pos.line, u16: e.map.to_u16(end as u32) },
        ));
    }

    pub fn select_line(&mut self, pos: Pos) {
        let Some(e) = self.cache.get(&pos.line) else { return };
        self.selection = Some((Pos { line: pos.line, u16: 0 }, Pos { line: pos.line, u16: e.map.len_u16() }));
    }
}

/// Distance from the top of a message's text to its preview card.
fn card_offset(e: &Cached) -> f32 {
    let reactions = e.reactions.as_ref().map_or(0.0, |r| text::metrics(r).height + 6.0);
    e.msg_h + 4.0 + reactions
}

/// Cheap fingerprint of the parts of a line that can change after it was added.
fn signature(l: &Line) -> u64 {
    let (emotes, reactions, reply) = l.extra.as_ref().map_or((0, 0, 0), |e| {
        (
            e.emotes.len() as u64,
            e.reactions.iter().map(|(_, n)| n.len() as u64).sum::<u64>(),
            e.reply_to.is_some() as u64,
        )
    });
    l.flags.0 as u64 | emotes << 16 | reactions << 24 | reply << 40 | (l.text.len() as u64) << 41
}

impl ChatView {
    /// The line drawn at vertical position `y`, if any.
    pub fn line_at(&self, y: f32) -> Option<u64> {
        self.drawn.iter().find(|d| y >= d.y && y < d.y + d.h).map(|d| d.id)
    }
}

/// The color of a nick in the chat. Your own nick keeps its color. A Twitch user's chosen color
/// (made readable on the background) is used when the network shows Twitch colors
/// (`twitch_colors`); otherwise "Colored nicks" (`palette`) picks a theme color, or the text color.
/// What the nick column shows for a line: Twitch badges and the status prefix (messages only),
/// then the name or a symbol for the line kind. Also returns the length (UTF-16) of the part
/// before the name, which is drawn dimmed.
fn nick_column_text(line: &Line) -> (String, u32) {
    let nick = line.display_nick();
    let name = match line.kind {
        LineKind::Message => nick.to_owned(),
        LineKind::Action => "•".to_owned(),
        LineKind::Notice => format!("-{nick}-"),
        LineKind::Join => "→".into(),
        LineKind::Part | LineKind::Quit | LineKind::Kick | LineKind::Netsplit => "←".into(),
        LineKind::Nick => "⇄".into(),
        LineKind::Error => "!".into(),
        LineKind::System => "★".into(),
        LineKind::Topic | LineKind::Mode | LineKind::Invite => "–".into(),
        LineKind::Server | LineKind::Status | LineKind::Ctcp if !line.nick.is_empty() => nick.to_owned(),
        _ => "·".into(),
    };
    let mut full = String::new();
    if line.kind == LineKind::Message {
        if let Some(e) = line.extra.as_ref() {
            full.extend(e.badges.iter().map(|b| schwaetz_core::twitch::badge_label(b)));
        }
        if let Some(p) = line.prefix {
            full.push(p);
        }
    }
    let pre16 = full.encode_utf16().count() as u32;
    full.push_str(&name);
    (full, pre16)
}

pub fn pick_nick_color(
    th: &Theme,
    palette: bool,
    twitch_colors: bool,
    own: bool,
    twitch: Option<u32>,
    nick: &str,
) -> crate::gfx::Color {
    // The color from the `color` tag, exactly as sent (own lines carry the one from USERSTATE).
    if twitch_colors && let Some(rgb) = twitch {
        return crate::gfx::hex(rgb);
    }
    if own {
        return th.own_nick;
    }
    if palette { th.nick_color(nick) } else { th.text }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twitch_colors_and_palette_are_separate() {
        let th = Theme::dark();
        let red = Some(0xff0000);
        let twitch_red = crate::gfx::hex(0xff0000);
        // Twitch colors on: used whatever the palette setting says.
        assert_eq!(pick_nick_color(&th, false, true, false, red, "alice"), twitch_red);
        assert_eq!(pick_nick_color(&th, true, true, false, red, "alice"), twitch_red);
        // Twitch colors off: the palette setting decides.
        assert_eq!(pick_nick_color(&th, true, false, false, red, "alice"), th.nick_color("alice"));
        assert_eq!(pick_nick_color(&th, false, false, false, red, "alice"), th.text);
        // Users without a Twitch color, and yourself.
        assert_eq!(pick_nick_color(&th, true, true, false, None, "bob"), th.nick_color("bob"));
        assert_eq!(pick_nick_color(&th, false, true, false, None, "bob"), th.text);
        // Your own lines use your Twitch color too; the theme's own-nick color without one.
        assert_eq!(pick_nick_color(&th, true, true, true, red, "me"), twitch_red);
        assert_eq!(pick_nick_color(&th, true, false, true, red, "me"), th.own_nick);
        assert_eq!(pick_nick_color(&th, true, true, true, None, "me"), th.own_nick);
        // Dark colors are not lightened.
        let navy = Some(0x000080);
        assert_eq!(pick_nick_color(&th, true, true, false, navy, "carol"), crate::gfx::hex(0x000080));
    }
}
