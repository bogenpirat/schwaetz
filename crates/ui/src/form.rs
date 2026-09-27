//! Form dialogs: the network editor and the settings dialog.

use crate::editor::Editor;
use crate::gfx::{Painter, Rect, with_alpha};
use crate::text::{self, Text};
use crate::theme::Theme;
use schwaetz_core::config::{NetworkKind, SaslMechanism};
use schwaetz_core::secrets::{self, SecretKind};
use schwaetz_core::services::ScriptInfo;
use schwaetz_core::{Config, NetworkConfig};
use std::cell::Cell;
use std::ops::Range;
use windows::Win32::UI::Input::KeyboardAndMouse::*;

const ROW_H: f32 = 40.0;
const LABEL_W: f32 = 190.0;
/// Width of the page list on the left.
const NAV_W: f32 = 190.0;
const FOOTER_H: f32 = 68.0;
const OPTION_H: f32 = 32.0;

pub enum FieldKind {
    Header,
    Text(Editor),
    /// Masked input; the flag says whether a secret is already stored.
    Password(Editor, bool),
    Choice(Vec<(&'static str, &'static str)>, usize),
    Check(bool),
    /// A push button with a note next to it (both can be updated while the dialog is open).
    Button {
        caption: String,
        note: String,
    },
    /// A script file with an on/off switch and its state.
    Script {
        name: String,
        file: String,
        enabled: bool,
        note: String,
        failed: bool,
    },
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
    /// A button field was pressed (its key).
    Button(&'static str),
    /// A script's switch was flipped (takes effect immediately, not on Save).
    ScriptSwitch {
        name: String,
        enabled: bool,
    },
}

pub struct Form {
    pub title: String,
    pub kind: FormKind,
    pub fields: Vec<Field>,
    pub focus: usize,
    /// One page per header: (title, field range). The left pane switches between them.
    sections: Vec<(&'static str, Range<usize>)>,
    section: usize,
    scroll: f32,
    content_h: f32,
    list_h: f32,
    pub error: Option<String>,
    /// The field the last validation error is about (to show its page).
    error_key: Cell<&'static str>,
    /// Laid-out rows of the current page: (field, row, control).
    rows: Vec<(usize, Rect, Rect)>,
    nav: Vec<Rect>,
    nav_hover: Option<usize>,
    buttons: Vec<(FormAction, Rect)>,
    /// Open dropdown: (field, highlighted option) and its option rects.
    dropdown: Option<(usize, usize)>,
    popup: Vec<Rect>,
    /// Scrollbar (track, thumb) when the page overflows; `grab` is the pointer's offset
    /// into the thumb while dragging it.
    scrollbar: Option<(Rect, Rect)>,
    grab: Option<f32>,
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

fn button(key: &'static str, label: &'static str, caption: &str) -> Field {
    Field { key, label, hint: "", kind: FieldKind::Button { caption: caption.into(), note: String::new() } }
}

fn script_row(s: &ScriptInfo) -> Field {
    let (note, failed) = script_note(s);
    let kind = FieldKind::Script { name: s.name.clone(), file: s.file.clone(), enabled: s.enabled, note, failed };
    Field { key: "script", label: "", hint: "", kind }
}

/// One line about a script's state, and whether it is an error.
fn script_note(s: &ScriptInfo) -> (String, bool) {
    if !s.enabled {
        return ("Off".into(), false);
    }
    if !s.running {
        return match &s.error {
            Some(e) => (format!("Failed: {}", e.lines().next().unwrap_or_default()), true),
            None => ("Not running".into(), false),
        };
    }
    let mut parts = vec!["Running".to_owned()];
    if s.network {
        parts.push("network access".into());
    }
    if !s.commands.is_empty() {
        let shown: Vec<String> = s.commands.iter().take(3).map(|c| format!("/{c}")).collect();
        let more = if s.commands.len() > 3 { " …" } else { "" };
        parts.push(format!("{}{more}", shown.join(" ")));
    }
    if let Some(e) = &s.error {
        parts.push(format!("last error: {}", e.lines().next().unwrap_or_default()));
    }
    (parts.join(" · "), false)
}

/// One page per header: (title, field range).
fn sections(fields: &[Field]) -> Vec<(&'static str, Range<usize>)> {
    let mut sections: Vec<(&'static str, Range<usize>)> = Vec::new();
    for (i, f) in fields.iter().enumerate() {
        match (&f.kind, sections.last_mut()) {
            (FieldKind::Header, _) => sections.push((f.label, i + 1..i + 1)),
            (_, Some((_, r))) => r.end = i + 1,
            (_, None) => sections.push(("General", i..i + 1)),
        }
    }
    sections.retain(|(_, r)| !r.is_empty());
    sections
}

fn header(label: &'static str) -> Field {
    Field { key: "", label, hint: "", kind: FieldKind::Header }
}

impl Form {
    fn new(title: String, kind: FormKind, fields: Vec<Field>) -> Form {
        let sections = sections(&fields);
        let focus = sections.first().map_or(0, |(_, r)| r.start);
        Form {
            title,
            kind,
            fields,
            focus,
            sections,
            section: 0,
            scroll: 0.0,
            content_h: 0.0,
            list_h: 400.0,
            error: None,
            error_key: Cell::new(""),
            rows: Vec::new(),
            nav: Vec::new(),
            nav_hover: None,
            buttons: Vec::new(),
            dropdown: None,
            popup: Vec::new(),
            scrollbar: None,
            grab: None,
        }
    }

    /// Replaces the script rows with the host's current list (after a switch, reload or new
    /// examples), keeping the focus on the same row.
    pub fn set_scripts(&mut self, infos: &[ScriptInfo]) {
        let focused_key = self.fields.get(self.focus).map(|f| f.key);
        let focused_script = match self.fields.get(self.focus).map(|f| &f.kind) {
            Some(FieldKind::Script { name, .. }) => Some(name.clone()),
            _ => None,
        };
        self.fields.retain(|f| !matches!(f.kind, FieldKind::Script { .. }));
        let at = self.fields.iter().rposition(|f| f.key.starts_with("scripts_")).map_or(self.fields.len(), |i| i + 1);
        self.fields.splice(at..at, infos.iter().map(script_row));
        self.sections = sections(&self.fields);
        self.section = self.section.min(self.sections.len().saturating_sub(1));
        let refocus = match &focused_script {
            Some(n) => self.fields.iter().position(|f| matches!(&f.kind, FieldKind::Script { name, .. } if name == n)),
            None => self.fields.iter().position(|f| Some(f.key) == focused_key),
        };
        match refocus {
            Some(i) => self.focus = i,
            None if !self.page().contains(&self.focus) => self.focus = self.page().start,
            None => {}
        }
        self.dropdown = None;
    }

    /// Updates a button field's caption and note.
    pub fn set_button(&mut self, key: &str, caption: &str, note: &str) {
        if let Some(FieldKind::Button { caption: c, note: n }) =
            self.fields.iter_mut().find(|f| f.key == key).map(|f| &mut f.kind)
        {
            if c != caption {
                *c = caption.to_owned();
            }
            if n != note {
                *n = note.to_owned();
            }
        }
    }

    /// Records which field a validation error is about and returns the message.
    fn fail(&self, key: &'static str, msg: impl Into<String>) -> String {
        self.error_key.set(key);
        msg.into()
    }

    /// Shows an error and brings the offending field into view.
    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
        let key = self.error_key.replace("");
        if let Some(i) = self.fields.iter().position(|f| !key.is_empty() && f.key == key)
            && let Some(s) = self.sections.iter().position(|(_, r)| r.contains(&i))
        {
            self.show_section(s);
            self.focus = i;
            self.reveal_focus();
        }
    }

    fn show_section(&mut self, s: usize) {
        if s == self.section || s >= self.sections.len() {
            return;
        }
        self.section = s;
        self.scroll = 0.0;
        self.dropdown = None;
        self.focus = self.sections[s].1.start;
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
            header("Twitch"),
            button("twitch_signin", "Account", "Sign in with Twitch"),
            password("twitch_api", "Manual API token", has(SecretKind::TwitchApi)),
            text("live_check_secs", "Live check every (seconds)", "at least 30", &c.live_check_secs.to_string()),
        ];
        let title = match cfg {
            Some(c) => format!("Edit network — {}", c.name),
            None => "Add network".into(),
        };
        Form::new(title, FormKind::Network { original: cfg.map(|c| c.name.clone()) }, fields)
    }

    pub fn settings(c: &Config, scripts: Option<(&[ScriptInfo], &str)>) -> Form {
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
        let mut fields = fields;
        if let Some((infos, dir)) = scripts {
            fields.push(header("Scripts"));
            let mut folder = button("scripts_folder", "Folder", "Open scripts folder");
            if let FieldKind::Button { note, .. } = &mut folder.kind {
                *note = dir.to_owned();
            }
            fields.push(folder);
            fields.push(button("scripts_reload", "Reload", "Reload all scripts"));
            let mut examples = button("scripts_examples", "Examples", "Add example scripts");
            if let (true, FieldKind::Button { note, .. }) = (infos.is_empty(), &mut examples.kind) {
                *note = "No scripts yet: start with the examples".into();
            }
            fields.push(examples);
            fields.extend(infos.iter().map(script_row));
        }
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
            return Err(self.fail("name", "Give the network a name."));
        }
        let servers = self.list("servers", ',');
        if servers.is_empty() {
            return Err(self.fail("servers", "Add at least one server."));
        }
        if let Some(bad) = servers.iter().find(|s| NetworkConfig::parse_server(s).is_none()) {
            return Err(self.fail("servers", format!("\"{bad}\" is not a valid server address.")));
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
        let api = self.text("twitch_api");
        if !api.is_empty() {
            secrets.push((SecretKind::TwitchApi, api));
        }
        let live_check_secs = match self.text("live_check_secs") {
            s if s.is_empty() => 120,
            s => match s.parse::<u32>() {
                Ok(n) if n >= 30 => n,
                _ => {
                    return Err(self
                        .fail("live_check_secs", "The live check interval must be a number of seconds, at least 30."));
                }
            },
        };
        let sasl_pw = self.text("sasl_password");
        if !sasl_pw.is_empty() {
            secrets.push((SecretKind::Sasl, sasl_pw));
        }
        if matches!(sasl, SaslMechanism::Plain | SaslMechanism::ScramSha256)
            && secrets.iter().all(|(k, _)| *k != SecretKind::Sasl)
            && !self.password_stored("sasl_password")
        {
            return Err(self.fail("sasl_password", "SASL needs an account password."));
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
            live_check_secs,
            ..base
        };
        Ok((cfg, secrets))
    }

    pub fn apply_settings(&self, c: &mut Config) -> Result<(), String> {
        let num = |key: &'static str, label: &str| -> Result<u64, String> {
            self.text(key).parse::<u64>().map_err(|_| self.fail(key, format!("{label} must be a whole number.")))
        };
        let font_size: f32 = self
            .text("font_size")
            .replace(',', ".")
            .parse()
            .map_err(|_| self.fail("font_size", "Font size must be a number."))?;
        if !(8.0..=40.0).contains(&font_size) {
            return Err(self.fail("font_size", "Font size must be between 8 and 40."));
        }
        let nick = self.text("nick");
        if nick.is_empty() || nick.contains(' ') {
            return Err(self.fail("nick", "Enter a nickname without spaces."));
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
        let w = 820.0f32.min(win.w - 40.0);
        let h = (win.h - 60.0).min(640.0);
        Rect::new(win.x + (win.w - w) / 2.0, win.y + (win.h - h) / 2.0, w, h)
    }

    fn nav_rect(&self, win: Rect) -> Rect {
        let p = self.panel(win);
        Rect::new(p.x + 12.0, p.y + 64.0, NAV_W - 12.0, p.h - 64.0 - FOOTER_H)
    }

    fn list_rect(&self, win: Rect) -> Rect {
        let p = self.panel(win);
        let x = p.x + NAV_W + 12.0;
        Rect::new(x, p.y + 64.0 + 40.0, p.right() - 12.0 - x, p.h - 64.0 - 40.0 - FOOTER_H)
    }

    fn page(&self) -> Range<usize> {
        self.sections.get(self.section).map_or(0..0, |(_, r)| r.clone())
    }

    pub fn render(&mut self, p: &Painter, text: &Text, th: &Theme, win: Rect, caret_on: bool) {
        let f = &text.fonts;
        p.fill(win, th.overlay_scrim);
        let panel = self.panel(win);
        p.fill_round(panel, 12.0, th.panel_bg);
        p.stroke_round(panel, 12.0, th.border, 1.0);
        let t = text.layout(&self.title, &f.title, panel.w - 40.0, 30.0);
        p.text(&t, panel.x + 24.0, panel.y + 20.0, th.text);

        // Left pane: one entry per page.
        let nav = self.nav_rect(win);
        p.line(nav.right() + 6.0, nav.y, nav.right() + 6.0, nav.bottom(), th.border, 1.0);
        self.nav.clear();
        for (i, (label, _)) in self.sections.iter().enumerate() {
            let r = Rect::new(nav.x, nav.y + i as f32 * 36.0, nav.w - 8.0, 32.0);
            if i == self.section {
                p.fill_round(r, 6.0, th.sidebar_selected);
                p.fill_round(Rect::new(r.x, r.y + 8.0, 3.0, r.h - 16.0), 1.5, th.accent);
            } else if self.nav_hover == Some(i) {
                p.fill_round(r, 6.0, th.sidebar_hover);
            }
            let font = if i == self.section { &f.ui_semibold } else { &f.ui };
            let l = text.layout(label, font, r.w - 24.0, 20.0);
            p.text(&l, r.x + 14.0, r.y + 7.0, if i == self.section { th.text } else { th.text_dim });
            self.nav.push(r);
        }

        // Page title and rows.
        let list = self.list_rect(win);
        if let Some((label, _)) = self.sections.get(self.section) {
            let l = text.layout(label, &f.title, list.w - 16.0, 30.0);
            p.text(&l, list.x + 16.0, list.y - 36.0, th.text);
        }
        let page = self.page();
        self.content_h = page.len() as f32 * ROW_H + 8.0;
        self.list_h = list.h;
        let overflow = self.content_h > list.h;
        self.scroll = self.scroll.clamp(0.0, (self.content_h - list.h).max(0.0));
        let ctl_w = list.w - LABEL_W - if overflow { 30.0 } else { 16.0 };
        p.clip(list);
        self.rows.clear();
        let mut y = list.y + 4.0 - self.scroll;
        for i in page {
            let focused = i == self.focus;
            let open = self.dropdown.is_some_and(|(d, _)| d == i);
            let field = &mut self.fields[i];
            let row = Rect::new(list.x, y, list.w, ROW_H);
            let ctl = Rect::new(list.x + LABEL_W, y + 4.0, ctl_w, ROW_H - 8.0);
            // Switches sit at the right edge, so their labels can use the whole row.
            let label_w =
                if matches!(field.kind, FieldKind::Check(_)) { ctl.right() - list.x - 76.0 } else { LABEL_W - 28.0 };
            let l = text.layout(field.label, &f.ui, label_w, 20.0);
            p.text(&l, list.x + 16.0, y + 11.0, th.text);
            match &mut field.kind {
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
                    let border = if focused || open { with_alpha(th.accent, 0.8) } else { th.border };
                    p.stroke_round(ctl, 6.0, border, if focused || open { 1.5 } else { 1.0 });
                    let l = text.layout(opts[*idx].1, &f.ui, ctl.w - 44.0, 20.0);
                    p.text(&l, ctl.x + 10.0, ctl.y + 8.0, th.text);
                    chevron(p, ctl.right() - 20.0, ctl.y + ctl.h / 2.0, open, th.text_dim);
                }
                FieldKind::Check(on) => {
                    let bx = Rect::new(ctl.right() - 40.0, ctl.y + (ctl.h - 20.0) / 2.0, 36.0, 20.0);
                    p.fill_round(bx, 10.0, if *on { th.accent } else { th.badge_bg });
                    if focused {
                        p.stroke_round(bx.inset(-2.0, -2.0), 12.0, with_alpha(th.accent, 0.6), 1.0);
                    }
                    let knob_x = if *on { bx.right() - 10.0 } else { bx.x + 10.0 };
                    p.circle(knob_x, bx.y + 10.0, 7.0, if *on { th.accent_fg } else { th.text_dim });
                }
                FieldKind::Script { file, enabled, note, failed, .. } => {
                    // File name in the label column, state beside it, switch at the right edge.
                    let fl = text.layout(file, &f.ui_semibold, LABEL_W - 28.0, 20.0);
                    p.text(&fl, list.x + 16.0, y + 11.0, th.text);
                    let nl = text.layout(note, &f.ui, (ctl.w - 56.0).max(1.0), 20.0);
                    p.text(&nl, ctl.x, ctl.y + 8.0, if *failed { th.error } else { th.text_dim });
                    let bx = Rect::new(ctl.right() - 40.0, ctl.y + (ctl.h - 20.0) / 2.0, 36.0, 20.0);
                    p.fill_round(bx, 10.0, if *enabled { th.accent } else { th.badge_bg });
                    if focused {
                        p.stroke_round(bx.inset(-2.0, -2.0), 12.0, with_alpha(th.accent, 0.6), 1.0);
                    }
                    let knob_x = if *enabled { bx.right() - 10.0 } else { bx.x + 10.0 };
                    p.circle(knob_x, bx.y + 10.0, 7.0, if *enabled { th.accent_fg } else { th.text_dim });
                }
                FieldKind::Button { caption, note } => {
                    let cl = text.layout(caption, &f.ui_semibold, ctl.w, 20.0);
                    let bw = (text::metrics(&cl).width + 32.0).min(ctl.w);
                    let b = Rect::new(ctl.x, ctl.y, bw, ctl.h);
                    p.fill_round(b, 6.0, th.accent);
                    if focused {
                        p.stroke_round(b.inset(-2.0, -2.0), 8.0, with_alpha(th.accent, 0.6), 1.0);
                    }
                    p.text(&cl, b.x + 16.0, b.y + 8.0, th.accent_fg);
                    if !note.is_empty() {
                        let nl = text.layout(note, &f.ui, (ctl.w - bw - 14.0).max(1.0), 20.0);
                        p.text(&nl, b.right() + 14.0, b.y + 8.0, th.text_dim);
                    }
                }
                FieldKind::Header => {}
            }
            self.rows.push((i, row, ctl));
            y += ROW_H;
        }
        p.unclip();

        // Scrollbar for pages taller than the dialog.
        self.scrollbar = overflow.then(|| {
            let track = Rect::new(list.right() - 10.0, list.y + 2.0, 6.0, list.h - 4.0);
            let th_h = (track.h * list.h / self.content_h).max(28.0);
            let max = (self.content_h - list.h).max(1.0);
            let thumb = Rect::new(track.x, track.y + (track.h - th_h) * (self.scroll / max), track.w, th_h);
            (track, thumb)
        });
        if let Some((track, thumb)) = self.scrollbar {
            p.fill_round(track, 3.0, with_alpha(th.scrollbar, th.scrollbar.a * 0.35));
            let c = if self.grab.is_some() { with_alpha(th.text_dim, 0.9) } else { th.scrollbar };
            p.fill_round(thumb, 3.0, c);
        }

        // Footer.
        self.buttons.clear();
        let by = panel.bottom() - 52.0;
        p.line(
            panel.x,
            panel.bottom() - FOOTER_H + 0.5,
            panel.right(),
            panel.bottom() - FOOTER_H + 0.5,
            th.border,
            1.0,
        );
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

        // Open dropdown, drawn over everything else.
        self.popup.clear();
        if let Some((fi, hi)) = self.dropdown
            && let Some(&(_, _, ctl)) = self.rows.iter().find(|(i, ..)| *i == fi)
            && let FieldKind::Choice(opts, idx) = &self.fields[fi].kind
        {
            let h = opts.len() as f32 * OPTION_H + 8.0;
            let below = ctl.bottom() + 4.0;
            let y = if below + h <= win.bottom() - 8.0 { below } else { (ctl.y - 4.0 - h).max(win.y + 8.0) };
            let r = Rect::new(ctl.x, y, ctl.w, h);
            p.fill_round(Rect::new(r.x + 1.0, r.y + 3.0, r.w, r.h), 8.0, with_alpha(th.overlay_scrim, 0.6));
            p.fill_round(r, 8.0, th.panel_bg);
            p.fill_round(r, 8.0, th.sidebar_hover);
            p.stroke_round(r, 8.0, th.border, 1.0);
            for (i, (_, label)) in opts.iter().enumerate() {
                let item = Rect::new(r.x + 4.0, r.y + 4.0 + i as f32 * OPTION_H, r.w - 8.0, OPTION_H);
                if i == hi {
                    p.fill_round(item, 5.0, th.sidebar_selected);
                }
                if i == *idx {
                    let c = text.layout("✓", &f.ui_semibold, 20.0, 20.0);
                    p.text(&c, item.x + 8.0, item.y + 7.0, th.accent);
                }
                let l = text.layout(label, &f.ui, item.w - 40.0, 20.0);
                p.text(&l, item.x + 30.0, item.y + 7.0, th.text);
                self.popup.push(item);
            }
        }
    }

