//! Emotes beyond those Twitch marks in messages: what can be typed in a Twitch channel (`:`
//! completion) and which words show as images.
//!
//! Sources: Twitch's API (the ones the signed-in user may use: follower, subscriber tiers,
//! globals …), 7TV/FrankerFaceZ/BetterTTV (`emote_providers`), and scripts
//! (`schwaetz.setEmotes`). Everything but the scripts' sets is fetched per connection, see
//! [`Job`].

use crate::emote_providers::{NamedUrls, Provider};
use schwaetz_net::NetworkId;
use std::collections::{BTreeMap, HashMap};

/// Where an emote comes from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Twitch,
    Provider(Provider),
    /// A script's set, named as the script called it.
    Script(String),
}

impl Source {
    /// A script names its set; one named after a provider ("7tv" …) counts as that provider's.
    pub fn from_name(name: &str) -> Source {
        Provider::from_id(name).map_or_else(|| Source::Script(name.to_owned()), Source::Provider)
    }

    /// Completion order: Twitch, then the providers (in [`Provider::ALL`] order), then scripts.
    fn order(&self) -> u8 {
        match self {
            Source::Twitch => 0,
            Source::Provider(p) => 1 + Provider::ALL.iter().position(|x| x == p).unwrap_or(0) as u8,
            Source::Script(_) => 1 + Provider::ALL.len() as u8,
        }
    }

    fn name(&self) -> &str {
        match self {
            Source::Twitch => "Twitch",
            Source::Provider(p) => p.short_name(),
            Source::Script(name) => name,
        }
    }
}

/// One completion candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmoteEntry {
    pub name: String,
    pub url: String,
    pub source: Source,
    /// Specific to the channel (the channel's own Twitch emotes, its 7TV/FFZ/BTTV sets) rather
    /// than available everywhere.
    pub channel: bool,
}

impl EmoteEntry {
    /// Display order: the channel's Twitch emotes, then the channel's 7TV, FFZ and BTTV emotes,
    /// then everything global (Twitch, 7TV, FFZ, BTTV).
    pub fn group(&self) -> u8 {
        self.source.order() + if self.channel { 0 } else { 10 }
    }

    /// Short source label for the list ("Twitch", "7TV", …; global ones marked as such).
    pub fn label(&self) -> String {
        let name = self.source.name();
        if self.channel { name.to_owned() } else { format!("{name} · global") }
    }
}

/// Emotes matching `query`, best first: names starting with it before names containing it (case
/// insensitive), each in source order, then alphabetically. Duplicate names keep the first
/// (highest-ranked) source.
pub fn complete<'a>(candidates: impl Iterator<Item = &'a EmoteEntry>, query: &str, limit: usize) -> Vec<EmoteEntry> {
    let q = query.to_lowercase();
    let mut hits: Vec<(u8, u8, String, &EmoteEntry)> = candidates
        .filter_map(|e| {
            let name = e.name.to_lowercase();
            let tier = if name.starts_with(&q) {
                0
            } else if name.contains(&q) {
                1
            } else {
                return None;
            };
            Some((tier, e.group(), name, e))
        })
        .collect();
    hits.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    let mut seen = std::collections::HashSet::new();
    hits.into_iter().filter(|h| seen.insert(h.3.name.clone())).take(limit).map(|h| h.3.clone()).collect()
}

/// A set of emotes: name → image URL.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmoteSet {
    pub emotes: HashMap<String, String>,
}

impl EmoteSet {
    /// As completion candidates.
    pub fn entries<'a>(&'a self, source: &'a Source, channel: bool) -> impl Iterator<Item = EmoteEntry> + 'a {
        self.emotes.iter().map(move |(name, url)| EmoteEntry {
            name: name.clone(),
            url: url.clone(),
            source: source.clone(),
            channel,
        })
    }
}

