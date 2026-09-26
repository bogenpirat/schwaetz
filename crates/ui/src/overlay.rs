//! Modal overlays drawn over the main window: quick switcher, confirmations, channel list.

use crate::editor::Editor;
use crate::gfx::{Painter, Rect, with_alpha};
use crate::text::{self, Text};
use crate::theme::Theme;
use schwaetz_core::{Activity, App, BufferId, BufferKind};
use schwaetz_net::NetworkId;

pub enum ConfirmAction {
    Paste { buffer: BufferId, text: String },
    AddZncNetworks { net: NetworkId, names: Vec<String> },
}

pub enum Overlay {
    QuickSwitch { input: Editor, selected: usize, results: Vec<BufferId> },
    Confirm { title: String, body: String, yes: String, action: ConfirmAction },
    ChannelList { net: NetworkId, filter: Editor, scroll: f32, selected: usize, rows: Vec<usize>, by_users: bool },
}

/// Subsequence fuzzy match; higher is better, `None` if not all characters match.
pub fn fuzzy_score(pattern: &str, candidate: &str) -> Option<i32> {
    if pattern.is_empty() {
        return Some(0);
    }
    let c: Vec<char> = candidate.to_lowercase().chars().collect();
    let mut score = 0;
    let mut ci = 0;
    let mut prev_match: Option<usize> = None;
    for pc in pattern.to_lowercase().chars() {
        let mut found = None;
        while ci < c.len() {
            if c[ci] == pc {
                found = Some(ci);
                ci += 1;
                break;
            }
            ci += 1;
        }
        let i = found?;
        score += 1;
        if prev_match.is_some_and(|p| p + 1 == i) {
            score += 8;
        }
        if i == 0 || " #&@".contains(c[i - 1]) {
            score += 8;
        } else if !c[i - 1].is_alphanumeric() {
            score += 1;
        }
        prev_match = Some(i);
    }
    Some(score * 10 - c.len() as i32)
}

impl Overlay {
    pub fn quick_switch(app: &App) -> Overlay {
        let mut o = Overlay::QuickSwitch { input: Editor::single_line(), selected: 0, results: Vec::new() };
        o.refresh(app);
        o
    }

    pub fn channel_list(net: NetworkId) -> Overlay {
        Overlay::ChannelList {
            net,
            filter: Editor::single_line(),
            scroll: 0.0,
            selected: 0,
            rows: Vec::new(),
            by_users: true,
        }
    }

    pub fn editor(&mut self) -> Option<&mut Editor> {
        match self {
            Overlay::QuickSwitch { input, .. } => Some(input),
            Overlay::ChannelList { filter, .. } => Some(filter),
            Overlay::Confirm { .. } => None,
        }
    }

    /// Recomputes results after the filter text changed.
    pub fn refresh(&mut self, app: &App) {
        match self {
            Overlay::QuickSwitch { input, selected, results } => {
                let q = input.text().trim().to_owned();
                let mut scored: Vec<(i32, Activity, BufferId)> = app
                    .sidebar_order()
                    .into_iter()
                    .filter(|id| *id != app.active)
                    .filter_map(|id| {
                        let b = app.buffer(id)?;
                        let net = b
                            .network
                            .and_then(|n| app.network(n))
                            .map(|n| n.display_name().to_owned())
                            .unwrap_or_default();
                        let hay = if b.kind == BufferKind::Server { net } else { b.name.clone() };
                        Some((fuzzy_score(&q, &hay)?, b.activity, id))
                    })
                    .collect();
                if q.is_empty() {
                    // Most active first.
                    scored.sort_by_key(|s| std::cmp::Reverse(s.1));
                } else {
                    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
                }
                *results = scored.into_iter().map(|s| s.2).take(12).collect();
                *selected = 0;
            }
            Overlay::ChannelList { net, filter, rows, selected, scroll, by_users } => {
                let Some(n) = app.network(*net) else { return };
                let f = filter.text().to_lowercase();
                let mut v: Vec<usize> = n
                    .channel_list
                    .iter()
                    .enumerate()
                    .filter(|(_, (name, _, topic))| {
                        f.is_empty() || name.to_lowercase().contains(&f) || topic.to_lowercase().contains(&f)
                    })
                    .map(|(i, _)| i)
                    .collect();
                if *by_users {
                    v.sort_by(|a, b| n.channel_list[*b].1.cmp(&n.channel_list[*a].1));
                } else {
                    v.sort_by(|a, b| n.channel_list[*a].0.to_lowercase().cmp(&n.channel_list[*b].0.to_lowercase()));
                }
                *rows = v;
                *selected = (*selected).min(rows.len().saturating_sub(1));
                *scroll = scroll.min((rows.len() as f32 * 26.0).max(0.0));
            }
            Overlay::Confirm { .. } => {}
        }
    }

