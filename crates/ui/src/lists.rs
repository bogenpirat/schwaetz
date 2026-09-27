//! Sidebar (networks and buffers) and nick list.

use crate::anim::{Anims, Control, mix};
use crate::gfx::{Painter, Rect, with_alpha};
use crate::text::{self, Text};
use crate::theme::Theme;
use schwaetz_core::{Activity, App, BufferId, BufferKind, ConnState, NotifyLevel};

const NET_ROW_H: f32 = 32.0;
const ROW_H: f32 = 28.0;
/// Channels and queries sit this far right of their network.
const INDENT: f32 = 12.0;
/// The bar at the bottom with the status, add-network and settings buttons.
pub const FOOTER_H: f32 = 52.0;

/// Segoe Fluent Icons / MDL2 Assets code points.
const ICON_STATUS: &str = "\u{E8BD}";
const ICON_SETTINGS: &str = "\u{E713}";

/// Buttons in the sidebar footer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarButton {
    /// The global status buffer (client messages, script output).
    Status,
    /// Opens a menu: settings, add network.
    Settings,
}

impl SidebarButton {
    /// Hover text for the icon-only buttons.
    pub fn tooltip(self) -> Option<&'static str> {
        match self {
            SidebarButton::Status => None,
            SidebarButton::Settings => Some("Settings and networks"),
        }
    }
}

struct Row {
    id: BufferId,
    y: f32,
    h: f32,
    /// Network of the row and whether it is a channel (only channels can be rearranged, and
    /// only within their network).
    net: Option<u32>,
    channel: bool,
}

/// A channel row being dragged to a new position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowDrag {
    pub id: BufferId,
    /// Where it would land: before this channel (`None` = at the end), and the y of the line
    /// showing it.
    pub before: Option<BufferId>,
    pub line_y: f32,
}

#[derive(Default)]
pub struct Sidebar {
    pub rect: Rect,
    pub scroll: f32,
    rows: Vec<Row>,
    pub hover: Option<BufferId>,
    content_h: f32,
    buttons: Vec<(SidebarButton, Rect)>,
    /// Channel being dragged (set by the shell while the mouse moves).
    pub drag: Option<RowDrag>,
    /// Settings buttons at the right end of network rows: (server buffer, bounds).
    net_buttons: Vec<(BufferId, Rect)>,
}

impl Sidebar {
    /// The scrolling buffer list (everything above the footer).
    fn list_rect(&self) -> Rect {
        Rect::new(self.rect.x, self.rect.y, self.rect.w, (self.rect.h - FOOTER_H).max(0.0))
    }

