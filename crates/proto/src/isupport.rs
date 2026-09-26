//! RPL_ISUPPORT (005) parameters (<https://modern.ircdocs.horse/#rplisupport-parameter>).

use crate::casemap::CaseMapping;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeType {
    /// Type A: list modes (always take a parameter; without one, lists entries).
    List,
    /// Type B: always take a parameter.
    AlwaysParam,
    /// Type C: take a parameter only when set.
    ParamWhenSet,
    /// Type D: never take a parameter.
    NoParam,
    /// Membership prefix mode (e.g. `o`, `v`); always takes a nick.
    Prefix,
}

#[derive(Clone, Debug)]
pub struct ISupport {
    /// All tokens seen, unescaped. `None` for tokens without a value.
    pub raw: HashMap<String, Option<String>>,
    pub casemapping: CaseMapping,
    pub chantypes: String,
    /// (mode, symbol) ordered from highest to lowest rank.
    pub prefix: Vec<(char, char)>,
    /// CHANMODES=A,B,C,D
    pub chanmodes: [String; 4],
    pub statusmsg: String,
    pub network: Option<String>,
    pub nicklen: Option<usize>,
    pub channellen: Option<usize>,
    pub topiclen: Option<usize>,
    pub kicklen: Option<usize>,
    pub awaylen: Option<usize>,
    /// Max mode changes with parameters per MODE command.
    pub modes: usize,
    /// Per-command target limits; `None` value means unlimited.
    pub targmax: HashMap<String, Option<usize>>,
    pub monitor: Option<usize>,
    pub whox: bool,
    pub bot: Option<char>,
    pub utf8only: bool,
    pub excepts: Option<char>,
    pub invex: Option<char>,
    pub elist: String,
    pub clienttagdeny: Vec<String>,
    pub chathistory: Option<usize>,
    pub msgreftypes: Vec<String>,
    /// Maximum line length in bytes, including CRLF (LINELEN, default 512).
    pub linelen: usize,
}

impl Default for ISupport {
    fn default() -> Self {
        ISupport {
            raw: HashMap::new(),
            casemapping: CaseMapping::Rfc1459,
            chantypes: "#&".into(),
            prefix: vec![('o', '@'), ('v', '+')],
            chanmodes: ["beI".into(), "k".into(), "l".into(), "imnpst".into()],
            statusmsg: String::new(),
            network: None,
            nicklen: None,
            channellen: None,
            topiclen: None,
            kicklen: None,
            awaylen: None,
            modes: 3,
            targmax: HashMap::new(),
            monitor: None,
            whox: false,
            bot: None,
            utf8only: false,
            excepts: None,
            invex: None,
            elist: String::new(),
            clienttagdeny: Vec::new(),
            chathistory: None,
            msgreftypes: Vec::new(),
            linelen: crate::MAX_LINE_LEN,
        }
    }
}

impl ISupport {
    /// Applies the tokens of one RPL_ISUPPORT line: `<client> <token>... :are supported by this server`.
    pub fn apply_reply(&mut self, params: &[String]) {
        if params.len() < 2 {
            return;
        }
        for tok in &params[1..params.len() - 1] {
            self.apply_token(tok);
        }
    }

    pub fn apply_token(&mut self, tok: &str) {
        if let Some(key) = tok.strip_prefix('-') {
            self.raw.remove(key);
            self.reset_key(key);
            return;
        }
        let (key, value) = match tok.split_once('=') {
            Some((k, v)) => (k, Some(unescape_value(v))),
            None => (tok, None),
        };
        self.set(key, value.as_deref());
        self.raw.insert(key.to_owned(), value);
    }

    fn reset_key(&mut self, key: &str) {
        let d = ISupport::default();
        match key {
            "CASEMAPPING" => self.casemapping = d.casemapping,
            "CHANTYPES" => self.chantypes = d.chantypes,
            "PREFIX" => self.prefix = d.prefix,
            "CHANMODES" => self.chanmodes = d.chanmodes,
            "STATUSMSG" => self.statusmsg.clear(),
            "NETWORK" => self.network = None,
            "NICKLEN" => self.nicklen = None,
            "CHANNELLEN" => self.channellen = None,
            "TOPICLEN" => self.topiclen = None,
            "KICKLEN" => self.kicklen = None,
            "AWAYLEN" => self.awaylen = None,
            "MODES" => self.modes = d.modes,
            "TARGMAX" => self.targmax.clear(),
            "MONITOR" => self.monitor = None,
            "WHOX" => self.whox = false,
            "BOT" => self.bot = None,
            "UTF8ONLY" => self.utf8only = false,
            "EXCEPTS" => self.excepts = None,
            "INVEX" => self.invex = None,
            "ELIST" => self.elist.clear(),
            "CLIENTTAGDENY" => self.clienttagdeny.clear(),
            "CHATHISTORY" | "draft/CHATHISTORY" => self.chathistory = None,
            "MSGREFTYPES" => self.msgreftypes.clear(),
            "LINELEN" => self.linelen = d.linelen,
            _ => {}
        }
    }