    pub fn click(&mut self, win: Rect, x: f32, y: f32) -> FormAction {
        // With a dropdown open, a click picks an option or just closes it.
        if let Some((fi, _)) = self.dropdown.take() {
            if let Some(i) = self.popup.iter().position(|r| r.contains(x, y))
                && let FieldKind::Choice(_, idx) = &mut self.fields[fi].kind
            {
                *idx = i;
            }
            return FormAction::None;
        }
        if !self.panel(win).contains(x, y) {
            return FormAction::None;
        }
        if let Some(i) = self.buttons.iter().position(|(_, r)| r.contains(x, y)) {
            return std::mem::replace(&mut self.buttons[i].0, FormAction::None);
        }
        if let Some(s) = self.nav.iter().position(|r| r.contains(x, y)) {
            self.show_section(s);
            return FormAction::None;
        }
        if let Some((track, thumb)) = self.scrollbar
            && x >= track.x - 6.0
            && x <= track.right() + 6.0
            && y >= track.y
            && y <= track.bottom()
        {
            // Grab the thumb where it was hit; a click on the track centers the thumb there.
            let grab = if thumb.contains(track.x, y) { y - thumb.y } else { thumb.h / 2.0 };
            self.grab = Some(grab);
            self.drag_to(y);
            return FormAction::None;
        }
        if !self.list_rect(win).contains(x, y) {
            return FormAction::None;
        }
        let Some(&(i, _, ctl)) = self.rows.iter().find(|(_, r, _)| r.contains(x, y)) else { return FormAction::None };
        self.focus = i;
        if matches!(self.fields[i].kind, FieldKind::Button { .. }) {
            return if ctl.contains(x, y) { FormAction::Button(self.fields[i].key) } else { FormAction::None };
        }
        match &mut self.fields[i].kind {
            FieldKind::Check(on) => *on = !*on,
            FieldKind::Script { name, enabled, .. } => {
                return FormAction::ScriptSwitch { name: name.clone(), enabled: !*enabled };
            }
            FieldKind::Choice(_, idx) => {
                if ctl.contains(x, y) {
                    self.dropdown = Some((i, *idx));
                }
            }
            FieldKind::Text(ed) | FieldKind::Password(ed, _) => {
                if x >= ctl.x {
                    ed.click(x - ctl.x - 10.0, 10.0, false);
                }
            }
            FieldKind::Header | FieldKind::Button { .. } => {}
        }
        FormAction::None
    }