/// Finds the image for a word in a channel's chat, trying the sets in order.
#[derive(Default)]
pub struct Lookup<'a> {
    /// Checked first in the user's own lines: Twitch emotes (everyone else's messages mark theirs
    /// with tags; ours come back without).
    pub(crate) own: Vec<&'a EmoteSet>,
    pub(crate) sets: Vec<&'a EmoteSet>,
}

impl<'a> Lookup<'a> {
    fn sets(&self, own: bool) -> impl Iterator<Item = &&'a EmoteSet> {
        self.own.iter().filter(move |_| own).chain(&self.sets)
    }

    /// The image for `word`, in a line of the user's (`own`) or someone else's.
    pub fn get(&self, word: &str, own: bool) -> Option<&'a str> {
        self.sets(own).find_map(|s| s.emotes.get(word)).map(String::as_str)
    }

    /// Byte ranges and image URLs of the words in `text` (split at whitespace) that are emotes.
    pub fn find(&self, text: &str, own: bool) -> Vec<(u32, u32, &'a str)> {
        if self.sets(own).all(|s| s.emotes.is_empty()) {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut start = None;
        for (i, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
            match (c.is_whitespace(), start) {
                (false, None) => start = Some(i),
                (true, Some(s)) => {
                    if let Some(url) = self.get(&text[s..i], own) {
                        out.push((s as u32, i as u32, url));
                    }
                    start = None;
                }
                _ => {}
            }
        }
        out
    }
}

// ----- fetching ----------------------------------------------------------------------------------

/// One emote list to fetch for a Twitch network. Each is fetched once per connection.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Job {
    /// Every Twitch emote the signed-in user may use (in all channels).
    TwitchUser,
    /// A channel's Twitch follower emotes, if the user follows it.
    TwitchFollower { room_id: String },
    /// A provider's global set (`room_id` = None) or a channel's.
    Provider { provider: Provider, room_id: Option<String> },
}

