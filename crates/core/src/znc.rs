//! ZNC helpers.

/// Common `*status` commands, for completion in the bouncer buffer.
pub const STATUS_COMMANDS: &[&str] = &[
    "Help",
    "Version",
    "ListNetworks",
    "JumpNetwork",
    "ListChans",
    "ListNicks",
    "ListServers",
    "AddServer",
    "DelServer",
    "Connect",
    "Disconnect",
    "Detach",
    "Attach",
    "ClearBuffer",
    "ClearAllBuffers",
    "PlayBuffer",
    "SetBuffer",
    "ListMods",
    "ListAvailMods",
    "LoadMod",
    "UnloadMod",
    "ReloadMod",
    "ShowChan",
    "Uptime",
    "Traffic",
];

/// Extracts network names from a `ListNetworks` table:
///
/// ```text
/// +---------+-------+------------------+
/// | Network | OnIRC | IRC Server       |
/// +---------+-------+------------------+
/// | libera  | Yes   | irc.libera.chat  |
/// ```
///
/// Feed each line; returns `Some(name)` for data rows.
pub fn parse_list_networks_row(line: &str) -> Option<String> {
    let line = line.trim();
    if !line.starts_with('|') {
        return None;
    }
    let first = line.trim_matches('|').split('|').next()?.trim();
    if first.is_empty() || first.eq_ignore_ascii_case("Network") {
        return None;
    }
    Some(first.to_owned())
}

/// Builds the server password for a ZNC login.
pub fn pass(user: &str, network: Option<&str>, password: &str) -> String {
    match network {
        Some(n) if !n.is_empty() => format!("{user}/{n}:{password}"),
        _ => format!("{user}:{password}"),
    }
}

/// Whether a message is the notes module saying there is nothing to show (sent on every login
/// with `ShowNotesOnLogin`); it is kept in the buffer but doesn't count as unread.
pub fn is_empty_notes(nick: &str, text: &str) -> bool {
    nick.eq_ignore_ascii_case("*notes") && text.trim() == "You have no entries."
}

/// Parses a ZNC buffer-playback timestamp prefix (`[HH:MM:SS] text`), used when the bouncer can't
/// send server-time. Returns seconds since midnight and the remaining text.
pub fn strip_playback_timestamp(text: &str) -> Option<(u32, &str)> {
    let b = text.as_bytes();
    if b.len() < 11 || b[0] != b'[' || b[3] != b':' || b[6] != b':' || b[9] != b']' || b[10] != b' ' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| text.get(r)?.parse::<u32>().ok();
    let (h, m, s) = (n(1..3)?, n(4..6)?, n(7..9)?);
    if h > 23 || m > 59 || s > 60 {
        return None;
    }
    Some((h * 3600 + m * 60 + s, &text[11..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_networks() {
        let table = [
            "+---------+-------+------------------+",
            "| Network | OnIRC | IRC Server       |",
            "+---------+-------+------------------+",
            "| libera  | Yes   | irc.libera.chat  |",
            "| oftc    | No    |                  |",
            "+---------+-------+------------------+",
        ];
        let names: Vec<_> = table.iter().filter_map(|l| parse_list_networks_row(l)).collect();
        assert_eq!(names, ["libera", "oftc"]);
    }

    #[test]
    fn playback_prefix() {
        assert_eq!(strip_playback_timestamp("[12:34:56] hello"), Some((45296, "hello")));
        assert_eq!(strip_playback_timestamp("hello"), None);
        assert_eq!(pass("u", Some("libera"), "pw"), "u/libera:pw");
    }

    #[test]
    fn empty_notes() {
        assert!(is_empty_notes("*notes", "You have no entries."));
        assert!(!is_empty_notes("*notes", "remember the milk"));
        assert!(!is_empty_notes("alice", "You have no entries."));
    }
}
