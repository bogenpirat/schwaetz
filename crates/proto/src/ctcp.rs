//! Client-To-Client Protocol framing (<https://modern.ircdocs.horse/ctcp>).

const DELIM: char = '\x01';

/// A CTCP query or reply extracted from a PRIVMSG/NOTICE body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ctcp<'a> {
    /// Upper-cased by convention on send; compare case-insensitively.
    pub command: &'a str,
    pub params: &'a str,
}

impl<'a> Ctcp<'a> {
    pub fn is(&self, command: &str) -> bool {
        self.command.eq_ignore_ascii_case(command)
    }
}

/// Parses `\x01CMD params\x01`. The trailing delimiter is optional, as many clients omit it.
pub fn parse(body: &str) -> Option<Ctcp<'_>> {
    let inner = body.strip_prefix(DELIM)?;
    let inner = inner.strip_suffix(DELIM).unwrap_or(inner);
    let (command, params) = inner.split_once(' ').unwrap_or((inner, ""));
    if command.is_empty() {
        return None;
    }
    Some(Ctcp { command, params })
}

pub fn encode(command: &str, params: &str) -> String {
    if params.is_empty() { format!("\x01{command}\x01") } else { format!("\x01{command} {params}\x01") }
}

/// Wraps text as a `/me` action.
pub fn action(text: &str) -> String {
    encode("ACTION", text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let c = parse("\x01ACTION waves\x01").unwrap();
        assert!(c.is("action"));
        assert_eq!(c.params, "waves");
        assert_eq!(parse("\x01VERSION").unwrap(), Ctcp { command: "VERSION", params: "" });
        assert_eq!(parse("hello"), None);
        assert_eq!(action("hi"), "\x01ACTION hi\x01");
    }
}