    /// Pointer movement; returns whether the dialog needs a repaint.
    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        if self.grab.is_some() {
            self.drag_to(y);
            return true;
        }
        let mut changed = false;
        if let Some((fi, hi)) = self.dropdown
            && let Some(i) = self.popup.iter().position(|r| r.contains(x, y))
            && i != hi
        {
            self.dropdown = Some((fi, i));
            changed = true;
        }
        let hover = self.nav.iter().position(|r| r.contains(x, y));
        if hover != self.nav_hover {
            self.nav_hover = hover;
            changed = true;
        }
        changed
    }

    pub fn mouse_up(&mut self) -> bool {
        self.grab.take().is_some()
    }

    fn drag_to(&mut self, y: f32) {
        let (Some((track, thumb)), Some(grab)) = (self.scrollbar, self.grab) else { return };
        let room = (track.h - thumb.h).max(1.0);
        let t = ((y - grab - track.y) / room).clamp(0.0, 1.0);
        self.scroll = t * (self.content_h - self.list_h).max(0.0);
    }

    /// Whether the pointer is over a text box (for the I-beam cursor).
    pub fn text_at(&self, x: f32, y: f32) -> bool {
        self.dropdown.is_none()
            && self.rows.iter().any(|(i, _, ctl)| {
                ctl.contains(x, y) && matches!(self.fields[*i].kind, FieldKind::Text(_) | FieldKind::Password(..))
            })
    }

