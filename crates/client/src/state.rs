//! Tracked network state: channels, members, users and modes.

use schwaetz_proto::isupport::{ISupport, ModeType};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Debug, Default)]
pub struct User {
    pub nick: String,
    pub user: Option<String>,
    pub host: Option<String>,
    pub realname: Option<String>,
    /// `None` = unknown/not logged in, `Some("")` never occurs.
    pub account: Option<String>,
    pub away: Option<String>,
    pub bot: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub nick: String,
    /// Membership prefix symbols (e.g. `@`, `+`) ordered by rank, highest first.
    pub prefixes: Vec<char>,
}

impl Member {
    pub fn highest(&self) -> Option<char> {
        self.prefixes.first().copied()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Channel {
    pub name: String,
    pub topic: Option<String>,
    pub topic_set_by: Option<String>,
    pub topic_time: Option<i64>,
    pub created: Option<i64>,
    /// Non-list channel modes with optional argument (e.g. `k` → key, `l` → limit).
    pub modes: BTreeMap<char, Option<String>>,
    /// Keyed by case-folded nick.
    pub members: HashMap<String, Member>,
    /// True while collecting a NAMES reply; the next 353 replaces the member list.
    pub(crate) names_fresh: bool,
    pub(crate) names_done: bool,
    pub key: Option<String>,
}

impl Channel {
    pub fn new(name: &str) -> Channel {
        Channel { name: name.to_owned(), names_fresh: true, ..Default::default() }
    }

    pub fn names_complete(&self) -> bool {
        self.names_done
    }

    /// Mode string such as `+ntk key`.
    pub fn mode_string(&self) -> String {
        let mut flags = String::from("+");
        let mut args = Vec::new();
        for (m, a) in &self.modes {
            flags.push(*m);
            if let Some(a) = a {
                args.push(a.as_str());
            }
        }
        if args.is_empty() { flags } else { format!("{flags} {}", args.join(" ")) }
    }

    /// Members sorted by rank then nick (case-insensitive), as shown in a nick list.
    pub fn sorted_members(&self, isupport: &ISupport) -> Vec<&Member> {
        let mut v: Vec<&Member> = self.members.values().collect();
        v.sort_by(|a, b| {
            let ra = a.highest().map_or(usize::MAX, |p| isupport.prefix_rank(p));
            let rb = b.highest().map_or(usize::MAX, |p| isupport.prefix_rank(p));
            ra.cmp(&rb).then_with(|| isupport.casemapping.fold(&a.nick).cmp(&isupport.casemapping.fold(&b.nick)))
        });
        v
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeChange {
    pub add: bool,
    pub mode: char,
    pub arg: Option<String>,
}

/// Parses a channel MODE parameter list (`+ov-k nick1 nick2 key`) according to CHANMODES/PREFIX.
pub fn parse_channel_modes(isupport: &ISupport, params: &[String]) -> Vec<ModeChange> {
    let mut out = Vec::new();
    let Some((flags, args)) = params.split_first() else { return out };
    let mut args = args.iter();
    let mut add = true;
    for c in flags.chars() {
        match c {
            '+' => add = true,
            '-' => add = false,
            m => {
                let takes_arg = match isupport.mode_type(m) {
                    ModeType::List | ModeType::AlwaysParam | ModeType::Prefix => true,
                    ModeType::ParamWhenSet => add,
                    ModeType::NoParam => false,
                };
                let arg = if takes_arg { args.next().cloned() } else { None };
                out.push(ModeChange { add, mode: m, arg });
            }
        }
    }
    out
}

/// Parses user modes (never take arguments).
pub fn parse_user_modes(flags: &str) -> Vec<ModeChange> {
    let mut add = true;
    flags
        .chars()
        .filter_map(|c| match c {
            '+' => {
                add = true;
                None
            }
            '-' => {
                add = false;
                None
            }
            m => Some(ModeChange { add, mode: m, arg: None }),
        })
        .collect()
}

/// Accumulated WHOIS reply, emitted at RPL_ENDOFWHOIS.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WhoisInfo {
    pub nick: String,
    pub user: Option<String>,
    pub host: Option<String>,
    pub realname: Option<String>,
    pub server: Option<String>,
    pub server_info: Option<String>,
    pub operator: Option<String>,
    pub idle_secs: Option<u64>,
    pub signon: Option<i64>,
    pub channels: Vec<String>,
    pub account: Option<String>,
    pub secure: bool,
    pub away: Option<String>,
    pub actual_host: Option<String>,
    pub certfp: Option<String>,
    pub bot: bool,
    /// Any other WHOIS lines (RPL_WHOISSPECIAL, modes, etc.), verbatim.
    pub extra: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn channel_modes() {
        let is = ISupport::default();
        let changes = parse_channel_modes(&is, &s(&["+ov-k+lb", "alice", "bob", "key", "10", "*!*@x"]));
        assert_eq!(
            changes,
            vec![
                ModeChange { add: true, mode: 'o', arg: Some("alice".into()) },
                ModeChange { add: true, mode: 'v', arg: Some("bob".into()) },
                ModeChange { add: false, mode: 'k', arg: Some("key".into()) },
                ModeChange { add: true, mode: 'l', arg: Some("10".into()) },
                ModeChange { add: true, mode: 'b', arg: Some("*!*@x".into()) },
            ]
        );
        let changes = parse_channel_modes(&is, &s(&["-l+n"]));
        assert_eq!(changes[0], ModeChange { add: false, mode: 'l', arg: None });
    }
}

/// An entry of a ban/except/invex list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeListEntry {
    pub mask: String,
    pub set_by: Option<String>,
    /// Unix milliseconds.
    pub set_at: Option<i64>,
}
