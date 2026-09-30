//! Twitch chat badges as the images Twitch shows. A message's `badges` tag only names them
//! (`subscriber/12`); the images come from the Helix API: a global list (moderator, VIP …) and
//! each channel's own subscriber and bits badges. Lists are kept on disk for [`MAX_AGE_MS`]; the
//! images themselves never change (new artwork gets a new URL) and live in the media cache.

use crate::helix::BadgeImage;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// How long a fetched badge list is used before it is fetched again.
pub const MAX_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Badge (`set/version`) → image.
pub type BadgeMap = HashMap<String, BadgeImage>;

/// A badge list to load: the global one (`room_id` = None) or a channel's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BadgeRequest {
    pub room_id: Option<String>,
    /// Without a token only the disk cache is read.
    pub token: Option<String>,
    /// The client ID the token belongs to, if known.
    pub client_id: Option<String>,
    /// Fetch from Twitch even if the cached list is recent (a badge was missing from it).
    pub refresh: bool,
}

#[derive(Clone, Debug)]
pub struct BadgeResult {
    pub room_id: Option<String>,
    /// None: neither Twitch nor the disk cache had it.
    pub badges: Option<BadgeMap>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheFile {
    /// Unix ms.
    fetched_at: i64,
    badges: BadgeMap,
}

fn cache_file(dir: &Path, room_id: Option<&str>) -> PathBuf {
    match room_id {
        Some(id) => {
            let id: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            dir.join(format!("channel-{id}.json"))
        }
        None => dir.join("global.json"),
    }
}

/// Loads a badge list (blocking): from the cache in `dir` while it is younger than
/// [`MAX_AGE_MS`] (and no refresh was asked for), else from Twitch (writing the cache). If that
/// is not possible, an older cached list will do.
pub fn fetch(req: BadgeRequest, dir: Option<&Path>, now: i64) -> BadgeResult {
    let file = dir.map(|d| cache_file(d, req.room_id.as_deref()));
    let cached: Option<CacheFile> =
        file.as_ref().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice(&b).ok());
    let room_id = req.room_id.clone();
    if let Some(c) = cached.as_ref().filter(|c| !req.refresh && now - c.fetched_at < MAX_AGE_MS) {
        return BadgeResult { room_id, badges: Some(c.badges.clone()) };
    }
    if let Some(token) = &req.token {
        match crate::helix::chat_badges(token, req.client_id.as_deref(), req.room_id.as_deref()) {
            Ok(list) => {
                let badges: BadgeMap = list.into_iter().collect();
                if let (Some(f), Some(d)) = (&file, dir) {
                    let data = CacheFile { fetched_at: now, badges };
                    let _ = std::fs::create_dir_all(d);
                    if let Ok(json) = serde_json::to_vec(&data) {
                        let _ = std::fs::write(f, json);
                    }
                    return BadgeResult { room_id, badges: Some(data.badges) };
                }
                return BadgeResult { room_id, badges: Some(badges) };
            }
            Err(e) => tracing::warn!("badges ({}): {e}", req.room_id.as_deref().unwrap_or("global")),
        }
    }
    BadgeResult { room_id, badges: cached.map(|c| c.badges) }
}

/// Badge sets a channel can have its own images for; the others are the same everywhere.
fn channel_set(badge: &str) -> bool {
    matches!(badge.split('/').next(), Some("subscriber" | "bits"))
}

/// The badge lists loaded so far (shared by all Twitch networks).
#[derive(Default)]
pub struct Badges {
    global: BadgeMap,
    channels: HashMap<String, BadgeMap>,
    /// Lists asked for (and not failed), by room id (None: global).
    requested: HashSet<Option<String>>,
    /// Lists that arrived.
    loaded: HashSet<Option<String>>,
    /// Lists fetched again this run because a badge was missing from them.
    refreshed: HashSet<Option<String>>,
}

impl Badges {
    /// The image of a badge in a channel (`room_id`): the channel's own, else the global one.
    pub fn get(&self, room_id: Option<&str>, badge: &str) -> Option<&BadgeImage> {
        room_id.and_then(|r| self.channels.get(r)).and_then(|m| m.get(badge)).or_else(|| self.global.get(badge))
    }

    /// Whether a list should be asked for (marks it as asked).
    pub(crate) fn start(&mut self, room_id: Option<&str>) -> bool {
        self.requested.insert(room_id.map(str::to_owned))
    }

    /// The list to fetch again for a badge that neither the channel's nor the global list has,
    /// once both are loaded; each list at most once per run.
    pub(crate) fn refresh_for(&mut self, room_id: Option<&str>, badge: &str) -> Option<Option<String>> {
        let room = room_id.map(str::to_owned);
        if !self.loaded.contains(&None) || (room.is_some() && !self.loaded.contains(&room)) {
            return None;
        }
        if self.get(room_id, badge).is_some() {
            return None;
        }
        let scope = if channel_set(badge) && room.is_some() { room } else { None };
        self.refreshed.insert(scope.clone()).then_some(scope)
    }