    pub fn scroll_by(&mut self, dy: f32) {
        self.dropdown = None;
        self.scroll -= dy;
    }

    fn move_focus(&mut self, delta: i32) {
        let page = self.page();
        if page.is_empty() {
            return;
        }
        let n = page.len() as i32;
        let i = (self.focus.saturating_sub(page.start) as i32 + delta).rem_euclid(n);
        self.focus = page.start + i as usize;
        self.reveal_focus();
    }

    /// Scrolls so the focused row is visible.
    fn reveal_focus(&mut self) {
        let top = self.focus.saturating_sub(self.page().start) as f32 * ROW_H;
        if top < self.scroll {
            self.scroll = top;
        } else if top + ROW_H + 8.0 > self.scroll + self.list_h {
            self.scroll = top + ROW_H + 8.0 - self.list_h;
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
        if let Some((fi, hi)) = self.dropdown {
            let n = match &self.fields[fi].kind {
                FieldKind::Choice(o, _) => o.len(),
                _ => 1,
            };
            match v {
                VK_ESCAPE | VK_TAB => self.dropdown = None,
                VK_UP => self.dropdown = Some((fi, hi.saturating_sub(1))),
                VK_DOWN => self.dropdown = Some((fi, (hi + 1).min(n - 1))),
                VK_HOME => self.dropdown = Some((fi, 0)),
                VK_END => self.dropdown = Some((fi, n - 1)),
                VK_RETURN | VK_SPACE => {
                    if let FieldKind::Choice(_, idx) = &mut self.fields[fi].kind {
                        *idx = hi;
                    }
                    self.dropdown = None;
                }
                _ => {}
            }
            return FormAction::None;
        }
        let pages = self.sections.len().max(1);
        if matches!(v, VK_RETURN | VK_SPACE) && matches!(self.fields[self.focus].kind, FieldKind::Button { .. }) {
            return FormAction::Button(self.fields[self.focus].key);
        }
        match v {
            VK_ESCAPE => return FormAction::Cancel,
            VK_RETURN => return FormAction::Save,
            VK_TAB | VK_NEXT | VK_PRIOR if ctrl => {
                let back = if v == VK_TAB { shift } else { v == VK_PRIOR };
                self.show_section((self.section + if back { pages - 1 } else { 1 }) % pages);
            }
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
                FieldKind::Script { name, enabled, .. } if v == VK_SPACE => {
                    return FormAction::ScriptSwitch { name: name.clone(), enabled: !*enabled };
                }
                FieldKind::Choice(_, idx) if v == VK_SPACE || v == VK_F4 => self.dropdown = Some((self.focus, *idx)),
                FieldKind::Choice(opts, idx) if v == VK_RIGHT => *idx = (*idx + 1) % opts.len(),
                FieldKind::Choice(opts, idx) if v == VK_LEFT => *idx = (*idx + opts.len() - 1) % opts.len(),
                _ => {}
            },
        }
        FormAction::None
    }

