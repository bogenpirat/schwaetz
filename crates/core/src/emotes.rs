//! Emote completion: the emotes that can be typed in a Twitch channel, and their order.
//!
//! Twitch emotes come from the Helix API (the ones the signed-in user may use in that channel:
//! follower, subscriber tiers, globals …); 7TV/FrankerFaceZ/BetterTTV emotes are handed over by
//! scripts (`schwaetz.setEmotes`).

/// One completion candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmoteEntry {
    pub name: String,
    pub url: String,
    /// "twitch", "7tv", "ffz", "bttv" or whatever a script calls its source.
    pub provider: String,
    /// Specific to the channel (the channel's own Twitch emotes, its 7TV/FFZ/BTTV sets) rather
    /// than available everywhere.
    pub channel: bool,
}

impl EmoteEntry {
    /// Display order: the channel's Twitch emotes, then the channel's 7TV, FFZ and BTTV emotes,
    /// then everything global (Twitch, 7TV, FFZ, BTTV).
    pub fn group(&self) -> u8 {
        let provider = match self.provider.as_str() {
            "twitch" => 0,
            "7tv" => 1,
            "ffz" => 2,
            "bttv" => 3,
            _ => 4,
        };
        if self.channel { provider } else { 10 + provider }
    }

    /// Short source label for the list ("Twitch", "7TV", …; global ones marked as such).
    pub fn label(&self) -> String {
        let p = match self.provider.as_str() {
            "twitch" => "Twitch",
            "7tv" => "7TV",
            "ffz" => "FFZ",
            "bttv" => "BTTV",
            other => other,
        };
        if self.channel { p.to_owned() } else { format!("{p} · global") }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, provider: &str, channel: bool) -> EmoteEntry {
        EmoteEntry { name: name.into(), url: format!("u/{name}"), provider: provider.into(), channel }
    }

    #[test]
    fn channel_first_then_third_party_then_global() {
        let all = [
            e("LUL", "twitch", false),
            e("xqcL", "twitch", true),
            e("xqcLUL", "twitch", true),
            e("LULW", "7tv", true),
            e("lulWait", "bttv", true),
            e("LULE", "ffz", false),
            e("OMEGALUL", "7tv", false),
            e("LULW", "bttv", false),
        ];
        let names: Vec<String> = complete(all.iter(), "lul", 50).into_iter().map(|e| e.name).collect();
        // Prefix matches (channel Twitch, 7TV, BTTV, then global Twitch and FFZ), then contains.
        assert_eq!(names, ["LULW", "lulWait", "LUL", "LULE", "xqcLUL", "OMEGALUL"]);
        let x: Vec<String> = complete(all.iter(), "xqc", 50).into_iter().map(|e| e.name).collect();
        assert_eq!(x, ["xqcL", "xqcLUL"]);
        assert_eq!(complete(all.iter(), "lulw", 50)[0].provider, "7tv", "duplicates keep the better source");
        assert_eq!(e("A", "7tv", false).label(), "7TV · global");
    }
}
