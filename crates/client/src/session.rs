//! The per-network session state machine. It performs no I/O: feed it parsed server messages and
//! clock ticks, then drain outgoing messages and events.

use crate::event::{Chat, ChatKind, Event, SessionEvent, StandardReplyKind, Target, TwitchEvent};
use crate::sasl::{self, ChallengeBuffer, SaslConfig, SaslSession};
use crate::state::{self, Channel, Member, ModeListEntry, User, WhoisInfo};
use schwaetz_proto::isupport::ModeType;
use schwaetz_proto::numeric::*;
use schwaetz_proto::split::{self, DEFAULT_SOURCE_BUDGET};
use schwaetz_proto::tags::parse_server_time;
use schwaetz_proto::{CaseMapping, ISupport, Message, Source, Tags, ctcp};
use std::collections::{BTreeSet, HashMap, VecDeque};

/// Capabilities requested when offered. Vendor/legacy variants are only requested when the
/// standard one is absent (see [`Session::choose_caps`]).
pub const WANTED_CAPS: &[&str] = &[
    "account-notify",
    "account-tag",
    "away-notify",
    "batch",
    "cap-notify",
    "chghost",
    "echo-message",
    "extended-join",
    "invite-notify",
    "labeled-response",
    "message-tags",
    "multi-prefix",
    "sasl",
    "server-time",
    "setname",
    "userhost-in-names",
    "draft/chathistory",
    "draft/event-playback",
    "draft/read-marker",
    "draft/multiline",
    "draft/message-redaction",
    "draft/channel-rename",
    "draft/extended-monitor",
    "extended-monitor",
    "draft/pre-away",
    "soju.im/bouncer-networks",
    "soju.im/bouncer-networks-notify",
    "znc.in/self-message",
    "znc.in/playback",
    "znc.in/server-time-iso",
    "znc.in/batch",
];

pub const TWITCH_CAPS: &[&str] = &["twitch.tv/tags", "twitch.tv/commands", "twitch.tv/membership"];

/// WHOX token used for our own automatic channel WHO queries.
const WHOX_TOKEN: &str = "742";

#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub nick: String,
    pub alt_nicks: Vec<String>,
    pub username: String,
    pub realname: String,
    /// Server password (`PASS`). For ZNC: `user/network:password`; for Twitch: `oauth:token`.
    pub password: Option<String>,
    pub sasl: Option<SaslConfig>,
    /// Disconnect instead of continuing unauthenticated when SASL fails.
    pub sasl_required: bool,
    /// Channels (name, key) joined on the first successful connection.
    pub autojoin: Vec<(String, Option<String>)>,
    pub twitch: bool,
    /// Request `draft/no-implicit-names` (member lists are then fetched lazily).
    pub lazy_names: bool,
    pub extra_caps: Vec<String>,
    pub disabled_caps: Vec<String>,
    /// Whether the current transport is TLS (affects STS handling).
    pub tls: bool,
    pub port: u16,
    /// CTCP VERSION string.
    pub version: String,
    /// soju: bind this connection to one upstream network (`BOUNCER BIND`).
    pub bouncer_netid: Option<String>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            nick: "schwaetzer".into(),
            alt_nicks: Vec::new(),
            username: "schwaetz".into(),
            realname: "schwätz user".into(),
            password: None,
            sasl: None,
            sasl_required: false,
            autojoin: Vec::new(),
            twitch: false,
            lazy_names: false,
            extra_caps: Vec::new(),
            disabled_caps: Vec::new(),
            tls: true,
            port: 6697,
            version: format!("schwätz {}", env!("CARGO_PKG_VERSION")),
            bouncer_netid: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Disconnected,
    Registering,
    Registered,
    Ready,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Live,
    History,
}

struct Batch {
    kind: String,
    params: Vec<String>,
    tags: Tags,
    parent: Option<String>,
    items: Vec<BatchItem>,
}

enum BatchItem {
    Msg(Message),
    Batch(Batch),
}

pub struct Session {
    cfg: SessionConfig,
    phase: Phase,
    nick: String,
    nick_attempt: usize,
    self_user: Option<String>,
    self_host: Option<String>,
    caps_available: HashMap<String, Option<String>>,
    caps_enabled: BTreeSet<String>,
    cap_req_pending: usize,
    cap_ls_done: bool,
    cap_end_sent: bool,
    registering_since: i64,
    sasl: Option<SaslSession>,
    sasl_buf: ChallengeBuffer,
    sasl_active: bool,
    account: Option<String>,
    isupport: ISupport,
    channels: HashMap<String, Channel>,
    users: HashMap<String, User>,
    whois: HashMap<String, WhoisInfo>,
    mode_lists: HashMap<(String, char), Vec<ModeListEntry>>,
    batches: HashMap<String, Batch>,
    outgoing: VecDeque<Message>,
    events: VecDeque<SessionEvent>,
    next_label: u64,
    next_batch: u64,
    first_connect: bool,
    rejoin: Vec<(String, Option<String>)>,
    away: Option<String>,
    last_ison: i64,
    whox_pending: BTreeSet<String>,
}

impl Session {
    pub fn new(cfg: SessionConfig) -> Session {
        Session {
            nick: cfg.nick.clone(),
            cfg,
            phase: Phase::Disconnected,
            nick_attempt: 0,
            self_user: None,
            self_host: None,
            caps_available: HashMap::new(),
            caps_enabled: BTreeSet::new(),
            cap_req_pending: 0,
            cap_ls_done: false,
            cap_end_sent: false,
            registering_since: 0,
            sasl: None,
            sasl_buf: ChallengeBuffer::default(),
            sasl_active: false,
            account: None,
            isupport: ISupport::default(),
            channels: HashMap::new(),
            users: HashMap::new(),
            whois: HashMap::new(),
            mode_lists: HashMap::new(),
            batches: HashMap::new(),
            outgoing: VecDeque::new(),
            events: VecDeque::new(),
            next_label: 0,
            next_batch: 0,
            first_connect: true,
            rejoin: Vec::new(),
            away: None,
            last_ison: 0,
            whox_pending: BTreeSet::new(),
        }
    }

    // ----- accessors ---------------------------------------------------------------------------

    pub fn config(&self) -> &SessionConfig {
        &self.cfg
    }

    pub fn config_mut(&mut self) -> &mut SessionConfig {
        &mut self.cfg
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn nick(&self) -> &str {
        &self.nick
    }

    pub fn account(&self) -> Option<&str> {
        self.account.as_deref()
    }

    pub fn isupport(&self) -> &ISupport {
        &self.isupport
    }

    pub fn casemapping(&self) -> CaseMapping {
        self.isupport.casemapping
    }

    pub fn fold(&self, s: &str) -> String {
        self.isupport.casemapping.fold(s).into_owned()
    }

    pub fn is_me(&self, nick: &str) -> bool {
        self.isupport.casemapping.eq(nick, &self.nick)
    }

    pub fn is_channel(&self, name: &str) -> bool {
        self.isupport.is_channel(name)
    }

    pub fn has_cap(&self, cap: &str) -> bool {
        self.caps_enabled.contains(cap)
    }

    pub fn caps(&self) -> impl Iterator<Item = &str> {
        self.caps_enabled.iter().map(String::as_str)
    }

    pub fn available_caps(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.caps_available.iter().map(|(k, v)| (k.as_str(), v.as_deref()))
    }

    pub fn channel(&self, name: &str) -> Option<&Channel> {
        self.channels.get(&self.fold(name))
    }

    pub fn channels(&self) -> impl Iterator<Item = &Channel> {
        self.channels.values()
    }

    pub fn user(&self, nick: &str) -> Option<&User> {
        self.users.get(&self.fold(nick))
    }

    pub fn away_message(&self) -> Option<&str> {
        self.away.as_deref()
    }

    /// True when history is available via CHATHISTORY.
    pub fn supports_chathistory(&self) -> bool {
        self.has_cap("draft/chathistory") || self.isupport.chathistory.is_some() && self.has_cap("batch")
    }

    pub fn is_znc(&self) -> bool {
        self.caps_available.keys().any(|c| c.starts_with("znc.in/"))
    }

    // ----- I/O boundary ------------------------------------------------------------------------

    pub fn poll_outgoing(&mut self) -> Option<Message> {
        self.outgoing.pop_front()
    }

    pub fn poll_event(&mut self) -> Option<SessionEvent> {
        self.events.pop_front()
    }

    pub fn drain_outgoing(&mut self) -> impl Iterator<Item = Message> + '_ {
        self.outgoing.drain(..)
    }