    pub fn char(&mut self, s: &str) {
        if self.dropdown.is_some() {
            return;
        }
        if let FieldKind::Text(ed) | FieldKind::Password(ed, _) = &mut self.fields[self.focus].kind {
            ed.insert(s);
        }
    }
}

/// A small "v" (or "^" while open) drawn with two strokes.
fn chevron(p: &Painter, cx: f32, cy: f32, up: bool, c: crate::gfx::Color) {
    let d = if up { -3.0 } else { 3.0 };
    p.line(cx - 5.0, cy - d, cx, cy + d, c, 1.5);
    p.line(cx, cy + d, cx + 5.0, cy - d, c, 1.5);
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
        let mut f = Form::settings(&c, None);
        set(&mut f, "font_size", "15,5");
        set(&mut f, "highlight_words", "rust, irc");
        f.apply_settings(&mut c).unwrap();
        assert_eq!(c.appearance.font_size, 15.5);
        assert_eq!(c.highlight.words, ["rust", "irc"]);
        set(&mut f, "font_size", "99");
        assert!(f.apply_settings(&mut c).is_err());
    }

    #[test]
    fn script_switches_apply_immediately() {
        let info = |name: &str, enabled: bool| ScriptInfo {
            name: name.into(),
            file: format!("{name}.js"),
            enabled,
            running: enabled,
            error: None,
            network: false,
            commands: Vec::new(),
        };
        let mut c = Config::default();
        c.scripts.disabled = vec!["b".into()];
        let mut f = Form::settings(&c, Some((&[info("a", true), info("b", false)], r"C:scripts")));
        assert_eq!(f.sections.last().unwrap().0, "Scripts");
        f.show_section(f.sections.len() - 1);

        // Space on a script row asks to switch it right away instead of waiting for Save.
        f.focus =
            f.fields.iter().position(|x| matches!(&x.kind, FieldKind::Script { name, .. } if name == "b")).unwrap();
        let action = f.key(VK_SPACE, false, false, || None);
        assert!(matches!(action, FormAction::ScriptSwitch { ref name, enabled: true } if name == "b"));

        // The rows then show what the host reports, and the focus stays on the same script.
        f.set_scripts(&[info("a", true), info("b", true), info("c", true)]);
        assert!(matches!(&f.fields[f.focus].kind, FieldKind::Script { name, enabled: true, .. } if name == "b"));
        assert_eq!(f.fields.iter().filter(|x| matches!(x.kind, FieldKind::Script { .. })).count(), 3);

        // Save leaves the script list alone (it is already applied).
        f.apply_settings(&mut c).unwrap();
        assert_eq!(c.scripts.disabled, ["b"]);
    }

    #[test]
    fn settings_pages_errors_and_dropdown_keys() {
        let mut c = Config::default();
        let mut f = Form::settings(&c, None);
        let pages: Vec<_> = f.sections.iter().map(|(t, _)| *t).collect();
        assert_eq!(pages, ["Identity", "Appearance", "Chat", "Notifications", "Link previews", "History & window"]);

        // An error on another page brings that page and field up.
        set(&mut f, "nick", "");
        f.show_section(2);
        let e = f.apply_settings(&mut c).unwrap_err();
        f.set_error(e);
        assert_eq!((f.section, f.fields[f.focus].key), (0, "nick"));

        // Keyboard: Ctrl+Tab to Appearance, Space opens the theme dropdown, Down + Enter picks.
        f.key(VK_TAB, true, false, || None);
        assert_eq!(f.fields[f.focus].key, "theme");
        f.key(VK_SPACE, false, false, || None);
        assert!(f.dropdown.is_some());
        f.key(VK_DOWN, false, false, || None);
        f.key(VK_RETURN, false, false, || None);
        assert!(f.dropdown.is_none());
        assert_eq!(f.choice("theme"), "dark");
    }
}
