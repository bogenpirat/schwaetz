//! Form dialogs: the network editor and the settings dialog.

use crate::anim::{Anims, Control, button_face, mix};
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

/// Width of the "Browse…" button of file path fields.
const BROWSE_W: f32 = 96.0;
const ROW_H: f32 = 40.0;
const LABEL_W: f32 = 190.0;
/// Width of the page list on the left.
const NAV_W: f32 = 190.0;
const FOOTER_H: f32 = 68.0;
const OPTION_H: f32 = 32.0;
/// Space for the caption above a group of related fields: at the top of a page, and further
/// down (with room to set it apart from the fields above).
const GROUP_H: f32 = 30.0;
const GROUP_GAP: f32 = 14.0;

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
    /// A text field for a file path, with a "Browse…" button that opens a file dialog.
    pub browse: bool,
    /// Starts a group of related fields on its page, captioned with this.
    pub group: Option<&'static str>,
}

impl Field {
    /// Starts a captioned group of related fields with this one.
    fn group(self, caption: &'static str) -> Field {
        Field { group: Some(caption), ..self }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormKind {
    Network { original: Option<String> },
    Settings,
}

/// A button-like control in a dialog, remembered between press and release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Footer(usize),
    Field(usize),
}

pub enum FormAction {
    None,
    Save,
    Cancel,
    Delete,
    /// A button field was pressed (its key).
    Button(&'static str),
    /// The "Browse…" button of a file path field was pressed (its key).
    Browse(&'static str),
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
    /// Laid-out rows of the current page: (field, row, control, horizontal text scroll).
    rows: Vec<(usize, Rect, Rect, f32)>,
    /// "Browse…" buttons of file path fields on the current page: (field, bounds).
    browse: Vec<(usize, Rect)>,
    /// Text field being drag-selected with the mouse.
    selecting: Option<usize>,
    /// Button or switch under a pressed left button (it acts on release).
    pressed: Option<Target>,
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
    Field { browse: false, group: None, key, label, hint, kind: FieldKind::Text(e) }
}

/// A text field for a file path, with a "Browse…" button.
fn file(key: &'static str, label: &'static str, hint: &'static str, value: &str) -> Field {
    Field { browse: true, ..text(key, label, hint, value) }
}

fn password(key: &'static str, label: &'static str, stored: bool) -> Field {
    let mut e = Editor::single_line();
    e.masked = true;
    let hint = if stored { "stored — leave empty to keep" } else { "not set" };
    Field { browse: false, group: None, key, label, hint, kind: FieldKind::Password(e, stored) }
}

fn check(key: &'static str, label: &'static str, value: bool) -> Field {
    Field { browse: false, group: None, key, label, hint: "", kind: FieldKind::Check(value) }
}

fn choice(key: &'static str, label: &'static str, options: Vec<(&'static str, &'static str)>, value: &str) -> Field {
    let idx = options.iter().position(|(v, _)| *v == value).unwrap_or(0);
    Field { browse: false, group: None, key, label, hint: "", kind: FieldKind::Choice(options, idx) }
}

fn button(key: &'static str, label: &'static str, caption: &str) -> Field {
    Field {
        browse: false,
        group: None,
        key,
        label,
        hint: "",
        kind: FieldKind::Button { caption: caption.into(), note: String::new() },
    }
}

fn script_row(s: &ScriptInfo) -> Field {
    let (note, failed) = script_note(s);
    let kind = FieldKind::Script { name: s.name.clone(), file: s.file.clone(), enabled: s.enabled, note, failed };
    Field { browse: false, group: None, key: "script", label: "", hint: "", kind }
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

/// An on/off switch whose knob slides and whose track fades between off and on.
fn draw_switch(p: &Painter, th: &Theme, anims: &Anims, c: Control, bx: Rect, on: bool, focused: bool) {
    let s = anims.switch(c, on);
    p.fill_round(bx, 10.0, mix(th.badge_bg, th.accent, s));
    let hover = anims.hover(c);
    if hover > 0.0 {
        p.fill_round(bx, 10.0, with_alpha(th.text, 0.08 * hover));
    }
    if focused {
        p.stroke_round(bx.inset(-2.0, -2.0), 12.0, with_alpha(th.accent, 0.6), 1.0);
    }
    // Pressing squeezes the knob a little, like a finger on it.
    let knob_r = 7.0 - 1.5 * anims.press(c);
    let knob_x = bx.x + 10.0 + (bx.w - 20.0) * s;
    p.circle(knob_x, bx.y + 10.0, knob_r, mix(th.text_dim, th.accent_fg, s));
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
    Field { browse: false, group: None, key: "", label, hint: "", kind: FieldKind::Header }
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
            browse: Vec::new(),
            selecting: None,
            pressed: None,
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
            file(
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
                "Mechanism",
                vec![
                    ("none", "None"),
                    ("plain", "PLAIN"),
                    ("scram-sha-256", "SCRAM-SHA-256"),
                    ("external", "EXTERNAL (certificate)"),
                ],
                sasl,
            )
            .group("SASL (services account)"),
            text("sasl_username", "Account", "empty = nickname", c.sasl_username.as_deref().unwrap_or("")),
            password("sasl_password", "Account password", has(SecretKind::Sasl)),
            check("sasl_required", "Disconnect if SASL fails", c.sasl_required),
            text("znc_user", "ZNC user", "your ZNC username", c.znc_user.as_deref().unwrap_or("")).group("ZNC login"),
            text("znc_network", "ZNC network", "network name in ZNC", c.znc_network.as_deref().unwrap_or("")),
            header("Channels"),
            text("autojoin", "Join on connect", "#chan, #other key", &c.autojoin.join(", ")),
            text("perform", "Commands on connect", "separate with ;  e.g. /mode $nick +x", &c.perform.join(" ; ")),
            check("rejoin_on_kick", "Rejoin when kicked", c.rejoin_on_kick),
            check("previews", "Link previews on this network", c.previews),
            choice(
                "unread_badges",
                "Unread badges",
                vec![
                    ("", "As in the settings"),
                    ("all", "New messages"),
                    ("highlights", "Highlights only"),
                    ("none", "Off"),
                ],
                c.unread_badges.as_deref().unwrap_or(""),
            ),
            choice(
                "joins_parts",
                "Joins and parts",
                vec![("all", "Show"), ("smart", "Only for active users"), ("none", "Hide")],
                c.joins_parts_mode(),
            ),
            header("Twitch"),
            button("twitch_signin", "Account", "Sign in with Twitch"),
            password("twitch_api", "Manual API token", has(SecretKind::TwitchApi)),
            text("live_check_secs", "Live check every (seconds)", "at least 30", &c.live_check_secs.to_string()),
            check("twitch_colors", "Show Twitch name colors in the chat", c.twitch_colors),
            check("twitch_popout", "Open streams in the popout player", c.twitch_popout),
            check("twitch_live_first", "List live channels first", c.twitch_live_first),
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
            check("nick_column_auto", "Fit the nick column to the names", a.nick_column_auto),
            check("colored_nicks", "Colored nicks (theme palette)", a.colored_nicks),
            check("show_mirc_colors", "Show mIRC colors", a.show_mirc_colors),
            check("show_nicklist", "Show member list", a.show_nicklist),
            choice(
                "unread_badges",
                "Unread badges",
                vec![("all", "New messages"), ("highlights", "Highlights only"), ("none", "Off")],
                &a.unread_badges,
            ),
            check("mica", "Mica backdrop (Windows 11)", a.mica),
            check("gpu_acceleration", "GPU rendering (uses more memory)", a.gpu_acceleration),
            header("Chat"),
            text("highlight_words", "Highlight words", "comma-separated", &c.highlight.words.join(", ")),
            text("scrollback_lines", "Lines kept in memory", "", &g.scrollback_lines.to_string()),
            check("ctcp_replies", "Answer CTCP requests", g.ctcp_replies),
            check("remember_channels", "Keep \"Join on connect\" in sync with joins and parts", g.remember_channels),
            check("copy_on_select", "Copy selected chat text right away", g.copy_on_select),
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

    /// Replaces a text field's value (e.g. with a path picked in a file dialog).
    pub fn set_text(&mut self, key: &str, value: &str) {
        if let Some(FieldKind::Text(e)) = self.fields.iter_mut().find(|f| f.key == key).map(|f| &mut f.kind) {
            e.set_text(value, None);
        }
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
            twitch_colors: self.check("twitch_colors"),
            twitch_popout: self.check("twitch_popout"),
            twitch_live_first: self.check("twitch_live_first"),
            unread_badges: Some(self.choice("unread_badges")).filter(|m| !m.is_empty()).map(str::to_owned),
            joins_parts: Some(self.choice("joins_parts"))
                .filter(|m| *m != NetworkConfig::default_joins_parts(kind))
                .map(str::to_owned),
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
        c.appearance.nick_column_auto = self.check("nick_column_auto");
        c.appearance.colored_nicks = self.check("colored_nicks");
        c.appearance.show_mirc_colors = self.check("show_mirc_colors");
        c.appearance.show_nicklist = self.check("show_nicklist");
        c.appearance.unread_badges = self.choice("unread_badges").into();
        c.appearance.mica = self.check("mica");
        c.appearance.gpu_acceleration = self.check("gpu_acceleration");
        c.highlight.words = self.list("highlight_words", ',');
        c.general.scrollback_lines = scrollback;
        c.general.ctcp_replies = self.check("ctcp_replies");
        c.general.remember_channels = self.check("remember_channels");
        c.general.copy_on_select = self.check("copy_on_select");
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

    /// Height of field `i`'s row, including the caption of a group it starts.
    fn row_h(&self, i: usize) -> f32 {
        ROW_H + self.caption_h(i)
    }

    /// Space above field `i` for the caption of a group it starts.
    fn caption_h(&self, i: usize) -> f32 {
        match self.fields[i].group {
            None => 0.0,
            Some(_) if i == self.page().start => GROUP_H,
            Some(_) => GROUP_H + GROUP_GAP,
        }
    }

    /// Top of field `i`'s row (below its group caption) relative to the top of the page.
    fn row_top(&self, i: usize) -> f32 {
        let page = self.page();
        let before: f32 = (page.start..i.max(page.start)).map(|j| self.row_h(j)).sum();
        before + self.caption_h(i)
    }

    pub fn render(&mut self, p: &Painter, text: &Text, th: &Theme, win: Rect, caret_on: bool, anims: &Anims) {
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
        self.content_h = page.clone().map(|i| self.row_h(i)).sum::<f32>() + 8.0;
        self.list_h = list.h;
        let overflow = self.content_h > list.h;
        self.scroll = self.scroll.clamp(0.0, (self.content_h - list.h).max(0.0));
        let ctl_w = list.w - LABEL_W - if overflow { 30.0 } else { 16.0 };
        p.clip(list);
        self.rows.clear();
        self.browse.clear();
        let mut y = list.y + 4.0 - self.scroll;
        for i in page {
            let focused = i == self.focus;
            let open = self.dropdown.is_some_and(|(d, _)| d == i);
            let caption_h = self.caption_h(i);
            let field = &mut self.fields[i];
            if let Some(caption) = field.group {
                // Caption with a hairline running to the right edge, just above the group.
                let cl = text.layout(caption, &f.ui_semibold, list.w - 32.0, 20.0);
                let cm = text::metrics(&cl);
                let cy = y + caption_h - GROUP_H + 8.0;
                p.text(&cl, list.x + 16.0, cy, th.text_dim);
                let lx = list.x + 16.0 + cm.width + 12.0;
                let ly = (cy + cm.height / 2.0).round() + 0.5;
                p.line(lx, ly, list.right() - 16.0, ly, th.border, 1.0);
                y += caption_h;
            }
            let row = Rect::new(list.x, y, list.w, ROW_H);
            let mut text_dx = 0.0;
            let mut ctl = Rect::new(list.x + LABEL_W, y + 4.0, ctl_w, ROW_H - 8.0);
            // File path fields: the text box leaves room for "Browse…" at its right.
            let browse = field.browse.then(|| {
                let b = Rect::new(ctl.right() - BROWSE_W, ctl.y, BROWSE_W, ctl.h);
                ctl.w -= BROWSE_W + 8.0;
                b
            });
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
                    text_dx = dx;
                    p.clip(inner);
                    if ed.is_empty() && !field.hint.is_empty() {
                        let hl = text.layout(field.hint, &f.ui, inner.w, 20.0);
                        p.text(&hl, inner.x, ty, with_alpha(th.text_dim, 0.8));
                    }
                    // Only the focused field shows its selection, as in Windows edit boxes.
                    if focused {
                        for (sx, sy, sw, sh) in ed.selection_rects() {
                            p.fill(Rect::new(inner.x + sx - dx, ty + sy, sw, sh), th.selection);
                        }
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
                    draw_switch(p, th, anims, Control::DialogField(i), bx, *on, focused);
                }
                FieldKind::Script { file, enabled, note, failed, .. } => {
                    // File name in the label column, state beside it, switch at the right edge.
                    let fl = text.layout(file, &f.ui_semibold, LABEL_W - 28.0, 20.0);
                    p.text(&fl, list.x + 16.0, y + 11.0, th.text);
                    let nl = text.layout(note, &f.ui, (ctl.w - 56.0).max(1.0), 20.0);
                    p.text(&nl, ctl.x, ctl.y + 8.0, if *failed { th.error } else { th.text_dim });
                    let bx = Rect::new(ctl.right() - 40.0, ctl.y + (ctl.h - 20.0) / 2.0, 36.0, 20.0);
                    draw_switch(p, th, anims, Control::DialogField(i), bx, *enabled, focused);
                }
                FieldKind::Button { caption, note } => {
                    let cl = text.layout(caption, &f.ui_semibold, ctl.w, 20.0);
                    let bw = (text::metrics(&cl).width + 32.0).min(ctl.w);
                    let b = Rect::new(ctl.x, ctl.y, bw, ctl.h);
                    let c = Control::DialogField(i);
                    let face = button_face(p, b, 6.0, th.accent, th, anims.hover(c), anims.press(c));
                    if focused {
                        p.stroke_round(b.inset(-2.0, -2.0), 8.0, with_alpha(th.accent, 0.6), 1.0);
                    }
                    let cw = text::metrics(&cl).width;
                    p.text(&cl, face.x + (face.w - cw) / 2.0, face.y + (face.h - 18.0) / 2.0, th.accent_fg);
                    if !note.is_empty() {
                        let nl = text.layout(note, &f.ui, (ctl.w - bw - 14.0).max(1.0), 20.0);
                        p.text(&nl, b.right() + 14.0, b.y + 8.0, th.text_dim);
                    }
                }
                FieldKind::Header => {}
            }
            if let Some(b) = browse {
                let c = Control::DialogField(i);
                let face = button_face(p, b, 6.0, th.badge_bg, th, anims.hover(c), anims.press(c));
                let bl = text.layout("Browse…", &f.ui_semibold, b.w, 20.0);
                p.text(
                    &bl,
                    face.x + (face.w - text::metrics(&bl).width) / 2.0,
                    face.y + (face.h - 18.0) / 2.0,
                    th.text,
                );
                self.browse.push((i, b));
            }
            self.rows.push((i, row, ctl, text_dx));
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
        // Footer buttons are animated by position: Save 0, Cancel 1, Remove 2.
        let footer = |i: usize| (anims.hover(Control::DialogFooter(i)), anims.press(Control::DialogFooter(i)));
        let (h, pz) = footer(0);
        let face = button_face(p, save, 6.0, th.accent, th, h, pz);
        let sl = text.layout("Save", &f.ui_semibold, save.w, 30.0);
        p.text(&sl, face.x + (face.w - text::metrics(&sl).width) / 2.0, face.y + (face.h - 18.0) / 2.0, th.accent_fg);
        let (h, pz) = footer(1);
        let face = button_face(p, cancel, 6.0, th.badge_bg, th, h, pz);
        let cl = text.layout("Cancel", &f.ui_semibold, cancel.w, 30.0);
        p.text(&cl, face.x + (face.w - text::metrics(&cl).width) / 2.0, face.y + (face.h - 18.0) / 2.0, th.text);
        self.buttons.push((FormAction::Save, save));
        self.buttons.push((FormAction::Cancel, cancel));
        if matches!(&self.kind, FormKind::Network { original: Some(_) }) && self.error.is_none() {
            let del = Rect::new(panel.x + 24.0, by, 150.0, 34.0);
            let (h, pz) = (anims.hover(Control::DialogFooter(2)), anims.press(Control::DialogFooter(2)));
            let face = del.inset(pz * 1.5, pz * 1.5);
            if h + pz > 0.0 {
                p.fill_round(face, 6.0, with_alpha(th.error, 0.10 * h + 0.10 * pz));
            }
            p.stroke_round(face, 6.0, th.error, 1.0);
            let dl = text.layout("Remove network", &f.ui_semibold, del.w, 30.0);
            p.text(&dl, face.x + (face.w - text::metrics(&dl).width) / 2.0, face.y + (face.h - 18.0) / 2.0, th.error);
            self.buttons.push((FormAction::Delete, del));
        }

        // Open dropdown, drawn over everything else.
        self.popup.clear();
        if let Some((fi, hi)) = self.dropdown
            && let Some(&(_, _, ctl, _)) = self.rows.iter().find(|(i, ..)| *i == fi)
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

    /// The button-like control under a point (acts on release): footer buttons, button fields,
    /// switches and script switches.
    fn target_at(&self, win: Rect, x: f32, y: f32) -> Option<Target> {
        if !self.panel(win).contains(x, y) {
            return None;
        }
        if let Some(i) = self.buttons.iter().position(|(_, r)| r.contains(x, y)) {
            return Some(Target::Footer(i));
        }
        if !self.list_rect(win).contains(x, y) {
            return None;
        }
        if let Some(&(i, _)) = self.browse.iter().find(|(_, r)| r.contains(x, y)) {
            return Some(Target::Field(i));
        }
        let &(i, _, ctl, _) = self.rows.iter().find(|(_, r, ..)| r.contains(x, y))?;
        match self.fields[i].kind {
            FieldKind::Button { .. } if ctl.contains(x, y) => Some(Target::Field(i)),
            // A switch toggles from anywhere on its row.
            FieldKind::Check(_) | FieldKind::Script { .. } => Some(Target::Field(i)),
            _ => None,
        }
    }

    /// The animated control (button or switch) under a point.
    pub fn control_at(&self, win: Rect, x: f32, y: f32) -> Option<crate::anim::Control> {
        self.target_at(win, x, y).map(|t| match t {
            Target::Footer(i) => crate::anim::Control::DialogFooter(i),
            Target::Field(i) => crate::anim::Control::DialogField(i),
        })
    }

    /// Left button pressed. Selecting things (pages, text, opening a dropdown, the scrollbar)
    /// happens now; buttons and switches only remember the press and act in [`Form::release`].
    /// `double` is the second press of a double-click, `shift` extends text selections.
    pub fn press(&mut self, win: Rect, x: f32, y: f32, double: bool, shift: bool) -> FormAction {
        self.pressed = None;
        if self.dropdown.is_some() {
            // Options are picked on release; pressing anywhere else closes the list.
            if !self.popup.iter().any(|r| r.contains(x, y)) {
                self.dropdown = None;
            }
            return FormAction::None;
        }
        if let Some(t) = self.target_at(win, x, y) {
            if let Target::Field(i) = t {
                self.focus = i;
            }
            self.pressed = Some(t);
            return FormAction::None;
        }
        if !self.panel(win).contains(x, y) {
            return FormAction::None;
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
        let Some(&(i, _, ctl, dx)) = self.rows.iter().find(|(_, r, ..)| r.contains(x, y)) else {
            return FormAction::None;
        };
        self.focus = i;
        match &mut self.fields[i].kind {
            FieldKind::Choice(_, idx) => {
                if ctl.contains(x, y) {
                    self.dropdown = Some((i, *idx));
                }
            }
            FieldKind::Text(ed) | FieldKind::Password(ed, _) => {
                if double {
                    // Double-click selects the whole value.
                    ed.select_all();
                } else {
                    ed.click((x - ctl.x - 10.0 + dx).max(0.0), 10.0, shift);
                    self.selecting = Some(i);
                }
            }
            _ => {}
        }
        FormAction::None
    }

    /// Left button released: a pressed button or switch acts if the pointer is still on it, and
    /// an open dropdown takes the option under the pointer (also after dragging from the box).
    pub fn release(&mut self, win: Rect, x: f32, y: f32) -> FormAction {
        self.selecting = None;
        self.grab = None;
        let pressed = self.pressed.take();
        if let Some((fi, _)) = self.dropdown {
            if let Some(i) = self.popup.iter().position(|r| r.contains(x, y)) {
                if let FieldKind::Choice(_, idx) = &mut self.fields[fi].kind {
                    *idx = i;
                }
                self.dropdown = None;
            }
            return FormAction::None;
        }
        let Some(t) = pressed.filter(|t| self.target_at(win, x, y) == Some(*t)) else { return FormAction::None };
        match t {
            Target::Footer(i) => std::mem::replace(&mut self.buttons[i].0, FormAction::None),
            // File path fields are targets only through their "Browse…" button.
            Target::Field(i) if self.fields[i].browse => FormAction::Browse(self.fields[i].key),
            Target::Field(i) => match &mut self.fields[i].kind {
                FieldKind::Check(on) => {
                    *on = !*on;
                    FormAction::None
                }
                FieldKind::Script { name, enabled, .. } => {
                    FormAction::ScriptSwitch { name: name.clone(), enabled: !*enabled }
                }
                FieldKind::Button { .. } => FormAction::Button(self.fields[i].key),
                _ => FormAction::None,
            },
        }
    }

    /// Pointer movement; returns whether the dialog needs a repaint.
    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        if self.grab.is_some() {
            self.drag_to(y);
            return true;
        }
        if let Some(i) = self.selecting
            && let Some(&(_, _, ctl, dx)) = self.rows.iter().find(|(r, ..)| *r == i)
            && let FieldKind::Text(ed) | FieldKind::Password(ed, _) = &mut self.fields[i].kind
        {
            ed.click((x - ctl.x - 10.0 + dx).max(0.0), 10.0, true);
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

    fn drag_to(&mut self, y: f32) {
        let (Some((track, thumb)), Some(grab)) = (self.scrollbar, self.grab) else { return };
        let room = (track.h - thumb.h).max(1.0);
        let t = ((y - grab - track.y) / room).clamp(0.0, 1.0);
        self.scroll = t * (self.content_h - self.list_h).max(0.0);
    }

    /// The text box under a right-click (focused for its edit menu).
    pub fn editor_at(&mut self, win: Rect, x: f32, y: f32) -> Option<&mut Editor> {
        if self.dropdown.is_some() || !self.list_rect(win).contains(x, y) {
            return None;
        }
        let &(i, ..) = self.rows.iter().find(|(_, _, ctl, _)| ctl.contains(x, y))?;
        if !matches!(self.fields[i].kind, FieldKind::Text(_) | FieldKind::Password(..)) {
            return None;
        }
        self.focus = i;
        match &mut self.fields[i].kind {
            FieldKind::Text(ed) | FieldKind::Password(ed, _) => Some(ed),
            _ => None,
        }
    }

    /// Whether the pointer is over a text box (for the I-beam cursor).
    pub fn text_at(&self, x: f32, y: f32) -> bool {
        self.dropdown.is_none()
            && self.rows.iter().any(|(i, _, ctl, _)| {
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
        let top = self.row_top(self.focus);
        if top < self.scroll {
            // A group's first field brings its caption into view too.
            self.scroll = top - self.caption_h(self.focus);
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
        hwnd: windows::Win32::Foundation::HWND,
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
                FieldKind::Text(ed) | FieldKind::Password(ed, _) => {
                    crate::editor::edit_key(ed, v, ctrl, shift, hwnd);
                }
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
    use windows::Win32::Foundation::HWND;

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
        let action = f.key(VK_SPACE, false, false, HWND::default());
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
        f.key(VK_TAB, true, false, HWND::default());
        assert_eq!(f.fields[f.focus].key, "theme");
        f.key(VK_SPACE, false, false, HWND::default());
        assert!(f.dropdown.is_some());
        f.key(VK_DOWN, false, false, HWND::default());
        f.key(VK_RETURN, false, false, HWND::default());
        assert!(f.dropdown.is_none());
        assert_eq!(f.choice("theme"), "dark");
    }
}