    pub fn render(&mut self, p: &Painter, text: &Text, th: &Theme, app: &App, anims: &Anims) {
        let f = &text.fonts;
        self.rows.clear();
        self.net_buttons.clear();
        let list = self.list_rect();
        p.clip(list);
        let x = list.x;
        let w = list.w;
        let mut y = list.y + 10.0 - self.scroll;
        for id in app.sidebar_order() {
            // The status buffer lives in the footer.
            if id == app.status_buffer {
                continue;
            }
            let Some(b) = app.buffer(id) else { continue };
            let is_net = b.kind == BufferKind::Server;
            let indent = if is_net || b.network.is_none() { 0.0 } else { INDENT };
            let h = if is_net { NET_ROW_H + 6.0 } else { ROW_H };
            let row_y = if is_net { y + 6.0 } else { y };
            let row_h = if is_net { NET_ROW_H } else { h };
            let r = Rect::new(x + 8.0 + indent, row_y, w - 16.0 - indent, row_h);
            let active = app.active == id;
            row_background(p, th, r, active, if self.hover == Some(id) { 1.0 } else { 0.0 });
            // Networks have a settings button at the very right; the unread badge sits before it.
            let badge_area = if is_net {
                let gear = Rect::new(r.right() - 30.0, r.y + (r.h - 26.0) / 2.0, 26.0, 26.0);
                let c = Control::NetworkSettings(id.0);
                let (h, pr) = (anims.hover(c), anims.press(c));
                let face = icon_face(p, th, gear, h, pr);
                let g = text.layout(ICON_SETTINGS, &f.icons, gear.w, gear.h);
                let m = text::metrics(&g);
                let color = mix(th.sidebar_dim, th.sidebar_header, h.max(pr));
                p.text(&g, face.x + (face.w - m.width) / 2.0, face.y + (face.h - m.height) / 2.0, color);
                self.net_buttons.push((id, gear));
                Rect::new(r.x, r.y, gear.x - 2.0 - r.x, r.h)
            } else {
                r
            };
            let right = badge(p, text, th, app.badge(id).filter(|_| !active), badge_area);

            if is_net {
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
                let live = b.stream.as_ref().is_some_and(|s| s.live);
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
                if live {
                    // Twitch: a red dot instead of "#" while the stream is live.
                    p.circle(r.x + 17.5, r.y + r.h / 2.0, 4.0, th.live);
                } else {
                    let g = text.layout(glyph, &f.ui, 20.0, 30.0);
                    let gh = text::metrics(&g).height;
                    p.text(&g, r.x + 14.0, r.y + (r.h - gh) / 2.0, th.sidebar_dim);
                }
                let l = text.layout(&name, fmt, (right - r.x - 30.0).max(10.0), 30.0);
                let lh = text::metrics(&l).height;
                p.text(&l, r.x + 28.0, r.y + (r.h - lh) / 2.0, color);
                if b.kind == BufferKind::Query && b.joined {
                    p.circle(r.x + 25.0, r.y + r.h / 2.0 + 5.0, 2.5, th.online);
                }
            }
            if self.drag.is_some_and(|d| d.id == id) {
                // The dragged row stays in place, faded, until dropped.
                p.fill(Rect::new(x, row_y, w, row_h), with_alpha(th.backdrop_opaque, 0.6));
            }
            let (net, channel) = (b.network.map(|n| n.0), b.kind == BufferKind::Channel);
            self.rows.push(Row { id, y: row_y, h: row_h, net, channel });
            y += h;
        }
        self.content_h = y + self.scroll - list.y;
        if let Some(d) = self.drag {
            // Where the dragged channel would land.
            let lx = x + 8.0 + INDENT;
            p.fill_round(Rect::new(lx, d.line_y - 1.0, w - 16.0 - INDENT, 2.0), 1.0, th.accent);
            p.circle(lx, d.line_y, 3.0, th.accent);
        }
        p.unclip();
        self.render_footer(p, text, th, app, anims);
    }