    /// Takes a loaded list; a failed one may be asked for again.
    pub(crate) fn insert(&mut self, r: BadgeResult) {
        let Some(badges) = r.badges else {
            self.requested.remove(&r.room_id);
            return;
        };
        self.loaded.insert(r.room_id.clone());
        match r.room_id {
            Some(room) => {
                self.channels.insert(room, badges);
            }
            None => self.global = badges,
        }
    }

    /// Forgets which lists failed to load, so they are asked for again (e.g. after signing in).
    pub(crate) fn retry_missing(&mut self) {
        let loaded = &self.loaded;
        self.requested.retain(|r| loaded.contains(r));
    }
}

/// The badge images of one buffer's lines.
#[derive(Clone, Copy)]
pub struct Lookup<'a> {
    pub badges: &'a Badges,
    /// The channel (None: whispers, the server buffer …).
    pub room_id: Option<&'a str>,
}

impl<'a> Lookup<'a> {
    pub fn get(&self, badge: &str) -> Option<&'a BadgeImage> {
        self.badges.get(self.room_id, badge)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(title: &str) -> BadgeImage {
        BadgeImage { title: title.into(), urls: [format!("{title}/1"), format!("{title}/2"), format!("{title}/3")] }
    }

    fn map(pairs: &[(&str, &str)]) -> Option<BadgeMap> {
        Some(pairs.iter().map(|(id, t)| (id.to_string(), img(t))).collect())
    }

    #[test]
    fn channel_badges_before_global_ones() {
        let mut b = Badges::default();
        b.insert(BadgeResult { room_id: None, badges: map(&[("subscriber/0", "global sub"), ("vip/1", "VIP")]) });
        b.insert(BadgeResult { room_id: Some("7".into()), badges: map(&[("subscriber/0", "chan sub")]) });
        assert_eq!(b.get(Some("7"), "subscriber/0").unwrap().title, "chan sub");
        assert_eq!(b.get(Some("7"), "vip/1").unwrap().title, "VIP");
        assert_eq!(b.get(Some("8"), "subscriber/0").unwrap().title, "global sub");
        assert_eq!(b.get(None, "subscriber/0").unwrap().title, "global sub");
        assert!(b.get(Some("7"), "moderator/1").is_none());
    }

    #[test]
    fn missing_badges_refresh_their_list_once() {
        let mut b = Badges::default();
        assert_eq!(b.refresh_for(Some("7"), "subscriber/24"), None, "nothing loaded yet");
        b.insert(BadgeResult { room_id: None, badges: map(&[("vip/1", "VIP")]) });
        assert_eq!(b.refresh_for(Some("7"), "subscriber/24"), None, "the channel's list is not loaded yet");
        b.insert(BadgeResult { room_id: Some("7".into()), badges: map(&[("subscriber/0", "sub")]) });
        assert_eq!(b.refresh_for(Some("7"), "vip/1"), None, "known");
        assert_eq!(b.refresh_for(Some("7"), "subscriber/24"), Some(Some("7".into())));
        assert_eq!(b.refresh_for(Some("7"), "bits/5000"), None, "once per run");
        assert_eq!(b.refresh_for(Some("7"), "newevent/1"), Some(None), "others are global");
        assert_eq!(b.refresh_for(None, "newevent/2"), None);
    }

    #[test]
    fn failed_lists_are_asked_for_again() {
        let mut b = Badges::default();
        assert!(b.start(None));
        assert!(!b.start(None));
        b.insert(BadgeResult { room_id: None, badges: None });
        assert!(b.start(None));
        assert!(b.start(Some("7")));
        b.insert(BadgeResult { room_id: Some("7".into()), badges: map(&[]) });
        b.retry_missing();
        assert!(b.start(None), "not loaded");
        assert!(!b.start(Some("7")), "loaded");
    }

    #[test]
    fn cache_is_used_while_recent() {
        let dir = std::env::temp_dir().join(format!("schwaetz-badges-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data = CacheFile { fetched_at: 1_000, badges: map(&[("subscriber/0", "sub")]).unwrap() };
        std::fs::write(cache_file(&dir, Some("7")), serde_json::to_vec(&data).unwrap()).unwrap();
        let req = |refresh| BadgeRequest { room_id: Some("7".into()), token: None, client_id: None, refresh };

        let r = fetch(req(false), Some(&dir), 1_000 + MAX_AGE_MS - 1);
        assert_eq!(r.badges.unwrap()["subscriber/0"].title, "sub");
        // Expired (or refreshing) without a token to fetch with: the old list still does.
        assert!(fetch(req(false), Some(&dir), 1_000 + MAX_AGE_MS).badges.is_some());
        assert!(fetch(req(true), Some(&dir), 1_000).badges.is_some());
        let other = BadgeRequest { room_id: Some("8".into()), ..req(false) };
        assert!(fetch(other, Some(&dir), 1_000).badges.is_none());
        assert_eq!(cache_file(&dir, Some("../x")), dir.join("channel-x.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
