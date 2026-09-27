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

    /// One-line summary ("🔴 Live · Game — Title · 1,234 viewers").
    pub fn summary(&self) -> String {
        let (lead, title, tail) = self.parts();
        format!("{lead}{title}{tail}")
    }

    /// The summary in three pieces for the topic bar: status and game (always shown), the title
    /// (the part to shorten when space runs out) and the viewer count (always shown).
    pub fn parts(&self) -> (String, String, String) {
        let status = if self.live { "🔴 Live" } else { "Offline" };
        let (game, title) = (self.game.trim(), self.title.trim());
        let lead = match (game.is_empty(), title.is_empty()) {
            (false, false) => format!("{status} · {game} — "),
            (false, true) => format!("{status} · {game}"),
            (true, false) => format!("{status} · "),
            (true, true) => status.to_owned(),
        };
        let tail = if self.live { format!(" · {} viewers", group_digits(self.viewers)) } else { String::new() };
        (lead, title.to_owned(), tail)
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
    /// Twitch rejected the token (HTTP 401): refresh it, or ask for a new one.
    pub unauthorized: bool,
}

const HELIX: &str = "https://api.twitch.tv/helix";
const MAX_BODY: u64 = 1 << 20;
const UNAUTHORIZED: &str = "the API token is invalid or expired";

/// Runs a live check (blocking).
pub fn check(req: LiveRequest) -> LiveResult {
    let mut client_id = req.client_id.clone();
    let mut ids = req.ids.clone();
    let result = run(&req, &mut client_id, &mut ids);
    let unauthorized = matches!(&result, Err(e) if e == UNAUTHORIZED);
    if result.is_err() {
        // The token may have been replaced or revoked; validate again next time.
        client_id = None;
    }
    LiveResult { network: req.network, client_id, ids, result, unauthorized }
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
                401 => return Err(UNAUTHORIZED.into()),
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
            401 => Err(UNAUTHORIZED.into()),
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

// ----- emotes ------------------------------------------------------------------------------------

/// Fetch the Twitch emotes usable in a channel (for completion).
#[derive(Clone, Debug, PartialEq)]
pub struct EmoteRequest {
    pub network: NetworkId,
    /// `#channel`, lowercase.
    pub channel: String,
    /// The channel's Twitch user id (ROOMSTATE `room-id`).
    pub broadcaster_id: String,
    pub token: String,
}

#[derive(Clone, Debug)]
pub struct EmoteResult {
    pub network: NetworkId,
    pub channel: String,
    /// The emotes, and whether only global ones could be fetched because the token lacks the
    /// `user:read:emotes` permission.
    pub result: Result<(Vec<crate::emotes::EmoteEntry>, bool), String>,
}

/// Needed to list the emotes a user may use (follower, subscriber …).
pub const EMOTES_SCOPE: &str = "user:read:emotes";

/// Runs an emote fetch (blocking).
pub fn fetch_emotes(req: EmoteRequest) -> EmoteResult {
    EmoteResult { network: req.network, channel: req.channel.clone(), result: run_emotes(&req) }
}

fn run_emotes(req: &EmoteRequest) -> Result<(Vec<crate::emotes::EmoteEntry>, bool), String> {
    let token = normalize_token(&req.token);
    // Who the token belongs to, for which app, and what it may do.
    let auth = format!("OAuth {token}");
    let r = schwaetz_net::http::get_with(
        "https://id.twitch.tv/oauth2/validate",
        &[("Authorization", &auth)],
        MAX_BODY,
        false,
    )?;
    if r.status != 200 {
        return Err(UNAUTHORIZED.into());
    }
    #[derive(Deserialize)]
    struct V {
        client_id: String,
        #[serde(default)]
        user_id: String,
        #[serde(default)]
        scopes: Vec<String>,
    }
    let v: V = json(&r.body)?;
    let bearer = format!("Bearer {token}");
    let headers = [("Authorization", bearer.as_str()), ("Client-Id", v.client_id.as_str())];
    let full = !v.user_id.is_empty() && v.scopes.iter().any(|s| s == EMOTES_SCOPE);
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..30 {
        let url = if full {
            let mut u = format!(
                "{HELIX}/chat/emotes/user?user_id={}&broadcaster_id={}",
                encode(&v.user_id),
                encode(&req.broadcaster_id)
            );
            if let Some(a) = &after {
                u.push_str(&format!("&after={}", encode(a)));
            }
            u
        } else {
            format!("{HELIX}/chat/emotes/global")
        };
        let r = schwaetz_net::http::get_with(&url, &headers, 4 << 20, false)?;
        match r.status {
            200 => {}
            401 => return Err(UNAUTHORIZED.into()),
            s => return Err(format!("emote request failed (HTTP {s})")),
        }
        let (page, cursor) = parse_emotes(&r.body, &req.broadcaster_id)?;
        out.extend(page);
        match cursor {
            Some(c) if full => after = Some(c),
            _ => break,
        }
    }
    Ok((out, !full))
}

/// Parses a Helix emote list (user or global emotes) and its pagination cursor.
pub fn parse_emotes(
    body: &[u8],
    broadcaster_id: &str,
) -> Result<(Vec<crate::emotes::EmoteEntry>, Option<String>), String> {
    #[derive(Deserialize)]
    struct E {
        id: String,
        name: String,
        #[serde(default)]
        owner_id: String,
    }
    #[derive(Deserialize, Default)]
    struct P {
        #[serde(default)]
        cursor: Option<String>,
    }
    #[derive(Deserialize)]
    struct R {
        data: Vec<E>,
        #[serde(default)]
        pagination: P,
    }
    let r: R = json(body)?;
    let list = r
        .data
        .into_iter()
        .map(|e| crate::emotes::EmoteEntry {
            url: format!("https://static-cdn.jtvnw.net/emoticons/v2/{}/default/dark/1.0", e.id),
            channel: !broadcaster_id.is_empty() && e.owner_id == broadcaster_id,
            name: e.name,
            provider: "twitch".into(),
        })
        .collect();
    Ok((list, r.pagination.cursor.filter(|c| !c.is_empty())))
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
    fn parses_emote_lists() {
        let body = br#"{"data":[
            {"id":"emotesv2_1","name":"xqcL","emote_type":"subscriptions","emote_set_id":"1","owner_id":"71092938","format":["static"],"scale":["1.0"],"theme_mode":["dark"]},
            {"id":"25","name":"Kappa","emote_type":"globals","emote_set_id":"0","owner_id":"0"}],
            "template":"https://static-cdn.jtvnw.net/emoticons/v2/{{id}}/{{format}}/{{theme_mode}}/{{scale}}",
            "pagination":{"cursor":"next-page"}}"#;
        let (list, cursor) = parse_emotes(body, "71092938").unwrap();
        assert_eq!(cursor.as_deref(), Some("next-page"));
        assert_eq!((list[0].name.as_str(), list[0].channel), ("xqcL", true));
        assert_eq!((list[1].name.as_str(), list[1].channel), ("Kappa", false));
        assert_eq!(list[1].url, "https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/1.0");
        let (_, none) = parse_emotes(br#"{"data":[],"pagination":{}}"#, "1").unwrap();
        assert_eq!(none, None);
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
