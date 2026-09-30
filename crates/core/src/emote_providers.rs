//! 7TV, BetterTTV and FrankerFaceZ emotes for Twitch channels, from the providers' public APIs:
//! each has a global set and a set per channel (looked up by the channel's Twitch user id).

use serde::Deserialize;
use std::collections::HashMap;

/// Emote names and their image URLs.
pub type NamedUrls = Vec<(String, String)>;

/// A third-party emote provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Provider {
    SevenTv,
    Ffz,
    Bttv,
}

impl Provider {
    /// All providers, in the order a word is looked up in.
    pub const ALL: [Provider; 3] = [Provider::SevenTv, Provider::Ffz, Provider::Bttv];

    /// Short id ("7tv", "ffz", "bttv"), as scripts name providers.
    pub fn id(self) -> &'static str {
        match self {
            Provider::SevenTv => "7tv",
            Provider::Ffz => "ffz",
            Provider::Bttv => "bttv",
        }
    }

    pub fn from_id(id: &str) -> Option<Provider> {
        Provider::ALL.into_iter().find(|p| p.id().eq_ignore_ascii_case(id))
    }

    /// Short name, as in the completion list ("7TV", "FFZ", "BTTV").
    pub fn short_name(self) -> &'static str {
        match self {
            Provider::SevenTv => "7TV",
            Provider::Ffz => "FFZ",
            Provider::Bttv => "BTTV",
        }
    }

    /// The network setting that switches the provider on or off, and its caption.
    pub fn setting(self) -> (&'static str, &'static str) {
        match self {
            Provider::SevenTv => ("emotes_7tv", "7TV emotes"),
            Provider::Ffz => ("emotes_ffz", "FrankerFaceZ emotes"),
            Provider::Bttv => ("emotes_bttv", "BetterTTV emotes"),
        }
    }

    /// Fetches the global set (`room_id` = None) or a channel's (blocking). A channel without an
    /// account with the provider has none.
    pub fn fetch(self, room_id: Option<&str>) -> Result<NamedUrls, String> {
        let r = schwaetz_net::http::get(&self.url(room_id), 4 << 20, false)?;
        match r.status {
            200 => self.parse(room_id.is_some(), &r.body),
            404 => Ok(Vec::new()),
            s => Err(format!("{} request failed (HTTP {s})", self.id())),
        }
    }

    /// Where the global set (`room_id` = None) or a channel's set lives.
    fn url(self, room_id: Option<&str>) -> String {
        let id = room_id.map(crate::helix::encode);
        match (self, id) {
            (Provider::SevenTv, None) => "https://7tv.io/v3/emote-sets/global".into(),
            (Provider::SevenTv, Some(id)) => format!("https://7tv.io/v3/users/twitch/{id}"),
            (Provider::Bttv, None) => "https://api.betterttv.net/3/cached/emotes/global".into(),
            (Provider::Bttv, Some(id)) => format!("https://api.betterttv.net/3/cached/users/twitch/{id}"),
            (Provider::Ffz, None) => "https://api.frankerfacez.com/v1/set/global".into(),
            (Provider::Ffz, Some(id)) => format!("https://api.frankerfacez.com/v1/room/id/{id}"),
        }
    }

    /// Parses a global (`channel` = false) or channel response into (name, image URL) pairs.
    pub fn parse(self, channel: bool, body: &[u8]) -> Result<NamedUrls, String> {
        let bad = |e: serde_json::Error| format!("unexpected {} response: {e}", self.id());
        match (self, channel) {
            (Provider::SevenTv, false) => Ok(seven_tv(serde_json::from_slice::<SevenTvSet>(body).map_err(bad)?)),
            (Provider::SevenTv, true) => {
                #[derive(Deserialize)]
                struct User {
                    #[serde(default)]
                    emote_set: Option<SevenTvSet>,
                }
                let u: User = serde_json::from_slice(body).map_err(bad)?;
                Ok(u.emote_set.map(seven_tv).unwrap_or_default())
            }
            (Provider::Bttv, false) => Ok(bttv(serde_json::from_slice::<Vec<BttvEmote>>(body).map_err(bad)?)),
            (Provider::Bttv, true) => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct User {
                    #[serde(default)]
                    channel_emotes: Vec<BttvEmote>,
                    #[serde(default)]
                    shared_emotes: Vec<BttvEmote>,
                }
                let u: User = serde_json::from_slice(body).map_err(bad)?;
                Ok(bttv(u.channel_emotes.into_iter().chain(u.shared_emotes).collect()))
            }
            (Provider::Ffz, _) => {
                #[derive(Deserialize)]
                struct Sets {
                    /// Global response: the sets everyone may use (`sets` also holds ones only
                    /// some users may, e.g. for FFZ supporters).
                    #[serde(default)]
                    default_sets: Option<Vec<u64>>,
                    #[serde(default)]
                    sets: HashMap<String, FfzSet>,
                }
                let s: Sets = serde_json::from_slice(body).map_err(bad)?;
                let mut sets: Vec<(u64, FfzSet)> =
                    s.sets.into_iter().filter_map(|(id, set)| Some((id.parse().ok()?, set))).collect();
                if let Some(only) = s.default_sets.filter(|_| !channel) {
                    sets.retain(|(id, _)| only.contains(id));
                }
                sets.sort_by_key(|(id, _)| *id);
                Ok(sets.into_iter().flat_map(|(_, set)| ffz(set)).collect())
            }
        }
    }
}