/// Where a fetched set is kept. Channels are identified by their Twitch user id (`room-id`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SetKey {
    /// Twitch emotes the user may use, by the channel they belong to ("0": global ones).
    Twitch { owner: String },
    /// A provider's global set (`room` = None) or a channel's.
    Provider { provider: Provider, room: Option<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmoteRequest {
    pub network: NetworkId,
    /// The connection the request belongs to; results for an earlier one are dropped.
    pub connection: u64,
    /// For the Twitch jobs.
    pub token: Option<String>,
    pub jobs: Vec<Job>,
}

#[derive(Clone, Debug)]
pub struct EmoteResult {
    pub network: NetworkId,
    pub connection: u64,
    /// Emotes to add to the sets (failed jobs are logged and tried again on the next connection).
    pub sets: Vec<(SetKey, NamedUrls)>,
    /// The token lacks the `user:read:emotes` permission: only Twitch's global emotes came.
    pub twitch_limited: bool,
}

/// Runs the jobs of a request (blocking).
pub fn fetch(req: EmoteRequest) -> EmoteResult {
    let mut out =
        EmoteResult { network: req.network, connection: req.connection, sets: Vec::new(), twitch_limited: false };
    for job in &req.jobs {
        let sets = match job {
            Job::TwitchUser | Job::TwitchFollower { .. } => {
                let Some(token) = &req.token else { continue };
                let follower_of = match job {
                    Job::TwitchFollower { room_id } => Some(room_id.as_str()),
                    _ => None,
                };
                crate::helix::user_emotes(token, follower_of).map(|(list, limited)| {
                    out.twitch_limited |= limited;
                    by_owner(list)
                })
            }
            Job::Provider { provider, room_id } => provider
                .fetch(room_id.as_deref())
                .map(|list| vec![(SetKey::Provider { provider: *provider, room: room_id.clone() }, list)]),
        };
        match sets {
            Ok(sets) => out.sets.extend(sets),
            Err(e) => tracing::warn!("emotes ({job:?}): {e}"),
        }
    }
    out
}

/// Twitch emotes, grouped by the channel they belong to.
fn by_owner(list: Vec<crate::helix::TwitchEmote>) -> Vec<(SetKey, NamedUrls)> {
    let mut sets: BTreeMap<String, NamedUrls> = BTreeMap::new();
    for e in list {
        let url = e.url();
        sets.entry(e.owner_id).or_default().push((e.name, url));
    }
    sets.into_iter().map(|(owner, list)| (SetKey::Twitch { owner }, list)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, source: Source, channel: bool) -> EmoteEntry {
        EmoteEntry { name: name.into(), url: format!("u/{name}"), source, channel }
    }

    #[test]
    fn channel_first_then_third_party_then_global() {
        let (twitch, seven, ffz, bttv) = (
            Source::Twitch,
            Source::Provider(Provider::SevenTv),
            Source::Provider(Provider::Ffz),
            Source::Provider(Provider::Bttv),
        );
        let all = [
            e("LUL", twitch.clone(), false),
            e("xqcL", twitch.clone(), true),
            e("xqcLUL", twitch, true),
            e("LULW", seven.clone(), true),
            e("lulWait", bttv.clone(), true),
            e("LULE", ffz, false),
            e("OMEGALUL", seven.clone(), false),
            e("LULW", bttv, false),
        ];
        let names: Vec<String> = complete(all.iter(), "lul", 50).into_iter().map(|e| e.name).collect();
        // Prefix matches (channel Twitch, 7TV, BTTV, then global Twitch and FFZ), then contains.
        assert_eq!(names, ["LULW", "lulWait", "LUL", "LULE", "xqcLUL", "OMEGALUL"]);
        let x: Vec<String> = complete(all.iter(), "xqc", 50).into_iter().map(|e| e.name).collect();
        assert_eq!(x, ["xqcL", "xqcLUL"]);
        assert_eq!(complete(all.iter(), "lulw", 50)[0].source, seven, "duplicates keep the better source");
        assert_eq!(e("A", seven, false).label(), "7TV · global");
        assert_eq!(Source::from_name("7TV"), Source::Provider(Provider::SevenTv));
        assert_eq!(e("A", Source::from_name("mine"), true).label(), "mine");
    }

    #[test]
    fn looks_up_words() {
        let set = |pairs: &[(&str, &str)]| EmoteSet {
            emotes: pairs.iter().map(|(n, u)| (n.to_string(), u.to_string())).collect(),
        };
        let (twitch, channel, global) =
            (set(&[("Kappa", "twitch")]), set(&[("KEKW", "chan")]), set(&[("KEKW", "global"), ("Clap", "clap")]));
        let l = Lookup { own: vec![&twitch], sets: vec![&channel, &global] };
        assert_eq!(l.get("KEKW", false), Some("chan"), "channel sets first");
        assert_eq!(l.find("so KEKW  Clap, Clap", false), [(3, 7, "chan"), (15, 19, "clap")]);
        assert_eq!(l.get("Kappa", false), None, "others' Twitch emotes come with their messages");
        assert_eq!(l.find("Kappa Clap", true), [(0, 5, "twitch"), (6, 10, "clap")]);
        assert!(Lookup::default().find("KEKW", true).is_empty());
    }

    #[test]
    fn twitch_emotes_by_owner() {
        let t = |id: &str, name: &str, owner: &str| crate::helix::TwitchEmote {
            id: id.into(),
            name: name.into(),
            owner_id: owner.into(),
            emote_type: String::new(),
        };
        let sets = by_owner(vec![t("1", "xqcL", "71092938"), t("25", "Kappa", "0"), t("2", "xqcW", "71092938")]);
        let keys: Vec<_> = sets.iter().map(|(k, l)| (k.clone(), l.len())).collect();
        assert_eq!(keys, [(SetKey::Twitch { owner: "0".into() }, 1), (SetKey::Twitch { owner: "71092938".into() }, 2)]);
        assert_eq!(sets[0].1[0].1, crate::twitch::emote_url("25", true));
    }
}
