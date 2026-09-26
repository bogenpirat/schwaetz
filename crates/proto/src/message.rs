//! IRC message parsing and serialization.

use crate::tags::Tags;
use std::fmt;

/// The source (prefix) of a message: `nick!user@host` or a server name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Source {
    pub nick: String,
    pub user: Option<String>,
    pub host: Option<String>,
}

impl Source {
    /// Splits `nick!user@host`. Any part may be missing.
    pub fn parse(raw: &str) -> Source {
        let (rest, host) = match raw.split_once('@') {
            Some((r, h)) => (r, Some(h.to_owned())),
            None => (raw, None),
        };
        let (nick, user) = match rest.split_once('!') {
            Some((n, u)) => (n, Some(u.to_owned())),
            None => (rest, None),
        };
        Source { nick: nick.to_owned(), user, host }
    }

    pub fn nick(nick: impl Into<String>) -> Source {
        Source { nick: nick.into(), user: None, host: None }
    }

    /// Heuristic: a bare name containing a dot and no user/host is a server.
    pub fn is_server(&self) -> bool {
        self.user.is_none() && self.host.is_none() && self.nick.contains('.')
    }

    pub fn write_to(&self, out: &mut String) {
        out.push_str(&self.nick);
        if let Some(u) = &self.user {
            out.push('!');
            out.push_str(u);
        }
        if let Some(h) = &self.host {
            out.push('@');
            out.push_str(h);
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = String::new();
        self.write_to(&mut s);
        f.write_str(&s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    MissingCommand,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Empty => f.write_str("empty line"),
            ParseError::MissingCommand => f.write_str("missing command"),
        }
    }
}

impl std::error::Error for ParseError {}

/// A borrowed, allocation-free view of a line; used on hot paths (e.g. answering PING on the
/// network thread) before paying for a full [`Message`].
#[derive(Clone, Copy, Debug)]
pub struct MessageRef<'a> {
    pub tags: Option<&'a str>,
    pub source: Option<&'a str>,
    pub command: &'a str,
    /// Everything after the command, unparsed.
    pub rest: &'a str,
}

impl<'a> MessageRef<'a> {
    pub fn parse(line: &'a str) -> Result<Self, ParseError> {
        let mut s = trim_line_end(line);
        if s.trim_matches(' ').is_empty() {
            return Err(ParseError::Empty);
        }
        let mut tags = None;
        if let Some(t) = s.strip_prefix('@') {
            let (t, r) = split_word(t);
            tags = Some(t);
            s = r;
        }
        let mut source = None;
        if let Some(src) = s.strip_prefix(':') {
            let (src, r) = split_word(src);
            source = Some(src);
            s = r;
        }
        let (command, rest) = split_word(s);
        if command.is_empty() {
            return Err(ParseError::MissingCommand);
        }
        Ok(MessageRef { tags, source, command, rest })
    }

    pub fn params(&self) -> Params<'a> {
        Params { rest: self.rest }
    }

    pub fn to_owned(&self) -> Message {
        Message {
            tags: self.tags.map(Tags::parse).unwrap_or_default(),
            source: self.source.map(Source::parse),
            command: self.command.to_owned(),
            params: self.params().map(str::to_owned).collect(),
        }
    }
}

/// Iterator over the parameters of a [`MessageRef`].
pub struct Params<'a> {
    rest: &'a str,
}

impl<'a> Iterator for Params<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        let s = self.rest.trim_start_matches(' ');
        if s.is_empty() {
            self.rest = s;
            return None;
        }
        if let Some(trailing) = s.strip_prefix(':') {
            self.rest = "";
            return Some(trailing);
        }
        let (p, r) = split_word(s);
        self.rest = r;
        Some(p)
    }
}

