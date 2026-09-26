//! Form dialogs: the network editor and the settings dialog.

use crate::editor::Editor;
use crate::gfx::{Painter, Rect, with_alpha};
use crate::text::{self, Text};
use crate::theme::Theme;
use schwaetz_core::config::{NetworkKind, SaslMechanism};
use schwaetz_core::secrets::{self, SecretKind};
use schwaetz_core::{Config, NetworkConfig};
use windows::Win32::UI::Input::KeyboardAndMouse::*;

const ROW_H: f32 = 40.0;
const LABEL_W: f32 = 200.0;

pub enum FieldKind {
    Header,
    Text(Editor),
    /// Masked input; the flag says whether a secret is already stored.
    Password(Editor, bool),
    Choice(Vec<(&'static str, &'static str)>, usize),
    Check(bool),
}

pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    pub kind: FieldKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormKind {
    Network { original: Option<String> },
    Settings,
}

pub enum FormAction {
    None,
    Save,
    Cancel,
    Delete,
}

pub struct Form {
    pub title: String,
    pub kind: FormKind,
    pub fields: Vec<Field>,
    pub focus: usize,
    scroll: f32,
    pub error: Option<String>,
    rows: Vec<(usize, Rect)>,
    buttons: Vec<(FormAction, Rect)>,
}

fn text(key: &'static str, label: &'static str, hint: &'static str, value: &str) -> Field {
    let mut e = Editor::single_line();
    e.set_text(value, None);
    Field { key, label, hint, kind: FieldKind::Text(e) }
}

fn password(key: &'static str, label: &'static str, stored: bool) -> Field {
    let mut e = Editor::single_line();
    e.masked = true;
    let hint = if stored { "stored — leave empty to keep" } else { "not set" };
    Field { key, label, hint, kind: FieldKind::Password(e, stored) }
}

fn check(key: &'static str, label: &'static str, value: bool) -> Field {
    Field { key, label, hint: "", kind: FieldKind::Check(value) }
}

fn choice(key: &'static str, label: &'static str, options: Vec<(&'static str, &'static str)>, value: &str) -> Field {
    let idx = options.iter().position(|(v, _)| *v == value).unwrap_or(0);
    Field { key, label, hint: "", kind: FieldKind::Choice(options, idx) }
}

fn header(label: &'static str) -> Field {
    Field { key: "", label, hint: "", kind: FieldKind::Header }
}

impl Form {
    fn new(title: String, kind: FormKind, fields: Vec<Field>) -> Form {
        let focus = fields.iter().position(|f| !matches!(f.kind, FieldKind::Header)).unwrap_or(0);
        Form { title, kind, fields, focus, scroll: 0.0, error: None, rows: Vec::new(), buttons: Vec::new() }
    }

