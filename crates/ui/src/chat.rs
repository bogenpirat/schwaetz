//! The chat view: a virtualized, bottom-anchored list of buffer lines.

use crate::gfx::{Painter, Rect, with_alpha};
use crate::text::{self, BgRun, Brushes, Text, U16Map};
use crate::theme::{Theme, ensure_contrast};
use schwaetz_core::buffer::{Buffer, BufferId, Line, LineFlags, LineKind};
use schwaetz_core::time;
use std::collections::HashMap;
use windows::Win32::Graphics::Direct2D::ID2D1DeviceContext;
use windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;

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
    /// Plain text actually laid out in `msg` (for copying).
    plain: String,
    map: U16Map,
    height: f32,
    used: u64,
}

/// A line as drawn in the last frame (for hit testing).
struct Drawn {
    id: u64,
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
pub enum Hit {
    Link(LinkTarget),
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
    cache_key: (u32, u64, u64),
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
    /// Line id → position in the buffer, refreshed while a selection exists.
    order: HashMap<u64, usize>,
    /// Line id → sender nick for drawn lines (nick-column clicks).
    nicks: HashMap<u64, String>,
}

pub struct Ctx<'a> {
    pub text: &'a Text,
    pub theme: &'a Theme,
    pub brushes: &'a mut Brushes,
    pub dc: &'a ID2D1DeviceContext,
    pub gfx_gen: u64,
    pub ts_format: &'a str,
    pub nick_column: bool,
    pub nick_column_chars: u32,
    pub colors: bool,
    pub colored_nicks: bool,
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
            cache_key: (0, 0, 0),
            frame: 0,
            drawn: Vec::new(),
            selection: None,
            selecting: false,
            marker: None,
            show_filtered: false,
            wants_older: false,
            style_gen: 0,
            order: HashMap::new(),
            nicks: HashMap::new(),
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
        let nick_w = if c.nick_column { c.nick_column_chars.clamp(4, 40) as f32 * f.char_width } else { 0.0 };
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

        // Nick column.
        let nick = if c.nick_column {
            let (sym, name) = match line.kind {
                LineKind::Message => (String::new(), nick_display.clone()),
                LineKind::Action => (String::new(), "•".to_owned()),
                LineKind::Notice => (String::new(), format!("-{nick_display}-")),
                LineKind::Join => (String::new(), "→".into()),
                LineKind::Part | LineKind::Quit | LineKind::Kick | LineKind::Netsplit => (String::new(), "←".into()),
                LineKind::Nick => (String::new(), "⇄".into()),
                LineKind::Error => (String::new(), "!".into()),
                LineKind::System => (String::new(), "★".into()),
                LineKind::Topic | LineKind::Mode | LineKind::Invite => (String::new(), "–".into()),
                LineKind::Server | LineKind::Status | LineKind::Ctcp if !line.nick.is_empty() => {
                    (String::new(), nick_display.clone())
                }
                _ => (String::new(), "·".into()),
            };
            let mut full = sym;
            let badges: String = line
                .extra
                .as_ref()
                .map(|e| {
                    e.badges.iter().map(|b| schwaetz_core::twitch::badge_label(b)).filter(|s| !s.is_empty()).collect()
                })
                .unwrap_or_default();
            if matches!(line.kind, LineKind::Message) {
                full.push_str(&badges);
                if let Some(p) = line.prefix {
                    full.push(p);
                }
            }
            full.push_str(&name);
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
            let pre16 = (badges.encode_utf16().count()
                + usize::from(line.prefix.is_some() && line.kind == LineKind::Message)) as u32;
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
        Cached { msg, msg_h, nick, ts, reply, reactions, links, bgs, plain, map, height, used: self.frame }
    }

    fn nick_color(&self, c: &Ctx, line: &Line) -> crate::gfx::Color {
        let th = c.theme;
        if line.flags.has(LineFlags::OWN) {
            return th.own_nick;
        }
        if let Some(rgb) = line.extra.as_ref().and_then(|e| e.color) {
            return ensure_contrast(crate::gfx::hex(rgb), th.chat_bg, 3.0);
        }
        if c.colored_nicks { th.nick_color(&line.nick) } else { th.text }
    }

    fn ensure(&mut self, c: &mut Ctx, line: &Line, m: &Metrics) -> f32 {
        let frame = self.frame;
        if let Some(e) = self.cache.get_mut(&line.id) {
            e.used = frame;
            return e.height;
        }
        let e = self.layout_line(c, line, m);
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
        let key = (self.rect.w.round() as u32, c.gfx_gen, self.style_gen);
        if key != self.cache_key {
            self.cache.clear();
            self.cache_key = key;
        }
    }

    /// Scrolls by `dy` DIPs (positive = towards older lines).
    pub fn scroll(&mut self, c: &mut Ctx, b: &Buffer, dy: f32) {
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

    pub fn render(&mut self, p: &Painter, c: &mut Ctx, b: &Buffer) {
        self.frame += 1;
        self.check_cache_key(c);
        let th = c.theme;
        p.fill(self.rect, th.chat_bg);
        self.drawn.clear();
        if self.nicks.len() > 2000 {
            self.nicks.clear();
        }
        let vis: Vec<usize> = (0..b.lines.len()).filter(|&i| visible(&b.lines[i], self.show_filtered)).collect();
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

        // Scrollbar (only when not pinned).
        if !self.pinned && vis.len() > 1 {
            let frac = ai as f32 / (vis.len() - 1) as f32;
            let track = self.rect.h - 16.0;
            let thumb_h = (track * 0.08).max(24.0);
            let ty = self.rect.y + 8.0 + (track - thumb_h) * frac;
            p.fill_round(Rect::new(self.rect.right() - 7.0, ty, 4.0, thumb_h), 2.0, th.scrollbar);
            // "Jump to latest" pill.
            let label = c.text.layout("↓  Jump to latest", &c.text.fonts.ui_small, 200.0, 20.0);
            let w = text::metrics(&label).width + 24.0;
            let r = Rect::new(self.rect.x + (self.rect.w - w) / 2.0, self.rect.bottom() - 34.0, w, 24.0);
            p.fill_round(r, 12.0, th.accent);
            p.text(&label, r.x + 12.0, r.y + 5.0, th.accent_fg);
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
        if matches!(line.kind, LineKind::Message | LineKind::Action | LineKind::Notice) && !line.nick.is_empty() {
            self.nicks.insert(line.id, line.nick.to_string());
        }
        self.drawn.push(Drawn {
            id: line.id,
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
            if y < d.y || y >= d.y + d.h {
                continue;
            }
            if d.nick_rect.contains(x, y)
                && let Some(n) = self.nicks.get(&d.id)
            {
                return Hit::Nick(n.clone());
            }
            let Some(e) = self.cache.get(&d.id) else { return Hit::Nothing };
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