    fn set(&mut self, key: &str, value: Option<&str>) {
        let v = value.unwrap_or("");
        let num = || v.parse::<usize>().ok();
        match key {
            "CASEMAPPING" => self.casemapping = CaseMapping::from_token(v),
            "CHANTYPES" => self.chantypes = v.to_owned(),
            "PREFIX" => {
                self.prefix = parse_prefix(v);
            }
            "CHANMODES" => {
                let mut parts = v.splitn(4, ',').map(str::to_owned);
                self.chanmodes = std::array::from_fn(|_| parts.next().unwrap_or_default());
            }
            "STATUSMSG" => self.statusmsg = v.to_owned(),
            "NETWORK" => self.network = Some(v.to_owned()).filter(|s| !s.is_empty()),
            "NICKLEN" => self.nicklen = num(),
            "CHANNELLEN" => self.channellen = num(),
            "TOPICLEN" => self.topiclen = num(),
            "KICKLEN" => self.kicklen = num(),
            "AWAYLEN" => self.awaylen = num(),
            "MODES" => self.modes = num().unwrap_or(usize::MAX),
            "TARGMAX" => {
                self.targmax = v
                    .split(',')
                    .filter_map(|item| {
                        let (cmd, n) = item.split_once(':')?;
                        Some((cmd.to_ascii_uppercase(), n.parse().ok()))
                    })
                    .collect();
            }
            "MONITOR" => self.monitor = Some(num().unwrap_or(usize::MAX)),
            "WHOX" => self.whox = true,
            "BOT" => self.bot = v.chars().next(),
            "UTF8ONLY" => self.utf8only = true,
            "EXCEPTS" => self.excepts = Some(v.chars().next().unwrap_or('e')),
            "INVEX" => self.invex = Some(v.chars().next().unwrap_or('I')),
            "ELIST" => self.elist = v.to_ascii_uppercase(),
            "CLIENTTAGDENY" => self.clienttagdeny = v.split(',').filter(|s| !s.is_empty()).map(str::to_owned).collect(),
            "CHATHISTORY" | "draft/CHATHISTORY" => self.chathistory = Some(num().unwrap_or(0)),
            "MSGREFTYPES" => self.msgreftypes = v.split(',').filter(|s| !s.is_empty()).map(str::to_owned).collect(),
            "LINELEN" => self.linelen = num().unwrap_or(crate::MAX_LINE_LEN).max(crate::MAX_LINE_LEN),
            _ => {}
        }
    }

    pub fn is_channel(&self, name: &str) -> bool {
        name.chars().next().is_some_and(|c| self.chantypes.contains(c))
    }