    pub fn network(cfg: Option<&NetworkConfig>) -> Form {
        let d = NetworkConfig::default();
        let c = cfg.unwrap_or(&d);
        let has = |k| cfg.is_some_and(|c| secrets::get(&c.name, k).is_some());
        let kind = match c.kind {
            NetworkKind::Irc => "irc",
            NetworkKind::Znc => "znc",
            NetworkKind::Soju => "soju",
            NetworkKind::Twitch => "twitch",
        };
        let sasl = match c.sasl {
            SaslMechanism::None => "none",
            SaslMechanism::Plain => "plain",
            SaslMechanism::ScramSha256 => "scram-sha-256",
            SaslMechanism::External => "external",
        };
        let fields = vec![
            header("Connection"),
            text("name", "Name", "e.g. Libera.Chat", &c.name),
            choice(
                "kind",
                "Type",
                vec![
                    ("irc", "IRC server"),
                    ("znc", "ZNC bouncer"),
                    ("soju", "soju bouncer"),
                    ("twitch", "Twitch chat"),
                ],
                kind,
            ),
            text("servers", "Servers", "host:+6697 (plus = TLS), comma-separated", &c.servers.join(", ")),
            password(
                "server_password",
                "Server password / token",
                has(SecretKind::ServerPassword) || has(SecretKind::TwitchToken),
            ),
            check("auto_connect", "Connect on startup", c.auto_connect),
            check("reconnect", "Reconnect automatically", c.reconnect),
            check("accept_invalid_certs", "Accept invalid certificates", c.accept_invalid_certs),
            text(
                "client_cert",
                "Client certificate",
                "PEM file for CertFP / SASL EXTERNAL",
                c.client_cert.as_deref().unwrap_or(""),
            ),
            header("Identity"),
            text("nick", "Nickname", "empty = global setting", c.nick.as_deref().unwrap_or("")),
            text("username", "Username", "empty = global setting", c.username.as_deref().unwrap_or("")),
            text("realname", "Real name", "empty = global setting", c.realname.as_deref().unwrap_or("")),
            header("Authentication"),
            choice(
                "sasl",
                "SASL",
                vec![
                    ("none", "None"),
                    ("plain", "PLAIN"),
                    ("scram-sha-256", "SCRAM-SHA-256"),
                    ("external", "EXTERNAL (certificate)"),
                ],
                sasl,
            ),
            text("sasl_username", "Account", "empty = nickname", c.sasl_username.as_deref().unwrap_or("")),
            password("sasl_password", "Account password", has(SecretKind::Sasl)),
            check("sasl_required", "Disconnect if SASL fails", c.sasl_required),
            text("znc_user", "ZNC user", "ZNC only", c.znc_user.as_deref().unwrap_or("")),
            text("znc_network", "ZNC network", "ZNC only", c.znc_network.as_deref().unwrap_or("")),
            header("Channels"),
            text("autojoin", "Join on connect", "#chan, #other key", &c.autojoin.join(", ")),
            text("perform", "Commands on connect", "separate with ;  e.g. /mode $nick +x", &c.perform.join(" ; ")),
            check("rejoin_on_kick", "Rejoin when kicked", c.rejoin_on_kick),
            check("previews", "Link previews on this network", c.previews),
        ];
        let title = match cfg {
            Some(c) => format!("Edit network — {}", c.name),
            None => "Add network".into(),
        };
        Form::new(title, FormKind::Network { original: cfg.map(|c| c.name.clone()) }, fields)
    }

    pub fn settings(c: &Config) -> Form {
        let g = &c.general;
        let a = &c.appearance;
        let fields = vec![
            header("Identity"),
            text("nick", "Nickname", "", &g.nick),
            text("alt_nicks", "Alternative nicks", "comma-separated", &g.alt_nicks.join(", ")),
            text("realname", "Real name", "", &g.realname),
            text("quit_message", "Quit message", "", &g.quit_message),
            header("Appearance"),
            choice(
                "theme",
                "Theme",
                vec![("system", "Follow Windows"), ("dark", "Dark"), ("light", "Light")],
                &a.theme,
            ),
            text("font", "Chat font", "e.g. Segoe UI Variable Text, Cascadia Mono", &a.font),
            text("font_size", "Font size", "", &a.font_size.to_string()),
            text("timestamp_format", "Timestamp format", "%H:%M, %H:%M:%S; empty hides", &a.timestamp_format),
            check("nick_column", "Align nicks in a column", a.nick_column),
            check("colored_nicks", "Colored nicks", a.colored_nicks),
            check("show_mirc_colors", "Show mIRC colors", a.show_mirc_colors),
            check("show_nicklist", "Show member list", a.show_nicklist),
            check("mica", "Mica backdrop (Windows 11)", a.mica),
            check("gpu_acceleration", "GPU rendering (uses more memory)", a.gpu_acceleration),
            header("Chat"),
            choice(
                "show_joins_parts",
                "Joins and parts",
                vec![("smart", "Only for active users"), ("all", "Always"), ("none", "Never")],
                &g.show_joins_parts,
            ),
            text("highlight_words", "Highlight words", "comma-separated", &c.highlight.words.join(", ")),
            text("scrollback_lines", "Lines kept in memory", "", &g.scrollback_lines.to_string()),
            check("ctcp_replies", "Answer CTCP requests", g.ctcp_replies),
            check("remember_channels", "Rejoin channels after restart", g.remember_channels),
            header("Notifications"),
            check("on_highlight", "Notify on highlights", c.notifications.on_highlight),
            check("on_private", "Notify on private messages", c.notifications.on_private),
            check("flash_taskbar", "Flash the taskbar", c.notifications.flash_taskbar),
            header("Link previews"),
            check("previews_enabled", "Show link previews", c.previews.enabled),
            check("previews_auto", "Load without clicking", c.previews.auto_load),
            text(
                "allow_hosts",
                "Only auto-load from",
                "hosts, comma-separated; empty = any",
                &c.previews.allow_hosts.join(", "),
            ),
            header("History & window"),
            check("log_to_files", "Write text log files", g.log_to_files),
            text("history_days", "Delete history after days", "0 = keep forever", &g.history_days.to_string()),
            check("minimize_to_tray", "Minimize to tray", g.minimize_to_tray),
            check("close_to_tray", "Close to tray", g.close_to_tray),
        ];
        Form::new("Settings".into(), FormKind::Settings, fields)
    }

