//! Tab completion for nicks, channels, commands and `:emoji:` shortcodes.

pub struct Candidates<'a> {
    /// Ordered by preference (most recent speakers first).
    pub nicks: &'a [String],
    pub channels: &'a [String],
    pub commands: &'a [String],
    /// `@nick` completes to `@nick` (Twitch style) rather than to the nick alone.
    pub at_mentions: bool,
}

struct State {
    /// Input as it was after our last completion (to detect continued cycling).
    produced: String,
    word_start: usize,
    prefix_input: String,
    suffix_input: String,
    options: Vec<String>,
    index: usize,
    at_line_start: bool,
}

#[derive(Default)]
pub struct Completer {
    state: Option<State>,
}

pub const NICK_SUFFIX: &str = ": ";

impl Completer {
    pub fn reset(&mut self) {
        self.state = None;
    }

    /// Completes the word before `cursor` (byte offset). Returns the new input and cursor.
    pub fn complete(
        &mut self,
        input: &str,
        cursor: usize,
        c: &Candidates<'_>,
        backwards: bool,
    ) -> Option<(String, usize)> {
        if let Some(st) = self.state.as_mut().filter(|s| s.produced == input) {
            let n = st.options.len();
            st.index = if backwards { (st.index + n - 1) % n } else { (st.index + 1) % n };
            return Some(Self::apply(st));
        }
        let cursor = cursor.min(input.len());
        let before = &input[..cursor];
        let word_start =
            before.rfind(char::is_whitespace).map_or(0, |i| i + before[i..].chars().next().unwrap().len_utf8());
        let word = &before[word_start..];
        if word.is_empty() {
            return None;
        }
        let lw = word.to_lowercase();
        let at_line_start = word_start == 0;
        let options: Vec<String> = if at_line_start && word.starts_with('/') {
            let w = &lw[1..];
            c.commands.iter().filter(|cmd| cmd.to_lowercase().starts_with(w)).map(|cmd| format!("/{cmd}")).collect()
        } else if word.starts_with(':') && word.len() >= 3 {
            let w = &lw[1..];
            EMOJI.iter().filter(|(name, _)| name.starts_with(w)).map(|(_, e)| (*e).to_owned()).collect()
        } else if word.starts_with(['#', '&']) {
            c.channels.iter().filter(|ch| ch.to_lowercase().starts_with(&lw)).cloned().collect()
        } else if let Some(w) = lw.strip_prefix('@').filter(|_| c.at_mentions) {
            // Mentions stay mentions: `@nick` and a space, even at the start of the line.
            c.nicks.iter().filter(|n| n.to_lowercase().starts_with(w)).map(|n| format!("@{n}")).collect()
        } else {
            let w = lw.trim_start_matches('@');
            c.nicks.iter().filter(|n| n.to_lowercase().starts_with(w)).cloned().collect()
        };
        if options.is_empty() {
            return None;
        }
        let mut st = State {
            produced: String::new(),
            word_start,
            prefix_input: input[..word_start].to_owned(),
            suffix_input: input[cursor..].to_owned(),
            index: if backwards { options.len() - 1 } else { 0 },
            options,
            at_line_start,
        };
        let r = Self::apply(&mut st);
        self.state = Some(st);
        Some(r)
    }

    fn apply(st: &mut State) -> (String, usize) {
        let choice = &st.options[st.index];
        let is_nick = !choice.starts_with(['/', '#', '&'])
            && choice.chars().next().is_some_and(|c| !c.is_ascii_punctuation() || "[]\\`^{}|_".contains(c));
        let is_emoji = !choice.is_ascii();
        let suffix = if st.at_line_start && is_nick && !is_emoji {
            NICK_SUFFIX
        } else if st.suffix_input.starts_with(' ') {
            ""
        } else {
            " "
        };
        let mut out = String::with_capacity(st.prefix_input.len() + choice.len() + 2 + st.suffix_input.len());
        out.push_str(&st.prefix_input);
        out.push_str(choice);
        out.push_str(suffix);
        let cursor = out.len();
        out.push_str(&st.suffix_input);
        debug_assert!(st.word_start <= st.prefix_input.len());
        st.produced = out.clone();
        (out, cursor)
    }
}

