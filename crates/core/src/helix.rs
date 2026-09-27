//! A minimal Twitch Helix API client: whether channels are live, with their title and game.
//!
//! The app model decides *when* to check ([`crate::app::Effect::TwitchLive`]); the UI runs
//! [`check`] on a worker thread (it blocks on HTTPS) and hands the [`LiveResult`] back to
//! [`crate::App::on_live_result`].

use schwaetz_net::NetworkId;
use serde::Deserialize;
use std::collections::HashMap;

/// Stream state of one channel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamInfo {
    pub live: bool,
    pub title: String,
    pub game: String,
    /// Viewer count while live.
    pub viewers: u64,
    /// RFC 3339 start time while live.
    pub started_at: String,
}

impl StreamInfo {
    /// "Game — title" (either part may be missing).
    pub fn what(&self) -> String {
        match (self.game.is_empty(), self.title.trim().is_empty()) {
            (false, false) => format!("{} — {}", self.game, self.title.trim()),
            (false, true) => self.game.clone(),
            (true, false) => self.title.trim().to_owned(),
            (true, true) => String::new(),
        }
    }

    /// One-line summary for the topic bar.
    pub fn summary(&self) -> String {
        let what = self.what();
        let sep = if what.is_empty() { "" } else { " · " };
        if self.live {
            format!("🔴 Live{sep}{what} · {} viewers", group_digits(self.viewers))
        } else {
            format!("Offline{sep}{what}")
        }
    }

    /// Whether the change from `old` is worth a line in the channel (not just viewer counts).
    pub fn differs_from(&self, old: &StreamInfo) -> bool {
        self.live != old.live || self.title != old.title || self.game != old.game
    }
}

fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A live check to run for one network.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveRequest {
    pub network: NetworkId,
    pub token: String,
    /// Client ID belonging to the token (learned from the first validation).
    pub client_id: Option<String>,
    /// Known login → user id mappings.
    pub ids: HashMap<String, String>,
    /// Channel logins (lowercase, without `#`).
    pub logins: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct LiveResult {
    pub network: NetworkId,
    pub client_id: Option<String>,
    pub ids: HashMap<String, String>,
    /// Login → state for every channel that exists.
    pub result: Result<Vec<(String, StreamInfo)>, String>,
}

const HELIX: &str = "https://api.twitch.tv/helix";
const MAX_BODY: u64 = 1 << 20;

/// Runs a live check (blocking).
pub fn check(req: LiveRequest) -> LiveResult {
    let mut client_id = req.client_id.clone();
    let mut ids = req.ids.clone();
    let result = run(&req, &mut client_id, &mut ids);
    if result.is_err() {
        // The token may have been replaced or revoked; validate again next time.
        client_id = None;
    }
    LiveResult { network: req.network, client_id, ids, result }
}

fn run(
    req: &LiveRequest,
    client_id: &mut Option<String>,
    ids: &mut HashMap<String, String>,
) -> Result<Vec<(String, StreamInfo)>, String> {
    let token = normalize_token(&req.token);
    if token.is_empty() {
        return Err("no API token".into());
    }
    let cid = match client_id.clone() {
        Some(c) => c,
        None => {
            let auth = format!("OAuth {token}");
            let r = schwaetz_net::http::get_with(
                "https://id.twitch.tv/oauth2/validate",
                &[("Authorization", &auth)],
                MAX_BODY,
                false,
            )?;
            let c = match r.status {
                200 => parse_validate(&r.body)?,
                401 => return Err("the API token is invalid or expired".into()),
                s => return Err(format!("token validation failed (HTTP {s})")),
            };
            *client_id = Some(c.clone());
            c
        }
    };
    let bearer = format!("Bearer {token}");
    let headers = [("Authorization", bearer.as_str()), ("Client-Id", cid.as_str())];
    let get = |path: &str, key: &str, values: &[&String]| -> Result<Vec<u8>, String> {
        let query: Vec<String> = values.iter().map(|v| format!("{key}={}", encode(v))).collect();
        let url = format!("{HELIX}/{path}?first=100&{}", query.join("&"));
        let r = schwaetz_net::http::get_with(&url, &headers, MAX_BODY, false)?;
        match r.status {
            200 => Ok(r.body),
            401 => Err("the API token is invalid or expired".into()),
            429 => Err("rate limited by Twitch".into()),
            s => Err(format!("{path} request failed (HTTP {s})")),
        }
    };

    let logins: Vec<String> = req.logins.iter().map(|l| l.trim_start_matches('#').to_ascii_lowercase()).collect();
    let unknown: Vec<&String> = logins.iter().filter(|l| !ids.contains_key(*l)).collect();
    for chunk in unknown.chunks(100) {
        for (login, id) in parse_users(&get("users", "login", chunk)?)? {
            ids.insert(login, id);
        }
    }
    let wanted: Vec<&String> = logins.iter().filter_map(|l| ids.get(l)).collect();
    let mut out: HashMap<String, StreamInfo> = HashMap::new();
    for chunk in wanted.chunks(100) {
        for (login, info) in parse_streams(&get("streams", "user_id", chunk)?)? {
            out.insert(login, info);
        }
    }
    // Offline channels: their current title and category.
    let offline: Vec<&String> = logins.iter().filter(|l| !out.contains_key(*l)).filter_map(|l| ids.get(l)).collect();
    for chunk in offline.chunks(100) {
        for (login, info) in parse_channels(&get("channels", "broadcaster_id", chunk)?)? {
            out.insert(login, info);
        }
    }
    Ok(logins.into_iter().filter_map(|l| out.remove(&l).map(|i| (l, i))).collect())
}

/// Accepts tokens pasted as `oauth:xyz`, `Bearer xyz` or plain.
pub fn normalize_token(t: &str) -> &str {
    let t = t.trim();
    let t = t.strip_prefix("oauth:").unwrap_or(t);
    t.strip_prefix("Bearer ").unwrap_or(t).trim()
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b == b'_' { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

#[derive(Deserialize)]
struct Data<T> {
    data: Vec<T>,
}

fn json<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, String> {
    serde_json::from_slice(body).map_err(|e| format!("unexpected API response: {e}"))
}

pub fn parse_validate(body: &[u8]) -> Result<String, String> {
    #[derive(Deserialize)]
    struct V {
        client_id: String,
    }
    Ok(json::<V>(body)?.client_id)
}

pub fn parse_users(body: &[u8]) -> Result<Vec<(String, String)>, String> {
    #[derive(Deserialize)]
    struct U {
        id: String,
        login: String,
    }
    Ok(json::<Data<U>>(body)?.data.into_iter().map(|u| (u.login, u.id)).collect())
}

pub fn parse_streams(body: &[u8]) -> Result<Vec<(String, StreamInfo)>, String> {
    #[derive(Deserialize)]
    struct S {
        user_login: String,
        #[serde(default)]
        game_name: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        viewer_count: u64,
        #[serde(default)]
        started_at: String,
        #[serde(default, rename = "type")]
        kind: String,
    }
    Ok(json::<Data<S>>(body)?
        .data
        .into_iter()
        .filter(|s| s.kind.is_empty() || s.kind == "live")
        .map(|s| {
            let info = StreamInfo {
                live: true,
                title: s.title,
                game: s.game_name,
                viewers: s.viewer_count,
                started_at: s.started_at,
            };
            (s.user_login, info)
        })
        .collect())
}

pub fn parse_channels(body: &[u8]) -> Result<Vec<(String, StreamInfo)>, String> {
    #[derive(Deserialize)]
    struct C {
        broadcaster_login: String,
        #[serde(default)]
        game_name: String,
        #[serde(default)]
        title: String,
    }
    Ok(json::<Data<C>>(body)?
        .data
        .into_iter()
        .map(|c| (c.broadcaster_login, StreamInfo { title: c.title, game: c.game_name, ..Default::default() }))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_helix_responses() {
        let v = br#"{"client_id":"abc123","login":"me","scopes":[],"user_id":"1","expires_in":5000}"#;
        assert_eq!(parse_validate(v).unwrap(), "abc123");
        let u = br#"{"data":[{"id":"71092938","login":"xqc","display_name":"xQc"}]}"#;
        assert_eq!(parse_users(u).unwrap(), [("xqc".to_owned(), "71092938".to_owned())]);
        let s = br#"{"data":[{"id":"1","user_id":"71092938","user_login":"xqc","game_name":"Just Chatting",
            "type":"live","title":"hi chat","viewer_count":45123,"started_at":"2026-09-27T10:00:00Z"}],
            "pagination":{}}"#;
        let (login, info) = parse_streams(s).unwrap().remove(0);
        assert_eq!(login, "xqc");
        assert_eq!(info.summary(), "🔴 Live · Just Chatting — hi chat · 45,123 viewers");
        let c = r#"{"data":[{"broadcaster_id":"2","broadcaster_login":"ibai","game_name":"","title":"mañana"}]}"#
            .as_bytes();
        let (_, info) = parse_channels(c).unwrap().remove(0);
        assert_eq!(info.summary(), "Offline · mañana");
        assert!(parse_streams(b"<html>").is_err());
    }

    #[test]
    fn tokens_and_changes() {
        assert_eq!(normalize_token(" oauth:abc "), "abc");
        assert_eq!(normalize_token("Bearer abc"), "abc");
        let a = StreamInfo { live: true, title: "t".into(), viewers: 5, ..Default::default() };
        let b = StreamInfo { viewers: 900, ..a.clone() };
        assert!(!b.differs_from(&a));
        assert!(StreamInfo { live: false, ..a.clone() }.differs_from(&a));
        assert_eq!(group_digits(1_234_567), "1,234,567");
        assert_eq!(encode("a b"), "a%20b");
    }
}
