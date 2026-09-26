//! IRCv3 message tags (<https://ircv3.net/specs/extensions/message-tags>).

use std::fmt::Write as _;

/// An ordered set of message tags. Keys are unique; a tag without a value is stored with an empty
/// value, since the specification treats a missing and an empty value as equivalent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tags(Vec<(String, String)>);

impl Tags {
    pub const fn new() -> Self {
        Tags(Vec::new())
    }

    /// Parses the raw tag section (without the leading `@`). Later duplicates win.
    pub fn parse(raw: &str) -> Self {
        let mut tags = Tags(Vec::with_capacity(raw.bytes().filter(|&b| b == b';').count() + 1));
        for item in raw.split(';') {
            if item.is_empty() {
                continue;
            }
            let (key, value) = match item.split_once('=') {
                Some((k, v)) => (k, unescape(v)),
                None => (item, String::new()),
            };
            if key.is_empty() {
                continue;
            }
            tags.insert(key, value);
        }
        tags
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// Returns the value of `key` if it is present and non-empty.
    pub fn value(&self, key: &str) -> Option<&str> {
        self.get(key).filter(|v| !v.is_empty())
    }

    pub fn contains(&self, key: &str) -> bool {
        self.0.iter().any(|(k, _)| k == key)
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        match self.0.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => self.0.push((key, value)),
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<String> {
        let idx = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(idx).1)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Client-only tags are prefixed with `+`.
    pub fn client_only(&self) -> impl Iterator<Item = (&str, &str)> {
        self.iter().filter(|(k, _)| k.starts_with('+'))
    }

    /// Writes the tag section without the leading `@` or trailing space.
    pub fn write_to(&self, out: &mut String) {
        for (i, (k, v)) in self.0.iter().enumerate() {
            if i > 0 {
                out.push(';');
            }
            out.push_str(k);
            if !v.is_empty() {
                out.push('=');
                escape_into(v, out);
            }
        }
    }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for Tags {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut tags = Tags::new();
        for (k, v) in iter {
            tags.insert(k, v);
        }
        tags
    }
}

impl std::fmt::Display for Tags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = String::new();
        self.write_to(&mut s);
        f.write_str(&s)
    }
}

/// Unescapes a tag value. Invalid escapes drop the backslash; a trailing lone backslash is dropped.
pub fn unescape(value: &str) -> String {
    if !value.contains('\\') {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(':') => out.push(';'),
            Some('s') => out.push(' '),
            Some('\\') => out.push('\\'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

pub fn escape_into(value: &str, out: &mut String) {
    for c in value.chars() {
        match c {
            ';' => out.push_str("\\:"),
            ' ' => out.push_str("\\s"),
            '\\' => out.push_str("\\\\"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
}

pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    escape_into(value, &mut out);
    out
}

/// Formats an IRCv3 `server-time` timestamp (`YYYY-MM-DDThh:mm:ss.sssZ`) from Unix milliseconds.
pub fn format_server_time(unix_ms: i64) -> String {
    let secs = unix_ms.div_euclid(1000);
    let ms = unix_ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let mut s = String::with_capacity(24);
    let _ = write!(s, "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z", rem / 3600, (rem / 60) % 60, rem % 60);
    s
}

/// Parses an IRCv3 `server-time` / ISO-8601 UTC timestamp into Unix milliseconds.
/// Accepts an optional fractional part of any precision and a `Z` or `+00:00` suffix.
pub fn parse_server_time(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> { s.get(r)?.parse().ok() };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut ms = 0i64;
    let mut rest = &s[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        let mut scale = 100;
        for c in frac[..digits].bytes().take(3) {
            ms += i64::from(c - b'0') * scale;
            scale /= 10;
        }
        rest = &frac[digits..];
    }
    let offset_secs = match rest {
        "" | "Z" | "z" => 0,
        tz if tz.len() == 6 && (tz.starts_with('+') || tz.starts_with('-')) => {
            let sign = if tz.starts_with('-') { -1 } else { 1 };
            let oh: i64 = tz.get(1..3)?.parse().ok()?;
            let om: i64 = tz.get(4..6)?.parse().ok()?;
            sign * (oh * 3600 + om * 60)
        }
        _ => return None,
    };
    let days = days_from_civil(y, mo, d);
    Some((days * 86_400 + h * 3600 + mi * 60 + sec - offset_secs) * 1000 + ms)
}

// Howard Hinnant's civil date algorithms.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_roundtrip() {
        let v = "a;b c\\d\r\n";
        assert_eq!(unescape(&escape(v)), v);
    }

    #[test]
    fn server_time_roundtrip() {
        let t = parse_server_time("2023-11-14T22:13:20.123Z").unwrap();
        assert_eq!(t, 1_700_000_000_123);
        assert_eq!(format_server_time(t), "2023-11-14T22:13:20.123Z");
        assert_eq!(parse_server_time("2023-11-14T22:13:20Z"), Some(1_700_000_000_000));
        assert_eq!(parse_server_time("2023-11-14T23:13:20.5+01:00"), Some(1_700_000_000_500));
        assert_eq!(parse_server_time("garbage"), None);
    }
}