fn trim_line_end(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

/// Splits at the first space, returning (word, remainder with leading spaces stripped).
fn split_word(s: &str) -> (&str, &str) {
    match memchr::memchr(b' ', s.as_bytes()) {
        Some(i) => (&s[..i], s[i + 1..].trim_start_matches(' ')),
        None => (s, ""),
    }
}

/// An owned IRC message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Message {
    pub tags: Tags,
    pub source: Option<Source>,
    pub command: String,
    pub params: Vec<String>,
}

impl Message {
    pub fn parse(line: &str) -> Result<Message, ParseError> {
        MessageRef::parse(line).map(|m| m.to_owned())
    }

    pub fn new<I, S>(command: impl Into<String>, params: I) -> Message
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Message {
            tags: Tags::new(),
            source: None,
            command: command.into(),
            params: params.into_iter().map(Into::into).collect(),
        }
    }

    pub fn with_tag(mut self, key: impl Into<String>, value: impl Into<String>) -> Message {
        self.tags.insert(key, value);
        self
    }

    pub fn with_source(mut self, source: Source) -> Message {
        self.source = Some(source);
        self
    }

    /// Case-insensitive command comparison.
    pub fn is(&self, command: &str) -> bool {
        self.command.eq_ignore_ascii_case(command)
    }

    /// The numeric reply code, if this is a three-digit numeric.
    pub fn numeric(&self) -> Option<u16> {
        let b = self.command.as_bytes();
        if b.len() == 3 && b.iter().all(u8::is_ascii_digit) { self.command.parse().ok() } else { None }
    }

    pub fn param(&self, i: usize) -> Option<&str> {
        self.params.get(i).map(String::as_str)
    }

    /// Parameter `i`, or the empty string.
    pub fn arg(&self, i: usize) -> &str {
        self.param(i).unwrap_or("")
    }

    pub fn last_param(&self) -> Option<&str> {
        self.params.last().map(String::as_str)
    }

    pub fn source_nick(&self) -> Option<&str> {
        self.source.as_ref().map(|s| s.nick.as_str())
    }

    /// Serializes without the trailing CRLF. `\r`, `\n` and NUL inside parameters are replaced by
    /// spaces so that user input can never inject extra lines.
    pub fn write_to(&self, out: &mut String) {
        if !self.tags.is_empty() {
            out.push('@');
            self.tags.write_to(out);
            out.push(' ');
        }
        if let Some(src) = &self.source {
            out.push(':');
            src.write_to(out);
            out.push(' ');
        }
        push_sanitized(out, &self.command);
        let n = self.params.len();
        for (i, p) in self.params.iter().enumerate() {
            out.push(' ');
            let last = i + 1 == n;
            if last && (p.is_empty() || p.starts_with(':') || p.contains(' ')) {
                out.push(':');
            }
            push_sanitized(out, p);
        }
    }

    pub fn to_line(&self) -> String {
        let mut s = String::with_capacity(64);
        self.write_to(&mut s);
        s
    }
}

fn push_sanitized(out: &mut String, s: &str) {
    if s.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
        out.extend(s.chars().map(|c| if matches!(c, '\r' | '\n' | '\0') { ' ' } else { c }));
    } else {
        out.push_str(s);
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_line())
    }
}

impl std::str::FromStr for Message {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Message::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let m = Message::parse("@time=2020-01-01T00:00:00.000Z :nick!u@h PRIVMSG #c :hi there\r\n").unwrap();
        assert_eq!(m.tags.get("time"), Some("2020-01-01T00:00:00.000Z"));
        assert_eq!(m.source_nick(), Some("nick"));
        assert_eq!(m.command, "PRIVMSG");
        assert_eq!(m.params, ["#c", "hi there"]);
    }

    #[test]
    fn no_injection() {
        let m = Message::new("PRIVMSG", ["#c", "hi\r\nQUIT :bye"]);
        assert_eq!(m.to_line(), "PRIVMSG #c :hi  QUIT :bye");
    }

    #[test]
    fn empty() {
        assert_eq!(Message::parse("\r\n"), Err(ParseError::Empty));
        assert_eq!(Message::parse("@a=b "), Err(ParseError::MissingCommand));
    }
}