#[derive(Deserialize)]
struct SevenTvSet {
    #[serde(default)]
    emotes: Vec<SevenTvEmote>,
}

#[derive(Deserialize)]
struct SevenTvEmote {
    /// The name in this set (channels can rename emotes).
    name: String,
    #[serde(default)]
    data: Option<SevenTvData>,
}

#[derive(Deserialize)]
struct SevenTvData {
    host: SevenTvHost,
}

#[derive(Deserialize)]
struct SevenTvHost {
    /// `//cdn.7tv.app/emote/<id>`
    url: String,
}

fn seven_tv(set: SevenTvSet) -> NamedUrls {
    set.emotes
        .into_iter()
        .filter_map(|e| {
            let base = e.data?.host.url;
            let base = if base.starts_with("//") { format!("https:{base}") } else { base };
            Some((e.name, format!("{base}/2x.webp")))
        })
        .collect()
}

#[derive(Deserialize)]
struct BttvEmote {
    id: String,
    code: String,
}

fn bttv(list: Vec<BttvEmote>) -> NamedUrls {
    list.into_iter().map(|e| (e.code, format!("https://cdn.betterttv.net/emote/{}/2x", e.id))).collect()
}

#[derive(Deserialize)]
struct FfzSet {
    #[serde(default)]
    emoticons: Vec<FfzEmote>,
}

#[derive(Deserialize)]
struct FfzEmote {
    name: String,
    /// Scale ("1", "2", "4") → URL.
    #[serde(default)]
    urls: HashMap<String, String>,
}

fn ffz(set: FfzSet) -> NamedUrls {
    set.emoticons
        .into_iter()
        .filter_map(|mut e| {
            let url = e.urls.remove("2").or_else(|| e.urls.remove("1"))?;
            let url = if url.starts_with("//") { format!("https:{url}") } else { url };
            Some((e.name, url))
        })
        .collect()
}