    pub fn move_selection(&mut self, delta: i32) {
        match self {
            Overlay::QuickSwitch { selected, results, .. } => {
                let n = results.len() as i32;
                if n > 0 {
                    *selected = ((*selected as i32 + delta).rem_euclid(n)) as usize;
                }
            }
            Overlay::ChannelList { selected, rows, scroll, .. } => {
                let n = rows.len() as i32;
                if n > 0 {
                    *selected = (*selected as i32 + delta).clamp(0, n - 1) as usize;
                    let y = *selected as f32 * 26.0;
                    if y < *scroll {
                        *scroll = y;
                    } else if y > *scroll + 26.0 * 14.0 {
                        *scroll = y - 26.0 * 14.0;
                    }
                }
            }
            Overlay::Confirm { .. } => {}
        }
    }

    fn panel(&self, win: Rect) -> Rect {
        let (w, h) = match self {
            Overlay::QuickSwitch { results, .. } => (520.0, 64.0 + results.len().max(1) as f32 * 34.0 + 12.0),
            Overlay::Confirm { .. } => (440.0, 190.0),
            Overlay::ChannelList { .. } => (760.0f32.min(win.w - 40.0), (win.h - 80.0).min(560.0)),
        };
        let top = if matches!(self, Overlay::QuickSwitch { .. }) { win.y + 90.0 } else { win.y + (win.h - h) / 2.0 };
        Rect::new(win.x + (win.w - w) / 2.0, top, w, h)
    }