/// A compact set of common emoji shortcodes.
pub const EMOJI: &[(&str, &str)] = &[
    ("+1", "👍"),
    ("-1", "👎"),
    ("100", "💯"),
    ("angry", "😠"),
    ("beer", "🍺"),
    ("blush", "😊"),
    ("bug", "🐛"),
    ("check", "✅"),
    ("clap", "👏"),
    ("coffee", "☕"),
    ("confused", "😕"),
    ("cool", "😎"),
    ("cry", "😢"),
    ("eyes", "👀"),
    ("facepalm", "🤦"),
    ("fire", "🔥"),
    ("grin", "😁"),
    ("heart", "❤️"),
    ("hug", "🤗"),
    ("joy", "😂"),
    ("kiss", "😘"),
    ("laughing", "😆"),
    ("lol", "😂"),
    ("ok", "👌"),
    ("party", "🥳"),
    ("pray", "🙏"),
    ("rocket", "🚀"),
    ("rofl", "🤣"),
    ("sad", "😞"),
    ("scream", "😱"),
    ("see_no_evil", "🙈"),
    ("shrug", "🤷"),
    ("skull", "💀"),
    ("sleeping", "😴"),
    ("smile", "😄"),
    ("smirk", "😏"),
    ("sob", "😭"),
    ("sparkles", "✨"),
    ("star", "⭐"),
    ("sunglasses", "😎"),
    ("sweat_smile", "😅"),
    ("tada", "🎉"),
    ("thinking", "🤔"),
    ("thumbsdown", "👎"),
    ("thumbsup", "👍"),
    ("tongue", "😛"),
    ("upside_down", "🙃"),
    ("warning", "⚠️"),
    ("wave", "👋"),
    ("wink", "😉"),
    ("x", "❌"),
    ("zap", "⚡"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn cands() -> (Vec<String>, Vec<String>, Vec<String>) {
        (
            vec!["alice".into(), "Albert".into(), "bob".into()],
            vec!["#rust".into(), "#rustaceans".into()],
            vec!["join".into(), "part".into(), "jump".into()],
        )
    }

    #[test]
    fn nick_at_start_and_cycling() {
        let (n, ch, cmd) = cands();
        let c = Candidates { nicks: &n, channels: &ch, commands: &cmd, at_mentions: false };
        let mut comp = Completer::default();
        let (s, cur) = comp.complete("al", 2, &c, false).unwrap();
        assert_eq!((s.as_str(), cur), ("alice: ", 7));
        let (s, _) = comp.complete(&s, cur, &c, false).unwrap();
        assert_eq!(s, "Albert: ");
        let (s, _) = comp.complete(&s, 8, &c, false).unwrap();
        assert_eq!(s, "alice: ");
    }

    #[test]
    fn at_mentions_keep_their_form_where_used() {
        let (n, ch, cmd) = cands();
        // IRC: the `@` goes, as usual for addressing someone.
        let irc = Candidates { nicks: &n, channels: &ch, commands: &cmd, at_mentions: false };
        let mut comp = Completer::default();
        assert_eq!(comp.complete("@al", 3, &irc, false).unwrap(), ("alice: ".to_owned(), 7));
        let c = Candidates { at_mentions: true, ..irc };
        let mut comp = Completer::default();
        let (s, cur) = comp.complete("@al", 3, &c, false).unwrap();
        assert_eq!((s.as_str(), cur), ("@alice ", 7));
        let (s, cur) = comp.complete(&s, cur, &c, false).unwrap();
        assert_eq!((s.as_str(), cur), ("@Albert ", 8));
        let (s, cur) = comp.complete(&s, cur, &c, false).unwrap();
        assert_eq!(s, "@alice ");
        let (s, _) = comp.complete(&s, cur, &c, true).unwrap();
        assert_eq!(s, "@Albert ", "backwards too");
        comp.reset();
        assert_eq!(comp.complete("hi @BO there", 6, &c, false).unwrap(), ("hi @bob there".to_owned(), 7));
        comp.reset();
        assert!(comp.complete("@zz", 3, &c, false).is_none());
    }

    #[test]
    fn mid_line_and_suffix_preserved() {
        let (n, ch, cmd) = cands();
        let c = Candidates { nicks: &n, channels: &ch, commands: &cmd, at_mentions: false };
        let mut comp = Completer::default();
        let (s, cur) = comp.complete("hi bo there", 5, &c, false).unwrap();
        assert_eq!(s, "hi bob there");
        assert_eq!(cur, 6);
    }

    #[test]
    fn commands_channels_emoji() {
        let (n, ch, cmd) = cands();
        let c = Candidates { nicks: &n, channels: &ch, commands: &cmd, at_mentions: false };
        let mut comp = Completer::default();
        assert_eq!(comp.complete("/ju", 3, &c, false).unwrap().0, "/jump ");
        comp.reset();
        assert_eq!(comp.complete("join #rustac", 12, &c, false).unwrap().0, "join #rustaceans ");
        comp.reset();
        assert_eq!(comp.complete("nice :tad", 9, &c, false).unwrap().0, "nice 🎉 ");
        comp.reset();
        assert!(comp.complete("zzz", 3, &c, false).is_none());
    }
}
