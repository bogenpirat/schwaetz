//! Sidebar (networks and buffers) and nick list.

use crate::anim::{Anims, Control, mix, smoothstep};
use crate::gfx::{Painter, Rect, with_alpha};
use crate::text::{self, Text};
use crate::theme::Theme;
use schwaetz_core::{Activity, App, BufferId, BufferKind, ConnState, NotifyLevel};
use std::cmp::Ordering;
use std::time::{Duration, Instant};

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
    /// Network of the row and whether it is a channel (channels can be rearranged only within
    /// their network) or a network (rearranged together with its buffers).
    net: Option<u32>,
    channel: bool,
    server: bool,
}

/// A channel or network row being dragged to a new position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowDrag {
    pub id: BufferId,
    /// Where it would land: before this channel or network (`None` = at the end), and the y of
    /// the line showing it.
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
    /// Channel or network being dragged (set by the shell while the mouse moves).
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
        // A dragged network fades with all its buffers.
        let dragged = self.drag.and_then(|d| app.buffer(d.id)).map(|b| (b.id, b.kind == BufferKind::Server, b.network));
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
                // Like channels: grey unless selected or something happened in the server buffer itself.
                let color = if b.notify == NotifyLevel::Mute {
                    with_alpha(th.sidebar_dim, 0.7)
                } else if active || b.activity >= Activity::Messages {
                    th.sidebar_header
                } else {
                    th.sidebar_dim
                };
                p.text(&l, r.x + 28.0, r.y + (r.h - lh) / 2.0, color);
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
            if dragged.is_some_and(|(d, server, net)| d == id || (server && net.is_some() && net == b.network)) {
                // The dragged row stays in place, faded, until dropped.
                p.fill(Rect::new(x, row_y, w, row_h), with_alpha(th.backdrop_opaque, 0.6));
            }
            let (net, channel, server) = (b.network.map(|n| n.0), b.kind == BufferKind::Channel, is_net);
            self.rows.push(Row { id, y: row_y, h: row_h, net, channel, server });
            y += h;
        }
        self.content_h = y + self.scroll - list.y;
        if let Some(d) = self.drag {
            // Where the dragged row would land (networks at their own, unindented level).
            let indent = if dragged.is_some_and(|(_, server, _)| server) { 0.0 } else { INDENT };
            let lx = x + 8.0 + indent;
            p.fill_round(Rect::new(lx, d.line_y - 1.0, w - 16.0 - indent, 2.0), 1.0, th.accent);
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

    /// Where a dragged channel or network would be dropped for pointer height `y`: before which
    /// channel of its network or which network (`None` = after the last), and the y of the
    /// insertion line.
    pub fn drop_target(&self, dragged: BufferId, y: f32) -> Option<(Option<BufferId>, f32)> {
        let d = self.rows.iter().find(|r| r.id == dragged && (r.channel || r.server))?;
        if d.server {
            return self.network_drop_target(y);
        }
        let group: Vec<&Row> = self.rows.iter().filter(|r| r.channel && r.net == d.net).collect();
        for r in &group {
            if y < r.y + r.h / 2.0 {
                return Some((Some(r.id), r.y));
            }
        }
        let last = group.last()?;
        Some((None, last.y + last.h))
    }

    /// A network goes before the first network whose block (its row and buffers) has its middle
    /// below `y`; the line sits in the gap above that network's row.
    fn network_drop_target(&self, y: f32) -> Option<(Option<BufferId>, f32)> {
        let mut blocks: Vec<(BufferId, f32, f32)> = Vec::new();
        for r in &self.rows {
            if r.server {
                blocks.push((r.id, r.y, r.y + r.h));
            } else if let Some(last) = blocks.last_mut()
                && r.net.is_some()
            {
                last.2 = r.y + r.h;
            }
        }
        for (id, top, bottom) in &blocks {
            if y < (top + bottom) / 2.0 {
                return Some((Some(*id), top - 3.0));
            }
        }
        let (_, _, bottom) = blocks.last()?;
        Some((None, bottom + 3.0))
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

/// A row of the member list.
#[derive(Clone, Debug)]
struct Member {
    prefix: Option<char>,
    nick: String,
    away: bool,
    bot: bool,
    /// Where it sorts: prefix rank, then the case-folded nick.
    key: (usize, String),
    /// Sliding in or out; `None` once settled.
    motion: Option<Motion>,
}

impl Member {
    /// How much of the row is there, eased: 0 collapsed, 1 full height.
    fn presence(&self, now: Instant) -> f32 {
        self.motion.map_or(1.0, |m| smoothstep(m.progress(now)))
    }

    fn leaving(&self) -> bool {
        self.motion.is_some_and(|m| m.leaving)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Motion {
    /// When the row starts to move (later rows of a batch wait their turn).
    start: Instant,
    leaving: bool,
}

impl Motion {
    /// Linear presence: rises from 0 (joining) or falls from 1 (leaving) once `start` is reached.
    fn progress(self, now: Instant) -> f32 {
        let t = (now.saturating_duration_since(self.start).as_secs_f32() / MEMBER_MOTION.as_secs_f32()).min(1.0);
        if self.leaving { 1.0 - t } else { t }
    }

    fn done(self, now: Instant) -> bool {
        now >= self.start + MEMBER_MOTION
    }

    /// The opposite motion, picking up where this one is (someone rejoining while sliding out, or
    /// leaving while still sliding in).
    fn reversed(self, now: Instant) -> Motion {
        let p = self.progress(now);
        let leaving = !self.leaving;
        let elapsed = MEMBER_MOTION.mul_f32(if leaving { 1.0 - p } else { p });
        Motion { start: now.checked_sub(elapsed).unwrap_or(now), leaving }
    }
}

/// How long a member takes to slide in or out.
const MEMBER_MOTION: Duration = Duration::from_millis(220);
/// The changes of one batch start one after another, top to bottom, this far apart...
const CASCADE_STEP: Duration = Duration::from_millis(35);
/// ...closer together in a big batch, so the last one starts at most this late.
const CASCADE_SPREAD: Duration = Duration::from_millis(350);

/// Merges a channel's members into the rows on screen; both are sorted by key, and the rows may
/// include members still sliding out. Changes within `view` (the visible span, measured from the
/// top of the first row) slide in and out in a cascade from top to bottom. Changes above it apply
/// at once, and the returned scroll shift keeps the visible rows still; changes below it just
/// apply.
fn merge(old: Vec<Member>, new: Vec<Member>, now: Instant, view: (f32, f32)) -> (Vec<Member>, f32) {
    let mut out = Vec::with_capacity(old.len().max(new.len()));
    let mut shift = 0.0;
    // Rows that start to move now, in order from the top.
    let mut cascade = Vec::new();
    // Where the next change lies on screen: the top of the next old row.
    let mut y = 0.0;
    let mut old = old.into_iter().peekable();
    let mut new = new.into_iter().peekable();
    loop {
        let order = match (old.peek(), new.peek()) {
            (None, None) => break,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some(o), Some(n)) => o.key.cmp(&n.key),
        };
        let m = match order {
            Ordering::Less => {
                let Some(mut m) = old.next() else { break };
                let h = MEMBER_H * m.presence(now);
                let top = y;
                y += h;
                match m.motion {
                    Some(mo) if mo.leaving => {}
                    _ if top + h <= view.0 => {
                        shift -= h;
                        continue;
                    }
                    _ if top >= view.1 => continue,
                    Some(mo) => m.motion = Some(mo.reversed(now)),
                    None => {
                        m.motion = Some(Motion { start: now, leaving: true });
                        cascade.push(out.len());
                    }
                }
                m
            }
            Ordering::Greater => {
                let Some(mut m) = new.next() else { break };
                if y < view.0 {
                    shift += MEMBER_H;
                } else if y < view.1 {
                    m.motion = Some(Motion { start: now, leaving: false });
                    cascade.push(out.len());
                }
                m
            }
            Ordering::Equal => {
                let (Some(o), Some(mut m)) = (old.next(), new.next()) else { break };
                y += MEMBER_H * o.presence(now);
                m.motion = match o.motion {
                    Some(mo) if mo.leaving => Some(mo.reversed(now)),
                    mo => mo,
                };
                m
            }
        };
        out.push(m);
    }
    if let Some(n) = u32::try_from(cascade.len()).ok().filter(|&n| n > 0) {
        let step = CASCADE_STEP.min(CASCADE_SPREAD / n);
        for (k, i) in (0u32..).zip(cascade) {
            if let Some(mo) = &mut out[i].motion {
                mo.start = now + step * k;
            }
        }
    }
    (out, shift)
}

#[derive(Default)]
pub struct NickList {
    pub rect: Rect,
    pub scroll: f32,
    members: Vec<Member>,
    /// Members in the channel (rows sliding out are not counted).
    count: usize,
    /// Some row is sliding in or out: rows vary in height and the list needs more frames.
    moving: bool,
    rows_top: f32,
    pub hover: Option<usize>,
    /// Scrollbar as last drawn (only while the members don't all fit).
    pub bar: Option<crate::scrollbar::Scrollbar>,
    /// The scrollbar is hovered or dragged (drawn wider).
    pub bar_hot: bool,
}

/// Height of a member row.
const MEMBER_H: f32 = 24.0;
/// Space above the first member (the header).
const MEMBERS_TOP: f32 = 36.0;

impl NickList {
    /// Rebuilds the member list for a channel buffer. `animate`: the list already shows this
    /// channel, and members joining or leaving in view slide in and out; otherwise the list is
    /// replaced at once.
    pub fn refresh(&mut self, app: &App, id: BufferId, animate: bool) {
        let now = Instant::now();
        let old = std::mem::take(&mut self.members);
        let hovered = self.hover.and_then(|i| old.get(i)).map(|m| m.key.clone());
        // A list filling up for the first time appears at once.
        let animate = animate && old.iter().any(|m| !m.leaving());
        let (old, view) =
            if animate { (old, (self.scroll, self.scroll + self.view_h())) } else { (Vec::new(), (0.0, 0.0)) };
        let (members, shift) = merge(old, members_of(app, id), now, view);
        self.members = members;
        self.count = self.members.iter().filter(|m| !m.leaving()).count();
        self.moving = self.members.iter().any(|m| m.motion.is_some());
        self.hover = hovered.and_then(|k| self.members.iter().position(|m| m.key == k && !m.leaving()));
        self.scroll = (self.scroll + shift).clamp(0.0, self.max_scroll(now));
    }

    /// Whether rows are still sliding in or out (the next frame should follow soon).
    pub fn animating(&self) -> bool {
        self.moving
    }

    /// Drops the rows that finished sliding out and settles the ones that finished sliding in.
    fn settle(&mut self, now: Instant) {
        if !self.moving {
            return;
        }
        let hovered = self.hover.and_then(|i| self.members.get(i)).map(|m| m.key.clone());
        self.members.retain_mut(|m| match m.motion {
            Some(mo) if mo.done(now) => {
                m.motion = None;
                !mo.leaving
            }
            _ => true,
        });
        self.moving = self.members.iter().any(|m| m.motion.is_some());
        self.hover = hovered.and_then(|k| self.members.iter().position(|m| m.key == k));
    }

    /// Height of the area the rows scroll in.
    fn view_h(&self) -> f32 {
        (self.rect.h - MEMBERS_TOP).max(0.0)
    }

    /// Height of all rows (rows sliding in or out count with their current height).
    fn content_h(&self, now: Instant) -> f32 {
        if self.moving {
            self.members.iter().map(|m| MEMBER_H * m.presence(now)).sum()
        } else {
            self.members.len() as f32 * MEMBER_H
        }
    }

    /// How far the list scrolls: the members' height beyond what fits below the header.
    fn max_scroll(&self, now: Instant) -> f32 {
        (self.content_h(now) - (self.view_h() - 4.0)).max(0.0)
    }

    /// Rows on screen: (member index, top measured from the first row, height).
    fn visible_rows(&self, now: Instant) -> Vec<(usize, f32, f32)> {
        let (top, bottom) = (self.scroll, self.scroll + self.view_h());
        if !self.moving {
            let first = (top / MEMBER_H) as usize;
            let end = ((bottom / MEMBER_H).ceil() as usize).min(self.members.len());
            return (first..end).map(|i| (i, i as f32 * MEMBER_H, MEMBER_H)).collect();
        }
        let mut rows = Vec::new();
        let mut y = 0.0;
        for (i, m) in self.members.iter().enumerate() {
            if y >= bottom {
                break;
            }
            let h = MEMBER_H * m.presence(now);
            if h > 0.0 && y + h > top {
                rows.push((i, y, h));
            }
            y += h;
        }
        rows
    }

    /// Scrolls to a scrollbar position (0 = top, 1 = bottom).
    pub fn scroll_to(&mut self, pos: f32) {
        self.scroll = self.max_scroll(Instant::now()) * pos.clamp(0.0, 1.0);
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// `colored`: the "Colored nicks" setting (off: all names in the text color).
    pub fn render(&mut self, p: &Painter, text: &Text, th: &Theme, colored: bool) {
        let now = Instant::now();
        self.settle(now);
        // Rows sliding out at the end let the list follow them down.
        let max = self.max_scroll(now);
        self.scroll = self.scroll.min(max);
        let f = &text.fonts;
        p.fill(self.rect, th.nicklist_bg);
        p.line(self.rect.x, self.rect.y, self.rect.x, self.rect.bottom(), th.border, 1.0);
        let n = self.count;
        let header =
            text.layout(&format!("{n} {}", if n == 1 { "MEMBER" } else { "MEMBERS" }), &f.ui_small, self.rect.w, 20.0);
        p.text(&header, self.rect.x + 16.0, self.rect.y + 12.0, th.text_dim);
        self.rows_top = self.rect.y + MEMBERS_TOP;
        // Rows scroll under the header, not over it.
        let area = Rect::new(self.rect.x, self.rows_top, self.rect.w, self.rect.bottom() - self.rows_top);
        p.clip(area);
        for (i, top, h) in self.visible_rows(now) {
            let m = &self.members[i];
            let y = self.rows_top + top - self.scroll;
            let r = Rect::new(self.rect.x + 6.0, y, self.rect.w - 12.0, h);
            // A row sliding in or out opens or closes around its middle and fades with it.
            let presence = h / MEMBER_H;
            let partial = presence < 1.0;
            if partial {
                p.clip(r);
            }
            let ty = y + (h - MEMBER_H) / 2.0 + 4.0;
            let fade = |c: crate::gfx::Color| if partial { with_alpha(c, c.a * presence) } else { c };
            if self.hover == Some(i) {
                p.fill_round(r, 5.0, fade(th.sidebar_hover));
            }
            if let Some(pf) = m.prefix {
                let color = match pf {
                    '~' | '&' => th.error,
                    '@' => th.highlight_bar,
                    '%' => th.notice,
                    '+' => th.join,
                    _ => th.text_dim,
                };
                let l = text.layout(&pf.to_string(), &f.ui_semibold, 20.0, 20.0);
                p.text(&l, r.x + 8.0, ty, fade(color));
            }
            let name = if m.bot { format!("{} 🤖", m.nick) } else { m.nick.clone() };
            let l = text.layout(&name, &f.ui, r.w - 30.0, 20.0);
            let color = if m.away {
                with_alpha(th.text_dim, 0.8)
            } else if colored {
                th.nick_color(&m.nick)
            } else {
                th.text
            };
            p.text(&l, r.x + 22.0, ty, fade(color));
            if partial {
                p.unclip();
            }
        }
        p.unclip();
        self.bar = (max > 0.0).then(|| {
            let content = self.content_h(now);
            crate::scrollbar::Scrollbar::new(area, (content - max) / content, self.scroll / max)
        });
        if let Some(bar) = self.bar {
            bar.draw(p, th, self.bar_hot);
        }
    }

    /// The member row under the pointer (not one sliding out).
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        if !self.rect.contains(x, y) || y < self.rows_top {
            return None;
        }
        let cy = y - self.rows_top + self.scroll;
        let (i, ..) = self.visible_rows(Instant::now()).into_iter().find(|&(_, top, h)| cy >= top && cy < top + h)?;
        (!self.members[i].leaving()).then_some(i)
    }

    pub fn nick(&self, i: usize) -> Option<&str> {
        self.members.get(i).map(|m| m.nick.as_str())
    }

    pub fn scroll_by(&mut self, dy: f32) {
        self.scroll = (self.scroll - dy).clamp(0.0, self.max_scroll(Instant::now()));
    }
}

/// A channel's members, sorted by rank and then name.
fn members_of(app: &App, id: BufferId) -> Vec<Member> {
    let Some(b) = app.buffer(id) else { return Vec::new() };
    let Some(net) = b.network.and_then(|n| app.network(n)) else { return Vec::new() };
    let Some(ch) = net.session.channel(&b.name) else { return Vec::new() };
    let is = net.session.isupport();
    let mut out: Vec<Member> = ch
        .members
        .values()
        .map(|m| {
            let u = net.session.user(&m.nick);
            let prefix = m.highest();
            Member {
                prefix,
                nick: m.nick.clone(),
                away: u.is_some_and(|u| u.away.is_some()),
                bot: u.is_some_and(|u| u.bot),
                key: (prefix.map_or(usize::MAX, |p| is.prefix_rank(p)), is.casemapping.fold(&m.nick).into_owned()),
                motion: None,
            }
        })
        .collect();
    out.sort_unstable_by(|a, b| a.key.cmp(&b.key));
    out
}

impl Sidebar {
    /// Row rectangles as last drawn (accessibility, hit testing).
    pub fn row_rects(&self) -> Vec<(BufferId, Rect)> {
        self.rows.iter().map(|r| (r.id, Rect::new(self.rect.x + 8.0, r.y, self.rect.w - 16.0, r.h))).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(nick: &str) -> Member {
        Member {
            prefix: None,
            nick: nick.to_owned(),
            away: false,
            bot: false,
            key: (usize::MAX, nick.to_owned()),
            motion: None,
        }
    }

    fn list(nicks: &[&str]) -> Vec<Member> {
        nicks.iter().map(|n| member(n)).collect()
    }

    fn nicks(members: &[Member]) -> Vec<&str> {
        members.iter().map(|m| m.nick.as_str()).collect()
    }

    const ALL: (f32, f32) = (0.0, 1000.0);

    #[test]
    fn joins_and_parts_in_view_cascade_from_the_top() {
        let now = Instant::now();
        let (out, shift) = merge(list(&["a", "c", "d"]), list(&["a", "b", "d"]), now, ALL);
        assert_eq!(nicks(&out), ["a", "b", "c", "d"]);
        assert_eq!(shift, 0.0);
        let (b, c) = (out[1].motion.unwrap(), out[2].motion.unwrap());
        assert!(!b.leaving && c.leaving);
        assert_eq!(b.start, now);
        assert_eq!(c.start, now + CASCADE_STEP);
        // Before its turn, a joining row is collapsed and a leaving one still full.
        assert_eq!(out[1].presence(now), 0.0);
        assert_eq!(out[2].presence(now), 1.0);
        assert!(out[0].motion.is_none() && out[3].motion.is_none());
    }

    #[test]
    fn a_big_batch_starts_within_the_spread() {
        let now = Instant::now();
        let new: Vec<String> = (0..100).map(|i| format!("n{i:03}")).collect();
        let new: Vec<&str> = new.iter().map(String::as_str).collect();
        let (out, _) = merge(list(&["zz"]), list(&new), now, (0.0, 10_000.0));
        let last = out.iter().filter_map(|m| m.motion).map(|m| m.start).max().unwrap();
        assert!(last <= now + CASCADE_SPREAD);
    }

    #[test]
    fn changes_outside_the_view_apply_at_once() {
        let now = Instant::now();
        // Rows b and c are scrolled out above, f and g lie below.
        let view = (2.0 * MEMBER_H, 4.0 * MEMBER_H);
        let (out, shift) =
            merge(list(&["a", "b", "c", "d", "e", "f"]), list(&["a", "aa", "c", "d", "e", "g"]), now, view);
        assert_eq!(nicks(&out), ["a", "aa", "c", "d", "e", "g"]);
        assert!(out.iter().all(|m| m.motion.is_none()));
        // One row in and one out above the view: the visible rows stay where they are.
        assert_eq!(shift, 0.0);
        let (_, shift) = merge(list(&["a", "b", "c"]), list(&["c"]), now, (2.0 * MEMBER_H, 3.0 * MEMBER_H));
        assert_eq!(shift, -2.0 * MEMBER_H);
    }

    #[test]
    fn rejoining_while_sliding_out_turns_around() {
        let now = Instant::now();
        let (out, _) = merge(list(&["a", "b"]), list(&["a"]), now, ALL);
        let later = now + MEMBER_MOTION / 2;
        let before = out[1].presence(later);
        let (out, _) = merge(out, list(&["a", "b"]), later, ALL);
        let m = out[1].motion.unwrap();
        assert!(!m.leaving);
        assert!((out[1].presence(later) - before).abs() < 1e-3);
    }

    #[test]
    fn a_member_still_sliding_out_stays_until_done() {
        let now = Instant::now();
        let (out, _) = merge(list(&["a", "b"]), list(&["a"]), now, ALL);
        let (out, _) = merge(out, list(&["a", "c"]), now, ALL);
        assert_eq!(nicks(&out), ["a", "b", "c"]);
        assert!(out[1].leaving());
    }
}
