//! Emotes beyond those Twitch marks in messages: what can be typed in a Twitch channel (`:`
//! completion) and which words show as images.
//!
//! Sources: Twitch's API (the ones the signed-in user may use: follower, subscriber tiers,
//! globals …), 7TV/FrankerFaceZ/BetterTTV (`emote_providers`: their global and channel sets, and
//! what the user has for themselves), and scripts (`schwaetz.setEmotes`). Everything but the
//! scripts' sets is fetched per connection, see [`Job`].

use crate::emote_providers::{NamedUrls, Provider};
use schwaetz_net::NetworkId;
use std::collections::{BTreeMap, HashMap};

/// Where an emote comes from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// Twitch, with Helix's `emote_type` ("subscriptions", "follower", "globals" …).
    Twitch(String),
    Provider(Provider),
    /// What the user has for themselves with a provider, usable in any channel.
    Personal(Provider),
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
            Source::Twitch(_) => 0,
            Source::Provider(p) | Source::Personal(p) => {
                1 + Provider::ALL.iter().position(|x| x == p).unwrap_or(0) as u8
            }
            Source::Script(_) => 1 + Provider::ALL.len() as u8,
        }
    }

    fn name(&self) -> &str {
        match self {
            Source::Twitch(_) => "Twitch",
            Source::Provider(p) | Source::Personal(p) => p.short_name(),
            Source::Script(name) => name,
        }
    }
}