    pub fn render(&mut self, p: &Painter, text: &Text, th: &Theme, app: &App, win: Rect, caret_on: bool) {
        let f = &text.fonts;
        p.fill(win, th.overlay_scrim);
        let panel = self.panel(win);
        p.fill_round(Rect::new(panel.x, panel.y + 4.0, panel.w, panel.h), 12.0, with_alpha(crate::gfx::hex(0), 0.25));
        p.fill_round(panel, 12.0, th.panel_bg);
        p.stroke_round(panel, 12.0, th.border, 1.0);
        match self {
            Overlay::QuickSwitch { input, selected, results } => {
                let field = Rect::new(panel.x + 12.0, panel.y + 12.0, panel.w - 24.0, 40.0);
                draw_field(p, text, th, input, field, "Jump to a channel, query or network…", caret_on);
                let mut y = field.bottom() + 8.0;
                if results.is_empty() {
                    let l = text.layout("No matches", &f.ui, panel.w, 30.0);
                    p.text(&l, panel.x + 24.0, y + 8.0, th.text_dim);
                }
                for (i, id) in results.iter().enumerate() {
                    let Some(b) = app.buffer(*id) else { continue };
                    let r = Rect::new(panel.x + 8.0, y, panel.w - 16.0, 32.0);
                    if i == *selected {
                        p.fill_round(r, 6.0, with_alpha(th.accent, 0.18));
                    }
                    let net =
                        b.network.and_then(|n| app.network(n)).map(|n| n.display_name().to_owned()).unwrap_or_default();
                    let (name, sub) = match b.kind {
                        BufferKind::Server => (net, "network".to_owned()),
                        _ => (b.name.clone(), net),
                    };
                    let l = text.layout(&name, &f.ui_semibold, r.w - 160.0, 30.0);
                    p.text(&l, r.x + 12.0, r.y + 7.0, th.text);
                    let s = text.layout(&sub, &f.ui, 140.0, 30.0);
                    let sw = text::metrics(&s).width;
                    p.text(&s, r.right() - sw - 12.0, r.y + 7.0, th.text_dim);
                    if b.unread > 0 {
                        let c = if b.highlights > 0 { th.badge_highlight } else { th.accent };
                        p.circle(r.right() - sw - 24.0, r.y + 16.0, 3.5, c);
                    }
                    y += 34.0;
                }
            }
            Overlay::Confirm { title, body, yes, .. } => {
                let t = text.layout(title, &f.title, panel.w - 48.0, 30.0);
                p.text(&t, panel.x + 24.0, panel.y + 22.0, th.text);
                let b = text.layout(body, &f.chat, panel.w - 48.0, 90.0);
                unsafe {
                    let _ = b.SetWordWrapping(windows::Win32::Graphics::DirectWrite::DWRITE_WORD_WRAPPING_WRAP);
                }
                p.text(&b, panel.x + 24.0, panel.y + 58.0, th.text_dim);
                let (yes_r, no_r) = confirm_buttons(panel);
                p.fill_round(yes_r, 6.0, th.accent);
                let yl = text.layout(yes, &f.ui_semibold, yes_r.w, 30.0);
                let yw = text::metrics(&yl).width;
                p.text(&yl, yes_r.x + (yes_r.w - yw) / 2.0, yes_r.y + 8.0, th.accent_fg);
                p.fill_round(no_r, 6.0, th.badge_bg);
                let nl = text.layout("Cancel", &f.ui_semibold, no_r.w, 30.0);
                let nw = text::metrics(&nl).width;
                p.text(&nl, no_r.x + (no_r.w - nw) / 2.0, no_r.y + 8.0, th.text);
            }
            Overlay::ChannelList { net, filter, scroll, selected, rows, by_users } => {
                let Some(n) = app.network(*net) else { return };
                let title = if n.channel_list_complete {
                    format!("Channels on {} — {} of {}", n.display_name(), rows.len(), n.channel_list.len())
                } else {
                    format!("Channels on {} — loading… {}", n.display_name(), n.channel_list.len())
                };
                let t = text.layout(&title, &f.title, panel.w - 40.0, 30.0);
                p.text(&t, panel.x + 20.0, panel.y + 16.0, th.text);
                let field = Rect::new(panel.x + 16.0, panel.y + 50.0, panel.w - 32.0, 36.0);
                draw_field(
                    p,
                    text,
                    th,
                    filter,
                    field,
                    "Filter by name or topic…  (Enter joins, Tab toggles sort)",
                    caret_on,
                );
                let sort = if *by_users { "sorted by users" } else { "sorted by name" };
                let sl = text.layout(sort, &f.ui_small, 200.0, 20.0);
                p.text(&sl, panel.right() - 20.0 - text::metrics(&sl).width, panel.y + 22.0, th.text_dim);
                let list = Rect::new(
                    panel.x + 8.0,
                    field.bottom() + 8.0,
                    panel.w - 16.0,
                    panel.bottom() - field.bottom() - 16.0,
                );
                p.clip(list);
                let first = (*scroll / 26.0) as usize;
                for (vi, &ri) in rows.iter().enumerate().skip(first).take((list.h / 26.0) as usize + 2) {
                    let (name, users, topic) = &n.channel_list[ri];
                    let y = list.y + vi as f32 * 26.0 - *scroll;
                    let r = Rect::new(list.x, y, list.w, 26.0);
                    if vi == *selected {
                        p.fill_round(r, 5.0, with_alpha(th.accent, 0.18));
                    }
                    let nl = text.layout(name, &f.ui_semibold, 200.0, 24.0);
                    p.text(&nl, r.x + 10.0, y + 5.0, th.text);
                    let ul = text.layout(&users.to_string(), &f.ui, 60.0, 24.0);
                    unsafe {
                        let _ =
                            ul.SetTextAlignment(windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_TRAILING);
                    }
                    p.text(&ul, r.x + 210.0, y + 5.0, th.text_dim);
                    let tl = text.layout(topic, &f.ui, (r.w - 300.0).max(10.0), 24.0);
                    p.text(&tl, r.x + 290.0, y + 5.0, th.text_dim);
                }
                p.unclip();
            }
        }
    }

