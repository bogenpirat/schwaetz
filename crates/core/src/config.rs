//! User configuration (`config.toml`).
//!
//! Secrets (SASL/server passwords, Twitch tokens) are never stored here; they live in the Windows
//! Credential Manager (see [`crate::secrets`]).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub appearance: Appearance,
    pub notifications: Notifications,
    pub highlight: Highlight,
    pub previews: Previews,
    #[serde(default, skip_serializing_if = "Scripts::is_default")]
    pub scripts: Scripts,
    #[serde(rename = "ignore", skip_serializing_if = "Vec::is_empty")]
    pub ignores: Vec<IgnoreRule>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub aliases: BTreeMap<String, String>,
    #[serde(rename = "network", skip_serializing_if = "Vec::is_empty")]
    pub networks: Vec<NetworkConfig>,
}

/// `[scripts]`: which script files stay switched off.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Scripts {
    /// File names without extension (e.g. "7tv-emotes").
    pub disabled: Vec<String>,
}

impl Scripts {
    fn is_default(&self) -> bool {
        self.disabled.is_empty()
    }

    pub fn is_enabled(&self, name: &str) -> bool {
        !self.disabled.iter().any(|d| d.eq_ignore_ascii_case(name))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct General {
    pub nick: String,
    pub alt_nicks: Vec<String>,
    pub username: String,
    pub realname: String,
    pub quit_message: String,
    pub part_message: String,
    /// In-memory lines per buffer; older lines are paged from history on demand.
    pub scrollback_lines: usize,
    /// "all", "smart" (only for recently active users) or "none".
    pub show_joins_parts: String,
    /// Seconds a user must have spoken within for smart filtering to show their part/quit.
    pub smart_filter_secs: u64,
    pub ctcp_replies: bool,
    pub log_to_files: bool,
    /// Delete history older than this many days (0 = keep forever).
    pub history_days: u32,
    pub confirm_paste_lines: usize,
    pub minimize_to_tray: bool,
    pub close_to_tray: bool,
    /// Add channels you join to the network's autojoin list (and remove them when you leave).
    pub remember_channels: bool,
    /// Copy text selected in the chat to the clipboard as soon as the selection is made.
    pub copy_on_select: bool,
}

impl Default for General {
    fn default() -> Self {
        let user = std::env::var("USERNAME").unwrap_or_default();
        let nick: String = user.chars().filter(|c| c.is_ascii_alphanumeric() || "_-[]\\`^{}|".contains(*c)).collect();
        let nick = if nick.is_empty() || nick.as_bytes()[0].is_ascii_digit() { "schwaetzer".to_owned() } else { nick };
        General {
            alt_nicks: vec![format!("{nick}_"), format!("{nick}__")],
            username: nick.to_ascii_lowercase(),
            nick,
            realname: "schwätz user".into(),
            quit_message: "schwätz — https://github.com/bogenpirat/schwaetz".into(),
            part_message: String::new(),
            scrollback_lines: 1500,
            show_joins_parts: "smart".into(),
            smart_filter_secs: 1200,
            ctcp_replies: true,
            log_to_files: true,
            history_days: 0,
            confirm_paste_lines: 4,
            minimize_to_tray: false,
            close_to_tray: false,
            remember_channels: true,
            copy_on_select: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Appearance {
    /// Theme file name without extension, or "system" to follow Windows light/dark mode.
    pub theme: String,
    pub font: String,
    pub font_size: f32,
    pub ui_font: String,
    /// strftime-like: %H %M %S %d %m %Y are supported.
    pub timestamp_format: String,
    /// Right-align nicks in a column (weechat style).
    pub nick_column: bool,
    /// Fit the nick column to the longest name (with badges) in the buffer; otherwise it is
    /// `nick_column_width` characters wide.
    pub nick_column_auto: bool,
    pub nick_column_width: u32,
    pub show_nicklist: bool,
    pub colored_nicks: bool,
    pub show_mirc_colors: bool,
    /// Unread badges in the sidebar: "all" (new messages), "highlights" (highlights only) or
    /// "none". Networks can override it (`NetworkConfig::unread_badges`).
    pub unread_badges: String,
    pub mica: bool,
    /// Render on the GPU. Off by default: the CPU rasterizer (WARP) is fast enough for text and
    /// avoids the GPU driver's large memory footprint.
    pub gpu_acceleration: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            theme: "system".into(),
            font: "Segoe UI Variable Text".into(),
            font_size: 14.0,
            ui_font: "Segoe UI Variable Text".into(),
            timestamp_format: "%H:%M".into(),
            nick_column: true,
            nick_column_auto: true,
            nick_column_width: 14,
            show_nicklist: true,
            colored_nicks: true,
            show_mirc_colors: true,
            unread_badges: "all".into(),
            mica: true,
            gpu_acceleration: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Notifications {
    pub on_highlight: bool,
    pub on_private: bool,
    /// Also notify when the window is focused but another buffer is active.
    pub when_focused: bool,
    pub flash_taskbar: bool,
    pub sound: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Notifications { on_highlight: true, on_private: true, when_focused: false, flash_taskbar: true, sound: false }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Highlight {
    /// Highlight on the current nick.
    pub nick: bool,
    /// Case-insensitive whole words.
    pub words: Vec<String>,
    /// Regular expressions (regex-lite syntax).
    pub patterns: Vec<String>,
    /// Never highlight messages from these nick masks.
    pub exclude_nicks: Vec<String>,
}

impl Default for Highlight {
    fn default() -> Self {
        Highlight { nick: true, words: Vec::new(), patterns: Vec::new(), exclude_nicks: Vec::new() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Previews {
    /// Global switch; networks can additionally opt out.
    pub enabled: bool,
    /// Fetch automatically instead of showing a click-to-load placeholder.
    pub auto_load: bool,
    /// When non-empty, only these hosts (and subdomains) are fetched automatically.
    pub allow_hosts: Vec<String>,
    pub max_bytes: u64,
    pub max_dimension: u32,
}

impl Default for Previews {
    fn default() -> Self {
        Previews { enabled: false, auto_load: false, allow_hosts: Vec::new(), max_bytes: 10 << 20, max_dimension: 4096 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IgnoreRule {
    /// `nick!user@host` glob; partial masks are normalized (`foo` → `foo!*@*`).
    pub mask: String,
    /// Message kinds to ignore: msg, notice, action, ctcp, invite, join, part, quit, nick, all.
    #[serde(default = "IgnoreRule::default_types")]
    pub types: Vec<String>,
    /// Limit to one network by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    /// Limit to channels (glob).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

impl IgnoreRule {
    fn default_types() -> Vec<String> {
        vec!["all".into()]
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NetworkKind {
    #[default]
    Irc,
    Znc,
    Soju,
    Twitch,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SaslMechanism {
    #[default]
    None,
    Plain,
    External,
    #[serde(rename = "scram-sha-256", alias = "scram-sha256")]
    ScramSha256,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NetworkConfig {
    pub name: String,
    pub kind: NetworkKind,
    /// `host:port` (plain) or `host:+port` (TLS). Tried in order.
    pub servers: Vec<String>,
    pub auto_connect: bool,
    /// Overrides of the global identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nick: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alt_nicks: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realname: Option<String>,
    pub sasl: SaslMechanism,
    /// SASL account name (password in Credential Manager).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sasl_username: Option<String>,
    pub sasl_required: bool,
    /// Whether a server password (`PASS`) is stored in Credential Manager.
    pub server_password: bool,
    /// ZNC: `user` and `network` are combined into `PASS user/network:password`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub znc_user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub znc_network: Option<String>,
    /// soju: bind this connection to a bouncer network id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bouncer_netid: Option<String>,
    /// PEM file holding the client certificate (and optionally key) for CertFP.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_cert: Option<String>,
    pub accept_invalid_certs: bool,
    /// `#chan` or `#chan key`.
    pub autojoin: Vec<String>,
    /// Commands run after registration, e.g. `/msg NickServ …` or `/mode $nick +x`.
    pub perform: Vec<String>,
    pub reconnect: bool,
    pub reconnect_max_attempts: Option<u32>,
    pub rejoin_on_kick: bool,
    pub previews: bool,
    /// Flood control: lines allowed in a burst, then one per `flood_interval_ms`.
    pub flood_burst: u32,
    pub flood_interval_ms: u64,
    /// Twitch: Client-ID for badge images / OAuth device flow (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub twitch_client_id: Option<String>,
    /// Twitch: how often (seconds) joined channels are checked for being live when a Helix API
    /// token is stored.
    pub live_check_secs: u32,
    /// Twitch: show the name colors users picked (the `color` tag) in the chat, independently of
    /// `appearance.colored_nicks`.
    pub twitch_colors: bool,
    /// Twitch: "Open stream" opens the popout player instead of the channel page.
    pub twitch_popout: bool,
    /// Twitch: list live channels before offline ones in the sidebar (each group keeping
    /// `channel_order`).
    pub twitch_live_first: bool,
    /// Sidebar order of the channels, as arranged by dragging (empty: alphabetical). Channels not
    /// listed come after, alphabetically.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub channel_order: Vec<String>,
    /// Unread badges for this network's buffers ("all", "highlights" or "none"); unset follows
    /// `appearance.unread_badges`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unread_badges: Option<String>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        NetworkConfig {
            name: String::new(),
            kind: NetworkKind::Irc,
            servers: Vec::new(),
            auto_connect: true,
            nick: None,
            alt_nicks: Vec::new(),
            username: None,
            realname: None,
            sasl: SaslMechanism::None,
            sasl_username: None,
            sasl_required: false,
            server_password: false,
            znc_user: None,
            znc_network: None,
            bouncer_netid: None,
            client_cert: None,
            accept_invalid_certs: false,
            autojoin: Vec::new(),
            perform: Vec::new(),
            reconnect: true,
            reconnect_max_attempts: None,
            rejoin_on_kick: false,
            previews: true,
            flood_burst: 5,
            flood_interval_ms: 2000,
            twitch_client_id: None,
            live_check_secs: 120,
            twitch_colors: true,
            twitch_popout: false,
            twitch_live_first: false,
            channel_order: Vec::new(),
            unread_badges: None,
        }
    }
}

impl NetworkConfig {
    pub fn twitch() -> NetworkConfig {
        NetworkConfig {
            name: "Twitch".into(),
            kind: NetworkKind::Twitch,
            servers: vec!["irc.chat.twitch.tv:+6697".into()],
            server_password: true,
            flood_burst: 20,
            flood_interval_ms: 1500,
            ..Default::default()
        }
    }

    /// Parses `host:port`, `host:+port`, `host` (TLS 6697) and `ircs://host:port` forms.
    pub fn parse_server(s: &str) -> Option<(String, u16, bool)> {
        let s = s.trim();
        let (s, scheme_tls) = if let Some(r) = s.strip_prefix("ircs://") {
            (r, Some(true))
        } else if let Some(r) = s.strip_prefix("irc://") {
            (r, Some(false))
        } else {
            (s, None)
        };
        let s = s.split('/').next()?;
        if s.is_empty() {
            return None;
        }
        // IPv6 literal: [::1]:6697
        let (host, port) = if let Some(rest) = s.strip_prefix('[') {
            let (h, p) = rest.split_once(']')?;
            (h.to_owned(), p.strip_prefix(':'))
        } else {
            match s.rsplit_once(':') {
                Some((h, p)) if !h.contains(':') => (h.to_owned(), Some(p)),
                _ => (s.to_owned(), None),
            }
        };
        match port {
            None => {
                let tls = scheme_tls.unwrap_or(true);
                Some((host, if tls { 6697 } else { 6667 }, tls))
            }
            Some(p) => {
                let (p, plus) = match p.strip_prefix('+') {
                    Some(p) => (p, true),
                    None => (p, false),
                };
                let port: u16 = p.parse().ok()?;
                let tls = scheme_tls.unwrap_or(plus || port == 6697 || port == 6698 || port == 7000 || port == 9999);
                Some((host, port, tls || plus))
            }
        }
    }

    /// Autojoin entries as (channel, key).
    pub fn autojoin_list(&self) -> Vec<(String, Option<String>)> {
        self.autojoin
            .iter()
            .filter_map(|e| {
                let mut it = e.split_whitespace();
                let chan = it.next()?.to_owned();
                Some((chan, it.next().map(str::to_owned)))
            })
            .collect()
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Writes atomically (temp file + rename) so a crash never leaves a truncated config.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    pub fn network(&self, name: &str) -> Option<&NetworkConfig> {
        self.networks.iter().find(|n| n.name.eq_ignore_ascii_case(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_forms() {
        assert_eq!(NetworkConfig::parse_server("irc.libera.chat"), Some(("irc.libera.chat".into(), 6697, true)));
        assert_eq!(NetworkConfig::parse_server("irc.x.org:6667"), Some(("irc.x.org".into(), 6667, false)));
        assert_eq!(NetworkConfig::parse_server("irc.x.org:+7001"), Some(("irc.x.org".into(), 7001, true)));
        assert_eq!(NetworkConfig::parse_server("ircs://irc.x.org:1234/#chan"), Some(("irc.x.org".into(), 1234, true)));
        assert_eq!(NetworkConfig::parse_server("[::1]:+6697"), Some(("::1".into(), 6697, true)));
        assert_eq!(NetworkConfig::parse_server("irc.x.org:abc"), None);
    }

    #[test]
    fn roundtrip_toml() {
        let mut c = Config::default();
        c.networks.push(NetworkConfig {
            name: "Libera".into(),
            servers: vec!["irc.libera.chat:+6697".into()],
            autojoin: vec!["#rust".into(), "#secret key".into()],
            sasl: SaslMechanism::ScramSha256,
            sasl_username: Some("me".into()),
            ..Default::default()
        });
        c.aliases.insert("j".into(), "/join $1-".into());
        c.ignores.push(IgnoreRule { mask: "spam!*@*".into(), types: vec!["msg".into()], network: None, channel: None });
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, c);
        assert_eq!(back.networks[0].autojoin_list()[1], ("#secret".into(), Some("key".into())));
    }

    #[test]
    fn partial_config_uses_defaults() {
        let c: Config =
            toml::from_str("[general]\nnick = \"bob\"\n[[network]]\nname = \"x\"\nservers = [\"a.b\"]\n").unwrap();
        assert_eq!(c.general.nick, "bob");
        assert_eq!(c.general.scrollback_lines, 1500);
        assert!(c.networks[0].reconnect);
    }
}

#[cfg(test)]
mod sasl_names {
    use super::*;

    #[test]
    fn scram_spelling() {
        let n: NetworkConfig = toml::from_str("sasl = \"scram-sha-256\"").unwrap();
        assert_eq!(n.sasl, SaslMechanism::ScramSha256);
        let n: NetworkConfig = toml::from_str("sasl = \"scram-sha256\"").unwrap();
        assert_eq!(n.sasl, SaslMechanism::ScramSha256);
        assert!(toml::to_string(&n).unwrap().contains("scram-sha-256"));
    }
}
