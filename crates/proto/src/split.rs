//! Splitting long outgoing messages so every relayed line fits the server's line limit.

use unicode_segmentation::UnicodeSegmentation;

/// Worst-case length of a relayed `nick!user@host` source when the host is unknown.
pub const DEFAULT_SOURCE_BUDGET: usize = 1 + 30 + 1 + 10 + 1 + 63;

/// Bytes available for the text of `PRIVMSG <target> :<text>` once the server prepends our source.
pub fn text_budget(linelen: usize, command: &str, target: &str, source_len: usize) -> usize {
    // ":" source " " command " " target " :" text "\r\n"
    let overhead = 1 + source_len + 1 + command.len() + 1 + target.len() + 2 + 2;
    linelen.saturating_sub(overhead).max(32)
}

/// Splits `text` into chunks of at most `max_bytes` bytes, preferring whitespace boundaries and
/// never breaking inside a grapheme cluster. Whitespace at a break is consumed.
pub fn split_text(text: &str, max_bytes: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while rest.len() > max_bytes {
        let mut cut = 0;
        let mut last_space = None;
        for (idx, g) in rest.grapheme_indices(true) {
            let end = idx + g.len();
            if end > max_bytes {
                break;
            }
            if g.chars().all(char::is_whitespace) {
                last_space = Some(idx);
            }
            cut = end;
        }
        if cut == 0 {
            // A single grapheme larger than the budget: fall back to a char boundary.
            cut = rest
                .char_indices()
                .map(|(i, c)| i + c.len_utf8())
                .take_while(|&e| e <= max_bytes)
                .last()
                .unwrap_or(rest.len());
            if cut == 0 {
                cut = rest.chars().next().map_or(rest.len(), char::len_utf8);
            }
        }
        match last_space {
            // Only break at a space if it doesn't make the chunk tiny.
            Some(sp) if sp > max_bytes / 2 => {
                out.push(&rest[..sp]);
                rest = rest[sp..].trim_start_matches(' ');
            }
            _ => {
                out.push(&rest[..cut]);
                rest = &rest[cut..];
            }
        }
    }
    if !rest.is_empty() || out.is_empty() {
        out.push(rest);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_passthrough() {
        assert_eq!(split_text("hello", 10), vec!["hello"]);
        assert_eq!(split_text("", 10), vec![""]);
    }

    #[test]
    fn word_boundaries() {
        assert_eq!(split_text("aaaa bbbb cccc", 10), vec!["aaaa bbbb", "cccc"]);
    }

    #[test]
    fn never_splits_graphemes() {
        let family = "👨‍👩‍👧‍👦"; // 25 bytes, one grapheme
        let text = family.repeat(3);
        for chunk in split_text(&text, 30) {
            assert!(chunk.len() <= 30);
            assert_eq!(chunk, family);
        }
    }

    #[test]
    fn budget() {
        let b = text_budget(512, "PRIVMSG", "#chan", DEFAULT_SOURCE_BUDGET);
        assert!(b < 512 - 100 && b > 300);
    }
}