    pub fn drain_events(&mut self) -> impl Iterator<Item = SessionEvent> + '_ {
        self.events.drain(..)
    }

    fn emit(&mut self, time: i64, kind: Event) {
        self.events.push_back(SessionEvent { time, label: None, kind });
    }

    fn status(&mut self, now: i64, text: impl Into<String>) {
        self.emit(now, Event::Status(text.into()));
    }

    pub fn send(&mut self, msg: Message) {
        self.outgoing.push_back(msg);
    }

    /// Sends with a `label` tag when `labeled-response` is enabled; returns the label.
    pub fn send_labeled(&mut self, mut msg: Message) -> Option<String> {
        if !self.has_cap("labeled-response") {
            self.send(msg);
            return None;
        }
        self.next_label += 1;
        let label = format!("s{}", self.next_label);
        msg.tags.insert("label", label.clone());
        self.send(msg);
        Some(label)
    }

    /// Called once the transport is connected; starts registration.
    pub fn on_connect(&mut self, now: i64) {
        self.reset_connection_state();
        self.phase = Phase::Registering;
        self.registering_since = now;
        self.nick = self.cfg.nick.clone();
        if self.cfg.twitch {
            self.send(Message::new("CAP", ["REQ".to_owned(), TWITCH_CAPS.join(" ")]));
            self.cap_req_pending += 1;
            self.cap_ls_done = true;
            self.cap_end_sent = true; // Twitch needs no CAP END.
        } else {
            self.send(Message::new("CAP", ["LS", "302"]));
        }
        if let Some(pass) = self.cfg.password.clone() {
            self.send(Message::new("PASS", [pass]));
        }
        self.send(Message::new("NICK", [self.nick.clone()]));
        if !self.cfg.twitch {
            self.send(Message::new(
                "USER",
                [self.cfg.username.clone(), "0".into(), "*".into(), self.cfg.realname.clone()],
            ));
        }
    }

    /// Called when the transport closes. Joined channels are remembered for rejoin.
    pub fn on_disconnect(&mut self) {
        if self.phase == Phase::Ready || self.phase == Phase::Registered {
            self.rejoin = self.channels.values().map(|c| (c.name.clone(), c.key.clone())).collect();
            self.first_connect = false;
        }
        self.phase = Phase::Disconnected;
        self.reset_connection_state();
    }

    fn reset_connection_state(&mut self) {
        self.caps_available.clear();
        self.caps_enabled.clear();
        self.cap_req_pending = 0;
        self.cap_ls_done = false;
        self.cap_end_sent = false;
        self.sasl = None;
        self.sasl_active = false;
        self.sasl_buf = ChallengeBuffer::default();
        self.account = None;
        self.isupport = ISupport::default();
        self.channels.clear();
        self.users.clear();
        self.whois.clear();
        self.mode_lists.clear();
        self.batches.clear();
        self.outgoing.clear();
        self.nick_attempt = 0;
        self.whox_pending.clear();
    }

    /// Periodic housekeeping; call about once per second.
    pub fn tick(&mut self, now: i64) {
        if self.phase == Phase::Registering && !self.cap_end_sent && now - self.registering_since > 15_000 {
            // A server that never answers CAP (or a stuck SASL exchange): finish registration.
            self.end_cap(now);
        }
        if self.phase == Phase::Ready
            && !self.is_me(&self.cfg.nick.clone())
            && self.isupport.monitor.is_none()
            && now - self.last_ison > 60_000
        {
            self.last_ison = now;
            self.send(Message::new("ISON", [self.cfg.nick.clone()]));
        }
    }

    // ----- inbound ----------------------------------------------------------------------------

    pub fn on_message(&mut self, msg: Message, now: i64) {
        // Collect messages that belong to an open batch.
        if let Some(r) = msg.tags.get("batch").map(str::to_owned)
            && self.batches.contains_key(&r)
        {
            if msg.is("BATCH") && msg.arg(0).starts_with('+') {
                self.start_batch(&msg, Some(r));
            } else if let Some(b) = self.batches.get_mut(&r) {
                b.items.push(BatchItem::Msg(msg));
            }
            return;
        }
        if msg.is("BATCH") {
            let reference = msg.arg(0);
            if reference.starts_with('+') {
                self.start_batch(&msg, None);
            } else if let Some(r) = reference.strip_prefix('-') {
                self.end_batch(r, now);
            }
            return;
        }
        let label = msg.tags.get("label").map(str::to_owned);
        let mut out = Vec::new();
        self.handle(&msg, now, Mode::Live, &mut out);
        for mut ev in out {
            if ev.label.is_none() {
                ev.label.clone_from(&label);
            }
            self.events.push_back(ev);
        }
    }

    fn start_batch(&mut self, msg: &Message, parent: Option<String>) {
        let reference = msg.arg(0)[1..].to_owned();
        let batch = Batch {
            kind: msg.arg(1).to_owned(),
            params: msg.params.iter().skip(2).cloned().collect(),
            tags: msg.tags.clone(),
            parent,
            items: Vec::new(),
        };
        self.batches.insert(reference, batch);
    }

    fn end_batch(&mut self, reference: &str, now: i64) {
        let Some(batch) = self.batches.remove(reference) else { return };
        if let Some(parent) = batch.parent.clone()
            && let Some(p) = self.batches.get_mut(&parent)
        {
            p.items.push(BatchItem::Batch(batch));
            return;
        }
        let mut out = Vec::new();
        self.finish_batch(batch, now, Mode::Live, None, &mut out);
        self.events.extend(out);
    }

    fn finish_batch(&mut self, batch: Batch, now: i64, mode: Mode, label: Option<String>, out: &mut Vec<SessionEvent>) {
        let kind = batch.kind.as_str();
        match kind {
            "chathistory" | "znc.in/playback" => {
                let target = batch.params.first().cloned().unwrap_or_default();
                let mut events = Vec::new();
                for item in batch.items {
                    match item {
                        BatchItem::Msg(m) => self.handle(&m, now, Mode::History, &mut events),
                        BatchItem::Batch(b) => self.finish_batch(b, now, Mode::History, None, &mut events),
                    }
                }
                out.push(SessionEvent { time: now, label, kind: Event::History { target, events } });
            }
            "draft/multiline" => {
                let target = batch.params.first().cloned().unwrap_or_default();
                let mut text = String::new();
                let mut first: Option<Message> = None;
                for item in batch.items {
                    let BatchItem::Msg(m) = item else { continue };
                    if !(m.is("PRIVMSG") || m.is("NOTICE")) {
                        continue;
                    }
                    if first.is_some() && !m.tags.contains("draft/multiline-concat") {
                        text.push('\n');
                    }
                    text.push_str(m.arg(1));
                    first.get_or_insert(m);
                }
                if let Some(mut m) = first {
                    // The batch carries msgid/time for the combined message.
                    for (k, v) in batch.tags.iter() {
                        if k != "batch" {
                            m.tags.insert(k, v);
                        }
                    }
                    m.tags.remove("batch");
                    m.params = vec![target, text];
                    self.handle(&m, now, mode, out);
                }
            }
            _ => {
                let label = label.or_else(|| batch.tags.get("label").map(str::to_owned));
                let netsplit = kind == "netsplit";
                for item in batch.items {
                    let mut evs = Vec::new();
                    match item {
                        BatchItem::Msg(m) => self.handle(&m, now, mode, &mut evs),
                        BatchItem::Batch(b) => self.finish_batch(b, now, mode, label.clone(), &mut evs),
                    }
                    for mut ev in evs {
                        if let Event::Quit { netsplit: ns, .. } = &mut ev.kind {
                            *ns |= netsplit;
                        }
                        if ev.label.is_none() {
                            ev.label.clone_from(&label);
                        }
                        out.push(ev);
                    }
                }
            }
        }
    }

    fn msg_time(&self, msg: &Message, now: i64) -> i64 {
        if let Some(t) = msg.tags.get("time").and_then(parse_server_time) {
            return t;
        }
        if self.cfg.twitch
            && let Some(t) = msg.tags.get("tmi-sent-ts").and_then(|v| v.parse().ok())
        {
            return t;
        }
        now
    }

    fn handle(&mut self, msg: &Message, now: i64, mode: Mode, out: &mut Vec<SessionEvent>) {
        let time = self.msg_time(msg, now);
        let live = mode == Mode::Live;
        macro_rules! push {
            ($ev:expr) => {
                out.push(SessionEvent { time, label: None, kind: $ev })
            };
        }
        if let Some(src) = &msg.source
            && live
        {
            self.learn_source(src);
        }

        if let Some(num) = msg.numeric() {
            if live {
                self.handle_numeric(num, msg, time, out);
            } else {
                push!(Event::ServerText { msg: msg.clone() });
            }
            return;
        }

        let cmd = msg.command.to_ascii_uppercase();
        match cmd.as_str() {
            "PING" if live => self.send(Message::new("PONG", msg.params.clone())),
            "PONG" => {}
            "CAP" if live => self.handle_cap(msg, time),
            "AUTHENTICATE" if live => self.handle_authenticate(msg, time),
            "PRIVMSG" | "NOTICE" | "TAGMSG" => self.handle_chat(msg, mode, time, out),
            "JOIN" => {
                let Some(src) = msg.source.clone() else { return };
                let channel = msg.arg(0).to_owned();
                let own = self.is_me(&src.nick);
                let account = msg.param(1).filter(|a| *a != "*").map(str::to_owned);
                let realname = msg.param(2).map(str::to_owned);
                if live {
                    let key = self.fold(&channel);
                    if own {
                        let pending_key = self
                            .rejoin
                            .iter()
                            .find(|(c, _)| self.isupport.casemapping.eq(c, &channel))
                            .and_then(|(_, k)| k.clone());
                        let mut ch = Channel::new(&channel);
                        ch.key = pending_key;
                        self.channels.insert(key.clone(), ch);
                    }
                    if let Some(ch) = self.channels.get_mut(&key) {
                        ch.members.insert(
                            self.isupport.casemapping.fold(&src.nick).into_owned(),
                            Member { nick: src.nick.clone(), prefixes: Vec::new() },
                        );
                    }
                    let u = self.user_entry(&src.nick);
                    if msg.params.len() >= 2 {
                        u.account = account.clone();
                    }
                    if let Some(r) = &realname {
                        u.realname = Some(r.clone());
                    }
                }
                push!(Event::Join { channel, user: src, account, realname, own });
            }
            "PART" => {
                let Some(src) = msg.source.clone() else { return };
                let own = self.is_me(&src.nick);
                let reason = msg.param(1).map(str::to_owned);
                for channel in msg.arg(0).split(',') {
                    if live {
                        self.remove_member(channel, &src.nick, own);
                    }
                    push!(Event::Part { channel: channel.to_owned(), user: src.clone(), reason: reason.clone(), own });
                }
            }
            "KICK" => {
                let Some(by) = msg.source.clone() else { return };
                let channel = msg.arg(0).to_owned();
                let nick = msg.arg(1).to_owned();
                let own = self.is_me(&nick);
                if live {
                    self.remove_member(&channel, &nick, own);
                }
                push!(Event::Kick { channel, by, nick, reason: msg.param(2).map(str::to_owned), own });
            }
            "QUIT" => {
                let Some(user) = msg.source.clone() else { return };
                let mut channels = Vec::new();
                if live {
                    let folded = self.fold(&user.nick);
                    for ch in self.channels.values_mut() {
                        if ch.members.remove(&folded).is_some() {
                            channels.push(ch.name.clone());
                        }
                    }
                    self.users.remove(&folded);
                }
                push!(Event::Quit { user, reason: msg.param(0).map(str::to_owned), channels, netsplit: false });
            }
            "NICK" => {
                let Some(src) = msg.source.clone() else { return };
                let new = msg.arg(0).to_owned();
                let own = self.is_me(&src.nick);
                let mut channels = Vec::new();
                if live {
                    let old_f = self.fold(&src.nick);
                    let new_f = self.fold(&new);
                    for ch in self.channels.values_mut() {
                        if let Some(mut m) = ch.members.remove(&old_f) {
                            m.nick = new.clone();
                            ch.members.insert(new_f.clone(), m);
                            channels.push(ch.name.clone());
                        }
                    }
                    if let Some(mut u) = self.users.remove(&old_f) {
                        u.nick = new.clone();
                        self.users.insert(new_f, u);
                    }
                    if own {
                        self.nick = new.clone();
                        push!(Event::NickChangedSelf { old: src.nick.clone(), new: new.clone() });
                    }
                }
                push!(Event::Nick { old: src.nick, new, channels, own });
            }
            "TOPIC" => {
                let channel = msg.arg(0).to_owned();
                let topic = msg.arg(1).to_owned();
                let by = msg.source_nick().map(str::to_owned);
                if live && let Some(ch) = self.channels.get_mut(&self.isupport.casemapping.fold(&channel).into_owned())
                {
                    ch.topic = Some(topic.clone());
                    ch.topic_set_by.clone_from(&by);
                    ch.topic_time = Some(time);
                }
                push!(Event::Topic { channel, topic, by, changed: true });
            }
            "MODE" => {
                let target = msg.arg(0).to_owned();
                if self.is_channel(&target) {
                    let changes = state::parse_channel_modes(&self.isupport, &msg.params[1..]);
                    if live {
                        self.apply_channel_modes(&target, &changes, time, out);
                    }
                    push!(Event::ChannelMode { channel: target, by: msg.source.clone(), changes });
                } else {
                    push!(Event::UserMode { changes: state::parse_user_modes(msg.arg(1)) });
                }
            }
            "INVITE" => {
                let Some(by) = msg.source.clone() else { return };
                push!(Event::Invite { by, nick: msg.arg(0).to_owned(), channel: msg.arg(1).to_owned() });
            }
            "AWAY" => {
                let Some(src) = msg.source.clone() else { return };
                if live {
                    self.user_entry(&src.nick).away = msg.param(0).map(str::to_owned);
                }
                self.push_user_updated(&src.nick, time, out);
            }
            "ACCOUNT" => {
                let Some(src) = msg.source.clone() else { return };
                if live {
                    let acct = msg.param(0).filter(|a| *a != "*").map(str::to_owned);
                    if self.is_me(&src.nick) {
                        self.account.clone_from(&acct);
                    }
                    self.user_entry(&src.nick).account = acct;
                }
                self.push_user_updated(&src.nick, time, out);
            }
            "CHGHOST" => {
                let Some(src) = msg.source.clone() else { return };
                if live {
                    let u = self.user_entry(&src.nick);
                    u.user = msg.param(0).map(str::to_owned);
                    u.host = msg.param(1).map(str::to_owned);
                    if self.is_me(&src.nick) {
                        self.self_user = msg.param(0).map(str::to_owned);
                        self.self_host = msg.param(1).map(str::to_owned);
                    }
                }
                self.push_user_updated(&src.nick, time, out);
            }
            "SETNAME" => {
                let Some(src) = msg.source.clone() else { return };
                if live {
                    self.user_entry(&src.nick).realname = msg.param(0).map(str::to_owned);
                }
                self.push_user_updated(&src.nick, time, out);
            }
            "ERROR" => push!(Event::Error { message: msg.arg(0).to_owned() }),
            "FAIL" | "WARN" | "NOTE" => {
                let kind = match cmd.as_str() {
                    "FAIL" => StandardReplyKind::Fail,
                    "WARN" => StandardReplyKind::Warn,
                    _ => StandardReplyKind::Note,
                };
                let n = msg.params.len();
                let context = if n > 3 { msg.params[2..n - 1].to_vec() } else { Vec::new() };
                if kind == StandardReplyKind::Fail && msg.arg(0) == "AUTHENTICATE" {
                    self.sasl_active = false;
                }
                push!(Event::StandardReply {
                    kind,
                    command: msg.arg(0).to_owned(),
                    code: msg.arg(1).to_owned(),
                    context,
                    description: msg.last_param().unwrap_or_default().to_owned(),
                });
            }
            "REDACT" => {
                let Some(by) = msg.source.clone() else { return };
                push!(Event::Redact {
                    target: msg.arg(0).to_owned(),
                    msgid: msg.arg(1).to_owned(),
                    by,
                    reason: msg.param(2).map(str::to_owned),
                });
            }
            "MARKREAD" => {
                let t = msg.param(1).and_then(|v| v.strip_prefix("timestamp=")).and_then(parse_server_time);
                push!(Event::ReadMarker { target: msg.arg(0).to_owned(), time: t });
            }
            "RENAME" => {
                let old = msg.arg(0).to_owned();
                let new = msg.arg(1).to_owned();
                if live {
                    let of = self.fold(&old);
                    if let Some(mut ch) = self.channels.remove(&of) {
                        ch.name = new.clone();
                        self.channels.insert(self.fold(&new), ch);
                    }
                }
                push!(Event::ChannelRename { old, new, reason: msg.param(2).map(str::to_owned) });
            }
            "BOUNCER" => {
                if msg.arg(0).eq_ignore_ascii_case("NETWORK") {
                    let id = msg.arg(1).to_owned();
                    let attrs = match msg.arg(2) {
                        "*" => None,
                        raw => Some(Tags::parse(raw).iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect()),
                    };
                    push!(Event::BouncerNetwork { id, attrs });
                } else {
                    push!(Event::ServerText { msg: msg.clone() });
                }
            }
            "RECONNECT" => push!(Event::ReconnectRequested { tls_port: None }),
            "CLEARCHAT" => push!(Event::Twitch(TwitchEvent::ClearChat {
                channel: msg.arg(0).to_owned(),
                nick: msg.param(1).map(str::to_owned),
                duration_secs: msg.tags.get("ban-duration").and_then(|d| d.parse().ok()),
            })),
            "CLEARMSG" => push!(Event::Twitch(TwitchEvent::ClearMsg {
                channel: msg.arg(0).to_owned(),
                target_msgid: msg.tags.get("target-msg-id").unwrap_or_default().to_owned(),
                login: msg.tags.value("login").map(str::to_owned),
                text: msg.arg(1).to_owned(),
            })),
            "USERNOTICE" => push!(Event::Twitch(TwitchEvent::UserNotice {
                channel: msg.arg(0).to_owned(),
                msg_id: msg.tags.get("msg-id").unwrap_or_default().to_owned(),
                system_msg: msg.tags.value("system-msg").map(str::to_owned),
                text: msg.param(1).map(str::to_owned),
                tags: msg.tags.clone(),
            })),
            "ROOMSTATE" => {
                push!(Event::Twitch(TwitchEvent::RoomState { channel: msg.arg(0).to_owned(), tags: msg.tags.clone() }))
            }
            "USERSTATE" => push!(Event::Twitch(TwitchEvent::UserState {
                channel: Some(msg.arg(0).to_owned()),
                tags: msg.tags.clone()
            })),
            "GLOBALUSERSTATE" => push!(Event::Twitch(TwitchEvent::UserState { channel: None, tags: msg.tags.clone() })),
            "WHISPER" => {
                // Twitch whispers arrive as `WHISPER <me> :text`; treat as a private message.
                let mut m = msg.clone();
                m.command = "PRIVMSG".into();
                self.handle_chat(&m, mode, time, out);
            }
            "HOSTTARGET" => {}
            _ => push!(Event::ServerText { msg: msg.clone() }),
        }
    }

    fn push_user_updated(&self, nick: &str, time: i64, out: &mut Vec<SessionEvent>) {
        out.push(SessionEvent { time, label: None, kind: Event::UserUpdated { nick: nick.to_owned() } });
    }

    fn learn_source(&mut self, src: &Source) {
        if src.user.is_none() && src.host.is_none() {
            return;
        }
        if self.is_me(&src.nick) {
            self.self_user.clone_from(&src.user);
            self.self_host.clone_from(&src.host);
        }
        let folded = self.fold(&src.nick);
        if let Some(u) = self.users.get_mut(&folded) {
            if src.user.is_some() {
                u.user.clone_from(&src.user);
            }
            if src.host.is_some() {
                u.host.clone_from(&src.host);
            }
        }
    }

    fn user_entry(&mut self, nick: &str) -> &mut User {
        let folded = self.isupport.casemapping.fold(nick).into_owned();
        self.users.entry(folded).or_insert_with(|| User { nick: nick.to_owned(), ..Default::default() })
    }

    fn remove_member(&mut self, channel: &str, nick: &str, own: bool) {
        let key = self.fold(channel);
        if own {
            self.channels.remove(&key);
            self.whox_pending.remove(&key);
            return;
        }
        let nf = self.fold(nick);
        if let Some(ch) = self.channels.get_mut(&key) {
            ch.members.remove(&nf);
        }
        if !self.channels.values().any(|c| c.members.contains_key(&nf)) {
            self.users.remove(&nf);
        }
    }

    fn apply_channel_modes(
        &mut self,
        channel: &str,
        changes: &[state::ModeChange],
        time: i64,
        out: &mut Vec<SessionEvent>,
    ) {
        let key = self.fold(channel);
        let Some(ch) = self.channels.get_mut(&key) else { return };
        let mut members_changed = false;
        for c in changes {
            match self.isupport.mode_type(c.mode) {
                ModeType::Prefix => {
                    let Some(symbol) = self.isupport.prefix_for_mode(c.mode) else { continue };
                    let Some(nick) = &c.arg else { continue };
                    if let Some(m) = ch.members.get_mut(self.isupport.casemapping.fold(nick).as_ref()) {
                        m.prefixes.retain(|p| *p != symbol);
                        if c.add {
                            m.prefixes.push(symbol);
                            let is = &self.isupport;
                            m.prefixes.sort_by_key(|p| is.prefix_rank(*p));
                        }
                        members_changed = true;
                    }
                }
                ModeType::List => {}
                _ => {
                    if c.add {
                        ch.modes.insert(c.mode, c.arg.clone());
                        if c.mode == 'k' {
                            ch.key.clone_from(&c.arg);
                        }
                    } else {
                        ch.modes.remove(&c.mode);
                        if c.mode == 'k' {
                            ch.key = None;
                        }
                    }
                }
            }
        }
        if members_changed {
            out.push(SessionEvent { time, label: None, kind: Event::MembersChanged { channel: ch.name.clone() } });
        }
    }

    fn handle_chat(&mut self, msg: &Message, mode: Mode, time: i64, out: &mut Vec<SessionEvent>) {
        let from = msg.source.clone().unwrap_or_else(|| Source::nick(""));
        let raw_target = msg.arg(0);
        let text = msg.param(1).unwrap_or_default();
        let own = !from.nick.is_empty() && self.is_me(&from.nick);
        let (status, bare) = self.isupport.split_statusmsg(raw_target);
        let target = if self.is_channel(bare) {
            Target::Channel { name: bare.to_owned(), status }
        } else if from.nick.is_empty()
            || from.is_server()
            || raw_target == "*"
            || raw_target.starts_with('$')
            || self.phase == Phase::Registering
        {
            Target::Server
        } else if own {
            Target::Query { peer: raw_target.to_owned() }
        } else {
            Target::Query { peer: from.nick.clone() }
        };
        let mut kind = if msg.is("PRIVMSG") {
            ChatKind::Privmsg
        } else if msg.is("NOTICE") {
            ChatKind::Notice
        } else {
            ChatKind::Tagmsg
        };
        let mut body = text.to_owned();
        if let Some(c) = ctcp::parse(text) {
            if c.is("ACTION") && kind == ChatKind::Privmsg {
                kind = ChatKind::Action;
                body = c.params.to_owned();
            } else if mode == Mode::Live && !own {
                let ev = if kind == ChatKind::Notice {
                    Event::CtcpReply { from, command: c.command.to_ascii_uppercase(), params: c.params.to_owned() }
                } else {
                    Event::CtcpRequest {
                        from,
                        target: raw_target.to_owned(),
                        command: c.command.to_ascii_uppercase(),
                        params: c.params.to_owned(),
                    }
                };
                out.push(SessionEvent { time, label: None, kind: ev });
                return;
            } else {
                return;
            }
        }
        let msgid = msg.tags.value("msgid").or_else(|| msg.tags.value("id")).map(str::to_owned);
        out.push(SessionEvent {
            time,
            label: None,
            kind: Event::Chat(Chat {
                kind,
                from,
                target,
                text: body,
                tags: msg.tags.clone(),
                msgid,
                own,
                history: mode == Mode::History,
            }),
        });
    }

    // ----- CAP / SASL --------------------------------------------------------------------------

    fn handle_cap(&mut self, msg: &Message, now: i64) {
        let sub = msg.arg(1).to_ascii_uppercase();
        let (more, list) =
            if msg.arg(2) == "*" && msg.params.len() >= 4 { (true, msg.arg(3)) } else { (false, msg.arg(2)) };
        match sub.as_str() {
            "LS" => {
                for tok in list.split_ascii_whitespace() {
                    let (name, value) = match tok.split_once('=') {
                        Some((n, v)) => (n.to_owned(), Some(v.to_owned())),
                        None => (tok.to_owned(), None),
                    };
                    self.caps_available.insert(name, value);
                }
                if more {
                    return;
                }
                self.cap_ls_done = true;
                self.handle_sts(now);
                let want = self.choose_caps(self.caps_available.keys().map(String::as_str));
                self.request_caps(want, now);
            }
            "ACK" => {
                for cap in list.split_ascii_whitespace() {
                    if let Some(c) = cap.strip_prefix('-') {
                        self.caps_enabled.remove(c);
                    } else {
                        self.caps_enabled.insert(cap.to_owned());
                    }
                }
                if !more {
                    self.cap_req_pending = self.cap_req_pending.saturating_sub(1);
                    self.emit(now, Event::CapsChanged { enabled: self.caps_enabled.iter().cloned().collect() });
                    if list.split_ascii_whitespace().any(|c| c == "sasl") {
                        self.begin_sasl(now);
                    }
                    self.maybe_end_cap(now);
                }
            }
            "NAK" => {
                if !more {
                    self.cap_req_pending = self.cap_req_pending.saturating_sub(1);
                    self.status(now, format!("Server rejected capabilities: {list}"));
                    self.maybe_end_cap(now);
                }
            }
            "NEW" => {
                let mut offered = Vec::new();
                for tok in list.split_ascii_whitespace() {
                    let (name, value) = match tok.split_once('=') {
                        Some((n, v)) => (n.to_owned(), Some(v.to_owned())),
                        None => (tok.to_owned(), None),
                    };
                    offered.push(name.clone());
                    self.caps_available.insert(name, value);
                }
                let want = self.choose_caps(offered.iter().map(String::as_str));
                self.request_caps(want, now);
            }
            "DEL" => {
                for cap in list.split_ascii_whitespace() {
                    self.caps_available.remove(cap);
                    self.caps_enabled.remove(cap);
                }
                self.emit(now, Event::CapsChanged { enabled: self.caps_enabled.iter().cloned().collect() });
            }
            _ => {}
        }
    }

    fn handle_sts(&mut self, now: i64) {
        let Some(Some(value)) = self.caps_available.get("sts").cloned() else { return };
        let mut port = None;
        let mut duration = None;
        for kv in value.split(',') {
            match kv.split_once('=') {
                Some(("port", p)) => port = p.parse().ok(),
                Some(("duration", d)) => duration = d.parse().ok(),
                _ => {}
            }
        }
        if !self.cfg.tls {
            if let Some(p) = port {
                self.status(now, format!("Server requires TLS (STS); reconnecting on port {p}"));
                self.emit(now, Event::ReconnectRequested { tls_port: Some(p) });
            }
        } else if let Some(d) = duration {
            self.emit(now, Event::StsPolicy { port: self.cfg.port, duration: d });
        }
    }

    /// Picks wanted caps from those offered, preferring standard caps over vendor/legacy ones.
    fn choose_caps<'a>(&self, offered: impl Iterator<Item = &'a str>) -> Vec<String> {
        let offered: Vec<&str> = offered.collect();
        let all_offered = |c: &str| self.caps_available.contains_key(c);
        let mut want: Vec<String> = offered
            .iter()
            .filter(|c| !self.caps_enabled.contains(**c))
            .filter(|c| WANTED_CAPS.contains(c) || self.cfg.extra_caps.iter().any(|e| e == *c))
            .filter(|c| !self.cfg.disabled_caps.iter().any(|d| d == *c))
            .filter(|c| **c != "sasl" || self.sasl_offered())
            .filter(|c| match **c {
                "znc.in/server-time-iso" => !all_offered("server-time"),
                "znc.in/batch" => !all_offered("batch"),
                "draft/extended-monitor" => !all_offered("extended-monitor"),
                // CHATHISTORY is more precise than ZNC-style playback when both exist.
                "znc.in/playback" => !all_offered("draft/chathistory"),
                _ => true,
            })
            .map(|c| (*c).to_owned())
            .collect();
        if self.cfg.lazy_names && offered.contains(&"draft/no-implicit-names") {
            want.push("draft/no-implicit-names".into());
        }
        want.sort();
        want
    }

    /// True when SASL is configured and the server offers our mechanism (or doesn't list any).
    fn sasl_offered(&self) -> bool {
        let Some(cfg) = &self.cfg.sasl else { return false };
        match self.caps_available.get("sasl") {
            Some(Some(mechs)) if !mechs.is_empty() => mechs.split(',').any(|m| m.eq_ignore_ascii_case(cfg.mechanism())),
            _ => true,
        }
    }

    fn request_caps(&mut self, want: Vec<String>, now: i64) {
        if want.is_empty() {
            self.maybe_end_cap(now);
            return;
        }
        // Keep each CAP REQ comfortably below the line limit.
        let mut chunk = String::new();
        for cap in want {
            if !chunk.is_empty() && chunk.len() + cap.len() + 1 > 400 {
                self.send(Message::new("CAP", ["REQ".to_owned(), std::mem::take(&mut chunk)]));
                self.cap_req_pending += 1;
            }
            if !chunk.is_empty() {
                chunk.push(' ');
            }
            chunk.push_str(&cap);
        }
        self.send(Message::new("CAP", ["REQ".to_owned(), chunk]));
        self.cap_req_pending += 1;
    }

    fn begin_sasl(&mut self, now: i64) {
        let Some(cfg) = self.cfg.sasl.clone() else { return };
        if self.phase != Phase::Registering {
            return; // SASL 3.2 re-authentication is initiated explicitly.
        }
        let offered = self.caps_available.get("sasl").cloned().flatten();
        if let Some(mechs) = offered.filter(|m| !m.is_empty())
            && !mechs.split(',').any(|m| m.eq_ignore_ascii_case(cfg.mechanism()))
        {
            self.status(now, format!("SASL mechanism {} not offered (server supports {mechs})", cfg.mechanism()));
            self.sasl_failed(now, "mechanism not supported".into());
            return;
        }
        self.sasl = Some(SaslSession::new(&cfg));
        self.sasl_active = true;
        self.send(Message::new("AUTHENTICATE", [cfg.mechanism()]));
    }

    fn handle_authenticate(&mut self, msg: &Message, now: i64) {
        let Some(challenge) = self.sasl_buf.push(msg.arg(0)) else { return };
        let Some(sasl) = self.sasl.as_mut() else { return };
        match sasl.step(&challenge) {
            Ok(Some(resp)) => {
                for chunk in sasl::encode_response(&resp) {
                    self.send(Message::new("AUTHENTICATE", [chunk]));
                }
            }
            Ok(None) => self.send(Message::new("AUTHENTICATE", ["+"])),
            Err(e) => {
                self.send(Message::new("AUTHENTICATE", ["*"]));
                self.status(now, format!("SASL aborted: {e:?}"));
            }
        }
    }

    fn sasl_failed(&mut self, now: i64, message: String) {
        self.sasl_active = false;
        self.sasl = None;
        self.emit(now, Event::SaslResult { success: false, message: message.clone() });
        if self.cfg.sasl_required {
            self.send(Message::new("QUIT", ["SASL authentication failed"]));
            self.emit(now, Event::Error { message: format!("SASL authentication failed: {message}") });
        } else {
            self.maybe_end_cap(now);
        }
    }

    fn maybe_end_cap(&mut self, now: i64) {
        if self.cap_ls_done && self.cap_req_pending == 0 && !self.sasl_active && !self.cap_end_sent {
            self.end_cap(now);
        }
    }

    fn end_cap(&mut self, _now: i64) {
        if !self.cap_end_sent {
            self.cap_end_sent = true;
            if let Some(id) = self.cfg.bouncer_netid.clone()
                && self.has_cap("soju.im/bouncer-networks")
            {
                self.send(Message::new("BOUNCER", ["BIND".to_owned(), id]));
            }
            self.send(Message::new("CAP", ["END"]));
        }
    }

    // ----- numerics ----------------------------------------------------------------------------

    fn handle_numeric(&mut self, num: u16, msg: &Message, time: i64, out: &mut Vec<SessionEvent>) {
        macro_rules! push {
            ($ev:expr) => {
                out.push(SessionEvent { time, label: None, kind: $ev })
            };
        }
        let text = || push_text(msg);
        match num {
            RPL_WELCOME => {
                self.phase = Phase::Registered;
                self.nick = msg.arg(0).to_owned();
                // Never keep CAP negotiation open past registration.
                self.cap_end_sent = true;
                push!(Event::Registered { nick: self.nick.clone() });
                push!(text());
            }
            RPL_ISUPPORT => {
                let old_cm = self.isupport.casemapping;
                self.isupport.apply_reply(&msg.params);
                if self.isupport.casemapping != old_cm {
                    self.refold();
                }
                push!(Event::ISupportChanged);
                push!(text());
            }
            RPL_ENDOFMOTD | ERR_NOMOTD => {
                push!(text());
                if self.phase != Phase::Ready {
                    self.phase = Phase::Ready;
                    push!(Event::Ready);
                    self.after_ready();
                }
            }
            ERR_NICKNAMEINUSE | ERR_ERRONEUSNICKNAME | ERR_UNAVAILRESOURCE | ERR_NICKCOLLISION
                if self.phase == Phase::Registering =>
            {
                let next = self.next_nick();
                push!(Event::Status(format!("Nickname {} unavailable, trying {next}", msg.arg(1))));
                self.nick = next.clone();
                self.send(Message::new("NICK", [next]));
            }
            RPL_LOGGEDIN => {
                self.account = msg.param(2).map(str::to_owned);
                push!(text());
            }
            RPL_LOGGEDOUT => {
                self.account = None;
                push!(text());
            }
            RPL_SASLSUCCESS => {
                self.sasl_active = false;
                self.sasl = None;
                push!(Event::SaslResult { success: true, message: msg.last_param().unwrap_or_default().to_owned() });
                self.maybe_end_cap(time);
            }
            ERR_SASLFAIL | ERR_SASLTOOLONG | ERR_SASLABORTED | ERR_NICKLOCKED => {
                if self.sasl_active {
                    self.sasl_failed(time, msg.last_param().unwrap_or_default().to_owned());
                } else {
                    push!(text());
                }
            }
            ERR_SASLALREADY | RPL_SASLMECHS => {}
            RPL_NAMREPLY => {
                let channel = msg.arg(2).to_owned();
                let key = self.fold(&channel);
                let names: Vec<String> = msg.arg(3).split_ascii_whitespace().map(str::to_owned).collect();
                if let Some(ch) = self.channels.get_mut(&key).filter(|c| !c.names_done || c.names_fresh) {
                    if ch.names_fresh {
                        ch.members.clear();
                        ch.names_fresh = false;
                    }
                    for entry in &names {
                        let (prefixes, rest) = self.isupport.split_prefixes(entry);
                        let src = Source::parse(rest);
                        let folded = self.isupport.casemapping.fold(&src.nick).into_owned();
                        ch.members.insert(folded.clone(), Member { nick: src.nick.clone(), prefixes });
                        let u = self
                            .users
                            .entry(folded)
                            .or_insert_with(|| User { nick: src.nick.clone(), ..Default::default() });
                        if src.user.is_some() {
                            u.user = src.user;
                            u.host = src.host;
                        }
                    }
                } else {
                    push!(Event::Names { channel, names });
                }
            }
            RPL_ENDOFNAMES => {
                let channel = msg.arg(1).to_owned();
                let key = self.fold(&channel);
                if let Some(ch) = self.channels.get_mut(&key) {
                    let was_done = ch.names_done;
                    ch.names_done = true;
                    ch.names_fresh = true; // a later /names refreshes the list
                    let count = ch.members.len();
                    let name = ch.name.clone();
                    push!(Event::MembersChanged { channel: name.clone() });
                    if !was_done && self.isupport.whox && count <= 1000 {
                        self.whox_pending.insert(key);
                        self.send(Message::new("WHO", [name, format!("%tcuhnfar,{WHOX_TOKEN}")]));
                    }
                } else {
                    push!(text());
                }
            }
            RPL_TOPIC | RPL_NOTOPIC => {
                let channel = msg.arg(1).to_owned();
                let topic = if num == RPL_TOPIC { msg.arg(2).to_owned() } else { String::new() };
                if let Some(ch) = self.channels.get_mut(&self.isupport.casemapping.fold(&channel).into_owned()) {
                    ch.topic = Some(topic.clone());
                }
                push!(Event::Topic { channel, topic, by: None, changed: false });
            }
            RPL_TOPICWHOTIME => {
                let key = self.fold(msg.arg(1));
                if let Some(ch) = self.channels.get_mut(&key) {
                    ch.topic_set_by = msg.param(2).map(|s| Source::parse(s).nick);
                    ch.topic_time = msg.param(3).and_then(|t| t.parse::<i64>().ok()).map(|t| t * 1000);
                } else {
                    push!(text());
                }
            }
            RPL_CHANNELMODEIS => {
                let channel = msg.arg(1).to_owned();
                let changes = state::parse_channel_modes(&self.isupport, &msg.params[2..]);
                let key = self.fold(&channel);
                if let Some(ch) = self.channels.get_mut(&key) {
                    ch.modes.clear();
                    for c in &changes {
                        if c.add {
                            ch.modes.insert(c.mode, c.arg.clone());
                            if c.mode == 'k' {
                                ch.key.clone_from(&c.arg);
                            }
                        }
                    }
                }
                push!(Event::ChannelMode { channel, by: None, changes });
            }
            RPL_CREATIONTIME => {
                let key = self.fold(msg.arg(1));
                if let Some(ch) = self.channels.get_mut(&key) {
                    ch.created = msg.param(2).and_then(|t| t.parse::<i64>().ok()).map(|t| t * 1000);
                }
            }
            RPL_WHOREPLY => {
                // <client> <channel> <user> <host> <server> <nick> <flags> :<hopcount> <realname>
                let nick = msg.arg(5).to_owned();
                let flags = msg.arg(6).to_owned();
                let realname = msg.arg(7).split_once(' ').map(|(_, r)| r.to_owned());
                self.apply_who(&nick, Some(msg.arg(2)), Some(msg.arg(3)), &flags, None, realname);
                if !self.whox_pending.contains(&self.fold(msg.arg(1))) {
                    push!(text());
                }
            }
            RPL_WHOSPCRPL => {
                if msg.arg(1) == WHOX_TOKEN {
                    // token channel user host nick flags account realname
                    let nick = msg.arg(5).to_owned();
                    let account = msg.param(7).map(str::to_owned);
                    self.apply_who(
                        &nick,
                        msg.param(3),
                        msg.param(4),
                        msg.arg(6),
                        account,
                        msg.param(8).map(str::to_owned),
                    );
                } else {
                    push!(text());
                }
            }
            RPL_ENDOFWHO => {
                let key = self.fold(msg.arg(1));
                if self.whox_pending.remove(&key) {
                    if let Some(ch) = self.channels.get(&key) {
                        push!(Event::MembersChanged { channel: ch.name.clone() });
                    }
                } else {
                    push!(text());
                }
            }
            RPL_AWAY if self.whois.contains_key(&self.fold(msg.arg(1))) => {
                let k = self.fold(msg.arg(1));
                self.whois.get_mut(&k).unwrap().away = msg.param(2).map(str::to_owned);
            }
            RPL_WHOISUSER | RPL_WHOISSERVER | RPL_WHOISOPERATOR | RPL_WHOISIDLE | RPL_WHOISCHANNELS
            | RPL_WHOISACCOUNT | RPL_WHOISSECURE | RPL_WHOISCERTFP | RPL_WHOISACTUALLY | RPL_WHOISHOST
            | RPL_WHOISMODES | RPL_WHOISSPECIAL | RPL_WHOISREGNICK | 335 => {
                let nick = msg.arg(1).to_owned();
                let key = self.fold(&nick);
                let w = self.whois.entry(key).or_insert_with(|| WhoisInfo { nick: nick.clone(), ..Default::default() });
                match num {
                    RPL_WHOISUSER => {
                        w.user = msg.param(2).map(str::to_owned);
                        w.host = msg.param(3).map(str::to_owned);
                        w.realname = msg.param(5).map(str::to_owned);
                    }
                    RPL_WHOISSERVER => {
                        w.server = msg.param(2).map(str::to_owned);
                        w.server_info = msg.param(3).map(str::to_owned);
                    }
                    RPL_WHOISOPERATOR => w.operator = msg.last_param().map(str::to_owned),
                    RPL_WHOISIDLE => {
                        w.idle_secs = msg.param(2).and_then(|s| s.parse().ok());
                        w.signon = msg.param(3).and_then(|s| s.parse::<i64>().ok()).map(|t| t * 1000);
                    }
                    RPL_WHOISCHANNELS => w.channels.extend(msg.arg(2).split_ascii_whitespace().map(str::to_owned)),
                    RPL_WHOISACCOUNT => w.account = msg.param(2).map(str::to_owned),
                    RPL_WHOISSECURE => w.secure = true,
                    RPL_WHOISCERTFP => w.certfp = msg.last_param().map(str::to_owned),
                    RPL_WHOISACTUALLY => w.actual_host = msg.param(2).map(str::to_owned),
                    335 => w.bot = true,
                    _ => w.extra.push(msg.params[2..].join(" ")),
                }
            }
            RPL_ENDOFWHOIS => {
                let key = self.fold(msg.arg(1));
                match self.whois.remove(&key) {
                    Some(w) => push!(Event::Whois(w)),
                    None => push!(text()),
                }
            }
            RPL_LISTSTART => {}
            RPL_LIST => push!(Event::ListEntry {
                channel: msg.arg(1).to_owned(),
                users: msg.arg(2).parse().unwrap_or(0),
                topic: msg.arg(3).to_owned(),
            }),
            RPL_LISTEND => push!(Event::ListEnd),
            RPL_BANLIST | RPL_EXCEPTLIST | RPL_INVEXLIST => {
                let mode = match num {
                    RPL_BANLIST => 'b',
                    RPL_EXCEPTLIST => self.isupport.excepts.unwrap_or('e'),
                    _ => self.isupport.invex.unwrap_or('I'),
                };
                let key = (self.fold(msg.arg(1)), mode);
                self.mode_lists.entry(key).or_default().push(ModeListEntry {
                    mask: msg.arg(2).to_owned(),
                    set_by: msg.param(3).map(str::to_owned),
                    set_at: msg.param(4).and_then(|t| t.parse::<i64>().ok()).map(|t| t * 1000),
                });
            }
            RPL_ENDOFBANLIST | RPL_ENDOFEXCEPTLIST | RPL_ENDOFINVEXLIST => {
                let mode = match num {
                    RPL_ENDOFBANLIST => 'b',
                    RPL_ENDOFEXCEPTLIST => self.isupport.excepts.unwrap_or('e'),
                    _ => self.isupport.invex.unwrap_or('I'),
                };
                let channel = msg.arg(1).to_owned();
                let entries = self.mode_lists.remove(&(self.fold(&channel), mode)).unwrap_or_default();
                push!(Event::ModeList { channel, mode, entries });
            }
            RPL_MONONLINE => {
                let nicks: Vec<Source> = msg.arg(1).split(',').map(Source::parse).collect();
                push!(Event::MonitorOnline { nicks });
            }
            RPL_MONOFFLINE => {
                let nicks: Vec<String> = msg.arg(1).split(',').map(str::to_owned).collect();
                let want = self.cfg.nick.clone();
                if self.phase == Phase::Ready
                    && !self.is_me(&want)
                    && nicks.iter().any(|n| self.isupport.casemapping.eq(n, &want))
                {
                    self.send(Message::new("NICK", [want]));
                }
                push!(Event::MonitorOffline { nicks });
            }
            RPL_ISON => {
                let want = self.cfg.nick.clone();
                let online = msg.arg(1).split_ascii_whitespace().any(|n| self.isupport.casemapping.eq(n, &want));
                if !online && !self.is_me(&want) {
                    self.send(Message::new("NICK", [want]));
                }
            }
            RPL_NOWAWAY => push!(Event::Away { own: true, message: self.away.clone().or(Some(String::new())) }),
            RPL_UNAWAY => push!(Event::Away { own: true, message: None }),
            _ => push!(text()),
        }
    }

    fn apply_who(
        &mut self,
        nick: &str,
        user: Option<&str>,
        host: Option<&str>,
        flags: &str,
        account: Option<String>,
        realname: Option<String>,
    ) {
        let u = self.user_entry(nick);
        u.user = user.map(str::to_owned);
        u.host = host.map(str::to_owned);
        if flags.starts_with('G') {
            if u.away.is_none() {
                u.away = Some(String::new());
            }
        } else if flags.starts_with('H') {
            u.away = None;
        }
        u.bot = flags.contains('B');
        if let Some(a) = account {
            u.account = if a == "0" || a == "*" { None } else { Some(a) };
        }
        if realname.is_some() {
            u.realname = realname;
        }
    }

    fn refold(&mut self) {
        let cm = self.isupport.casemapping;
        let channels = std::mem::take(&mut self.channels);
        for (_, mut ch) in channels {
            ch.members = ch.members.into_values().map(|m| (cm.fold(&m.nick).into_owned(), m)).collect();
            self.channels.insert(cm.fold(&ch.name).into_owned(), ch);
        }
        let users = std::mem::take(&mut self.users);
        self.users = users.into_values().map(|u| (cm.fold(&u.nick).into_owned(), u)).collect();
    }

    fn next_nick(&mut self) -> String {
        let n = self.nick_attempt;
        self.nick_attempt += 1;
        if let Some(alt) = self.cfg.alt_nicks.get(n) {
            return alt.clone();
        }
        let extra = n - self.cfg.alt_nicks.len();
        let base = &self.cfg.nick;
        if extra < 3 {
            format!("{base}{}", "_".repeat(extra + 1))
        } else {
            format!("{base}{}", (extra * 7919 + 13) % 1000)
        }
    }

    /// Joins channels and restores state once registration is complete.
    fn after_ready(&mut self) {
        let joins = if self.first_connect { self.cfg.autojoin.clone() } else { std::mem::take(&mut self.rejoin) };
        self.rejoin = joins.clone();
        self.join_many(&joins);
        if let Some(away) = self.away.clone() {
            self.send(Message::new("AWAY", [away]));
        }
        let want = self.cfg.nick.clone();
        if !self.is_me(&want) && self.isupport.monitor.is_some() {
            self.send(Message::new("MONITOR", ["+".to_owned(), want]));
        }
    }

    /// Sends JOINs for many channels, packing them into as few lines as possible.
    pub fn join_many(&mut self, channels: &[(String, Option<String>)]) {
        let mut sorted: Vec<&(String, Option<String>)> = channels.iter().collect();
        // Keyed channels must come first so keys line up positionally.
        sorted.sort_by_key(|(_, k)| k.is_none());
        let mut names = Vec::new();
        let mut keys = Vec::new();
        let mut len = 0;
        for (name, key) in sorted {
            let add = name.len() + key.as_ref().map_or(0, |k| k.len() + 1) + 1;
            if !names.is_empty() && len + add > 400 {
                self.flush_join(&mut names, &mut keys);
                len = 0;
            }
            names.push(name.clone());
            if let Some(k) = key {
                keys.push(k.clone());
            }
            len += add;
        }
        self.flush_join(&mut names, &mut keys);
    }

    fn flush_join(&mut self, names: &mut Vec<String>, keys: &mut Vec<String>) {
        if names.is_empty() {
            return;
        }
        let mut params = vec![names.join(",")];
        if !keys.is_empty() {
            params.push(keys.join(","));
        }
        self.send(Message::new("JOIN", params));
        names.clear();
        keys.clear();
    }

    // ----- outbound helpers --------------------------------------------------------------------

    fn source_len(&self) -> usize {
        match (&self.self_user, &self.self_host) {
            (Some(u), Some(h)) => self.nick.len() + u.len() + h.len() + 2,
            _ => DEFAULT_SOURCE_BUDGET.max(self.nick.len() + 75),
        }
    }

    /// Sends a PRIVMSG/NOTICE, splitting long text, using `draft/multiline` for multi-line input
    /// when available, and producing a local echo when the server won't echo.
    pub fn say(&mut self, kind: ChatKind, target: &str, text: &str, tags: Tags, now: i64) {
        let command = if kind == ChatKind::Notice { "NOTICE" } else { "PRIVMSG" };
        let lines: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches('\r')).collect();
        let body_of = |line: &str| if kind == ChatKind::Action { ctcp::action(line) } else { line.to_owned() };
        let budget = split::text_budget(self.isupport.linelen, command, target, self.source_len())
            .saturating_sub(if kind == ChatKind::Action { 9 } else { 0 });

        let multiline =
            lines.len() > 1 && kind != ChatKind::Action && self.has_cap("draft/multiline") && self.has_cap("batch");
        if multiline {
            self.next_batch += 1;
            let reference = format!("ml{}", self.next_batch);
            let mut start =
                Message::new("BATCH", [format!("+{reference}"), "draft/multiline".into(), target.to_owned()]);
            for (k, v) in tags.iter() {
                start.tags.insert(k, v);
            }
            self.send(start);
            for line in &lines {
                let pieces = split::split_text(line, budget);
                for (i, piece) in pieces.iter().enumerate() {
                    let mut m = Message::new(command, [target.to_owned(), piece.to_string()])
                        .with_tag("batch", reference.clone());
                    if i > 0 {
                        m.tags.insert("draft/multiline-concat", "");
                    }
                    self.send(m);
                }
            }
            self.send(Message::new("BATCH", [format!("-{reference}")]));
        } else {
            let mut first = true;
            for line in &lines {
                if line.is_empty() && lines.len() > 1 {
                    continue;
                }
                for piece in split::split_text(line, budget) {
                    let mut m = Message::new(command, [target.to_owned(), body_of(piece)]);
                    if first {
                        for (k, v) in tags.iter() {
                            if !self.isupport.tag_denied(k) {
                                m.tags.insert(k, v);
                            }
                        }
                        first = false;
                    }
                    self.send(m);
                }
            }
        }

        if !self.has_cap("echo-message") {
            let status_target = self.isupport.split_statusmsg(target);
            let t = if self.is_channel(status_target.1) {
                Target::Channel { name: status_target.1.to_owned(), status: status_target.0 }
            } else {
                Target::Query { peer: target.to_owned() }
            };
            let from = Source { nick: self.nick.clone(), user: self.self_user.clone(), host: self.self_host.clone() };
            self.emit(
                now,
                Event::Chat(Chat {
                    kind,
                    from,
                    target: t,
                    text: text.to_owned(),
                    tags,
                    msgid: None,
                    own: true,
                    history: false,
                }),
            );
        }
    }

    /// Sends a tags-only message (typing indicators, reactions) if the server supports it.
    pub fn tagmsg(&mut self, target: &str, tags: Tags) -> bool {
        if !self.has_cap("message-tags") {
            return false;
        }
        let mut m = Message::new("TAGMSG", [target]);
        for (k, v) in tags.iter() {
            if self.isupport.tag_denied(k) {
                return false;
            }
            m.tags.insert(k, v);
        }
        self.send(m);
        true
    }

    /// Sets or clears (`None`) away; remembered across reconnects.
    pub fn set_away(&mut self, message: Option<String>) {
        self.away = message.filter(|m| !m.is_empty());
        match &self.away {
            Some(m) => self.send(Message::new("AWAY", [m.clone()])),
            None => self.send(Message::new("AWAY", Vec::<String>::new())),
        }
    }

    /// Requests history via CHATHISTORY: messages after `after_ms` (reconnect gap-fill) or the
    /// latest `limit` messages when `after_ms` is `None`.
    pub fn request_history(&mut self, target: &str, after_ms: Option<i64>, limit: usize) -> bool {
        if !self.supports_chathistory() {
            return false;
        }
        let limit = self.isupport.chathistory.filter(|&l| l > 0).map_or(limit, |l| l.min(limit)).to_string();
        let anchor = match after_ms {
            Some(t) => format!("timestamp={}", schwaetz_proto::tags::format_server_time(t + 1)),
            None => "*".into(),
        };
        let sub = if after_ms.is_some() { "AFTER" } else { "LATEST" };
        self.send(Message::new("CHATHISTORY", [sub.to_owned(), target.to_owned(), anchor, limit]));
        true
    }

    /// Requests messages before `before_ms` (infinite scroll-back).
    pub fn request_history_before(&mut self, target: &str, before_ms: i64, limit: usize) -> bool {
        if !self.supports_chathistory() {
            return false;
        }
        let ts = format!("timestamp={}", schwaetz_proto::tags::format_server_time(before_ms));
        self.send(Message::new("CHATHISTORY", ["BEFORE".to_owned(), target.to_owned(), ts, limit.to_string()]));
        true
    }

    /// ZNC `*playback`: replay everything newer than `since_ms` for all buffers.
    pub fn znc_playback(&mut self, since_ms: i64) -> bool {
        if !self.has_cap("znc.in/playback") {
            return false;
        }
        let secs = since_ms as f64 / 1000.0;
        self.send(Message::new("PRIVMSG", ["*playback".to_owned(), format!("PLAY * {secs:.3}")]));
        true
    }

    /// Syncs the read marker (`draft/read-marker`).
    pub fn mark_read(&mut self, target: &str, time_ms: i64) -> bool {
        if !self.has_cap("draft/read-marker") {
            return false;
        }
        let ts = format!("timestamp={}", schwaetz_proto::tags::format_server_time(time_ms));
        self.send(Message::new("MARKREAD", [target.to_owned(), ts]));
        true
    }
}

fn push_text(msg: &Message) -> Event {
    Event::ServerText { msg: msg.clone() }
}
