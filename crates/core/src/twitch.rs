//! Twitch chat helpers: tags → display metadata.

use crate::buffer::{Emote, LineExtra};
use schwaetz_proto::Tags;

/// Parses a `#RRGGBB` color tag.
pub fn parse_color(v: &str) -> Option<u32> {
    let hex = v.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

pub fn emote_url(id: &str, dark: bool) -> String {
    format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/{}/2.0", if dark { "dark" } else { "light" })
}

/// Converts the `emotes` tag (`id:start-end,start-end/id2:…`, code-point indices, inclusive) into
/// byte ranges of `text`.
pub fn parse_emotes(tag: &str, text: &str) -> Vec<Emote> {
    if tag.is_empty() {
        return Vec::new();
    }
    // Map code-point index → byte offset.
    let offsets: Vec<usize> = text.char_indices().map(|(i, _)| i).chain(std::iter::once(text.len())).collect();
    let mut out = Vec::new();
    for group in tag.split('/') {
        let Some((id, ranges)) = group.split_once(':') else { continue };
        for range in ranges.split(',') {
            let Some((a, b)) = range.split_once('-') else { continue };
            let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) else { continue };
            if a > b || b + 1 > offsets.len() {
                continue;
            }
            let (Some(&start), Some(&end)) = (offsets.get(a), offsets.get(b + 1)) else { continue };
            out.push(Emote {
                start: start as u32,
                end: end as u32,
                url: emote_url(id, true),
                name: text[start..end].to_owned(),
            });
        }
    }
    out.sort_by_key(|e| e.start);
    out
}

/// Fills Twitch-derived display metadata from message tags.
pub fn apply_tags(extra: &mut LineExtra, tags: &Tags, text: &str, nick: &str) {
    if let Some(dn) = tags.value("display-name")
        && !dn.eq_ignore_ascii_case(nick)
    {
        // Localized display names (e.g. CJK) differ from the login; show both.
        extra.display_name = Some(format!("{dn} ({nick})"));
    } else if let Some(dn) = tags.value("display-name") {
        extra.display_name = Some(dn.to_owned());
    }
    extra.color = tags.value("color").and_then(parse_color);
    if let Some(b) = tags.value("badges") {
        extra.badges = b.split(',').filter(|s| !s.is_empty()).map(str::to_owned).collect();
    }
    if let Some(info) = tags.value("badge-info") {
        extra.sub_months = info.split(',').find_map(|b| match b.split_once('/')? {
            ("subscriber" | "founder", n) => n.parse().ok(),
            _ => None,
        });
    }
    if let Some(e) = tags.value("emotes") {
        extra.emotes = parse_emotes(e, text);
    }
    if let Some(parent) = tags.value("reply-parent-msg-id") {
        extra.reply_to = Some((
            parent.to_owned(),
            tags.value("reply-parent-display-name").or(tags.value("reply-parent-user-login")).unwrap_or("").to_owned(),
            tags.value("reply-parent-msg-body").unwrap_or("").to_owned(),
        ));
    }
}

/// Short human label for a badge id like `subscriber/12`.
pub fn badge_label(badge: &str) -> &str {
    match badge.split('/').next().unwrap_or(badge) {
        "broadcaster" => "📺",
        "moderator" => "🗡",
        "vip" => "💎",
        "staff" | "admin" | "global_mod" => "🔧",
        "subscriber" | "founder" => "★",
        "partner" | "verified" => "✔",
        "bits" => "◆",
        "turbo" | "premium" => "⚡",
        _ => "",
    }
}

/// Readable summary of ROOMSTATE for the topic bar.
pub fn room_state_summary(state: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (k, v) in state {
        match (k.as_str(), v.as_str()) {
            ("emote-only", "1") => out.push("emote-only".into()),
            ("subs-only", "1") => out.push("subs-only".into()),
            ("r9k", "1") => out.push("unique-chat".into()),
            ("slow", s) if s != "0" && !s.is_empty() => out.push(format!("slow {s}s")),
            ("followers-only", f) if f != "-1" && !f.is_empty() => {
                out.push(if f == "0" { "followers-only".into() } else { format!("followers-only {f}m") })
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emotes_code_points() {
        let text = "héllo Kappa and Kappa 😀 LUL";
        let e = parse_emotes("25:6-10,16-20/425618:24-26", text);
        assert_eq!(e.len(), 3);
        assert_eq!(&text[e[0].start as usize..e[0].end as usize], "Kappa");
        assert_eq!(&text[e[1].start as usize..e[1].end as usize], "Kappa");
        assert_eq!(&text[e[2].start as usize..e[2].end as usize], "LUL");
        assert!(parse_emotes("1:0-99", "short").is_empty());
    }

    #[test]
    fn colors_and_state() {
        assert_eq!(parse_color("#1E90FF"), Some(0x1e90ff));
        assert_eq!(parse_color(""), None);
        let s = vec![("slow".to_string(), "30".to_string()), ("followers-only".into(), "-1".into())];
        assert_eq!(room_state_summary(&s), vec!["slow 30s"]);
    }

    #[test]
    fn badges_and_months() {
        let tags = Tags::parse("badge-info=subscriber/14;badges=subscriber/12,premium/1");
        let mut extra = LineExtra::default();
        apply_tags(&mut extra, &tags, "hi", "u");
        assert_eq!(extra.badges, ["subscriber/12", "premium/1"]);
        assert_eq!(extra.sub_months, Some(14));
        let mut extra = LineExtra::default();
        apply_tags(&mut extra, &Tags::parse("badge-info=;badges=vip/1"), "hi", "u");
        assert_eq!(extra.sub_months, None);
    }
}