/// What a Twitch emote is, from its `emote_type`, as shown in the completion list.
fn twitch_kind(emote_type: &str) -> &str {
    match emote_type {
        "subscriptions" => "sub",
        "follower" => "follower",
        "bitstier" => "bits",
        "hypetrain" => "hype train",
        "limitedtime" => "limited",
        "rewards" | "channelpoints" => "reward",
        "prime" => "Prime",
        "turbo" => "Turbo",
        "twofactor" => "2FA",
        "globals" | "smilies" | "" => "global",
        other => other,
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
    /// then the user's personal ones, then everything global (Twitch, 7TV, FFZ, BTTV).
    pub fn group(&self) -> u8 {
        let base = match (&self.source, self.channel) {
            (Source::Personal(_), _) => 10,
            (_, true) => 0,
            (_, false) => 20,
        };
        base + self.source.order()
    }

    /// Short source label for the list: Twitch emotes by kind ("Twitch · sub", "Twitch ·
    /// global" …), others by source, personal and global ones marked as such ("7TV", "7TV ·
    /// personal", "7TV · global").
    pub fn label(&self) -> String {
        let name = self.source.name();
        match &self.source {
            Source::Twitch(emote_type) => format!("{name} · {}", twitch_kind(emote_type)),
            Source::Personal(_) => format!("{name} · personal"),
            _ if self.channel => name.to_owned(),
            _ => format!("{name} · global"),
        }
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

/// One emote list to fetch for a Twitch network, once per connection (and again when it failed or
/// is due for a refresh).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Job {
    /// Every Twitch emote the signed-in user may use (in all channels).
    TwitchUser,
    /// A channel's Twitch follower emotes, if the user follows it.
    TwitchFollower { room_id: String },
    /// A provider's global set (`room_id` = None) or a channel's.
    Provider { provider: Provider, room_id: Option<String> },
    /// What the user (by Twitch user id) has for themselves with a provider.
    ProviderPersonal { provider: Provider, user_id: String },
}

/// Where a fetched set is kept. Channels are identified by their Twitch user id (`room-id`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SetKey {
    /// Twitch emotes the user may use, by the channel they belong to ("0": global ones) and
    /// their `emote_type`.
    Twitch { owner: String, emote_type: String },
    /// A provider's global set (`room` = None) or a channel's.
    Provider { provider: Provider, room: Option<String> },
    /// What the user has for themselves with a provider.
    Personal { provider: Provider },
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
    /// Emotes for the sets: a provider's replace its set, Twitch's are added to theirs.
    pub sets: Vec<(SetKey, NamedUrls)>,
    /// The token lacks the `user:read:emotes` permission: only Twitch's global emotes came.
    pub twitch_limited: bool,
    /// Jobs that failed, to try again. What they got before failing is in `sets`.
    pub failed: Vec<Job>,
    /// Twitch rejected the token (HTTP 401): it needs refreshing.
    pub unauthorized: bool,
}

/// Runs the jobs of a request (blocking).
pub fn fetch(req: EmoteRequest) -> EmoteResult {
    let mut out = EmoteResult {
        network: req.network,
        connection: req.connection,
        sets: Vec::new(),
        twitch_limited: false,
        failed: Vec::new(),
        unauthorized: false,
    };
    for job in req.jobs {
        let result = match &job {
            Job::TwitchUser | Job::TwitchFollower { .. } => {
                let Some(token) = &req.token else { continue };
                let follower_of = match &job {
                    Job::TwitchFollower { room_id } => Some(room_id.as_str()),
                    _ => None,
                };
                let mut list = Vec::new();
                let result = crate::helix::user_emotes(token, follower_of, &mut list);
                out.sets.extend(twitch_sets(list));
                result.map(|limited| out.twitch_limited |= limited)
            }
            Job::Provider { provider, room_id } => provider.fetch(room_id.as_deref()).map(|list| {
                out.sets.push((SetKey::Provider { provider: *provider, room: room_id.clone() }, list));
            }),
            Job::ProviderPersonal { provider, user_id } => provider.fetch_personal(user_id).map(|list| {
                out.sets.push((SetKey::Personal { provider: *provider }, list));
            }),
        };
        if let Err(e) = result {
            tracing::warn!("emotes ({job:?}): {e}");
            out.unauthorized |= e == crate::helix::UNAUTHORIZED;
            out.failed.push(job);
        }
    }
    out
}

/// Twitch emotes, grouped by the channel they belong to and their type.
fn twitch_sets(list: Vec<crate::helix::TwitchEmote>) -> Vec<(SetKey, NamedUrls)> {
    let mut sets: BTreeMap<(String, String), NamedUrls> = BTreeMap::new();
    for e in list {
        let url = e.url();
        sets.entry((e.owner_id, e.emote_type)).or_default().push((e.name, url));
    }
    sets.into_iter().map(|((owner, emote_type), list)| (SetKey::Twitch { owner, emote_type }, list)).collect()
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
            Source::Twitch("subscriptions".into()),
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
        assert_eq!(e("A", Source::Personal(Provider::SevenTv), false).label(), "7TV · personal");
        assert_eq!(Source::from_name("7TV"), Source::Provider(Provider::SevenTv));
        assert_eq!(e("A", Source::from_name("mine"), true).label(), "mine");
    }

    #[test]
    fn personal_emotes_rank_between_the_channels_and_the_global_ones() {
        let all = [
            e("aGlobal", Source::Twitch("globals".into()), false),
            e("aMine", Source::Personal(Provider::SevenTv), false),
            e("aChannel", Source::Provider(Provider::Bttv), true),
        ];
        let names: Vec<String> = complete(all.iter(), "a", 50).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["aChannel", "aMine", "aGlobal"]);
    }

    #[test]
    fn twitch_emotes_are_labelled_by_kind() {
        let label = |emote_type: &str, channel| e("A", Source::Twitch(emote_type.into()), channel).label();
        assert_eq!(label("subscriptions", false), "Twitch · sub");
        assert_eq!(label("subscriptions", true), "Twitch · sub", "the channel's own too");
        assert_eq!(label("follower", true), "Twitch · follower");
        assert_eq!(label("bitstier", false), "Twitch · bits");
        assert_eq!(label("globals", false), "Twitch · global");
        assert_eq!(label("smilies", false), "Twitch · global");
        assert_eq!(label("", false), "Twitch · global", "the global list has no types");
        assert_eq!(label("somethingnew", false), "Twitch · somethingnew");
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
    fn twitch_emotes_by_owner_and_type() {
        let t = |id: &str, name: &str, owner: &str, emote_type: &str| crate::helix::TwitchEmote {
            id: id.into(),
            name: name.into(),
            owner_id: owner.into(),
            emote_type: emote_type.into(),
        };
        let sets = twitch_sets(vec![
            t("1", "xqcL", "71092938", "subscriptions"),
            t("25", "Kappa", "0", "globals"),
            t("2", "xqcW", "71092938", "subscriptions"),
            t("3", "xqcHi", "71092938", "follower"),
        ]);
        let key = |owner: &str, emote_type: &str| SetKey::Twitch { owner: owner.into(), emote_type: emote_type.into() };
        let keys: Vec<_> = sets.iter().map(|(k, l)| (k.clone(), l.len())).collect();
        assert_eq!(
            keys,
            [(key("0", "globals"), 1), (key("71092938", "follower"), 1), (key("71092938", "subscriptions"), 2)]
        );
        assert_eq!(sets[0].1[0].1, crate::twitch::emote_url("25", true));
    }
}