    /// Strips a STATUSMSG prefix (e.g. `@#chan`), returning (prefix symbol, channel).
    pub fn split_statusmsg<'a>(&self, target: &'a str) -> (Option<char>, &'a str) {
        let mut chars = target.chars();
        match chars.next() {
            Some(c) if self.statusmsg.contains(c) && self.is_channel(chars.as_str()) => (Some(c), chars.as_str()),
            _ => (None, target),
        }
    }

    pub fn mode_type(&self, mode: char) -> ModeType {
        if self.prefix.iter().any(|(m, _)| *m == mode) {
            return ModeType::Prefix;
        }
        let [a, b, c, _] = &self.chanmodes;
        if a.contains(mode) {
            ModeType::List
        } else if b.contains(mode) {
            ModeType::AlwaysParam
        } else if c.contains(mode) {
            ModeType::ParamWhenSet
        } else {
            ModeType::NoParam
        }
    }

    pub fn prefix_for_mode(&self, mode: char) -> Option<char> {
        self.prefix.iter().find(|(m, _)| *m == mode).map(|(_, s)| *s)
    }

    pub fn mode_for_prefix(&self, symbol: char) -> Option<char> {
        self.prefix.iter().find(|(_, s)| *s == symbol).map(|(m, _)| *m)
    }

    /// Rank of a prefix symbol; 0 is the highest. Unknown symbols rank last.
    pub fn prefix_rank(&self, symbol: char) -> usize {
        self.prefix.iter().position(|(_, s)| *s == symbol).unwrap_or(self.prefix.len())
    }

    /// Splits leading membership prefixes off a NAMES entry (supports `multi-prefix`).
    pub fn split_prefixes<'a>(&self, entry: &'a str) -> (Vec<char>, &'a str) {
        let mut prefixes = Vec::new();
        let mut rest = entry;
        while let Some(c) = rest.chars().next() {
            if self.prefix.iter().any(|(_, s)| *s == c) {
                prefixes.push(c);
                rest = &rest[c.len_utf8()..];
            } else {
                break;
            }
        }
        (prefixes, rest)
    }

    pub fn max_targets(&self, command: &str) -> Option<usize> {
        self.targmax.get(&command.to_ascii_uppercase()).copied().flatten()
    }

    pub fn tag_denied(&self, tag: &str) -> bool {
        let name = tag.trim_start_matches('+');
        let mut denied = false;
        for rule in &self.clienttagdeny {
            if rule == "*" {
                denied = true;
            } else if let Some(allowed) = rule.strip_prefix('-') {
                if allowed == name {
                    denied = false;
                }
            } else if rule == name {
                denied = true;
            }
        }
        denied
    }
}

fn parse_prefix(v: &str) -> Vec<(char, char)> {
    let Some(rest) = v.strip_prefix('(') else { return Vec::new() };
    let Some((modes, symbols)) = rest.split_once(')') else { return Vec::new() };
    modes.chars().zip(symbols.chars()).collect()
}

/// ISUPPORT values escape bytes as `\xHH`.
fn unescape_value(v: &str) -> String {
    if !v.contains("\\x") {
        return v.to_owned();
    }
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && b.get(i + 1) == Some(&b'x')
            && let Some(h) = b.get(i + 2..i + 4).and_then(|h| std::str::from_utf8(h).ok())
            && let Ok(byte) = u8::from_str_radix(h, 16)
        {
            out.push(byte);
            i += 4;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(tokens: &[&str]) -> ISupport {
        let mut is = ISupport::default();
        let mut params = vec!["me".to_owned()];
        params.extend(tokens.iter().map(|s| s.to_string()));
        params.push("are supported by this server".into());
        is.apply_reply(&params);
        is
    }

    #[test]
    fn typical() {
        let is = apply(&[
            "CASEMAPPING=ascii",
            "PREFIX=(qaohv)~&@%+",
            "CHANMODES=beI,k,l,imnpst",
            "NETWORK=Example\\x20Net",
            "TARGMAX=PRIVMSG:4,JOIN:",
            "MONITOR=100",
            "WHOX",
            "STATUSMSG=@+",
            "CLIENTTAGDENY=*,-draft/react",
        ]);
        assert_eq!(is.casemapping, CaseMapping::Ascii);
        assert_eq!(is.prefix.len(), 5);
        assert_eq!(is.prefix_rank('@'), 2);
        assert_eq!(is.mode_type('o'), ModeType::Prefix);
        assert_eq!(is.mode_type('b'), ModeType::List);
        assert_eq!(is.mode_type('k'), ModeType::AlwaysParam);
        assert_eq!(is.mode_type('l'), ModeType::ParamWhenSet);
        assert_eq!(is.mode_type('n'), ModeType::NoParam);
        assert_eq!(is.network.as_deref(), Some("Example Net"));
        assert_eq!(is.max_targets("privmsg"), Some(4));
        assert_eq!(is.max_targets("JOIN"), None);
        assert_eq!(is.monitor, Some(100));
        assert!(is.whox);
        assert_eq!(is.split_statusmsg("@#chan"), (Some('@'), "#chan"));
        assert_eq!(is.split_prefixes("@+nick"), (vec!['@', '+'], "nick"));
        assert!(is.tag_denied("+typing"));
        assert!(!is.tag_denied("+draft/react"));
    }

    #[test]
    fn negation() {
        let mut is = apply(&["NETWORK=Foo"]);
        is.apply_token("-NETWORK");
        assert_eq!(is.network, None);
    }
}
