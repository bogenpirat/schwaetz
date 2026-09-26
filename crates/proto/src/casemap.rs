//! Nick/channel case mapping as advertised by `CASEMAPPING` in ISUPPORT.

use std::borrow::Cow;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CaseMapping {
    Ascii,
    /// `[]\~` are the upper-case forms of `{}|^` (the historical default).
    #[default]
    Rfc1459,
    /// Like rfc1459 but without `~`/`^`.
    StrictRfc1459,
}

impl CaseMapping {
    pub fn from_token(s: &str) -> CaseMapping {
        match s.to_ascii_lowercase().as_str() {
            "ascii" => CaseMapping::Ascii,
            "strict-rfc1459" => CaseMapping::StrictRfc1459,
            // rfc7613 / precis servers still fold ASCII identically; unknown values fall back to
            // the default.
            _ => CaseMapping::Rfc1459,
        }
    }

    #[inline]
    pub fn fold_byte(self, b: u8) -> u8 {
        match (self, b) {
            (_, b'A'..=b'Z') => b + 32,
            (CaseMapping::Rfc1459 | CaseMapping::StrictRfc1459, b'[' | b']' | b'\\') => b + 32,
            (CaseMapping::Rfc1459, b'~') => b'^',
            _ => b,
        }
    }

    /// Folds to the canonical lower-case form. Non-ASCII characters are left untouched.
    pub fn fold<'a>(self, s: &'a str) -> Cow<'a, str> {
        if s.bytes().all(|b| self.fold_byte(b) == b) {
            return Cow::Borrowed(s);
        }
        // Only ASCII bytes change, so the result is still valid UTF-8.
        let bytes: Vec<u8> = s.bytes().map(|b| self.fold_byte(b)).collect();
        Cow::Owned(String::from_utf8(bytes).expect("ascii folding preserves utf-8"))
    }

    pub fn eq(self, a: &str, b: &str) -> bool {
        a.len() == b.len() && a.bytes().zip(b.bytes()).all(|(x, y)| self.fold_byte(x) == self.fold_byte(y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding() {
        let m = CaseMapping::Rfc1459;
        assert!(m.eq("Nick[a]~", "nick{a}^"));
        assert!(!CaseMapping::Ascii.eq("a[", "a{"));
        assert!(CaseMapping::StrictRfc1459.eq("a[", "a{"));
        assert!(!CaseMapping::StrictRfc1459.eq("a~", "a^"));
        assert_eq!(m.fold("ÄbC"), "Äbc");
    }
}
