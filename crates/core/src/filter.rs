//! Highlight and ignore matching.

use crate::config::{Highlight, IgnoreRule};
use regex_lite::{Regex, RegexBuilder};
use schwaetz_proto::{CaseMapping, Source, mask};

fn is_nick_char(c: char) -> bool {
    c.is_alphanumeric() || "_-[]\\`^{}|".contains(c)
}

/// Case-insensitive search for `needle` delimited by non-nick characters.
pub fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let hay = haystack.to_lowercase();
    let needle = needle.to_lowercase();
    let mut from = 0;
    while let Some(i) = hay[from..].find(&needle) {
        let start = from + i;
        let end = start + needle.len();
        let before_ok = hay[..start].chars().next_back().is_none_or(|c| !is_nick_char(c));
        let after_ok = hay[end..].chars().next().is_none_or(|c| !is_nick_char(c));
        if before_ok && after_ok {
            return true;
        }
        from = start + needle.chars().next().map_or(1, char::len_utf8);
    }
    false
}

#[derive(Default)]
pub struct Highlighter {
    nick: bool,
    words: Vec<String>,
    patterns: Vec<Regex>,
    exclude: Vec<String>,
}

impl Highlighter {
    /// Builds the matcher; invalid patterns are skipped and reported.
    pub fn new(cfg: &Highlight) -> (Highlighter, Vec<String>) {
        let mut errors = Vec::new();
        let patterns = cfg
            .patterns
            .iter()
            .filter_map(|p| match RegexBuilder::new(p).case_insensitive(true).build() {
                Ok(r) => Some(r),
                Err(e) => {
                    errors.push(format!("highlight pattern {p:?}: {e}"));
                    None
                }
            })
            .collect();
        let h = Highlighter {
            nick: cfg.nick,
            words: cfg.words.iter().filter(|w| !w.is_empty()).cloned().collect(),
            patterns,
            exclude: cfg.exclude_nicks.iter().map(|m| mask::normalize(m)).collect(),
        };
        (h, errors)
    }

    /// `text` should have formatting codes stripped.
    pub fn matches(&self, text: &str, my_nick: &str, sender: &Source, cm: CaseMapping) -> bool {
        if !self.exclude.is_empty() {
            let full = sender.to_string();
            let full = if full.contains('!') { full } else { format!("{full}!*@*") };
            if self.exclude.iter().any(|m| mask::matches(m, &full, cm)) {
                return false;
            }
        }
        (self.nick && contains_word(text, my_nick))
            || self.words.iter().any(|w| contains_word(text, w))
            || self.patterns.iter().any(|r| r.is_match(text))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IgnoreType {
    Msg,
    Notice,
    Action,
    Ctcp,
    Invite,
    Join,
    Part,
    Quit,
    Nick,
    Tagmsg,
}

impl IgnoreType {
    fn name(self) -> &'static str {
        match self {
            IgnoreType::Msg => "msg",
            IgnoreType::Notice => "notice",
            IgnoreType::Action => "action",
            IgnoreType::Ctcp => "ctcp",
            IgnoreType::Invite => "invite",
            IgnoreType::Join => "join",
            IgnoreType::Part => "part",
            IgnoreType::Quit => "quit",
            IgnoreType::Nick => "nick",
            IgnoreType::Tagmsg => "tagmsg",
        }
    }
}

#[derive(Default)]
pub struct Ignores {
    rules: Vec<(String, IgnoreRule)>,
}

impl Ignores {
    pub fn new(rules: &[IgnoreRule]) -> Ignores {
        Ignores { rules: rules.iter().map(|r| (mask::normalize(&r.mask), r.clone())).collect() }
    }

