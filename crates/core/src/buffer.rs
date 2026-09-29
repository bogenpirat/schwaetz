//! Buffers (server, channel, query and special views) and their lines.

use schwaetz_net::NetworkId;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BufferId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferKind {
    /// The network's status/server buffer.
    Server,
    Channel,
    Query,
    /// Client-side views: "schwätz" (global status, also script output), "search", channel list …
    Special,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
/// What a buffer has had since it was last looked at. Only messages count (and errors): joins,
/// parts, topics and other events are shown but leave a buffer idle.
pub enum Activity {
    #[default]
    None,
    Messages,
    Highlight,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NotifyLevel {
    /// Highlights, and every message in queries.
    #[default]
    Default,
    All,
    HighlightsOnly,
    Mute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Message,
    Action,
    Notice,
    Join,
    Part,
    Quit,
    Kick,
    Nick,
    Mode,
    Topic,
    Invite,
    /// Client-generated information.
    Status,
    Error,
    /// Numerics and other server text.
    Server,
    Motd,
    Ctcp,
    /// Twitch USERNOTICE (subs, raids …) and similar system announcements.
    System,
    Netsplit,
}

impl LineKind {
    pub fn is_message(self) -> bool {
        matches!(self, LineKind::Message | LineKind::Action | LineKind::Notice)
    }

    pub fn is_membership(self) -> bool {
        matches!(self, LineKind::Join | LineKind::Part | LineKind::Quit | LineKind::Nick | LineKind::Netsplit)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineFlags(pub u16);

impl LineFlags {
    pub const OWN: u16 = 1;
    pub const HIGHLIGHT: u16 = 1 << 1;
    pub const HISTORY: u16 = 1 << 2;
    /// Removed by a moderator (Twitch CLEARMSG/CLEARCHAT) or redacted (IRCv3).
    pub const DELETED: u16 = 1 << 3;
    /// Hidden by the smart join/part filter.
    pub const FILTERED: u16 = 1 << 4;
    pub const BOT: u16 = 1 << 5;
    /// Twitch: the sender's first message in the channel.
    pub const FIRST_MESSAGE: u16 = 1 << 6;
    /// Message addressed only to channel members with a given status (STATUSMSG).
    pub const STATUSMSG: u16 = 1 << 7;
    /// Written by a script (scripts never receive these, so a script cannot loop on its own output).
    pub const SCRIPT: u16 = 1 << 8;

    pub fn has(self, f: u16) -> bool {
        self.0 & f != 0
    }

    pub fn set(&mut self, f: u16, on: bool) {
        if on {
            self.0 |= f;
        } else {
            self.0 &= !f;
        }
    }
}

/// Inline image placed over a character range (Twitch/third-party emotes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Emote {
    /// Byte range into the line's (formatting-stripped) text.
    pub start: u32,
    pub end: u32,
    pub url: String,
    pub name: String,
}

/// Adds an emote to a list ordered by position, unless it is empty or overlaps one already there;
/// returns whether it was added.
pub fn add_emote(list: &mut Vec<Emote>, e: Emote) -> bool {
    if e.start >= e.end || list.iter().any(|x| x.start < e.end && e.start < x.end) {
        return false;
    }
    let i = list.partition_point(|x| x.start < e.start);
    list.insert(i, e);
    true
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineExtra {
    pub msgid: Option<String>,
    /// Display name when it differs from the nick (Twitch `display-name`).
    pub display_name: Option<String>,
    /// Sender-chosen color (Twitch `color`), 0xRRGGBB.
    pub color: Option<u32>,
    /// Badge ids such as `moderator/1`, `subscriber/12`.
    pub badges: Vec<String>,
    pub emotes: Vec<Emote>,
    /// `+draft/reply`: (msgid, nick, excerpt) of the parent message.
    pub reply_to: Option<(String, String, String)>,
    /// `+draft/react`: reaction → nicks.
    pub reactions: Vec<(String, Vec<String>)>,
    pub account: Option<String>,
    /// STATUSMSG prefix (`@` for `@#chan`).
    pub status: Option<char>,
    /// The IRC line as received, tags included (live messages only; not kept in history).
    pub raw: Option<Box<str>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub id: u64,
    /// Unix milliseconds.
    pub time: i64,
    pub kind: LineKind,
    pub flags: LineFlags,
    /// Sender (messages) or subject (events); empty for server/status text.
    pub nick: Box<str>,
    /// Membership prefix of the sender at the time.
    pub prefix: Option<char>,
    /// Message text with mIRC formatting codes; plain text for events.
    pub text: Box<str>,
    pub extra: Option<Box<LineExtra>>,
}

impl Line {
    pub fn msgid(&self) -> Option<&str> {
        self.extra.as_ref()?.msgid.as_deref()
    }

    pub fn display_nick(&self) -> &str {
        self.extra.as_ref().and_then(|e| e.display_name.as_deref()).unwrap_or(&self.nick)
    }

    pub fn extra_mut(&mut self) -> &mut LineExtra {
        self.extra.get_or_insert_with(Default::default)
    }
}

#[derive(Clone, Debug, Default)]
pub struct InputState {
    pub history: Vec<String>,
    /// Index into `history` while browsing with Up/Down.
    pub pos: Option<usize>,
    /// Unsent text, kept when switching buffers.
    pub draft: String,
    pub cursor: usize,
}

impl InputState {
    pub const MAX_HISTORY: usize = 200;

    pub fn push(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        if self.history.last().map(String::as_str) != Some(line) {
            self.history.push(line.to_owned());
            if self.history.len() > Self::MAX_HISTORY {
                self.history.remove(0);
            }
        }
        self.pos = None;
    }

    /// Returns the previous entry (Up key). `current` is saved as the draft on first use.
    pub fn older(&mut self, current: &str) -> Option<&str> {
        if self.history.is_empty() {
            return None;
        }
        let pos = match self.pos {
            None => {
                self.draft = current.to_owned();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(p) => p - 1,
        };
        self.pos = Some(pos);
        Some(&self.history[pos])
    }

    /// Next entry (Down key); past the newest returns the saved draft.
    pub fn newer(&mut self) -> Option<&str> {
        let p = self.pos?;
        if p + 1 < self.history.len() {
            self.pos = Some(p + 1);
            Some(&self.history[p + 1])
        } else {
            self.pos = None;
            Some(&self.draft)
        }
    }
}

#[derive(Clone, Debug)]
pub struct Typing {
    pub nick: String,
    pub expires: i64,
    pub paused: bool,
}

#[derive(Debug)]
pub struct Buffer {
    pub id: BufferId,
    pub network: Option<NetworkId>,
    pub kind: BufferKind,
    pub name: String,
    pub lines: VecDeque<Line>,
    pub max_lines: usize,
    pub activity: Activity,
    pub unread: u32,
    pub highlights: u32,
    /// Read marker (Unix ms): lines newer than this are unread.
    pub read_marker: Option<i64>,
    /// Channel: we are currently joined. Query: the peer is known to be online.
    pub joined: bool,
    pub notify: NotifyLevel,
    pub input: InputState,
    pub typing: Vec<Typing>,
    /// Case-folded nick → last time they spoke (smart filter, completion ordering).
    pub last_spoke: HashMap<String, i64>,
    /// Time of the newest live line; used for history gap-fill after reconnects.
    pub last_seen: i64,
    /// Oldest line time known to exist in history (for scroll-back paging).
    pub history_exhausted: bool,
    pub history_loading: bool,
    /// Twitch ROOMSTATE tags (slow, subs-only …).
    pub room_state: Vec<(String, String)>,
    /// Twitch: stream state from the Helix API (shown in place of a topic).
    pub stream: Option<crate::helix::StreamInfo>,
    /// Twitch: the channel's user id (ROOMSTATE `room-id`).
    pub room_id: Option<String>,
    recent_ids: VecDeque<String>,
    recent_set: HashSet<String>,
    /// Bumped on every change; the UI compares it to decide what to redraw.
    pub generation: u64,
}

const DEDUPE_WINDOW: usize = 1024;

impl Buffer {
    pub fn new(id: BufferId, network: Option<NetworkId>, kind: BufferKind, name: &str, max_lines: usize) -> Buffer {
        Buffer {
            id,
            network,
            kind,
            name: name.to_owned(),
            lines: VecDeque::new(),
            max_lines: max_lines.max(100),
            activity: Activity::None,
            unread: 0,
            highlights: 0,
            read_marker: None,
            joined: false,
            notify: NotifyLevel::Default,
            input: InputState::default(),
            typing: Vec::new(),
            last_spoke: HashMap::new(),
            last_seen: 0,
            history_exhausted: false,
            history_loading: false,
            room_state: Vec::new(),
            stream: None,
            room_id: None,
            recent_ids: VecDeque::new(),
            recent_set: HashSet::new(),
            generation: 0,
        }
    }

    pub fn is_channel(&self) -> bool {
        self.kind == BufferKind::Channel
    }

    /// Returns true if this msgid was already seen (duplicate from history/playback).
    pub fn seen_msgid(&mut self, msgid: &str) -> bool {
        if self.recent_set.contains(msgid) {
            return true;
        }
        self.recent_set.insert(msgid.to_owned());
        self.recent_ids.push_back(msgid.to_owned());
        if self.recent_ids.len() > DEDUPE_WINDOW
            && let Some(old) = self.recent_ids.pop_front()
        {
            self.recent_set.remove(&old);
        }
        false
    }

    /// Appends a line in time order. History lines older than the newest line are inserted at
    /// their chronological position.
    pub fn push(&mut self, line: Line) {
        let pos = if self.lines.back().is_none_or(|l| l.time <= line.time) {
            self.lines.len()
        } else {
            self.lines.partition_point(|l| l.time <= line.time)
        };
        self.lines.insert(pos, line);
        while self.lines.len() > self.max_lines {
            self.lines.pop_front();
            self.history_exhausted = false;
        }
        self.generation += 1;
    }

    /// Duplicate check for lines without msgid: same time, nick and text.
    pub fn has_equivalent(&self, time: i64, nick: &str, text: &str) -> bool {
        let start = self.lines.partition_point(|l| l.time < time);
        self.lines.range(start..).take_while(|l| l.time == time).any(|l| &*l.nick == nick && &*l.text == text)
    }

    pub fn find_msgid_mut(&mut self, msgid: &str) -> Option<&mut Line> {
        self.lines.iter_mut().rev().find(|l| l.msgid() == Some(msgid))
    }

    pub fn mark_read(&mut self) {
        self.activity = Activity::None;
        self.unread = 0;
        self.highlights = 0;
        if let Some(t) = self.lines.back().map(|l| l.time) {
            self.read_marker = Some(self.read_marker.map_or(t, |m| m.max(t)));
        }
        self.generation += 1;
    }

    /// The sidebar badge in a badge mode ("all", "highlights" or "none"): the count to show and
    /// whether it includes highlights.
    pub fn badge(&self, mode: &str) -> Option<(u32, bool)> {
        match mode {
            "none" => None,
            "highlights" => (self.highlights > 0).then_some((self.highlights, true)),
            _ => (self.unread > 0).then_some((self.unread, self.highlights > 0)),
        }
    }

    pub fn bump(&mut self, activity: Activity) {
        if activity > self.activity {
            self.activity = activity;
        }
        self.generation += 1;
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.generation += 1;
    }

    pub fn typing_nicks(&self, now: i64) -> Vec<&str> {
        self.typing.iter().filter(|t| t.expires > now && !t.paused).map(|t| t.nick.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emotes_stay_ordered_without_overlaps() {
        let e = |start, end| Emote { start, end, url: String::new(), name: String::new() };
        let mut list = vec![e(10, 14)];
        assert!(add_emote(&mut list, e(0, 4)));
        assert!(!add_emote(&mut list, e(12, 20)), "overlaps");
        assert!(!add_emote(&mut list, e(5, 5)), "empty");
        assert!(add_emote(&mut list, e(5, 9)));
        assert_eq!(list.iter().map(|e| e.start).collect::<Vec<_>>(), [0, 5, 10]);
    }

    fn line(id: u64, time: i64) -> Line {
        Line {
            id,
            time,
            kind: LineKind::Message,
            flags: LineFlags::default(),
            nick: "n".into(),
            prefix: None,
            text: "t".into(),
            extra: None,
        }
    }

    #[test]
    fn badge_modes() {
        let mut b = Buffer::new(BufferId(1), None, BufferKind::Channel, "#c", 100);
        assert_eq!(b.badge("all"), None);
        b.unread = 5;
        assert_eq!(b.badge("all"), Some((5, false)));
        assert_eq!(b.badge("highlights"), None);
        assert_eq!(b.badge("none"), None);
        b.highlights = 2;
        assert_eq!(b.badge("all"), Some((5, true)));
        assert_eq!(b.badge("highlights"), Some((2, true)));
        assert_eq!(b.badge("none"), None);
    }

    #[test]
    fn push_keeps_time_order_and_bounds() {
        let mut b = Buffer::new(BufferId(1), None, BufferKind::Channel, "#c", 100);
        for i in 0..150 {
            b.push(line(i, 1000 + i as i64 * 10));
        }
        assert_eq!(b.lines.len(), 100);
        b.push(line(999, 1995)); // history line between existing ones
        let times: Vec<i64> = b.lines.iter().map(|l| l.time).collect();
        assert!(times.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn input_history() {
        let mut i = InputState::default();
        i.push("one");
        i.push("two");
        assert_eq!(i.older("draft"), Some("two"));
        assert_eq!(i.older(""), Some("one"));
        assert_eq!(i.older(""), Some("one"));
        assert_eq!(i.newer(), Some("two"));
        assert_eq!(i.newer(), Some("draft"));
    }

    #[test]
    fn dedupe() {
        let mut b = Buffer::new(BufferId(1), None, BufferKind::Channel, "#c", 100);
        assert!(!b.seen_msgid("a"));
        assert!(b.seen_msgid("a"));
    }
}