    /// Result of a click inside the overlay.
    pub fn click(&mut self, win: Rect, x: f32, y: f32) -> OverlayClick {
        let panel = self.panel(win);
        if !panel.contains(x, y) {
            return OverlayClick::Dismiss;
        }
        match self {
            Overlay::QuickSwitch { results, selected, .. } => {
                let top = panel.y + 12.0 + 40.0 + 8.0;
                let i = ((y - top) / 34.0).floor();
                if i >= 0.0 && (i as usize) < results.len() {
                    *selected = i as usize;
                    return OverlayClick::Accept;
                }
                OverlayClick::None
            }
            Overlay::Confirm { .. } => {
                let (yes, no) = confirm_buttons(panel);
                if yes.contains(x, y) {
                    OverlayClick::Accept
                } else if no.contains(x, y) {
                    OverlayClick::Dismiss
                } else {
                    OverlayClick::None
                }
            }
            Overlay::ChannelList { rows, selected, scroll, .. } => {
                let top = panel.y + 50.0 + 36.0 + 8.0;
                if y < top {
                    return OverlayClick::None;
                }
                let i = ((y - top + *scroll) / 26.0) as usize;
                if i < rows.len() {
                    let was = *selected == i;
                    *selected = i;
                    return if was { OverlayClick::Accept } else { OverlayClick::None };
                }
                OverlayClick::None
            }
        }
    }

    pub fn scroll(&mut self, dy: f32) {
        if let Overlay::ChannelList { scroll, rows, .. } = self {
            *scroll = (*scroll - dy).clamp(0.0, (rows.len() as f32 * 26.0 - 200.0).max(0.0));
        }
    }
}

pub enum OverlayClick {
    None,
    Accept,
    Dismiss,
}

fn confirm_buttons(panel: Rect) -> (Rect, Rect) {
    let yes = Rect::new(panel.right() - 24.0 - 120.0, panel.bottom() - 56.0, 120.0, 34.0);
    let no = Rect::new(yes.x - 12.0 - 100.0, yes.y, 100.0, 34.0);
    (yes, no)
}

/// Draws a single-line text field with placeholder, selection and caret.
pub fn draw_field(p: &Painter, text: &Text, th: &Theme, ed: &mut Editor, r: Rect, placeholder: &str, caret_on: bool) {
    let f = &text.fonts;
    p.fill_round(r, 8.0, th.input_bg);
    p.stroke_round(r, 8.0, with_alpha(th.accent, 0.6), 1.5);
    let inner = r.inset(12.0, 0.0);
    let l = ed.layout(text, &f.chat, inner.w).clone();
    let lh = text::metrics(&l).height.max(f.line_height);
    let ty = r.y + (r.h - lh) / 2.0;
    // Horizontal scroll so the caret stays visible.
    let (cx, _, ch) = ed.caret();
    let dx = (cx - inner.w + 4.0).max(0.0);
    p.clip(inner);
    if ed.is_empty() {
        let ph = text.layout(placeholder, &f.chat, inner.w, 40.0);
        p.text(&ph, inner.x, ty, th.text_dim);
    }
    for (sx, sy, sw, sh) in ed.selection_rects() {
        p.fill(Rect::new(inner.x + sx - dx, ty + sy, sw, sh), th.selection);
    }
    p.text(&l, inner.x - dx, ty, th.text);
    if caret_on {
        p.fill(Rect::new(inner.x + cx - dx, ty, 1.5, ch.max(lh)), th.accent);
    }
    p.unclip();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy() {
        assert!(fuzzy_score("rst", "#rust").is_some());
        assert!(fuzzy_score("xyz", "#rust").is_none());
        let a = fuzzy_score("rust", "#rust").unwrap();
        let b = fuzzy_score("rust", "#r-u-s-t-aceans").unwrap();
        assert!(a > b);
    }
}