    fn field(&self, key: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.key == key)
    }

    pub fn text(&self, key: &str) -> String {
        match self.field(key).map(|f| &f.kind) {
            Some(FieldKind::Text(e) | FieldKind::Password(e, _)) => e.text().trim().to_owned(),
            _ => String::new(),
        }
    }

    fn opt(&self, key: &str) -> Option<String> {
        Some(self.text(key)).filter(|s| !s.is_empty())
    }

    fn list(&self, key: &str, sep: char) -> Vec<String> {
        self.text(key).split(sep).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect()
    }

    pub fn check(&self, key: &str) -> bool {
        matches!(self.field(key).map(|f| &f.kind), Some(FieldKind::Check(true)))
    }

    pub fn choice(&self, key: &str) -> &'static str {
        match self.field(key).map(|f| &f.kind) {
            Some(FieldKind::Choice(o, i)) => o[*i].0,
            _ => "",
        }
    }

    fn password_stored(&self, key: &str) -> bool {
        matches!(self.field(key).map(|f| &f.kind), Some(FieldKind::Password(_, true)))
    }

    /// Builds the network definition and the secrets to store.
    pub fn to_network(
        &self,
        base: Option<&NetworkConfig>,
    ) -> Result<(NetworkConfig, Vec<(SecretKind, String)>), String> {
        let name = self.text("name");
        if name.is_empty() {
            return Err("Give the network a name.".into());
        }
        let servers = self.list("servers", ',');
        if servers.is_empty() {
            return Err("Add at least one server.".into());
        }
        if let Some(bad) = servers.iter().find(|s| NetworkConfig::parse_server(s).is_none()) {
            return Err(format!("\"{bad}\" is not a valid server address."));
        }
        let kind = match self.choice("kind") {
            "znc" => NetworkKind::Znc,
            "soju" => NetworkKind::Soju,
            "twitch" => NetworkKind::Twitch,
            _ => NetworkKind::Irc,
        };
        let sasl = match self.choice("sasl") {
            "plain" => SaslMechanism::Plain,
            "scram-sha-256" => SaslMechanism::ScramSha256,
            "external" => SaslMechanism::External,
            _ => SaslMechanism::None,
        };
        let mut secrets = Vec::new();
        let server_pw = self.text("server_password");
        if !server_pw.is_empty() {
            let k = if kind == NetworkKind::Twitch { SecretKind::TwitchToken } else { SecretKind::ServerPassword };
            secrets.push((k, server_pw.clone()));
        }
        let sasl_pw = self.text("sasl_password");
        if !sasl_pw.is_empty() {
            secrets.push((SecretKind::Sasl, sasl_pw));
        }
        if matches!(sasl, SaslMechanism::Plain | SaslMechanism::ScramSha256)
            && secrets.iter().all(|(k, _)| *k != SecretKind::Sasl)
            && !self.password_stored("sasl_password")
        {
            return Err("SASL needs an account password.".into());
        }
        let base = base.cloned().unwrap_or_else(|| {
            if kind == NetworkKind::Twitch { NetworkConfig::twitch() } else { NetworkConfig::default() }
        });
        let cfg = NetworkConfig {
            name,
            kind,
            servers,
            auto_connect: self.check("auto_connect"),
            reconnect: self.check("reconnect"),
            accept_invalid_certs: self.check("accept_invalid_certs"),
            client_cert: self.opt("client_cert"),
            nick: self.opt("nick"),
            username: self.opt("username"),
            realname: self.opt("realname"),
            sasl,
            sasl_username: self.opt("sasl_username"),
            sasl_required: self.check("sasl_required"),
            server_password: kind != NetworkKind::Twitch
                && (!server_pw.is_empty() || self.password_stored("server_password")),
            znc_user: self.opt("znc_user"),
            znc_network: self.opt("znc_network"),
            autojoin: self.list("autojoin", ','),
            perform: self.list("perform", ';'),
            rejoin_on_kick: self.check("rejoin_on_kick"),
            previews: self.check("previews"),
            ..base
        };
        Ok((cfg, secrets))
    }

    pub fn apply_settings(&self, c: &mut Config) -> Result<(), String> {
        let num = |key: &str, label: &str| -> Result<u64, String> {
            self.text(key).parse::<u64>().map_err(|_| format!("{label} must be a whole number."))
        };
        let font_size: f32 =
            self.text("font_size").replace(',', ".").parse().map_err(|_| "Font size must be a number.".to_owned())?;
        if !(8.0..=40.0).contains(&font_size) {
            return Err("Font size must be between 8 and 40.".into());
        }
        let nick = self.text("nick");
        if nick.is_empty() || nick.contains(' ') {
            return Err("Enter a nickname without spaces.".into());
        }
        let scrollback = num("scrollback_lines", "Lines kept in memory")?.clamp(100, 100_000) as usize;
        let history_days = num("history_days", "History days")? as u32;
        c.general.nick = nick;
        c.general.alt_nicks = self.list("alt_nicks", ',');
        c.general.realname = self.text("realname");
        c.general.quit_message = self.text("quit_message");
        c.appearance.theme = self.choice("theme").into();
        c.appearance.font = self.text("font");
        c.appearance.font_size = font_size;
        c.appearance.timestamp_format = self.text("timestamp_format");
        c.appearance.nick_column = self.check("nick_column");
        c.appearance.colored_nicks = self.check("colored_nicks");
        c.appearance.show_mirc_colors = self.check("show_mirc_colors");
        c.appearance.show_nicklist = self.check("show_nicklist");
        c.appearance.mica = self.check("mica");
        c.appearance.gpu_acceleration = self.check("gpu_acceleration");
        c.general.show_joins_parts = self.choice("show_joins_parts").into();
        c.highlight.words = self.list("highlight_words", ',');
        c.general.scrollback_lines = scrollback;
        c.general.ctcp_replies = self.check("ctcp_replies");
        c.general.remember_channels = self.check("remember_channels");
        c.notifications.on_highlight = self.check("on_highlight");
        c.notifications.on_private = self.check("on_private");
        c.notifications.flash_taskbar = self.check("flash_taskbar");
        c.previews.enabled = self.check("previews_enabled");
        c.previews.auto_load = self.check("previews_auto");
        c.previews.allow_hosts = self.list("allow_hosts", ',');
        c.general.log_to_files = self.check("log_to_files");
        c.general.history_days = history_days;
        c.general.minimize_to_tray = self.check("minimize_to_tray");
        c.general.close_to_tray = self.check("close_to_tray");
        Ok(())
    }

    fn panel(&self, win: Rect) -> Rect {
        let w = 640.0f32.min(win.w - 40.0);
        let h = (win.h - 60.0).min(720.0);
        Rect::new(win.x + (win.w - w) / 2.0, win.y + (win.h - h) / 2.0, w, h)
    }

    fn list_rect(&self, win: Rect) -> Rect {
        let p = self.panel(win);
        Rect::new(p.x + 8.0, p.y + 60.0, p.w - 16.0, p.h - 60.0 - 64.0)
    }

    pub fn render(&mut self, p: &Painter, text: &Text, th: &Theme, win: Rect, caret_on: bool) {
        let f = &text.fonts;
        p.fill(win, th.overlay_scrim);
        let panel = self.panel(win);
        p.fill_round(panel, 12.0, th.panel_bg);
        p.stroke_round(panel, 12.0, th.border, 1.0);
        let t = text.layout(&self.title, &f.title, panel.w - 40.0, 30.0);
        p.text(&t, panel.x + 24.0, panel.y + 20.0, th.text);

        let list = self.list_rect(win);
        let content_h: f32 =
            self.fields.iter().map(|f| if matches!(f.kind, FieldKind::Header) { 34.0 } else { ROW_H }).sum();
        self.scroll = self.scroll.clamp(0.0, (content_h - list.h).max(0.0));
        p.clip(list);
        self.rows.clear();
        let mut y = list.y - self.scroll;
        for (i, field) in self.fields.iter_mut().enumerate() {
            let h = if matches!(field.kind, FieldKind::Header) { 34.0 } else { ROW_H };
            let focused = i == self.focus;
            match &mut field.kind {
                FieldKind::Header => {
                    let l = text.layout(&field.label.to_uppercase(), &f.ui_small, list.w, 20.0);
                    p.text(&l, list.x + 16.0, y + 14.0, th.accent);
                }
                kind => {
                    let l = text.layout(field.label, &f.ui, LABEL_W - 20.0, 20.0);
                    p.text(&l, list.x + 16.0, y + 11.0, th.text);
                    let ctl = Rect::new(list.x + LABEL_W, y + 4.0, list.w - LABEL_W - 16.0, ROW_H - 8.0);
                    match kind {
                        FieldKind::Text(ed) | FieldKind::Password(ed, _) => {
                            p.fill_round(ctl, 6.0, th.input_bg);
                            let border = if focused { with_alpha(th.accent, 0.8) } else { th.border };
                            p.stroke_round(ctl, 6.0, border, if focused { 1.5 } else { 1.0 });
                            let inner = ctl.inset(10.0, 0.0);
                            let lay = ed.layout(text, &f.ui, inner.w).clone();
                            let lh = text::metrics(&lay).height.max(16.0);
                            let ty = ctl.y + (ctl.h - lh) / 2.0;
                            let (cx, _, ch) = ed.caret();
                            let dx = (cx - inner.w + 4.0).max(0.0);
                            p.clip(inner);
                            if ed.is_empty() && !field.hint.is_empty() {
                                let hl = text.layout(field.hint, &f.ui, inner.w, 20.0);
                                p.text(&hl, inner.x, ty, with_alpha(th.text_dim, 0.8));
                            }
                            for (sx, sy, sw, sh) in ed.selection_rects() {
                                p.fill(Rect::new(inner.x + sx - dx, ty + sy, sw, sh), th.selection);
                            }
                            p.text(&lay, inner.x - dx, ty, th.text);
                            if focused && caret_on {
                                p.fill(Rect::new(inner.x + cx - dx, ty, 1.5, ch.max(lh)), th.accent);
                            }
                            p.unclip();
                        }
                        FieldKind::Choice(opts, idx) => {
                            p.fill_round(ctl, 6.0, th.input_bg);
                            p.stroke_round(ctl, 6.0, if focused { th.accent } else { th.border }, 1.0);
                            let l = text.layout(&format!("{}   ▾", opts[*idx].1), &f.ui, ctl.w - 20.0, 20.0);
                            p.text(&l, ctl.x + 10.0, ctl.y + 7.0, th.text);
                        }
                        FieldKind::Check(on) => {
                            let bx = Rect::new(ctl.x, ctl.y + (ctl.h - 20.0) / 2.0, 36.0, 20.0);
                            p.fill_round(bx, 10.0, if *on { th.accent } else { th.badge_bg });
                            if focused {
                                p.stroke_round(bx.inset(-2.0, -2.0), 12.0, with_alpha(th.accent, 0.6), 1.0);
                            }
                            let knob_x = if *on { bx.right() - 10.0 } else { bx.x + 10.0 };
                            p.circle(knob_x, bx.y + 10.0, 7.0, if *on { th.accent_fg } else { th.text_dim });
                        }
                        FieldKind::Header => {}
                    }
                    self.rows.push((i, Rect::new(list.x, y, list.w, h)));
                }
            }
            y += h;
        }
        p.unclip();

        // Footer.
        self.buttons.clear();
        let by = panel.bottom() - 52.0;
        if let Some(e) = &self.error {
            let l = text.layout(e, &f.ui, panel.w - 300.0, 40.0);
            p.text(&l, panel.x + 24.0, by + 8.0, th.error);
        }
        let save = Rect::new(panel.right() - 24.0 - 110.0, by, 110.0, 34.0);
        let cancel = Rect::new(save.x - 12.0 - 100.0, by, 100.0, 34.0);
        p.fill_round(save, 6.0, th.accent);
        let sl = text.layout("Save", &f.ui_semibold, save.w, 30.0);
        p.text(&sl, save.x + (save.w - text::metrics(&sl).width) / 2.0, save.y + 8.0, th.accent_fg);
        p.fill_round(cancel, 6.0, th.badge_bg);
        let cl = text.layout("Cancel", &f.ui_semibold, cancel.w, 30.0);
        p.text(&cl, cancel.x + (cancel.w - text::metrics(&cl).width) / 2.0, cancel.y + 8.0, th.text);
        self.buttons.push((FormAction::Save, save));
        self.buttons.push((FormAction::Cancel, cancel));
        if matches!(&self.kind, FormKind::Network { original: Some(_) }) && self.error.is_none() {
            let del = Rect::new(panel.x + 24.0, by, 150.0, 34.0);
            p.stroke_round(del, 6.0, th.error, 1.0);
            let dl = text.layout("Remove network", &f.ui_semibold, del.w, 30.0);
            p.text(&dl, del.x + (del.w - text::metrics(&dl).width) / 2.0, del.y + 8.0, th.error);
            self.buttons.push((FormAction::Delete, del));
        }
    }

    pub fn click(&mut self, win: Rect, x: f32, y: f32) -> FormAction {
        if !self.panel(win).contains(x, y) {
            return FormAction::None;
        }
        if let Some(i) = self.buttons.iter().position(|(_, r)| r.contains(x, y)) {
            return std::mem::replace(&mut self.buttons[i].0, FormAction::None);
        }
        let Some(&(i, r)) = self.rows.iter().find(|(_, r)| r.contains(x, y)) else { return FormAction::None };
        if !self.list_rect(win).contains(x, y) {
            return FormAction::None;
        }
        self.focus = i;
        let ctl_x = r.x + LABEL_W;
        match &mut self.fields[i].kind {
            FieldKind::Check(on) => *on = !*on,
            FieldKind::Choice(opts, idx) => *idx = (*idx + 1) % opts.len(),
            FieldKind::Text(ed) | FieldKind::Password(ed, _) => {
                if x >= ctl_x {
                    ed.click(x - ctl_x - 10.0, 10.0, false);
                }
            }
            FieldKind::Header => {}
        }
        FormAction::None
    }

    pub fn scroll_by(&mut self, dy: f32) {
        self.scroll -= dy;
    }

    fn move_focus(&mut self, delta: i32) {
        let n = self.fields.len() as i32;
        let mut i = self.focus as i32;
        for _ in 0..n {
            i = (i + delta).rem_euclid(n);
            if !matches!(self.fields[i as usize].kind, FieldKind::Header) {
                break;
            }
        }
        self.focus = i as usize;
        // Keep the focused row visible.
        let top: f32 = self.fields[..self.focus]
            .iter()
            .map(|f| if matches!(f.kind, FieldKind::Header) { 34.0 } else { ROW_H })
            .sum();
        if top < self.scroll {
            self.scroll = (top - 34.0).max(0.0);
        } else if top + ROW_H > self.scroll + 480.0 {
            self.scroll = top + ROW_H - 480.0;
        }
    }

    /// Keyboard handling; returns an action for Enter/Escape.
    pub fn key(
        &mut self,
        v: VIRTUAL_KEY,
        ctrl: bool,
        shift: bool,
        clipboard: impl FnOnce() -> Option<String>,
    ) -> FormAction {
        match v {
            VK_ESCAPE => return FormAction::Cancel,
            VK_RETURN => return FormAction::Save,
            VK_TAB => self.move_focus(if shift { -1 } else { 1 }),
            VK_DOWN => self.move_focus(1),
            VK_UP => self.move_focus(-1),
            _ => match &mut self.fields[self.focus].kind {
                FieldKind::Text(ed) | FieldKind::Password(ed, _) => match v {
                    VK_LEFT => ed.move_h(false, ctrl, shift),
                    VK_RIGHT => ed.move_h(true, ctrl, shift),
                    VK_HOME => ed.home(shift, true),
                    VK_END => ed.end(shift, true),
                    VK_BACK => ed.backspace(ctrl),
                    VK_DELETE => ed.delete(ctrl),
                    _ if ctrl && v.0 == b'A' as u16 => ed.select_all(),
                    _ if ctrl && v.0 == b'V' as u16 => {
                        if let Some(t) = clipboard() {
                            ed.insert(&t);
                        }
                    }
                    _ => {}
                },
                FieldKind::Check(on) if v == VK_SPACE => *on = !*on,
                FieldKind::Choice(opts, idx) if v == VK_SPACE || v == VK_RIGHT => *idx = (*idx + 1) % opts.len(),
                FieldKind::Choice(opts, idx) if v == VK_LEFT => *idx = (*idx + opts.len() - 1) % opts.len(),
                _ => {}
            },
        }
        FormAction::None
    }

    pub fn char(&mut self, s: &str) {
        if let FieldKind::Text(ed) | FieldKind::Password(ed, _) = &mut self.fields[self.focus].kind {
            ed.insert(s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(form: &mut Form, key: &str, value: &str) {
        let f = form.fields.iter_mut().find(|f| f.key == key).unwrap();
        if let FieldKind::Text(e) | FieldKind::Password(e, _) = &mut f.kind {
            e.set_text(value, None);
        }
    }

    #[test]
    fn network_form_roundtrip_and_validation() {
        let mut f = Form::network(None);
        assert!(f.to_network(None).is_err());
        set(&mut f, "name", "Libera");
        set(&mut f, "servers", "irc.libera.chat:+6697, backup.libera.chat");
        set(&mut f, "autojoin", "#rust, #secret key");
        set(&mut f, "perform", "/msg a b ; /mode $nick +x");
        let (cfg, secrets) = f.to_network(None).unwrap();
        assert_eq!(cfg.servers.len(), 2);
        assert_eq!(cfg.autojoin, ["#rust", "#secret key"]);
        assert_eq!(cfg.perform, ["/msg a b", "/mode $nick +x"]);
        assert!(secrets.is_empty());
        set(&mut f, "servers", "not a server:xx");
        assert!(f.to_network(None).is_err());
    }

    #[test]
    fn settings_form_applies() {
        let mut c = Config::default();
        let mut f = Form::settings(&c);
        set(&mut f, "font_size", "15,5");
        set(&mut f, "highlight_words", "rust, irc");
        f.apply_settings(&mut c).unwrap();
        assert_eq!(c.appearance.font_size, 15.5);
        assert_eq!(c.highlight.words, ["rust", "irc"]);
        set(&mut f, "font_size", "99");
        assert!(f.apply_settings(&mut c).is_err());
    }
}