    /// Status button (with the status buffer's unread count), then the settings menu button.
    fn render_footer(&mut self, p: &Painter, text: &Text, th: &Theme, app: &App, anims: &Anims) {
        let f = &text.fonts;
        self.buttons.clear();
        let fy = self.rect.bottom() - FOOTER_H;
        p.line(self.rect.x + 12.0, fy + 0.5, self.rect.right() - 12.0, fy + 0.5, th.border, 1.0);
        let icon = 32.0;
        let settings = Rect::new(self.rect.right() - 8.0 - icon, fy + 10.0, icon, icon);
        let status = Rect::new(self.rect.x + 8.0, fy + 8.0, (settings.x - 8.0 - self.rect.x - 8.0).max(40.0), 36.0);

        let sb = app.status_buffer;
        let active = app.active == sb;
        let status = status.inset(anims.press(Control::SidebarStatus) * 1.5, anims.press(Control::SidebarStatus) * 1.0);
        row_background(p, th, status, active, anims.hover(Control::SidebarStatus));
        let right = badge(p, text, th, app.badge(sb).filter(|_| !active), status);
        let g = text.layout(ICON_STATUS, &f.icons, 24.0, 24.0);
        let gm = text::metrics(&g);
        p.text(&g, status.x + 12.0, status.y + (status.h - gm.height) / 2.0, th.sidebar_fg);
        let l = text.layout("Status", &f.ui_semibold, (right - status.x - 40.0).max(10.0), 24.0);
        let lh = text::metrics(&l).height;
        p.text(&l, status.x + 36.0, status.y + (status.h - lh) / 2.0, th.sidebar_header);
        self.buttons.push((SidebarButton::Status, status));

        let c = Control::SidebarSettings;
        let face = icon_face(p, th, settings, anims.hover(c), anims.press(c));
        let g = text.layout(ICON_SETTINGS, &f.icons, settings.w, settings.h);
        let m = text::metrics(&g);
        p.text(&g, face.x + (face.w - m.width) / 2.0, face.y + (face.h - m.height) / 2.0, th.sidebar_fg);
        self.buttons.push((SidebarButton::Settings, settings));
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<BufferId> {
        if !self.list_rect().contains(x, y) {
            return None;
        }
        self.rows.iter().find(|r| y >= r.y && y < r.y + r.h).map(|r| r.id)
    }

    /// Where a dragged channel would be dropped for pointer height `y`: before which channel of
    /// its network (`None` = after the last), and the y of the insertion line.
    pub fn drop_target(&self, dragged: BufferId, y: f32) -> Option<(Option<BufferId>, f32)> {
        let d = self.rows.iter().find(|r| r.id == dragged && r.channel)?;
        let group: Vec<&Row> = self.rows.iter().filter(|r| r.channel && r.net == d.net).collect();
        for r in &group {
            if y < r.y + r.h / 2.0 {
                return Some((Some(r.id), r.y));
            }
        }
        let last = group.last()?;
        Some((None, last.y + last.h))
    }

    /// The settings button of a network row under the pointer: (server buffer, bounds).
    pub fn network_button_at(&self, x: f32, y: f32) -> Option<(BufferId, Rect)> {
        if !self.list_rect().contains(x, y) {
            return None;
        }
        self.net_buttons.iter().find(|(_, r)| r.contains(x, y)).copied()
    }

    /// The footer button under the pointer, with its bounds.
    pub fn button_at(&self, x: f32, y: f32) -> Option<(SidebarButton, Rect)> {
        self.buttons.iter().find(|(_, r)| r.contains(x, y)).copied()
    }

    pub fn scroll_by(&mut self, dy: f32) {
        let max = (self.content_h - self.list_rect().h + 10.0).max(0.0);
        self.scroll = (self.scroll - dy).clamp(0.0, max);
    }
}

fn row_background(p: &Painter, th: &Theme, r: Rect, active: bool, hover: f32) {
    if active {
        p.fill_round(r, 6.0, th.sidebar_selected);
        p.fill_round(Rect::new(r.x, r.y + 7.0, 3.0, r.h - 14.0), 1.5, th.accent);
    } else if hover > 0.0 {
        p.fill_round(r, 6.0, with_alpha(th.sidebar_hover, th.sidebar_hover.a * hover));
    }
}

/// Background of a small icon button: fades in on hover, deepens and shrinks while pressed.
/// Returns the rect to center the icon in.
fn icon_face(p: &Painter, th: &Theme, r: Rect, hover: f32, press: f32) -> Rect {
    let face = r.inset(press * 1.5, press * 1.5);
    let a = th.sidebar_hover.a * hover + th.sidebar_selected.a * press;
    if a > 0.0 {
        p.fill_round(face, 6.0, with_alpha(th.sidebar_hover, a));
    }
    face
}

/// Unread badge (count, includes highlights) at the right end of a row; returns where the row's
/// text has to end.
fn badge(p: &Painter, text: &Text, th: &Theme, badge: Option<(u32, bool)>, r: Rect) -> f32 {
    let right = r.right() - 8.0;
    let Some((count, highlight)) = badge else { return right };
    let label = if count > 999 { "999+".to_owned() } else { count.to_string() };
    let l = text.layout(&label, &text.fonts.ui_small, 60.0, 20.0);
    let tw = text::metrics(&l).width;
    let bw = (tw + 12.0).max(20.0);
    let br = Rect::new(right - bw, r.y + (r.h - 18.0) / 2.0, bw, 18.0);
    let (bg, fg) = if highlight { (th.badge_highlight, th.accent_fg) } else { (th.badge_bg, th.badge_fg) };
    p.fill_round(br, 9.0, bg);
    p.text(&l, br.x + (bw - tw) / 2.0, br.y + 2.0, fg);
    br.x - 6.0
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

    /// `colored`: the "Colored nicks" setting (off: all names in the text color).
    pub fn render(&mut self, p: &Painter, text: &Text, th: &Theme, colored: bool) {
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
            let color = if *away {
                with_alpha(th.text_dim, 0.8)
            } else if colored {
                th.nick_color(nick)
            } else {
                th.text
            };
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

impl Sidebar {
    /// Row rectangles as last drawn (accessibility, hit testing).
    pub fn row_rects(&self) -> Vec<(BufferId, Rect)> {
        self.rows.iter().map(|r| (r.id, Rect::new(self.rect.x + 8.0, r.y, self.rect.w - 16.0, r.h))).collect()
    }
}
