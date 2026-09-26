//! Wildcard masks (`*`, `?`) as used by bans, ignores and highlights.

use crate::casemap::CaseMapping;

/// Matches `text` against an IRC glob `mask` (`*` = any run, `?` = exactly one character),
/// case-insensitively according to `cm`. Other characters (including `[`) are literal.
pub fn matches(mask: &str, text: &str, cm: CaseMapping) -> bool {
    let m: Vec<char> = mask.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let fold = |c: char| if c.is_ascii() { cm.fold_byte(c as u8) as char } else { c };
    let (mut mi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if mi < m.len() && (m[mi] == '?' || (m[mi] != '*' && fold(m[mi]) == fold(t[ti]))) {
            mi += 1;
            ti += 1;
        } else if mi < m.len() && m[mi] == '*' {
            star = Some((mi, ti));
            mi += 1;
        } else if let Some((smi, sti)) = star {
            mi = smi + 1;
            ti = sti + 1;
            star = Some((smi, sti + 1));
        } else {
            return false;
        }
    }
    m[mi..].iter().all(|&c| c == '*')
}

/// Normalizes a partial mask to `nick!user@host` form (`foo` → `foo!*@*`, `*@host` → `*!*@host`).
pub fn normalize(mask: &str) -> String {
    let (nickuser, host) = match mask.split_once('@') {
        Some((nu, h)) => (nu, h),
        None => (mask, "*"),
    };
    let (nick, user) = match nickuser.split_once('!') {
        Some((n, u)) => (n, u),
        None if mask.contains('@') => ("*", nickuser),
        None => (nickuser, "*"),
    };
    let or_star = |s: &str| if s.is_empty() { "*".to_owned() } else { s.to_owned() };
    format!("{}!{}@{}", or_star(nick), or_star(user), or_star(host))
}

/// Validates a hostname as acceptable for IRC server names (must contain a dot, LDH labels).
pub fn is_valid_hostname(host: &str) -> bool {
    let h = host.strip_suffix('.').unwrap_or(host);
    if h.is_empty() || h.len() > 253 || !h.contains('.') && !host.ends_with('.') {
        return false;
    }
    h.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_masks() {
        assert_eq!(normalize("foo"), "foo!*@*");
        assert_eq!(normalize("*@host"), "*!*@host");
        assert_eq!(normalize("n!u"), "n!u@*");
        assert_eq!(normalize("n!u@h"), "n!u@h");
    }

    #[test]
    fn backtracking() {
        assert!(matches("*a*b", "xaxxab", CaseMapping::Ascii));
        assert!(!matches("*a*b", "xaxxa", CaseMapping::Ascii));
        assert!(matches("NICK!*", "nick!x@y", CaseMapping::Rfc1459));
    }
}