/// The URL of the largest size an emote's CDN offers (Twitch 3.0, 7TV 4x, BTTV 3x, FFZ 4), for
/// the hover preview. Other URLs are returned unchanged. The size may not exist (FFZ emotes
/// without a 4x image), so callers keep the original as a fallback.
pub fn largest_emote_url(url: &str) -> String {
    const SIZES: [(&str, &[&str], &str); 4] = [
        ("https://static-cdn.jtvnw.net/emoticons/v2/", &["/1.0", "/2.0"], "/3.0"),
        ("https://cdn.7tv.app/emote/", &["/1x.webp", "/2x.webp", "/3x.webp"], "/4x.webp"),
        ("https://cdn.betterttv.net/emote/", &["/1x", "/2x"], "/3x"),
        ("https://cdn.frankerfacez.com/emote/", &["/1", "/2"], "/4"),
    ];
    for (prefix, smaller, largest) in SIZES {
        if url.starts_with(prefix)
            && let Some(base) = smaller.iter().find_map(|s| url.strip_suffix(s))
        {
            return format!("{base}{largest}");
        }
    }
    url.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn largest_emote_urls() {
        for (small, large) in [
            (
                "https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/2.0",
                "https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/3.0",
            ),
            ("https://cdn.7tv.app/emote/1/2x.webp", "https://cdn.7tv.app/emote/1/4x.webp"),
            (
                "https://cdn.betterttv.net/emote/54fa8f1401e468494b85b537/2x",
                "https://cdn.betterttv.net/emote/54fa8f1401e468494b85b537/3x",
            ),
            ("https://cdn.frankerfacez.com/emote/1/2", "https://cdn.frankerfacez.com/emote/1/4"),
            ("https://cdn.frankerfacez.com/emote/1/1", "https://cdn.frankerfacez.com/emote/1/4"),
            ("https://static-cdn.jtvnw.net/badges/v1/5d9f2208/3", "https://static-cdn.jtvnw.net/badges/v1/5d9f2208/3"),
            ("https://example.com/emote/2x", "https://example.com/emote/2x"),
        ] {
            assert_eq!(largest_emote_url(small), large);
        }
    }

    #[test]
    fn requests_each_providers_endpoints() {
        let urls: Vec<_> = Provider::ALL.into_iter().flat_map(|p| [p.url(None), p.url(Some("71092938"))]).collect();
        assert_eq!(
            urls,
            [
                "https://7tv.io/v3/emote-sets/global",
                "https://7tv.io/v3/users/twitch/71092938",
                "https://api.frankerfacez.com/v1/set/global",
                "https://api.frankerfacez.com/v1/room/id/71092938",
                "https://api.betterttv.net/3/cached/emotes/global",
                "https://api.betterttv.net/3/cached/users/twitch/71092938",
            ]
        );
        assert_eq!(Provider::SevenTv.url(Some("a b")), "https://7tv.io/v3/users/twitch/a%20b", "ids are encoded");
        for p in Provider::ALL {
            assert_eq!(Provider::from_id(p.id()), Some(p));
            assert!(p.setting().0.starts_with("emotes_"));
        }
    }

    #[test]
    fn parses_provider_responses() {
        let seven = br#"{"id":"g","emotes":[
            {"id":"1","name":"peepoHey","data":{"name":"peepoHey","host":{"url":"//cdn.7tv.app/emote/1","files":[]}}},
            {"id":"2","name":"broken","data":null}]}"#;
        assert_eq!(
            Provider::SevenTv.parse(false, seven).unwrap(),
            [("peepoHey".to_owned(), "https://cdn.7tv.app/emote/1/2x.webp".to_owned())]
        );
        let seven_user = br#"{"id":"u","emote_set":{"emotes":[
            {"name":"Renamed","data":{"host":{"url":"//cdn.7tv.app/emote/9"}}}]}}"#;
        assert_eq!(Provider::SevenTv.parse(true, seven_user).unwrap()[0].0, "Renamed");
        assert!(Provider::SevenTv.parse(true, br#"{"id":"u","emote_set":null}"#).unwrap().is_empty());

        let bttv_global = br#"[{"id":"54fa8f1401e468494b85b537","code":":tf:","imageType":"png"}]"#;
        assert_eq!(
            Provider::Bttv.parse(false, bttv_global).unwrap(),
            [(":tf:".to_owned(), "https://cdn.betterttv.net/emote/54fa8f1401e468494b85b537/2x".to_owned())]
        );
        let bttv_user = br#"{"id":"x","channelEmotes":[{"id":"a","code":"A"}],"sharedEmotes":[{"id":"b","code":"B"}]}"#;
        let names: Vec<_> = Provider::Bttv.parse(true, bttv_user).unwrap().into_iter().map(|e| e.0).collect();
        assert_eq!(names, ["A", "B"]);

        let ffz_global = br#"{"default_sets":[3],"sets":{
            "3":{"id":3,"emoticons":[{"name":"ZreknarF","urls":{"1":"https://cdn.frankerfacez.com/emote/1/1","2":"https://cdn.frankerfacez.com/emote/1/2"}}]},
            "1532818":{"id":1532818,"emoticons":[{"name":"Supporters","urls":{"1":"//cdn.frankerfacez.com/x"}}]}}}"#;
        assert_eq!(
            Provider::Ffz.parse(false, ffz_global).unwrap(),
            [("ZreknarF".to_owned(), "https://cdn.frankerfacez.com/emote/1/2".to_owned())],
            "only the default sets"
        );
        let ffz_room =
            br#"{"room":{},"sets":{"7":{"emoticons":[{"name":"Room","urls":{"1":"//cdn.frankerfacez.com/r"}}]}}}"#;
        assert_eq!(
            Provider::Ffz.parse(true, ffz_room).unwrap(),
            [("Room".to_owned(), "https://cdn.frankerfacez.com/r".to_owned())]
        );
        assert!(Provider::Ffz.parse(false, b"<html>").is_err());
    }
}