    pub fn is_ignored(
        &self,
        network: &str,
        channel: Option<&str>,
        source: &Source,
        kind: IgnoreType,
        cm: CaseMapping,
    ) -> bool {
        if self.rules.is_empty() || source.nick.is_empty() {
            return false;
        }
        let full = format!(
            "{}!{}@{}",
            source.nick,
            source.user.as_deref().unwrap_or("*"),
            source.host.as_deref().unwrap_or("*")
        );
        self.rules.iter().any(|(m, r)| {
            r.network.as_ref().is_none_or(|n| n.eq_ignore_ascii_case(network))
                && r.channel.as_ref().is_none_or(|c| channel.is_some_and(|ch| mask::matches(c, ch, cm)))
                && r.types.iter().any(|t| t == "all" || t == kind.name())
                && mask::matches(m, &full, cm)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_boundaries() {
        assert!(contains_word("hey Bob, how are you", "bob"));
        assert!(contains_word("bob: hi", "bob"));
        assert!(!contains_word("bobby hi", "bob"));
        assert!(!contains_word("hi_bob", "bob"));
        assert!(contains_word("ärger bob!", "BOB"));
        assert!(contains_word("x bob", "bob"));
    }

    #[test]
    fn highlighter() {
        let cfg = Highlight {
            nick: true,
            words: vec!["rust".into()],
            patterns: vec![r"\bdeploy(ed|ing)?\b".into(), "(".into()],
            exclude_nicks: vec!["*bot".into()],
        };
        let (h, errs) = Highlighter::new(&cfg);
        assert_eq!(errs.len(), 1);
        let alice = Source::parse("alice!a@h");
        let cm = CaseMapping::Rfc1459;
        assert!(h.matches("me: ping", "me", &alice, cm));
        assert!(h.matches("I love Rust", "me", &alice, cm));
        assert!(h.matches("DEPLOYING now", "me", &alice, cm));
        assert!(!h.matches("nothing here", "me", &alice, cm));
        assert!(!h.matches("me: ping", "me", &Source::parse("newsbot!n@h"), cm));
    }

    #[test]
    fn ignores() {
        let ig = Ignores::new(&[
            IgnoreRule { mask: "spammer".into(), types: vec!["msg".into()], network: None, channel: None },
            IgnoreRule {
                mask: "*@*.bad.net".into(),
                types: vec!["all".into()],
                network: Some("Libera".into()),
                channel: None,
            },
        ]);
        let cm = CaseMapping::Rfc1459;
        assert!(ig.is_ignored("x", Some("#c"), &Source::parse("Spammer!u@h"), IgnoreType::Msg, cm));
        assert!(!ig.is_ignored("x", Some("#c"), &Source::parse("Spammer!u@h"), IgnoreType::Join, cm));
        assert!(ig.is_ignored("libera", None, &Source::parse("x!y@host.bad.net"), IgnoreType::Join, cm));
        assert!(!ig.is_ignored("oftc", None, &Source::parse("x!y@host.bad.net"), IgnoreType::Join, cm));
    }
}

/// Hides passwords in messages to services (`NickServ IDENTIFY …`, `REGISTER …`) before they are
/// displayed or logged. Returns `None` when nothing needs masking.
pub fn mask_service_secret(target: &str, text: &str) -> Option<String> {
    if !target.to_ascii_lowercase().ends_with("serv") {
        return None;
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    let cmd = words.first()?.to_ascii_uppercase();
    let masked = |keep: usize| -> String {
        let mut out: Vec<&str> = words[..keep.min(words.len())].to_vec();
        out.extend(std::iter::repeat_n("********", words.len().saturating_sub(keep)));
        out.join(" ")
    };
    match cmd.as_str() {
        // IDENTIFY [account] password, GHOST nick password, …
        "IDENTIFY" | "ID" | "LOGIN" | "GHOST" | "RECOVER" | "RELEASE" | "REGAIN" if words.len() >= 2 => {
            let keep = if words.len() >= 3 { 2 } else { 1 };
            Some(masked(keep))
        }
        "REGISTER" if words.len() >= 2 => {
            // REGISTER password [email]
            let mut out = vec![words[0], "********"];
            out.extend(&words[2..]);
            Some(out.join(" "))
        }
        "SET" if words.get(1).is_some_and(|w| w.eq_ignore_ascii_case("PASSWORD")) => Some(masked(2)),
        _ => None,
    }
}

#[cfg(test)]
mod mask_tests {
    use super::mask_service_secret as m;

    #[test]
    fn masks_service_passwords() {
        assert_eq!(m("NickServ", "IDENTIFY hunter2").as_deref(), Some("IDENTIFY ********"));
        assert_eq!(m("nickserv", "identify me hunter2").as_deref(), Some("identify me ********"));
        assert_eq!(m("NickServ", "REGISTER pw a@b.c").as_deref(), Some("REGISTER ******** a@b.c"));
        assert_eq!(m("NickServ", "SET PASSWORD new").as_deref(), Some("SET PASSWORD ********"));
        assert_eq!(m("NickServ", "INFO bob"), None);
        assert_eq!(m("bob", "IDENTIFY hunter2"), None);
    }
}
