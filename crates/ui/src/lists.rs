//! Sidebar (networks and buffers) and nick list.

use crate::gfx::{Painter, Rect, with_alpha};
use crate::text::{self, Text};
use crate::theme::Theme;
use schwaetz_core::{Activity, App, BufferId, BufferKind, ConnState, NotifyLevel};

const NET_ROW_H: f32 = 32.0;
const ROW_H: f32 = 28.0;
const HEADER_H: f32 = 44.0;

struct Row {
    id: BufferId,
    y: f32,
    h: f32,
}

#[derive(Default)]
pub struct Sidebar {
    pub rect: Rect,
    pub scroll: f32,
    rows: Vec<Row>,
    pub hover: Option<BufferId>,
    content_h: f32,
}

impl Sidebar {
    pub fn render(&mut self, p: &Painter, text: &Text, th: &Theme, app: &App) {
        let f = &text.fonts;
        self.rows.clear();
        p.clip(self.rect);
        let x = self.rect.x;
        let w = self.rect.w;
        // App title row (the global status buffer).
        let mut y = self.rect.y + 10.0 - self.scroll;
        let order = app.sidebar_order();
        for id in order {
            let Some(b) = app.buffer(id) else { continue };
            let is_net = b.kind == BufferKind::Server;
            let is_status = id == app.status_buffer;
            let h = if is_status {
                HEADER_H
            } else if is_net {
                NET_ROW_H + 6.0
            } else {
                ROW_H
            };
            let row_y = if is_net { y + 6.0 } else { y };
            let row_h = if is_net { NET_ROW_H } else { h };
            let r = Rect::new(x + 8.0, row_y, w - 16.0, row_h);
            let active = app.active == id;
            if active {
                p.fill_round(r, 6.0, th.sidebar_selected);
                p.fill_round(Rect::new(r.x, r.y + 7.0, 3.0, r.h - 14.0), 1.5, th.accent);
            } else if self.hover == Some(id) {
                p.fill_round(r, 6.0, th.sidebar_hover);
            }

            // Badge.
            let mut right = r.right() - 8.0;
            if b.unread > 0 && !active {
                let label = if b.unread > 999 { "999+".to_owned() } else { b.unread.to_string() };
                let l = text.layout(&label, &f.ui_small, 60.0, 20.0);
                let tw = text::metrics(&l).width;
                let bw = (tw + 12.0).max(20.0);
                let br = Rect::new(right - bw, r.y + (r.h - 18.0) / 2.0, bw, 18.0);
                let (bg, fg) =
                    if b.highlights > 0 { (th.badge_highlight, th.accent_fg) } else { (th.badge_bg, th.badge_fg) };
                p.fill_round(br, 9.0, bg);
                p.text(&l, br.x + (bw - tw) / 2.0, br.y + 2.0, fg);
                right = br.x - 6.0;
            }

            if is_status {
                let l = text.layout("schwätz", &f.title, w, 30.0);
                p.text(&l, r.x + 12.0, r.y + 10.0, th.sidebar_header);
            } else if is_net {
                let net = b.network.and_then(|n| app.network(n));
                let (dot, name) = match net {
                    Some(n) => (
                        match n.conn {
                            ConnState::Ready => th.online,
                            ConnState::Connecting | ConnState::Connected => th.connecting,
                            ConnState::Disconnected => th.offline,
                        },
                        n.display_name().to_owned(),
                    ),
                    None => (th.offline, b.name.clone()),
                };
                p.circle(r.x + 16.0, r.y + r.h / 2.0, 4.0, dot);
                let l = text.layout(&name, &f.ui_semibold, (right - r.x - 30.0).max(10.0), 30.0);
                let lh = text::metrics(&l).height;
                p.text(&l, r.x + 28.0, r.y + (r.h - lh) / 2.0, th.sidebar_header);
            } else {
                let (glyph, dim) = match b.kind {
                    BufferKind::Channel => ("#", !b.joined),
                    BufferKind::Query => ("@", false),
                    _ => ("·", false),
                };
                let name = match b.kind {
                    BufferKind::Channel => b.name.trim_start_matches(['#', '&']).to_owned(),
                    _ => b.name.clone(),
                };
                let color = if dim || b.notify == NotifyLevel::Mute {
                    with_alpha(th.sidebar_dim, 0.7)
                } else if active || b.activity >= Activity::Messages {
                    th.sidebar_header
                } else if b.activity == Activity::Events {
                    th.sidebar_fg
                } else {
                    th.sidebar_dim
                };
                let fmt = if b.activity >= Activity::Messages && !active { &f.ui_semibold } else { &f.ui };
                let g = text.layout(glyph, &f.ui, 20.0, 30.0);
                let gh = text::metrics(&g).height;
                p.text(&g, r.x + 14.0, r.y + (r.h - gh) / 2.0, th.sidebar_dim);
                let l = text.layout(&name, fmt, (right - r.x - 30.0).max(10.0), 30.0);
                let lh = text::metrics(&l).height;
                p.text(&l, r.x + 28.0, r.y + (r.h - lh) / 2.0, color);
                if b.kind == BufferKind::Query && b.joined {
                    p.circle(r.x + 25.0, r.y + r.h / 2.0 + 5.0, 2.5, th.online);
                }
            }
            self.rows.push(Row { id, y: row_y, h: row_h });
            y += h;
        }
        self.content_h = y + self.scroll - self.rect.y;
        p.unclip();
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<BufferId> {
        if !self.rect.contains(x, y) {
            return None;
        }
        self.rows.iter().find(|r| y >= r.y && y < r.y + r.h).map(|r| r.id)
    }

    pub fn scroll_by(&mut self, dy: f32) {
        let max = (self.content_h - self.rect.h + 10.0).max(0.0);
        self.scroll = (self.scroll - dy).clamp(0.0, max);
    }
}

#[derive(Default)]
pub struct NickList {
    pub rect: Rect,
    pub scroll: f32,
    members: Vec<(Option<char>, String, bool, bool)>,
    rows_top: f32,
    row_h: f32,
    pub hover: Option<usize>,
}

impl NickList {
    /// Rebuilds the member list for a channel buffer.
    pub fn refresh(&mut self, app: &App, id: BufferId) {
        self.members.clear();
        let Some(b) = app.buffer(id) else { return };
        let Some(net) = b.network.and_then(|n| app.network(n)) else { return };
        let Some(ch) = net.session.channel(&b.name) else { return };
        let is = net.session.isupport();
        for m in ch.sorted_members(is) {
            let u = net.session.user(&m.nick);
            let away = u.is_some_and(|u| u.away.is_some());
            let bot = u.is_some_and(|u| u.bot);
            self.members.push((m.highest(), m.nick.clone(), away, bot));
        }
        let max = (self.members.len() as f32 * 22.0 - self.rect.h + 40.0).max(0.0);
        self.scroll = self.scroll.min(max);
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    pub fn render(&mut self, p: &Painter, text: &Text, th: &Theme) {
        let f = &text.fonts;
        p.fill(self.rect, th.nicklist_bg);
        p.line(self.rect.x, self.rect.y, self.rect.x, self.rect.bottom(), th.border, 1.0);
        p.clip(self.rect);
        let n = self.members.len();
        let header =
            text.layout(&format!("{n} {}", if n == 1 { "MEMBER" } else { "MEMBERS" }), &f.ui_small, self.rect.w, 20.0);
        p.text(&header, self.rect.x + 16.0, self.rect.y + 12.0, th.text_dim);
        self.row_h = 24.0;
        self.rows_top = self.rect.y + 36.0;
        let first = (self.scroll / self.row_h) as usize;
        let count = (self.rect.h / self.row_h) as usize + 2;
        for (i, (prefix, nick, away, bot)) in self.members.iter().enumerate().skip(first).take(count) {
            let y = self.rows_top + i as f32 * self.row_h - self.scroll;
            let r = Rect::new(self.rect.x + 6.0, y, self.rect.w - 12.0, self.row_h);
            if self.hover == Some(i) {
                p.fill_round(r, 5.0, th.sidebar_hover);
            }
            if let Some(pf) = prefix {
                let color = match pf {
                    '~' | '&' => th.error,
                    '@' => th.highlight_bar,
                    '%' => th.notice,
                    '+' => th.join,
                    _ => th.text_dim,
                };
                let l = text.layout(&pf.to_string(), &f.ui_semibold, 20.0, 20.0);
                p.text(&l, r.x + 8.0, y + 4.0, color);
            }
            let name = if *bot { format!("{nick} 🤖") } else { nick.clone() };
            let l = text.layout(&name, &f.ui, r.w - 30.0, 20.0);
            let color = if *away { with_alpha(th.text_dim, 0.8) } else { th.nick_color(nick) };
            p.text(&l, r.x + 22.0, y + 4.0, color);
        }
        p.unclip();
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        if !self.rect.contains(x, y) || y < self.rows_top || self.row_h <= 0.0 {
            return None;
        }
        let i = ((y - self.rows_top + self.scroll) / self.row_h) as usize;
        (i < self.members.len()).then_some(i)
    }

    pub fn nick(&self, i: usize) -> Option<&str> {
        self.members.get(i).map(|m| m.1.as_str())
    }

    pub fn scroll_by(&mut self, dy: f32) {
        let max = (self.members.len() as f32 * self.row_h.max(1.0) - self.rect.h + 40.0).max(0.0);
        self.scroll = (self.scroll - dy).clamp(0.0, max);
    }
}
