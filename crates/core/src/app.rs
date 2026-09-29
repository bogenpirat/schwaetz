//! The application model. Owned by the UI thread; UI-agnostic and deterministic given its inputs
//! (config, network events, user input and the current time), which keeps it testable.

use crate::buffer::{
    Activity, Buffer, BufferId, BufferKind, Line, LineExtra, LineFlags, LineKind, NotifyLevel, Typing,
};
use crate::completion::Completer;
use crate::config::{Config, NetworkConfig, NetworkKind, SaslMechanism};
use crate::emote_providers::Provider;
use crate::emotes::{EmoteEntry, EmoteRequest, EmoteResult, EmoteSet, Job, Lookup, SetKey, Source as EmoteSource};
use crate::filter::{Highlighter, IgnoreType, Ignores};
use crate::secrets::{self, SecretKind};
use crate::services::HistoryStore;
use crate::{time, twitch, znc};
use schwaetz_client::{Chat, ChatKind, Event, SaslConfig, Session, SessionConfig, SessionEvent, Target, TwitchEvent};
use schwaetz_net::{
    ClientCert, ConnectParams, FloodControl, NetCommand, NetEvent, NetworkId, Reconnect, ServerAddr, TlsInfo,
};
use schwaetz_proto::{Message, Source, ctcp, format as fmt};
use std::collections::BTreeMap;
use std::time::Duration;

/// Messages older than this when received live are treated as playback (no notifications).
const PLAYBACK_AGE_MS: i64 = 60_000;
const TYPING_TIMEOUT_MS: i64 = 6_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnState {
    Disconnected,
    Connecting,
    /// Transport up, registering.
    Connected,
    Ready,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Desktop notification.
    Notify {
        title: String,
        body: String,
        buffer: BufferId,
    },
    FlashTaskbar,
    /// Channel list results are ready (or being streamed) for this network.
    ChannelList(NetworkId),
    /// ZNC `ListNetworks` finished; offer to add these networks.
    ZncNetworks {
        network: NetworkId,
        names: Vec<String>,
    },
    /// soju advertised/changed its networks.
    BouncerNetworks(NetworkId),
    /// A multi-line paste needs confirmation before sending.
    ConfirmPaste {
        buffer: BufferId,
        text: String,
        lines: usize,
    },
    SaveConfig,
    ConfigChanged,
    OpenUrl(String),
    /// Run a Twitch live check on a worker thread and pass the result to `App::on_live_result`.
    TwitchLive(crate::helix::LiveRequest),
    /// Fetch emotes on a worker thread (`emotes::fetch`) and pass the result to
    /// `App::on_emote_result`.
    FetchEmotes(EmoteRequest),
    /// Talk to Twitch's OAuth server on a worker thread (`twitch_auth::execute`) and pass the
    /// response to `App::on_auth_result`.
    TwitchAuth {
        network: NetworkId,
        request: crate::twitch_auth::AuthRequest,
    },
    ReloadScripts,
    /// Open the settings dialog.
    OpenSettings,
    /// Open the network editor (`None` = add a new network).
    OpenNetwork(Option<String>),
    Quit,
}

/// The topic bar of a buffer, split so the UI can shorten only `body`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopicParts {
    pub title: String,
    pub lead: String,
    pub body: String,
    pub tail: String,
}

/// What the UI has to refresh after a batch of updates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dirty {
    pub sidebar: bool,
    pub lines: bool,
    pub nicklist: bool,
    pub topic: bool,
    pub input: bool,
}

pub struct Network {
    pub id: NetworkId,
    pub cfg: NetworkConfig,
    pub session: Session,
    pub conn: ConnState,
    pub lag_ms: Option<u64>,
    pub server_buffer: BufferId,
    pub server: Option<ServerAddr>,
    pub tls: Option<TlsInfo>,
    /// When the next reconnect attempt happens (Unix ms).
    pub retry_at: Option<i64>,
    pub last_error: Option<String>,
    pub channel_list: Vec<(String, u32, String)>,
    pub channel_list_complete: bool,
    pub bouncer_networks: BTreeMap<String, Vec<(String, String)>>,
    /// Label → buffer that issued the command (labeled-response routing).
    pub(crate) label_buffers: BTreeMap<String, BufferId>,
    pub(crate) znc_collect: Option<Vec<String>>,
    ctcp_window: (i64, u32),
    /// Channels we asked to join from this client (switch to them on join).
    pub(crate) pending_joins: Vec<String>,
    /// Twitch: our own display tags from USERSTATE/GLOBALUSERSTATE.
    pub twitch_self: Option<schwaetz_proto::Tags>,
    /// Newest message time seen on this network, for ZNC playback.
    pub last_seen: i64,
    pub user_quit: bool,
    pub(crate) live: LiveCheck,
    /// Twitch: "Sign in with Twitch" tokens for the Helix API.
    pub(crate) auth: crate::twitch_auth::TokenManager,
    /// Twitch: emotes for completion, fetched once per connection.
    pub(crate) emotes: ConnEmotes,
}

/// Twitch live-check scheduling for one network.
#[derive(Default)]
pub(crate) struct LiveCheck {
    /// After connecting: autojoin channels not joined yet, and when to stop waiting for them.
    waiting: Option<(Vec<String>, i64)>,
    /// Next periodic check (Unix ms; 0 = none scheduled).
    next_at: i64,
    in_flight: bool,
    /// Channels to check once the running check returns.
    queued: Vec<String>,
    client_id: Option<String>,
    ids: std::collections::HashMap<String, String>,
    last_error: Option<String>,
}

impl Network {
    pub fn display_name(&self) -> &str {
        if self.cfg.name.is_empty() {
            self.session.isupport().network.as_deref().unwrap_or("network")
        } else {
            &self.cfg.name
        }
    }

    pub fn is_twitch(&self) -> bool {
        self.cfg.kind == NetworkKind::Twitch
    }
}

/// How many visited buffers [`App::go_back`] can return to.
const MAX_VISITED: usize = 100;

pub struct App {
    pub config: Config,
    pub networks: BTreeMap<NetworkId, Network>,
    buffers: Vec<Buffer>,
    pub active: BufferId,
    /// Buffers visited before the active one (most recent last), and the ones gone back from.
    visited: (Vec<BufferId>, Vec<BufferId>),
    pub status_buffer: BufferId,
    next_line: u64,
    next_buffer: u32,
    next_network: u32,
    pub(crate) highlighter: Highlighter,
    pub(crate) ignores: Ignores,
    pub focused: bool,
    pub dark_theme: bool,
    net_out: Vec<NetCommand>,
    effects: Vec<Effect>,
    pub(crate) completer: Completer,
    pub dirty: Dirty,
    /// Line ids of the per-buffer "last read" separator, for the UI.
    pub now: i64,
    /// Lines added since the last `take_new_lines` (for scripts).
    new_lines: Vec<(BufferId, u64)>,
    pub track_new_lines: bool,
    /// Mirror raw protocol traffic into per-network "raw log" buffers.
    pub rawlog: bool,
    /// Tests: secrets live here instead of the Windows Credential Manager.
    mem_secrets: Option<std::collections::HashMap<String, String>>,
    /// The chat line being processed, as received (attached to the chat line it produces).
    current_raw: Option<Box<str>>,
    /// Emotes handed over by scripts per (provider, `#channel` or `None` for global).
    script_emotes: BTreeMap<(String, Option<String>), EmoteSet>,
    /// Whether the "sign in again for your emotes" hint was shown, per network.
    emote_scope_hint: std::collections::HashSet<NetworkId>,
    /// Bumped whenever emote lists change, so an open (or not yet opened) completion updates.
    pub emote_gen: u64,
    /// Commands registered by scripts (completion and /help).
    pub extra_commands: Vec<(String, String)>,
    /// Persistent history (logging, scroll-back, search).
    history: Option<Box<dyn HistoryStore>>,
}

impl App {
    pub fn new(config: Config) -> App {
        let (highlighter, errors) = Highlighter::new(&config.highlight);
        let ignores = Ignores::new(&config.ignores);
        let mut app = App {
            networks: BTreeMap::new(),
            buffers: Vec::new(),
            active: BufferId(0),
            status_buffer: BufferId(0),
            next_line: 1,
            next_buffer: 0,
            visited: Default::default(),
            next_network: 1,
            highlighter,
            ignores,
            focused: true,
            dark_theme: true,
            net_out: Vec::new(),
            effects: Vec::new(),
            completer: Completer::default(),
            dirty: Dirty { sidebar: true, lines: true, nicklist: true, topic: true, input: true },
            now: time::now_ms(),
            new_lines: Vec::new(),
            track_new_lines: false,
            rawlog: false,
            mem_secrets: None,
            current_raw: None,
            script_emotes: Default::default(),
            emote_scope_hint: Default::default(),
            emote_gen: 0,
            extra_commands: Vec::new(),
            history: None,
            config,
        };
        app.status_buffer = app.create_buffer(None, BufferKind::Special, "schwätz");
        app.active = app.status_buffer;
        for e in errors {
            app.status(app.status_buffer, LineKind::Error, e);
        }
        let nets = app.config.networks.clone();
        for n in nets {
            app.add_network(n);
        }
        app
    }

    // ----- accessors -----------------------------------------------------------------------------

    pub fn buffers(&self) -> &[Buffer] {
        &self.buffers
    }

    pub fn buffer(&self, id: BufferId) -> Option<&Buffer> {
        self.buffers.iter().find(|b| b.id == id)
    }

    pub fn buffer_mut(&mut self, id: BufferId) -> Option<&mut Buffer> {
        self.buffers.iter_mut().find(|b| b.id == id)
    }

    pub fn active_buffer(&self) -> &Buffer {
        self.buffer(self.active).unwrap_or(&self.buffers[0])
    }

    pub fn network(&self, id: NetworkId) -> Option<&Network> {
        self.networks.get(&id)
    }

    /// The sidebar badge of a buffer (count, includes highlights), in the network's badge mode or
    /// else `appearance.unread_badges`.
    pub fn badge(&self, buffer: BufferId) -> Option<(u32, bool)> {
        let b = self.buffer(buffer)?;
        let mode = self.network_of(buffer).and_then(|n| n.cfg.unread_badges.as_deref());
        b.badge(mode.unwrap_or(&self.config.appearance.unread_badges))
    }

    pub fn network_of(&self, buffer: BufferId) -> Option<&Network> {
        self.buffer(buffer).and_then(|b| b.network).and_then(|n| self.networks.get(&n))
    }

    pub fn find_buffer(&self, network: NetworkId, name: &str) -> Option<BufferId> {
        let net = self.networks.get(&network)?;
        let cm = net.session.casemapping();
        self.buffers
            .iter()
            .find(|b| b.network == Some(network) && b.kind != BufferKind::Server && cm.eq(&b.name, name))
            .map(|b| b.id)
    }

    /// Networks in sidebar order: as arranged in the config, then any not in it.
    pub fn network_order(&self) -> Vec<&Network> {
        let mut nets: Vec<&Network> = self.networks.values().collect();
        nets.sort_by_key(|n| network_rank(&self.config.networks, &n.cfg.name));
        nets
    }

    /// Buffers in sidebar order: status buffer, then per network its server buffer, channels and
    /// queries (each sorted), then other special buffers.
    pub fn sidebar_order(&self) -> Vec<BufferId> {
        let mut out = vec![self.status_buffer];
        for net in self.network_order() {
            out.push(net.server_buffer);
            let cm = net.session.casemapping();
            let mut chans: Vec<&Buffer> =
                self.buffers.iter().filter(|b| b.network == Some(net.id) && b.kind == BufferKind::Channel).collect();
            let live_first = net.is_twitch() && net.cfg.twitch_live_first;
            chans.sort_by_key(|b| {
                let live = live_first && b.stream.as_ref().is_some_and(|s| s.live);
                (!live, channel_rank(&net.cfg.channel_order, cm, &b.name), cm.fold(&b.name).into_owned())
            });
            out.extend(chans.iter().map(|b| b.id));
            let mut queries: Vec<&Buffer> = self
                .buffers
                .iter()
                .filter(|b| b.network == Some(net.id) && matches!(b.kind, BufferKind::Query | BufferKind::Special))
                .collect();
            queries.sort_by_key(|b| cm.fold(&b.name).into_owned());
            out.extend(queries.iter().map(|b| b.id));
        }
        out.extend(self.buffers.iter().filter(|b| b.network.is_none() && b.id != self.status_buffer).map(|b| b.id));
        out
    }

    pub fn take_net_commands(&mut self) -> Vec<NetCommand> {
        std::mem::take(&mut self.net_out)
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    pub fn take_dirty(&mut self) -> Dirty {
        std::mem::take(&mut self.dirty)
    }

    pub fn take_new_lines(&mut self) -> Vec<(BufferId, u64)> {
        std::mem::take(&mut self.new_lines)
    }

    /// Removes a line (scripts hiding messages).
    pub fn remove_line(&mut self, buffer: BufferId, line: u64) {
        if let Some(b) = self.buffer_mut(buffer) {
            b.lines.retain(|l| l.id != line);
            b.generation += 1;
        }
        self.dirty.lines = true;
    }

    /// Prints a client-side line into a buffer (scripts, /echo).
    pub fn print(&mut self, buffer: BufferId, kind: LineKind, nick: &str, text: &str) -> u64 {
        self.print_flagged(buffer, kind, nick, text, 0)
    }

    /// [`App::print`] with [`LineFlags`] set on the line.
    pub fn print_flagged(&mut self, buffer: BufferId, kind: LineKind, nick: &str, text: &str, flags: u16) -> u64 {
        let mut line = self.new_line(self.now, kind, nick, text);
        line.flags.set(flags, flags != 0);
        let id = line.id;
        self.add_line(buffer, line, Activity::None);
        id
    }

    pub(crate) fn effect(&mut self, e: Effect) {
        self.effects.push(e);
    }

    // ----- buffers -------------------------------------------------------------------------------

    pub(crate) fn create_buffer(&mut self, network: Option<NetworkId>, kind: BufferKind, name: &str) -> BufferId {
        let id = BufferId(self.next_buffer);
        self.next_buffer += 1;
        self.buffers.push(Buffer::new(id, network, kind, name, self.config.general.scrollback_lines));
        self.dirty.sidebar = true;
        id
    }

    /// Returns the buffer for a channel/query, creating it if needed.
    pub(crate) fn ensure_buffer(&mut self, network: NetworkId, kind: BufferKind, name: &str) -> BufferId {
        if let Some(id) = self.find_buffer(network, name) {
            return id;
        }
        let id = self.create_buffer(Some(network), kind, name);
        if matches!(kind, BufferKind::Channel | BufferKind::Query) {
            self.restore_from_history(id);
        }
        id
    }

    /// Attaches the history store; logging, scroll-back and search use it from now on.
    pub fn set_history(&mut self, mut h: Box<dyn HistoryStore>) {
        for n in self.networks.values_mut() {
            let name = n.display_name().to_owned();
            n.last_seen = h.last_seen(&name).unwrap_or(0);
        }
        self.history = Some(h);
    }

    pub fn has_history(&self) -> bool {
        self.history.is_some()
    }

    /// Fills a freshly created buffer with its most recent logged lines, so reopening a channel
    /// shows the previous conversation (and chathistory only has to fill the gap).
    fn restore_from_history(&mut self, id: BufferId) {
        let Some(b) = self.buffer(id) else { return };
        let Some(net) = b.network.and_then(|n| self.networks.get(&n)).map(|n| n.display_name().to_owned()) else {
            return;
        };
        let name = b.name.clone();
        let Some(h) = self.history.as_mut() else { return };
        let lines = h.load_before(&net, &name, i64::MAX, 150);
        if lines.is_empty() {
            return;
        }
        let newest = lines.iter().filter(|l| l.kind.is_message()).map(|l| l.time).max().unwrap_or(0);
        self.insert_history(id, lines);
        if let Some(b) = self.buffer_mut(id) {
            b.last_seen = newest;
            // Everything restored from disk was seen in an earlier session.
            b.read_marker = Some(b.read_marker.unwrap_or(0).max(newest));
            b.history_exhausted = false;
        }
    }

    /// Full-text search over the history, printed into a "search" buffer.
    pub fn search(&mut self, query: &str, network: Option<String>) {
        let results = match self.history.as_mut() {
            Some(h) => h.search(query, network.as_deref(), None, 200),
            None => {
                let id = self.active;
                self.status(id, LineKind::Error, "Search needs the message history, which is disabled.");
                return;
            }
        };
        let sid = match self
            .buffers
            .iter()
            .find(|b| b.network.is_none() && b.kind == BufferKind::Special && b.name == "search")
        {
            Some(b) => b.id,
            None => self.create_buffer(None, BufferKind::Special, "search"),
        };
        if let Some(b) = self.buffer_mut(sid) {
            b.clear();
        }
        let header = format!(
            "{} result(s) for \"{query}\"{}",
            results.len(),
            network.map(|n| format!(" on {n}")).unwrap_or_default()
        );
        self.status(sid, LineKind::Status, header);
        for (net, buf, line) in results.into_iter().rev() {
            let when = time::format("%Y-%m-%d %H:%M", time::local(line.time));
            let text = format!("{when}  {net} / {buf}  <{}> {}", line.display_nick(), fmt::strip(&line.text));
            self.status(sid, LineKind::Server, text);
        }
        self.switch_to(sid);
    }

    /// Shows a buffer, remembering the one left for [`App::go_back`].
    pub fn switch_to(&mut self, id: BufferId) {
        if self.buffer(id).is_none() {
            return;
        }
        if id != self.active && self.buffer(self.active).is_some() {
            let back = &mut self.visited.0;
            back.push(self.active);
            if back.len() > MAX_VISITED {
                back.remove(0);
            }
            self.visited.1.clear();
        }
        self.show(id);
    }

    /// Goes back to the buffer visited before (`forward`: undoes that); false if there is none.
    /// Buffers closed since are skipped.
    pub fn go_back(&mut self, forward: bool) -> bool {
        loop {
            let (from, to) = if forward {
                (&mut self.visited.1, &mut self.visited.0)
            } else {
                (&mut self.visited.0, &mut self.visited.1)
            };
            let Some(id) = from.pop() else { return false };
            if id == self.active || !self.buffers.iter().any(|b| b.id == id) {
                continue;
            }
            to.push(self.active);
            self.show(id);
            return true;
        }
    }

    fn show(&mut self, id: BufferId) {
        let prev = self.active;
        if prev != id {
            self.sync_read_marker(prev);
        }
        self.active = id;
        // Twitch channels: have their emotes ready for completion.
        self.request_emotes(id);
        self.completer.reset();
        if let Some(b) = self.buffer_mut(id) {
            b.mark_read();
        }
        self.dirty = Dirty { sidebar: true, lines: true, nicklist: true, topic: true, input: true };
    }

    /// Sends the read marker to the server (draft/read-marker) for a buffer being left.
    fn sync_read_marker(&mut self, id: BufferId) {
        let Some(b) = self.buffer(id) else { return };
        let (Some(net), Some(marker), name) = (b.network, b.read_marker, b.name.clone()) else { return };
        if matches!(b.kind, BufferKind::Channel | BufferKind::Query)
            && let Some(n) = self.networks.get_mut(&net)
            && n.conn == ConnState::Ready
        {
            n.session.mark_read(&name, marker);
            self.flush(net);
        }
    }

    pub fn close_buffer(&mut self, id: BufferId) {
        let Some(b) = self.buffer(id) else { return };
        if id == self.status_buffer || b.kind == BufferKind::Server {
            return;
        }
        let order = self.sidebar_order();
        let pos = order.iter().position(|x| *x == id).unwrap_or(0);
        self.buffers.retain(|b| b.id != id);
        if self.active == id {
            let next = order.get(pos + 1).or(order.get(pos.wrapping_sub(1))).copied().unwrap_or(self.status_buffer);
            let next = if self.buffer(next).is_some() { next } else { self.status_buffer };
            self.switch_to(next);
        }
        self.dirty.sidebar = true;
    }

    fn line_id(&mut self) -> u64 {
        self.next_line += 1;
        self.next_line
    }

    pub(crate) fn new_line(&mut self, time: i64, kind: LineKind, nick: &str, text: impl Into<Box<str>>) -> Line {
        Line {
            id: self.line_id(),
            time,
            kind,
            flags: LineFlags::default(),
            nick: nick.into(),
            prefix: None,
            text: text.into(),
            extra: None,
        }
    }

    /// Adds a line, updating activity and emitting a log effect.
    pub(crate) fn add_line(&mut self, buffer: BufferId, line: Line, activity: Activity) {
        let active = self.active == buffer && self.focused;
        let is_active = self.active == buffer;
        let Some(b) = self.buffers.iter_mut().find(|b| b.id == buffer) else { return };
        let history = line.flags.has(LineFlags::HISTORY);
        let unread_by_marker = b.read_marker.is_none_or(|m| line.time > m);
        if !line.flags.has(LineFlags::FILTERED) && !line.flags.has(LineFlags::OWN) && !(history && !unread_by_marker) {
            let level = match b.notify {
                NotifyLevel::Mute => Activity::None,
                NotifyLevel::HighlightsOnly if activity < Activity::Highlight => Activity::None,
                _ => activity,
            };
            if !active && level != Activity::None {
                b.bump(level);
                if level >= Activity::Messages {
                    b.unread += 1;
                }
                if level == Activity::Highlight {
                    b.highlights += 1;
                }
                self.dirty.sidebar = true;
            }
        }
        if line.flags.has(LineFlags::OWN) && !history {
            // Sending a message means we've read everything before it.
            b.read_marker = Some(line.time);
        }
        // Only server traffic moves the gap-fill anchor; local status lines must not.
        if !history && (line.kind.is_message() || line.kind == LineKind::System) {
            b.last_seen = b.last_seen.max(line.time);
        }
        let logged = if self.history.is_some()
            && matches!(b.kind, BufferKind::Channel | BufferKind::Query | BufferKind::Server)
            && worth_logging(&line)
        {
            let net_name = b.network.and_then(|n| self.networks.get(&n)).map(|n| n.display_name().to_owned());
            net_name.map(|n| (n, b.name.clone(), line.clone()))
        } else {
            None
        };
        let (bid, lid) = (b.id, line.id);
        b.push(line);
        if self.track_new_lines {
            self.new_lines.push((bid, lid));
        }
        if is_active {
            self.dirty.lines = true;
        }
        if let Some((network, name, line)) = logged
            && let Some(h) = self.history.as_mut()
        {
            h.log(&network, &name, &line);
        }
    }

    pub(crate) fn status(&mut self, buffer: BufferId, kind: LineKind, text: impl Into<String>) {
        let line = self.new_line(self.now, kind, "", text.into());
        let activity = if kind == LineKind::Error { Activity::Messages } else { Activity::None };
        self.add_line(buffer, line, activity);
    }

    /// Prints into the active buffer if it belongs to `net`, else the network's server buffer.
    pub(crate) fn status_for(&mut self, net: NetworkId, kind: LineKind, text: impl Into<String>) {
        let target = self.contextual_buffer(net);
        self.status(target, kind, text);
    }

    pub(crate) fn contextual_buffer(&self, net: NetworkId) -> BufferId {
        if self.buffer(self.active).is_some_and(|b| b.network == Some(net)) {
            self.active
        } else {
            self.networks.get(&net).map_or(self.status_buffer, |n| n.server_buffer)
        }
    }

    // ----- networks ------------------------------------------------------------------------------

    pub fn add_network(&mut self, cfg: NetworkConfig) -> NetworkId {
        let auth = self.load_auth(&cfg);
        let id = NetworkId(self.next_network);
        self.next_network += 1;
        let name =
            if cfg.name.is_empty() { cfg.servers.first().cloned().unwrap_or_default() } else { cfg.name.clone() };
        let server_buffer = self.create_buffer(Some(id), BufferKind::Server, &name);
        let session = Session::new(self.session_config(&cfg, true, 6697));
        self.networks.insert(
            id,
            Network {
                id,
                cfg,
                session,
                conn: ConnState::Disconnected,
                lag_ms: None,
                server_buffer,
                server: None,
                tls: None,
                retry_at: None,
                last_error: None,
                channel_list: Vec::new(),
                channel_list_complete: false,
                bouncer_networks: BTreeMap::new(),
                label_buffers: BTreeMap::new(),
                znc_collect: None,
                ctcp_window: (0, 0),
                pending_joins: Vec::new(),
                twitch_self: None,
                last_seen: 0,
                user_quit: false,
                live: LiveCheck::default(),
                auth,
                emotes: ConnEmotes::default(),
            },
        );
        self.dirty.sidebar = true;
        id
    }

    pub fn remove_network(&mut self, id: NetworkId) {
        self.disconnect(id, None);
        self.networks.remove(&id);
        self.buffers.retain(|b| b.network != Some(id));
        if self.buffer(self.active).is_none() {
            self.active = self.status_buffer;
        }
        self.dirty = Dirty { sidebar: true, lines: true, nicklist: true, topic: true, input: true };
    }

    fn session_config(&self, cfg: &NetworkConfig, tls: bool, port: u16) -> SessionConfig {
        let g = &self.config.general;
        let nick = cfg.nick.clone().unwrap_or_else(|| g.nick.clone());
        let alt_nicks = if cfg.alt_nicks.is_empty() { g.alt_nicks.clone() } else { cfg.alt_nicks.clone() };
        let secret = |k| secrets::get(&cfg.name, k);
        let sasl_user = cfg.sasl_username.clone().unwrap_or_else(|| nick.clone());
        let sasl = match cfg.sasl {
            SaslMechanism::None => None,
            SaslMechanism::External => Some(SaslConfig::External),
            SaslMechanism::Plain => {
                secret(SecretKind::Sasl).map(|password| SaslConfig::Plain { username: sasl_user.clone(), password })
            }
            SaslMechanism::ScramSha256 => secret(SecretKind::Sasl)
                .map(|password| SaslConfig::ScramSha256 { username: sasl_user.clone(), password }),
        };
        let password = match cfg.kind {
            NetworkKind::Twitch => {
                secret(SecretKind::TwitchToken).map(|t| if t.starts_with("oauth:") { t } else { format!("oauth:{t}") })
            }
            NetworkKind::Znc if cfg.server_password => secret(SecretKind::ServerPassword)
                .map(|p| znc::pass(cfg.znc_user.as_deref().unwrap_or(&nick), cfg.znc_network.as_deref(), &p)),
            _ if cfg.server_password => secret(SecretKind::ServerPassword),
            _ => None,
        };
        let twitch = cfg.kind == NetworkKind::Twitch;
        SessionConfig {
            nick: if twitch { nick.to_lowercase() } else { nick },
            alt_nicks,
            username: cfg.username.clone().unwrap_or_else(|| g.username.clone()),
            realname: cfg.realname.clone().unwrap_or_else(|| g.realname.clone()),
            password,
            sasl,
            sasl_required: cfg.sasl_required,
            autojoin: cfg.autojoin_list(),
            twitch,
            tls,
            port,
            bouncer_netid: cfg.bouncer_netid.clone(),
            ..Default::default()
        }
    }

    fn connect_params(&self, cfg: &NetworkConfig) -> ConnectParams {
        let servers = cfg
            .servers
            .iter()
            .filter_map(|s| NetworkConfig::parse_server(s))
            .map(|(host, port, tls)| ServerAddr { host, port, tls, accept_invalid_certs: cfg.accept_invalid_certs })
            .collect();
        let client_cert = cfg.client_cert.as_ref().and_then(|p| {
            let pem = std::fs::read(p).ok()?;
            Some(ClientCert { key_pem: pem.clone(), cert_pem: pem })
        });
        ConnectParams {
            servers,
            client_cert,
            flood: if cfg.kind == NetworkKind::Twitch {
                FloodControl::twitch()
            } else {
                FloodControl {
                    burst: cfg.flood_burst.max(1),
                    interval: Duration::from_millis(cfg.flood_interval_ms.max(50)),
                }
            },
            reconnect: Reconnect {
                enabled: cfg.reconnect,
                max_attempts: cfg.reconnect_max_attempts,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    pub fn connect(&mut self, id: NetworkId) {
        let Some(net) = self.networks.get(&id) else { return };
        let params = self.connect_params(&net.cfg);
        if params.servers.is_empty() {
            let sb = net.server_buffer;
            self.status(sb, LineKind::Error, "No valid server address configured for this network.");
            return;
        }
        let net = self.networks.get_mut(&id).unwrap();
        net.user_quit = false;
        net.conn = ConnState::Connecting;
        self.net_out.push(NetCommand::Connect(id, Box::new(params)));
        self.dirty.sidebar = true;
    }

    pub fn connect_auto(&mut self) {
        let ids: Vec<NetworkId> = self.networks.values().filter(|n| n.cfg.auto_connect).map(|n| n.id).collect();
        for id in ids {
            self.connect(id);
        }
    }

    pub fn disconnect(&mut self, id: NetworkId, quit: Option<String>) {
        let Some(net) = self.networks.get_mut(&id) else { return };
        net.user_quit = true;
        let msg = quit.or_else(|| Some(self.config.general.quit_message.clone())).filter(|q| !q.is_empty());
        self.net_out.push(NetCommand::Disconnect(id, msg));
    }

    pub fn reconnect_now(&mut self, id: NetworkId) {
        let Some(net) = self.networks.get(&id) else { return };
        if net.conn == ConnState::Disconnected && net.user_quit {
            self.connect(id);
        } else {
            self.net_out.push(NetCommand::ReconnectNow(id));
        }
    }

    /// OS resumed from sleep or connectivity returned: retry disconnected networks right away.
    pub fn on_connectivity_restored(&mut self) {
        let ids: Vec<NetworkId> = self
            .networks
            .values()
            .filter(|n| !n.user_quit && matches!(n.conn, ConnState::Disconnected | ConnState::Connecting))
            .map(|n| n.id)
            .collect();
        for id in ids {
            self.net_out.push(NetCommand::ReconnectNow(id));
        }
    }

    pub fn quit_all(&mut self, message: Option<String>) {
        let ids: Vec<NetworkId> = self.networks.keys().copied().collect();
        for id in ids {
            self.disconnect(id, message.clone());
        }
    }

    /// Moves queued session output to the network thread.
    pub(crate) fn flush(&mut self, id: NetworkId) {
        self.flush_outgoing(id);
        let Some(net) = self.networks.get_mut(&id) else { return };
        let events: Vec<SessionEvent> = net.session.drain_events().collect();
        for ev in events {
            self.on_session_event(id, ev);
        }
        // Handling events may queue more output.
        self.flush_outgoing(id);
    }

    fn flush_outgoing(&mut self, id: NetworkId) {
        let Some(net) = self.networks.get_mut(&id) else { return };
        let out: Vec<Message> = net.session.drain_outgoing().collect();
        for m in out {
            if self.rawlog {
                self.raw_log(id, "»", &m);
            }
            self.net_out.push(NetCommand::Send(id, m));
        }
    }

    /// Appends a line to the network's raw log buffer, masking secrets.
    fn raw_log(&mut self, id: NetworkId, dir: &str, m: &Message) {
        let mut shown = m.clone();
        let secret = shown.is("PASS")
            || shown.is("OPER")
            || (shown.is("AUTHENTICATE")
                && shown.arg(0).len() > 1
                && !shown.arg(0).chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-'));
        if secret {
            let skip = usize::from(shown.is("OPER"));
            for p in shown.params.iter_mut().skip(skip) {
                *p = "********".into();
            }
        }
        let bid = self.ensure_buffer(id, BufferKind::Special, "raw log");
        let line = self.new_line(self.now, LineKind::Server, dir, shown.to_line());
        self.add_line(bid, line, Activity::None);
    }

    // ----- network events ------------------------------------------------------------------------

    pub fn on_net_event(&mut self, ev: NetEvent, now: i64) {
        self.now = now;
        match ev {
            NetEvent::Connecting { id, server, attempt } => {
                let Some(net) = self.networks.get_mut(&id) else { return };
                net.conn = ConnState::Connecting;
                net.retry_at = None;
                let sb = net.server_buffer;
                let text = if attempt > 1 {
                    format!("Connecting to {server} (attempt {attempt})…")
                } else {
                    format!("Connecting to {server}…")
                };
                self.status(sb, LineKind::Status, text);
                self.dirty.sidebar = true;
            }
            NetEvent::Connected { id, server, tls } => {
                let cfg = {
                    let Some(net) = self.networks.get(&id) else { return };
                    net.cfg.clone()
                };
                let scfg = self.session_config(&cfg, server.tls, server.port);
                let net = self.networks.get_mut(&id).unwrap();
                *net.session.config_mut() = scfg;
                net.conn = ConnState::Connected;
                net.server = Some(server.clone());
                net.tls = tls.clone();
                net.last_error = None;
                net.session.on_connect(now);
                let sb = net.server_buffer;
                let secure = match &tls {
                    Some(t) => format!(" ({} {})", t.protocol, t.cipher),
                    None => " (unencrypted)".into(),
                };
                self.status(sb, LineKind::Status, format!("Connected to {server}{secure}"));
                self.flush(id);
                self.dirty.sidebar = true;
            }
            NetEvent::Line { id, msg } => {
                if self.rawlog && self.networks.contains_key(&id) {
                    self.raw_log(id, "«", &msg);
                }
                // Chat lines keep their raw form ("View raw message").
                self.current_raw = matches!(msg.command.as_str(), "PRIVMSG" | "NOTICE" | "USERNOTICE")
                    .then(|| msg.to_line().into_boxed_str());
                if let Some(net) = self.networks.get_mut(&id) {
                    net.session.on_message(msg, now);
                    self.flush(id);
                }
                self.current_raw = None;
            }
            NetEvent::Lag { id, ms } => {
                if let Some(net) = self.networks.get_mut(&id) {
                    net.lag_ms = Some(ms);
                    self.dirty.topic = true;
                }
            }
            NetEvent::Disconnected { id, reason, retry_in } => {
                let Some(net) = self.networks.get_mut(&id) else { return };
                let was_up = matches!(net.conn, ConnState::Connected | ConnState::Ready);
                net.session.on_disconnect();
                net.emotes.reset();
                net.conn = if retry_in.is_some() { ConnState::Connecting } else { ConnState::Disconnected };
                net.lag_ms = None;
                net.retry_at = retry_in.map(|d| now + d.as_millis() as i64);
                net.last_error = Some(reason.clone());
                let sb = net.server_buffer;
                let text = match retry_in {
                    Some(d) if d.is_zero() && reason == "Reconnecting" => "Reconnecting…".to_owned(),
                    Some(d) if d.is_zero() => format!("Disconnected: {reason}. Reconnecting…"),
                    Some(d) => {
                        format!("Disconnected: {reason}. Reconnecting in {}…", time::duration(d.as_secs().max(1)))
                    }
                    None => format!("Disconnected: {reason}"),
                };
                if was_up {
                    let ids: Vec<BufferId> = self
                        .buffers
                        .iter()
                        .filter(|b| b.network == Some(id) && b.kind != BufferKind::Server)
                        .map(|b| b.id)
                        .collect();
                    for bid in ids {
                        if let Some(b) = self.buffer_mut(bid) {
                            b.joined = false;
                            b.typing.clear();
                        }
                        self.status(bid, LineKind::Error, text.clone());
                    }
                }
                if let Some(net) = self.networks.get_mut(&id) {
                    net.live.stop();
                }
                self.status(sb, LineKind::Error, text);
                self.dirty = Dirty { sidebar: true, lines: true, nicklist: true, topic: true, input: false };
            }
            NetEvent::Stopped { id } => {
                if let Some(net) = self.networks.get_mut(&id) {
                    net.conn = ConnState::Disconnected;
                    net.retry_at = None;
                    self.dirty.sidebar = true;
                }
            }
        }
    }

    /// Periodic housekeeping (about once per second).
    pub fn tick(&mut self, now: i64) {
        self.now = now;
        let ids: Vec<NetworkId> = self.networks.keys().copied().collect();
        for id in ids {
            if let Some(n) = self.networks.get_mut(&id) {
                n.session.tick(now);
            }
            self.flush(id);
            self.live_tick(id);
        }
        for id in self.networks.keys().copied().collect::<Vec<_>>() {
            self.auth_tick(id);
        }
        // Expire typing indicators.
        let active = self.active;
        for b in &mut self.buffers {
            let before = b.typing.len();
            b.typing.retain(|t| t.expires > now);
            if b.typing.len() != before && b.id == active {
                self.dirty.topic = true;
            }
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        if focused {
            let id = self.active;
            if let Some(b) = self.buffer_mut(id) {
                b.mark_read();
            }
            self.dirty.sidebar = true;
        }
    }

    // ----- session events ------------------------------------------------------------------------

    fn on_session_event(&mut self, net_id: NetworkId, ev: SessionEvent) {
        let time = ev.time;
        let label_buffer = ev.label.as_ref().and_then(|l| self.networks.get(&net_id)?.label_buffers.get(l).copied());
        let Some(net) = self.networks.get(&net_id) else { return };
        let sb = net.server_buffer;
        match ev.kind {
            Event::Status(text) => self.status(sb, LineKind::Status, text),
            Event::ServerText { msg } => self.server_text(net_id, &msg, time, label_buffer),
            Event::Registered { nick } => {
                self.status(sb, LineKind::Status, format!("Registered as {nick}"));
                self.dirty.sidebar = true;
            }
            Event::Ready => self.on_ready(net_id),
            Event::ISupportChanged => {
                self.dirty.sidebar = true;
                if let Some(b) = self.buffer(sb) {
                    // Show the network's own name once it's known.
                    let net = &self.networks[&net_id];
                    if net.cfg.name.is_empty() {
                        let name = net.display_name().to_owned();
                        if b.name != name {
                            self.buffer_mut(sb).unwrap().name = name;
                        }
                    }
                }
            }
            Event::CapsChanged { enabled } => {
                if self.networks[&net_id].conn == ConnState::Connected && !enabled.is_empty() {
                    self.status(sb, LineKind::Status, format!("Capabilities: {}", enabled.join(" ")));
                }
            }
            Event::SaslResult { success, message } => {
                let kind = if success { LineKind::Status } else { LineKind::Error };
                let text = if success {
                    format!("SASL authentication successful: {message}")
                } else {
                    format!("SASL authentication failed: {message}")
                };
                self.status(sb, kind, text);
            }
            Event::ReconnectRequested { tls_port } => {
                if let Some(port) = tls_port {
                    // STS: upgrade the stored server list to TLS on the advertised port.
                    let net = self.networks.get_mut(&net_id).unwrap();
                    let host = net.server.as_ref().map(|s| s.host.clone()).unwrap_or_default();
                    for s in &mut net.cfg.servers {
                        if NetworkConfig::parse_server(s).is_some_and(|(h, _, _)| h == host) {
                            *s = format!("{host}:+{port}");
                        }
                    }
                    let cfg = net.cfg.clone();
                    if let Some(c) = self.config.networks.iter_mut().find(|n| n.name == cfg.name) {
                        c.servers = cfg.servers.clone();
                        self.effects.push(Effect::SaveConfig);
                    }
                    let params = self.connect_params(&cfg);
                    self.net_out.push(NetCommand::Update(net_id, Box::new(params)));
                }
                self.net_out.push(NetCommand::ReconnectNow(net_id));
            }
            Event::StsPolicy { .. } => {}
            Event::Error { message } => self.status(sb, LineKind::Error, format!("Server error: {message}")),
            Event::NickChangedSelf { new, .. } => {
                self.status(sb, LineKind::Status, format!("You are now known as {new}"));
                self.dirty.sidebar = true;
                self.dirty.input = true;
            }
            Event::Chat(chat) => self.on_chat(net_id, chat, time, false),
            Event::CtcpRequest { from, target, command, params } => {
                self.on_ctcp_request(net_id, from, target, command, params, time)
            }
            Event::CtcpReply { from, command, params } => {
                let text = if command == "PING" {
                    match params.parse::<i64>() {
                        Ok(sent) => {
                            format!("CTCP PING reply from {}: {:.3}s", from.nick, (self.now - sent) as f64 / 1000.0)
                        }
                        Err(_) => format!("CTCP PING reply from {}: {params}", from.nick),
                    }
                } else {
                    format!("CTCP {command} reply from {}: {}", from.nick, fmt::strip(&params))
                };
                let target = label_buffer.unwrap_or_else(|| self.contextual_buffer(net_id));
                let line = self.new_line(time, LineKind::Ctcp, &from.nick, text);
                self.add_line(target, line, Activity::None);
            }
            Event::Join { channel, user, account, own, .. } => {
                self.on_join(net_id, &channel, &user, account, own, time, false)
            }
            Event::Part { channel, user, reason, own } => {
                // Before the buffer lookup: closing a channel removes its buffer before the
                // server confirms the part.
                if own {
                    self.remember_channel(net_id, &channel, false);
                }
                let bid = self.find_buffer(net_id, &channel);
                let Some(bid) = bid else { return };
                if own && let Some(b) = self.buffer_mut(bid) {
                    b.joined = false;
                }
                let text = format!(
                    "{} has left {channel}{}",
                    self.who(net_id, &user),
                    reason.filter(|r| !r.is_empty()).map(|r| format!(" ({})", fmt::strip(&r))).unwrap_or_default()
                );
                self.membership_line(net_id, bid, LineKind::Part, &user, text, time, own, false);
                self.dirty.nicklist = true;
                self.dirty.sidebar |= own;
            }
            Event::Kick { channel, by, nick, reason, own } => {
                let Some(bid) = self.find_buffer(net_id, &channel) else { return };
                let reason = reason.map(|r| fmt::strip(&r)).unwrap_or_default();
                let text = if own {
                    format!("You were kicked from {channel} by {} ({reason})", by.nick)
                } else {
                    format!("{nick} was kicked by {} ({reason})", by.nick)
                };
                let mut line = self.new_line(time, LineKind::Kick, &nick, text);
                line.flags.set(LineFlags::HIGHLIGHT, own);
                self.add_line(bid, line, if own { Activity::Highlight } else { Activity::None });
                if own {
                    if let Some(b) = self.buffer_mut(bid) {
                        b.joined = false;
                    }
                    let rejoin = self.networks[&net_id].cfg.rejoin_on_kick;
                    if rejoin {
                        let key = self.networks[&net_id].session.channel(&channel).and_then(|c| c.key.clone());
                        let mut params = vec![channel.clone()];
                        params.extend(key);
                        self.send(net_id, Message::new("JOIN", params));
                    }
                    self.notify(bid, format!("Kicked from {channel}"), format!("by {}: {reason}", by.nick));
                }
                self.dirty.nicklist = true;
                self.dirty.sidebar = true;
            }
            Event::Quit { user, reason, channels, netsplit } => {
                let reason = reason.map(|r| fmt::strip(&r)).unwrap_or_default();
                let split = netsplit || looks_like_netsplit(&reason);
                let mut targets: Vec<BufferId> = channels.iter().filter_map(|c| self.find_buffer(net_id, c)).collect();
                if let Some(q) = self.find_buffer(net_id, &user.nick) {
                    targets.push(q);
                }
                for bid in targets {
                    if split && self.fold_into_netsplit(bid, &user.nick, &reason, time) {
                        continue;
                    }
                    let text = if split {
                        format!("Netsplit {reason}: {}", user.nick)
                    } else if reason.is_empty() {
                        format!("{} has quit", self.who(net_id, &user))
                    } else {
                        format!("{} has quit ({reason})", self.who(net_id, &user))
                    };
                    let kind = if split { LineKind::Netsplit } else { LineKind::Quit };
                    self.membership_line(net_id, bid, kind, &user, text, time, false, false);
                }
                self.dirty.nicklist = true;
            }
            Event::Nick { old, new, channels, own } => {
                let mut targets: Vec<BufferId> = channels.iter().filter_map(|c| self.find_buffer(net_id, c)).collect();
                if let Some(q) = self.find_buffer(net_id, &old) {
                    // Follow the user: rename the query.
                    if let Some(b) = self.buffer_mut(q)
                        && b.kind == BufferKind::Query
                    {
                        b.name = new.clone();
                        self.dirty.sidebar = true;
                    }
                    targets.push(q);
                }
                let text =
                    if own { format!("You are now known as {new}") } else { format!("{old} is now known as {new}") };
                let cm = self.networks[&net_id].session.casemapping();
                for bid in targets {
                    let src = Source::nick(old.clone());
                    self.membership_line(net_id, bid, LineKind::Nick, &src, text.clone(), time, own, false);
                    if let Some(b) = self.buffer_mut(bid)
                        && let Some(t) = b.last_spoke.remove(cm.fold(&old).as_ref())
                    {
                        b.last_spoke.insert(cm.fold(&new).into_owned(), t);
                    }
                }
                self.dirty.nicklist = true;
            }
            Event::Topic { channel, topic, by, changed } => {
                let Some(bid) = self.find_buffer(net_id, &channel) else { return };
                let text = match (&by, changed) {
                    (Some(by), true) if topic.is_empty() => format!("{by} removed the topic"),
                    (Some(by), true) => format!("{by} changed the topic to: {topic}"),
                    _ if topic.is_empty() => "No topic is set".to_owned(),
                    _ => format!("Topic: {topic}"),
                };
                let line = self.new_line(time, LineKind::Topic, by.as_deref().unwrap_or(""), text);
                self.add_line(bid, line, Activity::None);
                self.dirty.topic = true;
            }
            Event::ChannelMode { channel, by, changes } => {
                let Some(bid) = self.find_buffer(net_id, &channel) else { return };
                let mut s = String::new();
                let mut args = Vec::new();
                let mut last = None;
                for c in &changes {
                    if last != Some(c.add) {
                        s.push(if c.add { '+' } else { '-' });
                        last = Some(c.add);
                    }
                    s.push(c.mode);
                    if let Some(a) = &c.arg {
                        args.push(a.as_str());
                    }
                }
                let modes = if args.is_empty() { s } else { format!("{s} {}", args.join(" ")) };
                let text = match &by {
                    Some(by) => format!("{} sets mode {modes}", by.nick),
                    None => format!("Channel modes: {modes}"),
                };
                let nick = by.as_ref().map(|b| b.nick.clone()).unwrap_or_default();
                let line = self.new_line(time, LineKind::Mode, &nick, text);
                self.add_line(bid, line, Activity::None);
                self.dirty.topic = true;
                self.dirty.nicklist = true;
            }
            Event::UserMode { changes } => {
                let modes: String =
                    changes.iter().map(|c| format!("{}{}", if c.add { '+' } else { '-' }, c.mode)).collect();
                self.status(sb, LineKind::Mode, format!("Your user mode: {modes}"));
            }
            Event::MembersChanged { channel } => {
                if self.buffer(self.active).is_some_and(|b| {
                    b.network == Some(net_id) && self.networks[&net_id].session.casemapping().eq(&b.name, &channel)
                }) {
                    self.dirty.nicklist = true;
                }
            }
            Event::UserUpdated { .. } => {
                if self.buffer(self.active).is_some_and(|b| b.network == Some(net_id)) {
                    self.dirty.nicklist = true;
                }
            }
            Event::Invite { by, nick, channel } => {
                let src = by.clone();
                if self.is_ignored(net_id, None, &src, IgnoreType::Invite) {
                    return;
                }
                let me = self.networks[&net_id].session.is_me(&nick);
                let text = if me {
                    format!("{} invites you to {channel}", by.nick)
                } else {
                    format!("{} invited {nick} to {channel}", by.nick)
                };
                let target = self.contextual_buffer(net_id);
                let line = self.new_line(time, LineKind::Invite, &by.nick, text.clone());
                self.add_line(target, line, if me { Activity::Highlight } else { Activity::None });
                if me {
                    self.notify(target, "Invitation".into(), text);
                }
            }
            Event::Whois(w) => {
                let target = label_buffer.unwrap_or_else(|| self.contextual_buffer(net_id));
                let mut lines = vec![format!(
                    "[{}] {}@{} — {}",
                    w.nick,
                    w.user.as_deref().unwrap_or("?"),
                    w.host.as_deref().unwrap_or("?"),
                    w.realname.as_deref().map(fmt::strip).unwrap_or_default()
                )];
                if let Some(a) = &w.account {
                    lines.push(format!("[{}] logged in as {a}", w.nick));
                }
                if !w.channels.is_empty() {
                    lines.push(format!("[{}] channels: {}", w.nick, w.channels.join(" ")));
                }
                if let Some(s) = &w.server {
                    lines.push(format!("[{}] server: {s} ({})", w.nick, w.server_info.as_deref().unwrap_or("")));
                }
                if let Some(o) = &w.operator {
                    lines.push(format!("[{}] {o}", w.nick));
                }
                if let Some(a) = &w.away {
                    lines.push(format!("[{}] away: {}", w.nick, fmt::strip(a)));
                }
                if w.secure {
                    lines.push(format!("[{}] is using a secure connection", w.nick));
                }
                if w.bot {
                    lines.push(format!("[{}] is a bot", w.nick));
                }
                if let Some(h) = &w.actual_host {
                    lines.push(format!("[{}] actual host: {h}", w.nick));
                }
                if let Some(c) = &w.certfp {
                    lines.push(format!("[{}] {c}", w.nick));
                }
                for e in &w.extra {
                    lines.push(format!("[{}] {e}", w.nick));
                }
                if let Some(idle) = w.idle_secs {
                    let signon = w.signon.map(|s| time::format("%Y-%m-%d %H:%M", time::local(s))).unwrap_or_default();
                    lines.push(format!("[{}] idle {}, signed on {signon}", w.nick, time::duration(idle)));
                }
                for l in lines {
                    let line = self.new_line(time, LineKind::Server, &w.nick, l);
                    self.add_line(target, line, Activity::None);
                }
            }
            Event::ListEntry { channel, users, topic } => {
                let net = self.networks.get_mut(&net_id).unwrap();
                if net.channel_list_complete {
                    net.channel_list.clear();
                    net.channel_list_complete = false;
                }
                net.channel_list.push((channel, users, fmt::strip(&topic)));
                if net.channel_list.len().is_multiple_of(500) {
                    self.effects.push(Effect::ChannelList(net_id));
                }
            }
            Event::ListEnd => {
                let net = self.networks.get_mut(&net_id).unwrap();
                net.channel_list_complete = true;
                let n = net.channel_list.len();
                self.effects.push(Effect::ChannelList(net_id));
                self.status_for(net_id, LineKind::Status, format!("Channel list: {n} channels"));
            }
            Event::ModeList { channel, mode, entries } => {
                let target = self.find_buffer(net_id, &channel).unwrap_or_else(|| self.contextual_buffer(net_id));
                let what = match mode {
                    'b' => "Ban",
                    'e' => "Exception",
                    'I' => "Invite exception",
                    'q' => "Quiet",
                    _ => "Mode list",
                };
                if entries.is_empty() {
                    self.status(target, LineKind::Server, format!("{what} list for {channel} is empty"));
                }
                for e in entries {
                    let when = e.set_at.map(|t| time::format(" on %Y-%m-%d %H:%M", time::local(t))).unwrap_or_default();
                    let by = e.set_by.map(|b| format!(" by {b}")).unwrap_or_default();
                    self.status(target, LineKind::Server, format!("{what}: {}{by}{when}", e.mask));
                }
            }
            Event::Names { channel, names } => {
                let target = label_buffer.unwrap_or_else(|| self.contextual_buffer(net_id));
                self.status(target, LineKind::Server, format!("Users on {channel}: {}", names.join(" ")));
            }
            Event::StandardReply { kind, command, code, context, description } => {
                use schwaetz_client::StandardReplyKind as K;
                let target = label_buffer
                    .or_else(|| context.iter().find_map(|c| self.find_buffer(net_id, c)))
                    .unwrap_or_else(|| self.contextual_buffer(net_id));
                let (lk, tag) = match kind {
                    K::Fail => (LineKind::Error, "Failed"),
                    K::Warn => (LineKind::Error, "Warning"),
                    K::Note => (LineKind::Server, "Note"),
                };
                if command == "CHATHISTORY" {
                    if let Some(b) = self.buffer_mut(target) {
                        b.history_loading = false;
                    }
                    if code == "INVALID_TARGET" || code == "MESSAGE_ERROR" {
                        return;
                    }
                }
                self.status(target, lk, format!("{tag} ({command} {code}): {description}"));
            }
            Event::MonitorOnline { nicks } => {
                for n in nicks {
                    if let Some(q) = self.find_buffer(net_id, &n.nick) {
                        self.buffer_mut(q).unwrap().joined = true;
                        self.status(q, LineKind::Status, format!("{} is online", n.nick));
                    }
                }
                self.dirty.sidebar = true;
            }
            Event::MonitorOffline { nicks } => {
                for n in nicks {
                    if let Some(q) = self.find_buffer(net_id, &n) {
                        self.buffer_mut(q).unwrap().joined = false;
                        self.status(q, LineKind::Status, format!("{n} is offline"));
                    }
                }
                self.dirty.sidebar = true;
            }
            Event::Away { own, message } => {
                if own {
                    let text = match message {
                        Some(_) => "You have been marked as being away",
                        None => "You are no longer marked as being away",
                    };
                    self.status(sb, LineKind::Status, text);
                    self.dirty.input = true;
                }
            }
            Event::Redact { target, msgid, by, reason } => {
                let name = self.chat_buffer_name(net_id, &target, &by.nick);
                if let Some(bid) = self.find_buffer(net_id, &name)
                    && let Some(b) = self.buffer_mut(bid)
                    && let Some(line) = b.find_msgid_mut(&msgid)
                {
                    line.flags.set(LineFlags::DELETED, true);
                    let _ = reason;
                    b.generation += 1;
                    self.dirty.lines |= bid == self.active;
                }
            }
            Event::ReadMarker { target, time: marker } => {
                if let Some(bid) = self.find_buffer(net_id, &target)
                    && let Some(b) = self.buffer_mut(bid)
                {
                    b.read_marker = marker.or(b.read_marker);
                    if b.lines.back().is_none_or(|l| marker.is_some_and(|m| l.time <= m)) {
                        b.activity = Activity::None;
                        b.unread = 0;
                        b.highlights = 0;
                    }
                    b.generation += 1;
                    self.dirty.sidebar = true;
                }
            }
            Event::ChannelRename { old, new, reason } => {
                if let Some(bid) = self.find_buffer(net_id, &old) {
                    self.buffer_mut(bid).unwrap().name = new.clone();
                    let r = reason.map(|r| format!(" ({r})")).unwrap_or_default();
                    self.status(bid, LineKind::Status, format!("Channel renamed from {old} to {new}{r}"));
                    self.dirty.sidebar = true;
                }
            }
            Event::History { target, events } => self.on_history(net_id, &target, events),
            Event::BouncerNetwork { id, attrs } => {
                let net = self.networks.get_mut(&net_id).unwrap();
                match attrs {
                    Some(a) => {
                        let entry = net.bouncer_networks.entry(id).or_default();
                        for (k, v) in a {
                            entry.retain(|(ek, _)| *ek != k);
                            entry.push((k, v));
                        }
                    }
                    None => {
                        net.bouncer_networks.remove(&id);
                    }
                }
                self.effects.push(Effect::BouncerNetworks(net_id));
            }
            Event::Twitch(t) => self.on_twitch(net_id, t, time),
        }
    }

    fn on_ready(&mut self, net_id: NetworkId) {
        let net = self.networks.get_mut(&net_id).unwrap();
        net.conn = ConnState::Ready;
        let sb = net.server_buffer;
        let nick = net.session.nick().to_owned();
        let perform = net.cfg.perform.clone();
        let last_seen = net.last_seen;
        let kind = net.cfg.kind;
        if net.session.has_cap("znc.in/playback") && !net.session.supports_chathistory() {
            // Only replay what we haven't seen; on first connect, everything the bouncer buffered.
            net.session.znc_playback(last_seen);
        }
        if net.session.has_cap("soju.im/bouncer-networks") && net.cfg.bouncer_netid.is_none() {
            net.session.send(Message::new("BOUNCER", ["LISTNETWORKS"]));
        }
        let _ = kind;
        self.dirty.sidebar = true;
        for cmd in perform {
            let cmd = cmd.replace("$nick", &nick);
            self.input_line(sb, &cmd);
        }
        self.flush(net_id);
        self.live_on_ready(net_id);
        self.request_emotes(sb);
    }

    fn server_text(&mut self, net_id: NetworkId, msg: &Message, time: i64, label_buffer: Option<BufferId>) {
        use schwaetz_proto::numeric::*;
        let sb = self.networks[&net_id].server_buffer;
        let num = msg.numeric();
        let (kind, text) = match num {
            Some(RPL_MOTD | RPL_MOTDSTART | RPL_ENDOFMOTD) => {
                (LineKind::Motd, msg.last_param().unwrap_or("").to_owned())
            }
            Some(n) if (400..600).contains(&n) => {
                (LineKind::Error, msg.params.iter().skip(1).map(String::as_str).collect::<Vec<_>>().join(" "))
            }
            Some(_) => (LineKind::Server, msg.params.iter().skip(1).map(String::as_str).collect::<Vec<_>>().join(" ")),
            None => (LineKind::Server, format!("{} {}", msg.command, msg.params.join(" "))),
        };
        // Route channel-related replies to the channel buffer.
        let target = label_buffer
            .or_else(|| match num {
                Some(
                    ERR_CANNOTSENDTOCHAN | ERR_CHANOPRIVSNEEDED | ERR_NOTONCHANNEL | ERR_USERNOTINCHANNEL
                    | ERR_CHANNELISFULL | ERR_INVITEONLYCHAN | ERR_BANNEDFROMCHAN | ERR_BADCHANNELKEY
                    | ERR_NEEDREGGEDNICK | RPL_INVITING | ERR_USERONCHANNEL | RPL_AWAY | ERR_NOSUCHNICK,
                ) => msg.param(1).and_then(|c| self.find_buffer(net_id, c)),
                _ => None,
            })
            .or_else(|| match num {
                Some(ERR_NOSUCHNICK | ERR_NOSUCHCHANNEL | RPL_UNAWAY | RPL_NOWAWAY) | None => None,
                Some(n) if n >= 400 => Some(self.contextual_buffer(net_id)),
                _ => None,
            })
            .unwrap_or(sb);
        let line = self.new_line(time, kind, "", text);
        let activity = if kind == LineKind::Error { Activity::Messages } else { Activity::None };
        self.add_line(target, line, activity);
    }

    fn chat_buffer_name(&self, net_id: NetworkId, target: &str, sender: &str) -> String {
        let s = &self.networks[&net_id].session;
        if s.is_channel(target) || s.is_me(sender) { target.to_owned() } else { sender.to_owned() }
    }

    fn is_ignored(&self, net_id: NetworkId, channel: Option<&str>, src: &Source, kind: IgnoreType) -> bool {
        let net = &self.networks[&net_id];
        self.ignores.is_ignored(net.display_name(), channel, src, kind, net.session.casemapping())
    }

    pub fn notify(&mut self, buffer: BufferId, title: String, body: String) {
        let n = &self.config.notifications;
        let is_active = self.active == buffer;
        if self.focused && (is_active || !n.when_focused) {
            return;
        }
        if self.buffer(buffer).is_some_and(|b| b.notify == NotifyLevel::Mute) {
            return;
        }
        if n.enabled && self.network_of(buffer).is_none_or(|net| net.cfg.notifications) {
            self.effects.push(Effect::Notify { title, body, buffer });
        }
        if n.flash_taskbar && !self.focused {
            self.effects.push(Effect::FlashTaskbar);
        }
    }

    fn on_chat(&mut self, net_id: NetworkId, chat: Chat, time: i64, in_history: bool) {
        let Chat { kind, from, target, text, tags, msgid, own, history } = chat;
        let history = history || in_history;
        let net = &self.networks[&net_id];
        let twitch = net.is_twitch();
        let session = &net.session;
        let cm = session.casemapping();
        let my_nick = session.nick().to_owned();
        let channel = match &target {
            Target::Channel { name, .. } => Some(name.clone()),
            _ => None,
        };
        let ig_kind = match kind {
            ChatKind::Privmsg => IgnoreType::Msg,
            ChatKind::Notice => IgnoreType::Notice,
            ChatKind::Action => IgnoreType::Action,
            ChatKind::Tagmsg => IgnoreType::Tagmsg,
        };
        if !own && self.is_ignored(net_id, channel.as_deref(), &from, ig_kind) {
            return;
        }

        // Resolve the buffer.
        let bid = match &target {
            Target::Channel { name, .. } => self.ensure_buffer(net_id, BufferKind::Channel, name),
            Target::Query { peer } => {
                let existing = self.find_buffer(net_id, peer);
                if own && existing.is_none() && peer.starts_with('*') && kind != ChatKind::Tagmsg {
                    // Our own commands to bouncer modules (*playback, *status …) echoed back.
                    return;
                }
                if kind == ChatKind::Notice && existing.is_none() && !own {
                    // Private notices (services, bots) go to the current context instead of opening queries.
                    self.contextual_buffer(net_id)
                } else {
                    existing.unwrap_or_else(|| self.ensure_buffer(net_id, BufferKind::Query, peer))
                }
            }
            Target::Server => self.networks[&net_id].server_buffer,
        };

        if kind == ChatKind::Tagmsg {
            self.on_tagmsg(bid, &from, &tags, time);
            return;
        }

        // ZNC: collect ListNetworks output.
        if from.nick.eq_ignore_ascii_case("*status")
            && let Some(rows) = self.networks.get_mut(&net_id).unwrap().znc_collect.as_mut()
        {
            if let Some(name) = znc::parse_list_networks_row(&text) {
                rows.push(name);
            } else if text.starts_with('+') && !rows.is_empty() {
                let names = std::mem::take(rows);
                self.networks.get_mut(&net_id).unwrap().znc_collect = None;
                self.effects.push(Effect::ZncNetworks { network: net_id, names });
            }
        }

        // Deduplicate history/playback against what we already have.
        let old = history || time < self.now - PLAYBACK_AGE_MS;
        if let Some(b) = self.buffer_mut(bid) {
            if let Some(id) = &msgid {
                if b.seen_msgid(id) {
                    return;
                }
            } else if old && b.has_equivalent(time, &from.nick, &text) {
                return;
            }
        }

        let stripped = fmt::strip(&text);
        // Private notices (services etc.) often contain our nick; don't treat those as highlights.
        let private_notice = kind == ChatKind::Notice && !matches!(target, Target::Channel { .. });
        let highlight = !own && !private_notice && self.highlighter.matches(&stripped, &my_nick, &from, cm);
        let net = &self.networks[&net_id];
        let prefix = channel
            .as_ref()
            .and_then(|c| net.session.channel(c))
            .and_then(|c| c.members.get(cm.fold(&from.nick).as_ref()))
            .and_then(|m| m.highest());

        let line_kind = match kind {
            ChatKind::Action => LineKind::Action,
            ChatKind::Notice => LineKind::Notice,
            _ => LineKind::Message,
        };
        let shown = match (&target, own) {
            (Target::Query { peer }, true) => crate::filter::mask_service_secret(peer, &text),
            _ => None,
        };
        let mut line = self.new_line(time, line_kind, &from.nick, shown.as_deref().unwrap_or(text.as_str()));
        line.prefix = prefix;
        line.flags.set(LineFlags::OWN, own);
        line.flags.set(LineFlags::HIGHLIGHT, highlight);
        line.flags.set(LineFlags::HISTORY, history);
        line.flags.set(LineFlags::BOT, tags.contains("bot") || tags.contains("draft/bot"));
        let mut extra = LineExtra { msgid: msgid.clone(), ..Default::default() };
        if !history {
            extra.raw = self.current_raw.take();
        }
        if let Target::Channel { status: Some(s), .. } = target {
            extra.status = Some(s);
            line.flags.set(LineFlags::STATUSMSG, true);
        }
        extra.account = tags.value("account").map(str::to_owned);
        if twitch {
            twitch::apply_tags(&mut extra, &tags, &stripped, &from.nick);
            line.flags.set(LineFlags::FIRST_MESSAGE, tags.get("first-msg") == Some("1"));
            if own && let Some(st) = self.networks[&net_id].twitch_self.clone() {
                twitch::apply_tags(&mut extra, &st, &stripped, &from.nick);
                extra.emotes.clear();
            }
        } else if let Some(parent) = tags.value("+draft/reply").or(tags.value("+reply")) {
            let parent_line = self.buffer(bid).and_then(|b| b.lines.iter().rev().find(|l| l.msgid() == Some(parent)));
            let (pn, pt) = parent_line.map(|l| (l.nick.to_string(), excerpt(&fmt::strip(&l.text)))).unwrap_or_default();
            extra.reply_to = Some((parent.to_owned(), pn, pt));
        }
        // Our own replies (local echo) carry only the parent id: fill in who and what from the buffer.
        if let Some((parent, pn, pt)) = extra.reply_to.as_mut()
            && pn.is_empty()
            && let Some(l) =
                self.buffer(bid).and_then(|b| b.lines.iter().rev().find(|l| l.msgid() == Some(parent.as_str())))
        {
            *pn = l.display_nick().to_owned();
            if pt.is_empty() {
                *pt = excerpt(&fmt::strip(&l.text));
            }
        }
        if extra != LineExtra::default() {
            line.extra = Some(Box::new(extra));
        }

        let is_query = matches!(target, Target::Query { .. });
        let activity = if znc::is_empty_notes(&from.nick, &stripped) {
            Activity::None
        } else if highlight || (is_query && !own && kind != ChatKind::Notice) {
            Activity::Highlight
        } else if matches!(target, Target::Server) || from.is_server() {
            // The server's own notices (Twitch: "now hosting", room modes …) are no messages.
            Activity::None
        } else {
            Activity::Messages
        };
        let is_playback = history || time < self.now - PLAYBACK_AGE_MS;
        let buffer_all = self.buffer(bid).is_some_and(|b| b.notify == NotifyLevel::All);
        self.add_line(bid, line, activity);
        if let Some(n) = self.networks.get_mut(&net_id)
            && !history
        {
            n.last_seen = n.last_seen.max(time);
        }
        if let Some(b) = self.buffer_mut(bid)
            && !own
        {
            b.last_spoke.insert(cm.fold(&from.nick).into_owned(), time);
            b.typing.retain(|t| !cm.eq(&t.nick, &from.nick));
        }

        let notify_cfg = &self.config.notifications;
        let should_notify = !own
            && !is_playback
            && activity != Activity::None
            && ((highlight && notify_cfg.on_highlight)
                || (is_query && kind != ChatKind::Notice && notify_cfg.on_private)
                || buffer_all);
        if should_notify {
            let bname = self.buffer(bid).map(|b| b.name.clone()).unwrap_or_default();
            let title = if is_query { from.nick.clone() } else { format!("{} in {bname}", from.nick) };
            let body = if kind == ChatKind::Action { format!("* {} {stripped}", from.nick) } else { stripped };
            self.notify(bid, title, body);
        }
    }

    fn on_tagmsg(&mut self, bid: BufferId, from: &Source, tags: &schwaetz_proto::Tags, time: i64) {
        let now = self.now;
        if let Some(state) = tags.get("+typing").or(tags.get("+draft/typing")) {
            let Some(b) = self.buffer_mut(bid) else { return };
            b.typing.retain(|t| t.nick != from.nick);
            match state {
                "active" => {
                    b.typing.push(Typing { nick: from.nick.clone(), expires: now + TYPING_TIMEOUT_MS, paused: false })
                }
                "paused" => b.typing.push(Typing { nick: from.nick.clone(), expires: now + 30_000, paused: true }),
                _ => {}
            }
            if bid == self.active {
                self.dirty.topic = true;
            }
        }
        if let (Some(reaction), Some(parent)) =
            (tags.value("+draft/react").or(tags.value("+react")), tags.value("+draft/reply").or(tags.value("+reply")))
        {
            let Some(b) = self.buffer_mut(bid) else { return };
            if let Some(line) = b.find_msgid_mut(parent) {
                let extra = line.extra_mut();
                match extra.reactions.iter_mut().find(|(r, _)| r == reaction) {
                    Some((_, nicks)) if !nicks.contains(&from.nick) => nicks.push(from.nick.clone()),
                    Some(_) => {}
                    None => extra.reactions.push((reaction.to_owned(), vec![from.nick.clone()])),
                }
                b.generation += 1;
                if bid == self.active {
                    self.dirty.lines = true;
                }
            }
        }
        let _ = time;
    }

    fn on_ctcp_request(
        &mut self,
        net_id: NetworkId,
        from: Source,
        target: String,
        command: String,
        params: String,
        time: i64,
    ) {
        let channel = self.networks[&net_id].session.is_channel(&target).then_some(target.as_str());
        if self.is_ignored(net_id, channel, &from, IgnoreType::Ctcp) {
            return;
        }
        let where_ = if channel.is_some() { format!(" (to {target})") } else { String::new() };
        let text = format!(
            "CTCP {command}{} from {}{where_}",
            if params.is_empty() { String::new() } else { format!(" {params}") },
            from.nick
        );
        let sb = self.networks[&net_id].server_buffer;
        let line = self.new_line(time, LineKind::Ctcp, &from.nick, text);
        self.add_line(sb, line, Activity::None);
        if !self.config.general.ctcp_replies {
            return;
        }
        // Rate limit: at most 4 replies per 10 seconds.
        let net = self.networks.get_mut(&net_id).unwrap();
        if self.now - net.ctcp_window.0 > 10_000 {
            net.ctcp_window = (self.now, 0);
        }
        if net.ctcp_window.1 >= 4 {
            return;
        }
        net.ctcp_window.1 += 1;
        let version = net.session.config().version.clone();
        let reply = match command.as_str() {
            "VERSION" => Some(version),
            "PING" => Some(params.clone()),
            "TIME" => Some(time::format("%A, %d %B %Y %H:%M:%S", time::local(self.now))),
            "CLIENTINFO" => Some("ACTION CLIENTINFO PING TIME VERSION".into()),
            "SOURCE" => Some("https://github.com/bogenpirat/schwaetz".into()),
            _ => None,
        };
        if let Some(r) = reply {
            self.send(net_id, Message::new("NOTICE", [from.nick.clone(), ctcp::encode(&command, &r)]));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn on_join(
        &mut self,
        net_id: NetworkId,
        channel: &str,
        user: &Source,
        account: Option<String>,
        own: bool,
        time: i64,
        history: bool,
    ) {
        if !own && !history && self.is_ignored(net_id, Some(channel), user, IgnoreType::Join) {
            return;
        }
        let bid = if own {
            self.ensure_buffer(net_id, BufferKind::Channel, channel)
        } else {
            match self.find_buffer(net_id, channel) {
                Some(b) => b,
                None => return,
            }
        };
        if own && !history {
            let net = self.networks.get_mut(&net_id).unwrap();
            let cm = net.session.casemapping();
            let requested = net.pending_joins.iter().position(|c| cm.eq(c, channel));
            if let Some(i) = requested {
                net.pending_joins.remove(i);
            }
            let b = self.buffer_mut(bid).unwrap();
            b.joined = true;
            b.name = channel.to_owned();
            if requested.is_some() {
                self.switch_to(bid);
            }
            self.remember_channel(net_id, channel, true);
            // Fill the gap since we last saw this channel, or fetch recent history.
            let last_seen = self.buffer(bid).map_or(0, |b| b.last_seen);
            let net = self.networks.get_mut(&net_id).unwrap();
            let asked = if last_seen > 0 {
                net.session.request_history(channel, Some(last_seen), 500)
            } else {
                net.session.request_history(channel, None, 100)
            };
            if asked && let Some(b) = self.buffer_mut(bid) {
                b.history_loading = true;
            }
            self.dirty.sidebar = true;
        }
        let acct = account.map(|a| format!(" [{a}]")).unwrap_or_default();
        let text = if own {
            format!("You have joined {channel}")
        } else {
            format!("{}{acct} has joined", self.who(net_id, user))
        };
        self.membership_line(net_id, bid, LineKind::Join, user, text, time, own, history);
        self.dirty.nicklist = true;
        if own && !history {
            self.live_on_join(net_id, channel);
        }
    }

    /// Adds a join/part/quit/nick line applying the smart filter.
    #[allow(clippy::too_many_arguments)]
    fn membership_line(
        &mut self,
        net_id: NetworkId,
        bid: BufferId,
        kind: LineKind,
        user: &Source,
        text: String,
        time: i64,
        own: bool,
        history: bool,
    ) {
        let net = &self.networks[&net_id];
        let cm = net.session.casemapping();
        let window = self.config.general.smart_filter_secs as i64 * 1000;
        let filtered = !own
            && match net.cfg.joins_parts_mode() {
                "none" => true,
                "all" => false,
                _ => self
                    .buffer(bid)
                    .and_then(|b| b.last_spoke.get(cm.fold(&user.nick).as_ref()))
                    .is_none_or(|t| time - *t > window),
            };
        if history && self.buffer(bid).is_some_and(|b| b.has_equivalent(time, &user.nick, &text)) {
            // Replayed by the server (event-playback) and already shown live.
            return;
        }
        let mut line = self.new_line(time, kind, &user.nick, text);
        line.flags.set(LineFlags::FILTERED, filtered);
        line.flags.set(LineFlags::OWN, own);
        line.flags.set(LineFlags::HISTORY, history);
        self.add_line(bid, line, Activity::None);
    }

    /// Appends a nick to a recent netsplit summary line instead of adding a new line.
    fn fold_into_netsplit(&mut self, bid: BufferId, nick: &str, reason: &str, time: i64) -> bool {
        let Some(b) = self.buffer_mut(bid) else { return false };
        let Some(last) = b.lines.back_mut() else { return false };
        if last.kind != LineKind::Netsplit
            || time - last.time > 10_000
            || !last.text.starts_with(&format!("Netsplit {reason}:"))
        {
            return false;
        }
        let mut t = last.text.to_string();
        t.push_str(", ");
        t.push_str(nick);
        last.text = t.into();
        b.generation += 1;
        true
    }

    fn on_history(&mut self, net_id: NetworkId, target: &str, events: Vec<SessionEvent>) {
        if let Some(bid) = self.find_buffer(net_id, target)
            && let Some(b) = self.buffer_mut(bid)
        {
            b.history_loading = false;
            if events.is_empty() {
                b.history_exhausted = true;
            }
        }
        for ev in events {
            match ev.kind {
                Event::Chat(c) => self.on_chat(net_id, c, ev.time, true),
                Event::Join { channel, user, account, own, .. } if !own => {
                    self.on_join(net_id, &channel, &user, account, false, ev.time, true)
                }
                Event::Part { channel, user, reason, own: false } => {
                    if let Some(bid) = self.find_buffer(net_id, &channel) {
                        let r = reason.map(|r| format!(" ({})", fmt::strip(&r))).unwrap_or_default();
                        let text = format!("{} has left {channel}{r}", self.who(net_id, &user));
                        self.membership_line(net_id, bid, LineKind::Part, &user, text, ev.time, false, true);
                    }
                }
                Event::Quit { user, reason, .. } => {
                    if let Some(bid) = self.find_buffer(net_id, target) {
                        let r = reason.map(|r| format!(" ({})", fmt::strip(&r))).unwrap_or_default();
                        let text = format!("{} has quit{r}", self.who(net_id, &user));
                        self.membership_line(net_id, bid, LineKind::Quit, &user, text, ev.time, false, true);
                    }
                }
                _ => {}
            }
        }
        self.dirty.lines = true;
    }

    fn on_twitch(&mut self, net_id: NetworkId, ev: TwitchEvent, time: i64) {
        match ev {
            TwitchEvent::ClearChat { channel, nick, duration_secs } => {
                let Some(bid) = self.find_buffer(net_id, &channel) else { return };
                let b = self.buffer_mut(bid).unwrap();
                match &nick {
                    Some(n) => {
                        for l in b.lines.iter_mut().filter(|l| l.nick.eq_ignore_ascii_case(n) && l.kind.is_message()) {
                            l.flags.set(LineFlags::DELETED, true);
                        }
                    }
                    None => {
                        for l in b.lines.iter_mut().filter(|l| l.kind.is_message()) {
                            l.flags.set(LineFlags::DELETED, true);
                        }
                    }
                }
                b.generation += 1;
                let text = match (nick, duration_secs) {
                    (Some(n), Some(d)) => format!("{n} has been timed out for {}", time::duration(d)),
                    (Some(n), None) => format!("{n} has been banned"),
                    (None, _) => "Chat was cleared by a moderator".into(),
                };
                let line = self.new_line(time, LineKind::System, "", text);
                self.add_line(bid, line, Activity::None);
            }
            TwitchEvent::ClearMsg { channel, target_msgid, .. } => {
                if let Some(bid) = self.find_buffer(net_id, &channel)
                    && let Some(b) = self.buffer_mut(bid)
                    && let Some(l) = b.find_msgid_mut(&target_msgid)
                {
                    l.flags.set(LineFlags::DELETED, true);
                    b.generation += 1;
                    self.dirty.lines |= bid == self.active;
                }
            }
            TwitchEvent::UserNotice { channel, system_msg, text, tags, .. } => {
                let Some(bid) = self.find_buffer(net_id, &channel) else { return };
                let who = tags.value("display-name").or(tags.value("login")).unwrap_or("").to_owned();
                let mut body = system_msg.unwrap_or_default();
                if let Some(t) = text.filter(|t| !t.is_empty()) {
                    if !body.is_empty() {
                        body.push_str(" — ");
                    }
                    body.push_str(&t);
                }
                let mut line = self.new_line(time, LineKind::System, &who, body);
                let mut extra = LineExtra { msgid: tags.value("id").map(str::to_owned), ..Default::default() };
                extra.raw = self.current_raw.take();
                extra.color = tags.value("color").and_then(twitch::parse_color);
                line.extra = Some(Box::new(extra));
                self.add_line(bid, line, Activity::Messages);
            }
            TwitchEvent::RoomState { channel, tags } => {
                if let Some(bid) = self.find_buffer(net_id, &channel)
                    && let Some(b) = self.buffer_mut(bid)
                {
                    for (k, v) in tags.iter() {
                        if matches!(k, "emote-only" | "followers-only" | "r9k" | "slow" | "subs-only") {
                            b.room_state.retain(|(ek, _)| ek != k);
                            b.room_state.push((k.to_owned(), v.to_owned()));
                        }
                    }
                    if let Some(id) = tags.value("room-id").filter(|id| !id.is_empty()) {
                        b.room_id = Some(id.to_owned());
                    }
                    self.dirty.topic = true;
                    // The channel's emotes can be fetched now.
                    self.request_emotes(bid);
                }
            }
            TwitchEvent::UserState { tags, .. } => {
                if let Some(n) = self.networks.get_mut(&net_id) {
                    n.twitch_self = Some(tags);
                }
            }
        }
    }

    /// Sends a raw message on a network (no-op when disconnected).
    pub(crate) fn send(&mut self, net_id: NetworkId, msg: Message) {
        if let Some(n) = self.networks.get_mut(&net_id) {
            n.session.send(msg);
            self.flush(net_id);
        }
    }

    /// Inserts lines loaded from the history database (scroll-back).
    pub fn insert_history(&mut self, buffer: BufferId, lines: Vec<Line>) {
        let active = self.active == buffer;
        let next = &mut self.next_line;
        let Some(b) = self.buffers.iter_mut().find(|b| b.id == buffer) else { return };
        b.history_loading = false;
        if lines.is_empty() {
            b.history_exhausted = true;
        }
        let room = b.max_lines.saturating_sub(b.lines.len());
        for mut l in lines.into_iter().rev().take(room.max(200)) {
            if let Some(id) = l.msgid().map(str::to_owned)
                && b.seen_msgid(&id)
            {
                continue;
            }
            *next += 1;
            l.id = *next;
            l.flags.set(LineFlags::HISTORY, true);
            let pos = b.lines.partition_point(|x| x.time <= l.time);
            b.lines.insert(pos, l);
        }
        b.generation += 1;
        self.dirty.lines |= active;
    }

    /// Scroll-back reached the top of a buffer: ask the server (CHATHISTORY) or the local database.
    pub fn request_older(&mut self, buffer: BufferId) {
        let Some(b) = self.buffer(buffer) else { return };
        if b.history_loading || b.history_exhausted {
            return;
        }
        let before = b.lines.front().map_or(self.now, |l| l.time);
        let (net, name, kind) = (b.network, b.name.clone(), b.kind);
        let Some(net) = net else { return };
        if !matches!(kind, BufferKind::Channel | BufferKind::Query) {
            return;
        }
        // Local history first (instant, works offline), then the server's CHATHISTORY.
        let network = self.networks.get(&net).map(|n| n.display_name().to_owned()).unwrap_or_default();
        if let Some(h) = self.history.as_mut() {
            let lines = h.load_before(&network, &name, before, 200);
            if !lines.is_empty() {
                self.insert_history(buffer, lines);
                return;
            }
        }
        let asked = match self.networks.get_mut(&net) {
            Some(n) if n.conn == ConnState::Ready => n.session.request_history_before(&name, before, 100),
            _ => false,
        };
        self.flush(net);
        if let Some(b) = self.buffer_mut(buffer) {
            if asked {
                b.history_loading = true;
            } else {
                b.history_exhausted = true;
            }
        }
    }

    /// Re-reads highlight/ignore settings after the config changed.
    pub fn apply_config(&mut self) {
        let (h, errors) = Highlighter::new(&self.config.highlight);
        self.highlighter = h;
        self.ignores = Ignores::new(&self.config.ignores);
        for e in errors {
            self.status(self.status_buffer, LineKind::Error, e);
        }
        for b in &mut self.buffers {
            b.max_lines = self.config.general.scrollback_lines.max(100);
        }
        self.dirty = Dirty { sidebar: true, lines: true, nicklist: true, topic: true, input: true };
        self.effects.push(Effect::ConfigChanged);
    }

    /// Topic-bar text for the active buffer: (title, subtitle).
    /// The topic bar split for display: `lead` and `tail` must stay visible, `body` (a topic or a
    /// stream title) is what gets shortened when the line is too long.
    pub fn topic_parts(&self, id: BufferId) -> TopicParts {
        let (title, sub) = self.topic_for(id);
        if let Some(b) = self.buffer(id).filter(|b| b.kind == BufferKind::Channel)
            && let Some(s) = &b.stream
        {
            let chips = twitch::room_state_summary(&b.room_state);
            let chips = if chips.is_empty() { String::new() } else { format!("[{}] ", chips.join(", ")) };
            let (lead, body, tail) = s.parts();
            return TopicParts { title, lead: chips + &lead, body, tail };
        }
        TopicParts { title, lead: String::new(), body: sub, tail: String::new() }
    }

    pub fn topic_for(&self, id: BufferId) -> (String, String) {
        let Some(b) = self.buffer(id) else { return Default::default() };
        let Some(net) = b.network.and_then(|n| self.networks.get(&n)) else {
            if id == self.status_buffer {
                // Opened from the "Status" button at the bottom of the sidebar.
                return (
                    "Status".into(),
                    format!("schwätz {} · client and script messages", env!("CARGO_PKG_VERSION")),
                );
            }
            return (b.name.clone(), format!("schwätz {}", env!("CARGO_PKG_VERSION")));
        };
        match b.kind {
            BufferKind::Channel => {
                let ch = net.session.channel(&b.name);
                let mut sub = ch.and_then(|c| c.topic.clone()).unwrap_or_default();
                if let Some(s) = &b.stream {
                    // Twitch channels have no topic; the stream title and game take its place.
                    sub = s.summary();
                }
                let chips = twitch::room_state_summary(&b.room_state);
                if !chips.is_empty() {
                    sub = format!("[{}] {sub}", chips.join(", "));
                }
                (b.name.clone(), sub)
            }
            BufferKind::Query => {
                let u = net.session.user(&b.name);
                let sub = u
                    .map(|u| {
                        let mut parts = Vec::new();
                        if let (Some(user), Some(host)) = (&u.user, &u.host) {
                            parts.push(format!("{user}@{host}"));
                        }
                        if let Some(r) = &u.realname {
                            parts.push(fmt::strip(r));
                        }
                        if let Some(a) = &u.away {
                            parts.push(if a.is_empty() { "away".into() } else { format!("away: {}", fmt::strip(a)) });
                        }
                        parts.join(" · ")
                    })
                    .unwrap_or_default();
                (b.name.clone(), sub)
            }
            _ => {
                let state = match net.conn {
                    ConnState::Disconnected => net
                        .last_error
                        .clone()
                        .map(|e| format!("disconnected — {e}"))
                        .unwrap_or_else(|| "disconnected".into()),
                    ConnState::Connecting => match net.retry_at {
                        Some(t) if t > self.now => {
                            format!("reconnecting in {}", time::duration(((t - self.now) / 1000).max(1) as u64))
                        }
                        _ => "connecting…".into(),
                    },
                    ConnState::Connected => "registering…".into(),
                    ConnState::Ready => {
                        let server = net.server.as_ref().map(|s| s.to_string()).unwrap_or_default();
                        let lag = net.lag_ms.map(|l| format!(" · lag {l} ms")).unwrap_or_default();
                        format!("{} on {server}{lag}", net.session.nick())
                    }
                };
                (net.display_name().to_owned(), state)
            }
        }
    }
}

impl App {
    /// "nick (user@host)" for membership lines; just the nick on Twitch, where the mask only
    /// repeats it.
    fn who(&self, net: NetworkId, user: &Source) -> String {
        if self.networks.get(&net).is_some_and(|n| n.is_twitch()) {
            user.nick.to_string()
        } else {
            format!("{} ({})", user.nick, userhost(user))
        }
    }

    /// Where "Open stream" goes for a Twitch channel buffer: the channel page, or the popout player
    /// when the network is set to `twitch_popout`.
    pub fn twitch_stream_url(&self, buffer: BufferId) -> Option<String> {
        let b = self.buffer(buffer).filter(|b| b.kind == BufferKind::Channel)?;
        let n = self.networks.get(&b.network?).filter(|n| n.is_twitch())?;
        let login = b.name.trim_start_matches('#').to_ascii_lowercase();
        Some(if n.cfg.twitch_popout {
            format!("https://player.twitch.tv/?channel={login}&parent=twitch.tv&player=popout")
        } else {
            format!("https://www.twitch.tv/{login}")
        })
    }

    /// The Twitch channel page of a user (the login name), when `buffer` is on Twitch.
    pub fn twitch_profile_url(&self, buffer: BufferId, login: &str) -> Option<String> {
        let net = self.buffer(buffer)?.network?;
        self.networks.get(&net).filter(|n| n.is_twitch())?;
        let login = login.trim_start_matches('#').to_ascii_lowercase();
        (!login.is_empty()).then(|| format!("https://www.twitch.tv/{login}"))
    }
}

fn userhost(s: &Source) -> String {
    match (&s.user, &s.host) {
        (Some(u), Some(h)) => format!("{u}@{h}"),
        (None, Some(h)) => h.clone(),
        _ => "?".into(),
    }
}

fn looks_like_netsplit(reason: &str) -> bool {
    let mut parts = reason.split(' ');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), None) => {
            let host = |h: &str| h.contains('.') && !h.contains(['/', ':', '(', ')']) && h.len() > 3;
            host(a) && host(b)
        }
        _ => false,
    }
}

fn excerpt(s: &str) -> String {
    if s.chars().count() > 80 { s.chars().take(77).chain("…".chars()).collect() } else { s.to_owned() }
}

impl App {
    /// A client-side buffer not tied to a network (e.g. "search"), created on demand.
    pub fn ensure_special(&mut self, name: &str) -> BufferId {
        match self.buffers.iter().find(|b| b.network.is_none() && b.kind == BufferKind::Special && b.name == name) {
            Some(b) => b.id,
            None => self.create_buffer(None, BufferKind::Special, name),
        }
    }

    pub fn network_by_name(&self, name: &str) -> Option<NetworkId> {
        self.networks
            .values()
            .find(|n| n.display_name().eq_ignore_ascii_case(name) || n.cfg.name.eq_ignore_ascii_case(name))
            .map(|n| n.id)
    }

    /// Sends a raw message on a network (scripts, automation).
    pub fn send_raw(&mut self, net: NetworkId, msg: Message) {
        self.send(net, msg);
    }

    /// The buffer for `name` on `net`, creating a query or channel buffer if needed.
    pub fn buffer_for(&mut self, net: NetworkId, name: &str) -> BufferId {
        let kind = match self.networks.get(&net) {
            Some(n) if n.session.is_channel(name) => BufferKind::Channel,
            _ => BufferKind::Query,
        };
        self.ensure_buffer(net, kind, name)
    }
}

impl App {
    /// Keeps the network's autojoin list in sync with the channels the user is in, so they are
    /// rejoined after a restart (`general.remember_channels`).
    pub(crate) fn remember_channel(&mut self, net: NetworkId, channel: &str, joined: bool) {
        if !self.config.general.remember_channels {
            return;
        }
        let Some(n) = self.networks.get_mut(&net) else { return };
        let cm = n.session.casemapping();
        let key = n.session.channel(channel).and_then(|c| c.key.clone());
        let pos = n.cfg.autojoin.iter().position(|e| e.split_whitespace().next().is_some_and(|c| cm.eq(c, channel)));
        let changed = match (joined, pos) {
            (true, None) => {
                n.cfg.autojoin.push(match key {
                    Some(k) => format!("{channel} {k}"),
                    None => channel.to_owned(),
                });
                true
            }
            (false, Some(i)) => {
                n.cfg.autojoin.remove(i);
                true
            }
            _ => false,
        };
        if !changed {
            return;
        }
        let list = n.cfg.autojoin.clone();
        let name = n.cfg.name.clone();
        n.session.config_mut().autojoin = n.cfg.autojoin_list();
        if let Some(c) = self.config.networks.iter_mut().find(|c| c.name == name) {
            c.autojoin = list;
            self.effects.push(Effect::SaveConfig);
        }
    }
}

impl App {
    /// Adds a network or replaces the definition of `old_name`. Connection changes (servers,
    /// identity) take effect on the next (re)connect. Returns the network id.
    pub fn upsert_network(&mut self, old_name: Option<&str>, cfg: NetworkConfig) -> NetworkId {
        let key = old_name.unwrap_or(&cfg.name).to_owned();
        match self.config.networks.iter_mut().find(|c| c.name.eq_ignore_ascii_case(&key)) {
            Some(c) => *c = cfg.clone(),
            None => self.config.networks.push(cfg.clone()),
        }
        self.effects.push(Effect::SaveConfig);
        let existing =
            old_name.and_then(|o| self.networks.values().find(|n| n.cfg.name.eq_ignore_ascii_case(o)).map(|n| n.id));
        match existing {
            Some(id) => {
                let n = self.networks.get_mut(&id).unwrap();
                n.cfg = cfg.clone();
                n.session.config_mut().autojoin = cfg.autojoin_list();
                let sb = n.server_buffer;
                if let Some(b) = self.buffer_mut(sb) {
                    b.name = cfg.name.clone();
                }
                self.dirty.sidebar = true;
                self.live_on_config(id);
                self.emotes_on_config(id);
                id
            }
            None => {
                let id = self.add_network(cfg.clone());
                if cfg.auto_connect {
                    self.connect(id);
                }
                id
            }
        }
    }
}

/// Conversation and channel events are persisted; client status text, MOTDs and our own
/// joins/parts (repeated on every connect) are not.
fn worth_logging(l: &Line) -> bool {
    match l.kind {
        LineKind::Message | LineKind::Action | LineKind::Notice | LineKind::System => true,
        LineKind::Join | LineKind::Part => !l.flags.has(LineFlags::OWN),
        LineKind::Quit
        | LineKind::Kick
        | LineKind::Nick
        | LineKind::Mode
        | LineKind::Topic
        | LineKind::Invite
        | LineKind::Netsplit => true,
        LineKind::Status | LineKind::Error | LineKind::Server | LineKind::Motd | LineKind::Ctcp => false,
    }
}

// ----- Twitch live checks ------------------------------------------------------------------------

impl LiveCheck {
    /// Connection lost: nothing is scheduled until the next registration.
    fn stop(&mut self) {
        self.waiting = None;
        self.next_at = 0;
        self.queued.clear();
    }
}

impl App {
    /// The network's Helix API token and the client ID it belongs to (if known): the "Sign in
    /// with Twitch" token, else a manually entered one.
    fn api_token(&self, net: NetworkId) -> Option<(String, Option<String>)> {
        let n = self.networks.get(&net).filter(|n| n.is_twitch())?;
        if let Some(t) = n.auth.access_token() {
            return Some((t.to_owned(), n.auth.client_id().map(str::to_owned)));
        }
        self.secret_get(&n.cfg.name, SecretKind::TwitchApi)
            .filter(|t| !crate::helix::normalize_token(t).is_empty())
            .map(|t| (t, None))
    }

    /// Stores `token` as the manually entered API token (tests use in-memory secrets).
    #[doc(hidden)]
    pub fn set_twitch_api_token(&mut self, net: NetworkId, token: Option<String>) {
        self.use_memory_secrets();
        let Some(name) = self.networks.get(&net).map(|n| n.cfg.name.clone()) else { return };
        match token {
            Some(t) => self.secret_set(&name, SecretKind::TwitchApi, &t),
            None => self.secret_delete(&name, SecretKind::TwitchApi),
        }
    }

    /// Keeps secrets in memory instead of the Windows Credential Manager (tests).
    #[doc(hidden)]
    pub fn use_memory_secrets(&mut self) {
        self.mem_secrets.get_or_insert_with(Default::default);
    }

    fn secret_key(network: &str, kind: SecretKind) -> String {
        format!("{}:{kind:?}", network.to_lowercase())
    }

    pub(crate) fn secret_get(&self, network: &str, kind: SecretKind) -> Option<String> {
        match &self.mem_secrets {
            Some(m) => m.get(&Self::secret_key(network, kind)).cloned(),
            None => secrets::get(network, kind),
        }
    }

    pub(crate) fn secret_set(&mut self, network: &str, kind: SecretKind, value: &str) {
        match &mut self.mem_secrets {
            Some(m) => {
                m.insert(Self::secret_key(network, kind), value.to_owned());
            }
            None => {
                if !secrets::set(network, kind, value) {
                    tracing::warn!("could not store the {kind:?} secret for {network}");
                }
            }
        }
    }

    pub(crate) fn secret_delete(&mut self, network: &str, kind: SecretKind) {
        match &mut self.mem_secrets {
            Some(m) => {
                m.remove(&Self::secret_key(network, kind));
            }
            None => {
                secrets::delete(network, kind);
            }
        }
    }

    /// Registered: check every channel once the autojoin channels are in (or right away if
    /// there are none), then periodically.
    fn live_on_ready(&mut self, net: NetworkId) {
        if self.api_token(net).is_none() {
            return;
        }
        let now = self.now;
        let n = self.networks.get_mut(&net).unwrap();
        n.live.stop();
        let pending: Vec<String> =
            n.session.config().autojoin.iter().map(|(c, _)| channel_login(c)).filter(|c| !c.is_empty()).collect();
        if pending.is_empty() {
            self.check_live(net, None);
        } else {
            // Twitch confirms joins quickly; don't wait forever for one that never comes.
            n.live.waiting = Some((pending, now + 20_000));
        }
    }

    /// We joined a Twitch channel: part of the autojoin batch, or a manual join (checked now).
    fn live_on_join(&mut self, net: NetworkId, channel: &str) {
        if self.api_token(net).is_none() {
            return;
        }
        let login = channel_login(channel);
        let n = self.networks.get_mut(&net).unwrap();
        match n.live.waiting.as_mut() {
            Some((pending, _)) if pending.contains(&login) => {
                pending.retain(|c| *c != login);
                if pending.is_empty() {
                    n.live.waiting = None;
                    self.check_live(net, None);
                }
            }
            _ => self.check_live(net, Some(vec![login])),
        }
    }

    fn live_tick(&mut self, net: NetworkId) {
        let now = self.now;
        let Some(n) = self.networks.get_mut(&net) else { return };
        if n.conn != ConnState::Ready || !n.is_twitch() {
            return;
        }
        if let Some((_, deadline)) = &n.live.waiting {
            if now >= *deadline {
                n.live.waiting = None;
                self.check_live(net, None);
            }
        } else if n.live.next_at != 0 && now >= n.live.next_at {
            self.check_live(net, None);
        }
    }

    /// Settings or the token changed: forget what belonged to the old token and check now.
    pub(crate) fn live_on_config(&mut self, net: NetworkId) {
        let Some(n) = self.networks.get_mut(&net).filter(|n| n.is_twitch()) else { return };
        n.live.client_id = None;
        n.live.last_error = None;
        if n.conn == ConnState::Ready && n.live.waiting.is_none() {
            self.check_live(net, None);
        }
    }

    /// Checks `only` these channels, or every joined channel (which also schedules the next
    /// periodic check).
    fn check_live(&mut self, net: NetworkId, only: Option<Vec<String>>) {
        let Some((token, token_client)) = self.api_token(net) else {
            if let Some(n) = self.networks.get_mut(&net) {
                n.live.next_at = 0;
            }
            return;
        };
        let now = self.now;
        let full = only.is_none();
        let logins = only.unwrap_or_else(|| {
            self.buffers
                .iter()
                .filter(|b| b.network == Some(net) && b.kind == BufferKind::Channel && b.joined)
                .map(|b| channel_login(&b.name))
                .collect()
        });
        let Some(n) = self.networks.get_mut(&net) else { return };
        if full {
            n.live.next_at = now + i64::from(n.cfg.live_check_secs.max(30)) * 1000;
        }
        if logins.is_empty() {
            return;
        }
        if n.live.in_flight {
            for l in logins {
                if !n.live.queued.contains(&l) {
                    n.live.queued.push(l);
                }
            }
            return;
        }
        n.live.in_flight = true;
        let req = crate::helix::LiveRequest {
            network: net,
            token,
            client_id: token_client.or_else(|| n.live.client_id.clone()),
            ids: n.live.ids.clone(),
            logins,
        };
        self.effects.push(Effect::TwitchLive(req));
    }

    /// A live check finished (see [`Effect::TwitchLive`]).
    pub fn on_live_result(&mut self, r: crate::helix::LiveResult) {
        let now = self.now;
        let Some(n) = self.networks.get_mut(&r.network) else { return };
        n.live.in_flight = false;
        n.live.client_id = r.client_id;
        n.live.ids = r.ids;
        let sb = n.server_buffer;
        let queued = std::mem::take(&mut n.live.queued);
        match r.result {
            Err(_) if r.unauthorized && n.auth.access_token().is_some() => {
                // The signed-in token expired early or was revoked: refresh it; the refresh
                // triggers a new check.
                n.auth.unauthorized(now);
            }
            Err(e) => {
                // Report each problem once, not on every interval.
                if n.live.last_error.as_deref() != Some(e.as_str()) {
                    n.live.last_error = Some(e.clone());
                    self.status(sb, LineKind::Error, format!("Twitch API: {e}"));
                }
            }
            Ok(list) => {
                n.live.last_error = None;
                for (login, info) in list {
                    self.apply_stream(r.network, &login, info);
                }
            }
        }
        if !queued.is_empty() {
            self.check_live(r.network, Some(queued));
        }
    }

    fn apply_stream(&mut self, net: NetworkId, login: &str, info: crate::helix::StreamInfo) {
        let Some(bid) = self.find_buffer(net, &format!("#{login}")) else { return };
        let Some(b) = self.buffer_mut(bid) else { return };
        let old = b.stream.replace(info.clone());
        let what = info.what();
        let text = match &old {
            None => Some(format!("Stream: {}", info.summary())),
            Some(o) if o.live != info.live && info.live => Some(if what.is_empty() {
                format!("{login} is now live")
            } else {
                format!("{login} is now live: {what}")
            }),
            Some(o) if o.live != info.live => Some(format!("{login} went offline")),
            Some(o) if info.differs_from(o) => Some(format!("Stream changed to: {what}")),
            _ => None,
        };
        if let Some(t) = text {
            self.print(bid, LineKind::Topic, "", &t);
        }
        if bid == self.active {
            self.dirty.topic = true;
        }
        self.dirty.sidebar = true;
    }
}

/// `#Name` → `name` (Twitch logins are lowercase).
fn channel_login(channel: &str) -> String {
    channel.trim_start_matches('#').to_ascii_lowercase()
}

// ----- Twitch sign-in ----------------------------------------------------------------------------

impl App {
    fn load_auth(&self, cfg: &NetworkConfig) -> crate::twitch_auth::TokenManager {
        use crate::twitch_auth::{TokenManager, Tokens, built_in_client_id};
        if cfg.kind != NetworkKind::Twitch {
            return TokenManager::default();
        }
        let client_id = cfg.twitch_client_id.clone().or_else(|| built_in_client_id().map(str::to_owned));
        let tokens = self.secret_get(&cfg.name, SecretKind::TwitchOAuth).and_then(|s| Tokens::from_json(&s));
        TokenManager::new(client_id, tokens)
    }

    /// Reloads a network's sign-in state from storage (after switching secret stores or
    /// renaming the network).
    #[doc(hidden)]
    pub fn reload_twitch_auth(&mut self, net: NetworkId) {
        let Some(cfg) = self.networks.get(&net).map(|n| n.cfg.clone()) else { return };
        let auth = self.load_auth(&cfg);
        if let Some(n) = self.networks.get_mut(&net) {
            n.auth = auth;
        }
    }

    /// Sign-in state of a Twitch network.
    pub fn twitch_auth(&self, net: NetworkId) -> Option<&crate::twitch_auth::TokenManager> {
        self.networks.get(&net).filter(|n| n.is_twitch()).map(|n| &n.auth)
    }

    /// Starts "Sign in with Twitch" (the browser opens once Twitch hands out a code).
    pub fn twitch_sign_in(&mut self, net: NetworkId) {
        let Some(n) = self.networks.get_mut(&net).filter(|n| n.is_twitch()) else { return };
        let sb = n.server_buffer;
        if !n.auth.available() {
            let msg = "This build has no Twitch application for signing in (see SCHWAETZ_TWITCH_CLIENT_ID).";
            return self.status(sb, LineKind::Error, msg);
        }
        if let Some(request) = n.auth.sign_in() {
            self.effects.push(Effect::TwitchAuth { network: net, request });
            self.status(sb, LineKind::Status, "Sign in with Twitch: asking Twitch for a sign-in code…");
        }
    }

    pub fn twitch_cancel_sign_in(&mut self, net: NetworkId) {
        let Some(n) = self.networks.get_mut(&net).filter(|n| n.is_twitch() && n.auth.signing_in()) else { return };
        n.auth.cancel();
        let sb = n.server_buffer;
        self.status(sb, LineKind::Status, "Twitch sign-in cancelled.");
    }

    /// Forgets (and revokes) the signed-in tokens.
    pub fn twitch_sign_out(&mut self, net: NetworkId) {
        let Some(n) = self.networks.get_mut(&net).filter(|n| n.is_twitch()) else { return };
        let (revoke, events) = n.auth.sign_out();
        let signed_in = !events.is_empty();
        if let Some(request) = revoke {
            self.effects.push(Effect::TwitchAuth { network: net, request });
        }
        self.apply_auth_events(net, events);
        if signed_in {
            let sb = self.networks[&net].server_buffer;
            self.status(sb, LineKind::Status, "Signed out of Twitch.");
        }
    }

    fn auth_tick(&mut self, net: NetworkId) {
        let now = self.now;
        let Some(n) = self.networks.get_mut(&net).filter(|n| n.is_twitch()) else { return };
        if let Some(request) = n.auth.poll(now) {
            self.effects.push(Effect::TwitchAuth { network: net, request });
        }
    }

    /// An [`Effect::TwitchAuth`] request finished.
    pub fn on_auth_result(
        &mut self,
        net: NetworkId,
        request: crate::twitch_auth::AuthRequest,
        response: crate::twitch_auth::AuthResponse,
    ) {
        let now = self.now;
        let Some(n) = self.networks.get_mut(&net) else { return };
        let events = n.auth.on_response(&request, response, now);
        self.apply_auth_events(net, events);
    }

    fn apply_auth_events(&mut self, net: NetworkId, events: Vec<crate::twitch_auth::AuthEvent>) {
        use crate::twitch_auth::AuthEvent;
        let Some(n) = self.networks.get(&net) else { return };
        let (name, sb) = (n.cfg.name.clone(), n.server_buffer);
        for ev in events {
            match ev {
                AuthEvent::Persist(Some(t)) => {
                    self.secret_set(&name, SecretKind::TwitchOAuth, &t.to_json());
                    // A new (or refreshed) token: check with it right away.
                    self.live_on_config(net);
                }
                AuthEvent::Persist(None) => self.secret_delete(&name, SecretKind::TwitchOAuth),
                AuthEvent::OpenBrowser { url, user_code } => {
                    self.effects.push(Effect::OpenUrl(url.clone()));
                    let text = format!(
                        "Sign in with Twitch: authorize schwätz in your browser (code {user_code}). \
                         If no browser opened, visit {url}"
                    );
                    self.status(sb, LineKind::Status, text);
                }
                AuthEvent::SignedIn { login } => {
                    // Possibly another account, or one that may now read its emotes.
                    if let Some(n) = self.networks.get_mut(&net) {
                        n.emotes.reset();
                    }
                    self.request_emotes(self.active);
                    let who = if login.is_empty() { String::new() } else { format!(" as {login}") };
                    self.status(sb, LineKind::Status, format!("Signed in to Twitch{who}."));
                }
                AuthEvent::SignInFailed(e) => self.status(sb, LineKind::Error, format!("Twitch sign-in failed: {e}.")),
                AuthEvent::SignedOut(e) => self.status(
                    sb,
                    LineKind::Error,
                    format!("Twitch sign-in ended: {e}. Sign in again in the network settings (or /twitch login)."),
                ),
                AuthEvent::Problem(e) => self.status(
                    sb,
                    LineKind::Error,
                    format!("Could not refresh the Twitch sign-in ({e}); retrying in 5 minutes."),
                ),
            }
        }
        self.dirty.sidebar = true;
    }
}

/// Position of a channel in a network's arranged order (`usize::MAX` if not arranged).
fn channel_rank(order: &[String], cm: schwaetz_proto::CaseMapping, name: &str) -> usize {
    order.iter().position(|c| cm.eq(c, name)).unwrap_or(usize::MAX)
}

/// Position of a network in the config's list (`usize::MAX` if not there).
fn network_rank(order: &[NetworkConfig], name: &str) -> usize {
    order.iter().position(|c| c.name.eq_ignore_ascii_case(name)).unwrap_or(usize::MAX)
}

impl App {
    /// Moves a network (given by its server buffer) in the sidebar to just before the network of
    /// server buffer `before`, or to the end with `None`, by rearranging the config's list.
    pub fn move_network(&mut self, id: BufferId, before: Option<BufferId>) {
        if before == Some(id) {
            return;
        }
        let name_of = |app: &App, b: BufferId| {
            app.buffer(b)
                .filter(|b| b.kind == BufferKind::Server)
                .and_then(|_| app.network_of(b))
                .map(|n| n.cfg.name.clone())
        };
        let Some(moving) = name_of(self, id) else { return };
        let target = before.and_then(|t| name_of(self, t));
        let nets = &mut self.config.networks;
        let Some(from) = nets.iter().position(|c| c.name.eq_ignore_ascii_case(&moving)) else { return };
        let cfg = nets.remove(from);
        let at = target.and_then(|t| nets.iter().position(|c| c.name.eq_ignore_ascii_case(&t))).unwrap_or(nets.len());
        nets.insert(at, cfg);
        if at != from {
            self.effects.push(Effect::SaveConfig);
        }
        self.dirty.sidebar = true;
    }

    /// Moves a channel in the sidebar to just before `before` (another channel of the same
    /// network), or to the end with `None`, and stores the resulting order in the config.
    pub fn move_channel(&mut self, id: BufferId, before: Option<BufferId>) {
        if before == Some(id) {
            return;
        }
        let Some(b) = self.buffer(id).filter(|b| b.kind == BufferKind::Channel) else { return };
        let Some(net_id) = b.network else { return };
        let moving = b.name.clone();
        let target = before.and_then(|t| self.buffer(t)).filter(|t| t.network == Some(net_id)).map(|t| t.name.clone());
        let Some(n) = self.networks.get(&net_id) else { return };
        let cm = n.session.casemapping();
        // The arranged order of all this network's channels, without the live-first grouping.
        let mut names: Vec<String> = self
            .buffers
            .iter()
            .filter(|b| b.network == Some(net_id) && b.kind == BufferKind::Channel)
            .map(|b| b.name.clone())
            .collect();
        names.sort_by_key(|c| (channel_rank(&n.cfg.channel_order, cm, c), cm.fold(c).into_owned()));
        names.retain(|c| !cm.eq(c, &moving));
        let at = target.and_then(|t| names.iter().position(|c| cm.eq(c, &t))).unwrap_or(names.len());
        names.insert(at, moving);
        let n = self.networks.get_mut(&net_id).unwrap();
        n.cfg.channel_order = names.clone();
        let name = n.cfg.name.clone();
        if let Some(c) = self.config.networks.iter_mut().find(|c| c.name == name) {
            c.channel_order = names;
            self.effects.push(Effect::SaveConfig);
        }
        self.dirty.sidebar = true;
    }
}

// ----- emotes ------------------------------------------------------------------------------------

/// A Twitch network's emotes (for completion and the chat). What a user may use only changes on
/// reconnect, and the providers' sets rarely change, so each [`Job`] runs once per connection:
/// the user's Twitch emotes and the providers' global sets right away, a channel's provider sets
/// once it is joined, its follower emotes when it is first looked at.
#[derive(Default)]
pub(crate) struct ConnEmotes {
    /// Counts connections, so results arriving after a reconnect are dropped.
    connection: u64,
    started: std::collections::HashSet<Job>,
    sets: std::collections::HashMap<SetKey, EmoteSet>,
}

impl ConnEmotes {
    /// Forgets everything (disconnected, or signed in anew).
    pub(crate) fn reset(&mut self) {
        *self = ConnEmotes { connection: self.connection + 1, ..Default::default() };
    }
}

impl App {
    /// A Twitch channel buffer, its network and its `#channel` (lowercase).
    fn twitch_channel(&self, buffer: BufferId) -> Option<(&Network, &Buffer, String)> {
        let b = self.buffer(buffer).filter(|b| b.kind == BufferKind::Channel)?;
        let n = b.network.and_then(|n| self.networks.get(&n)).filter(|n| n.is_twitch())?;
        Some((n, b, b.name.to_ascii_lowercase()))
    }

    /// Fetches what is missing on this connection of a buffer's network: the network-wide lists
    /// and, for a channel (once its id is known from ROOMSTATE), the providers' sets of it and,
    /// if it is being looked at, its follower emotes. Twitch emotes need an API token.
    pub fn request_emotes(&mut self, buffer: BufferId) {
        let Some(b) = self.buffer(buffer) else { return };
        let Some(net) = b.network else { return };
        let room = b.room_id.clone().filter(|_| b.kind == BufferKind::Channel);
        let looked_at = buffer == self.active;
        if !self.networks.get(&net).is_some_and(|n| n.is_twitch() && n.conn == ConnState::Ready) {
            return;
        }
        let token = self.api_token(net).map(|(t, _)| t);
        let n = self.networks.get_mut(&net).unwrap();
        let mut jobs = Vec::new();
        if token.is_some() {
            jobs.push(Job::TwitchUser);
            if let Some(room_id) = room.clone().filter(|_| looked_at) {
                jobs.push(Job::TwitchFollower { room_id });
            }
        }
        for provider in Provider::ALL.into_iter().filter(|&p| n.cfg.emote_provider(p)) {
            jobs.push(Job::Provider { provider, room_id: None });
            jobs.extend(room.clone().map(|r| Job::Provider { provider, room_id: Some(r) }));
        }
        jobs.retain(|j| n.emotes.started.insert(j.clone()));
        let connection = n.emotes.connection;
        // The user's Twitch emotes take many requests: fetched apart, so the rest need not wait.
        let (slow, fast): (Vec<Job>, Vec<Job>) = jobs.into_iter().partition(|j| *j == Job::TwitchUser);
        for jobs in [slow, fast].into_iter().filter(|j| !j.is_empty()) {
            let req = EmoteRequest { network: net, connection, token: token.clone(), jobs };
            self.effects.push(Effect::FetchEmotes(req));
        }
    }

    /// An [`Effect::FetchEmotes`] fetch finished.
    pub fn on_emote_result(&mut self, r: EmoteResult) {
        let Some(n) = self.networks.get_mut(&r.network).filter(|n| n.emotes.connection == r.connection) else {
            return;
        };
        for (key, list) in r.sets {
            n.emotes.sets.entry(key).or_default().emotes.extend(list);
        }
        let (signed_in, sb) = (n.auth.access_token().is_some(), n.server_buffer);
        self.emote_gen += 1;
        self.dirty.lines = true;
        if r.twitch_limited && signed_in && self.emote_scope_hint.insert(r.network) {
            self.status(
                sb,
                LineKind::Status,
                "Emote completion offers Twitch's global emotes only: sign in with Twitch again (network \
                 settings or /twitch login) to allow access to your follower and subscriber emotes.",
            );
        }
    }

    /// After a network's settings changed: providers switched on are fetched, and what is shown
    /// or completed is updated.
    fn emotes_on_config(&mut self, net: NetworkId) {
        self.emote_gen += 1;
        self.dirty.lines = true;
        let server = self.networks.get(&net).map(|n| n.server_buffer);
        let channels: Vec<BufferId> = self
            .buffers
            .iter()
            .filter(|b| b.network == Some(net) && b.kind == BufferKind::Channel)
            .map(|b| b.id)
            .collect();
        for b in server.into_iter().chain(channels) {
            self.request_emotes(b);
        }
    }

    /// The emote sets of a Twitch channel with where they come from and whether they are the
    /// channel's own, in lookup order: Twitch (the channel's first), then the channel's provider
    /// and script sets, then the global ones. Providers switched off are left out.
    fn channel_emote_sets(&self, buffer: BufferId) -> Vec<(EmoteSource, bool, &EmoteSet)> {
        let Some((n, b, channel)) = self.twitch_channel(buffer) else { return Vec::new() };
        let room = b.room_id.as_deref();
        let sets = &n.emotes.sets;
        let mut out = Vec::new();
        let mut twitch: Vec<_> = sets
            .iter()
            .filter_map(|(k, s)| match k {
                SetKey::Twitch { owner, emote_type } => Some((Some(owner.as_str()) == room, owner, emote_type, s)),
                _ => None,
            })
            .collect();
        // The channel's own first.
        twitch.sort_by(|a, b| (!a.0, a.1, a.2).cmp(&(!b.0, b.1, b.2)));
        out.extend(twitch.into_iter().map(|(own, _, t, s)| (EmoteSource::Twitch(t.clone()), own, s)));
        let rooms = room.map(|r| Some(r.to_owned())).into_iter().chain([None]);
        for room in rooms {
            let own = room.is_some();
            for provider in Provider::ALL.into_iter().filter(|&p| n.cfg.emote_provider(p)) {
                let key = SetKey::Provider { provider, room: room.clone() };
                out.extend(sets.get(&key).map(|s| (EmoteSource::Provider(provider), own, s)));
            }
            let script_channel = own.then(|| channel.clone());
            for ((name, _), s) in self.script_emotes.iter().filter(|((_, c), _)| *c == script_channel) {
                out.push((EmoteSource::from_name(name), own, s));
            }
        }
        out
    }

    /// Which words show as emotes in a Twitch channel: the enabled providers' and the scripts'
    /// sets, and in the user's own lines also their Twitch emotes.
    pub fn emote_lookup(&self, buffer: BufferId) -> Lookup<'_> {
        let mut l = Lookup::default();
        for (source, _, set) in self.channel_emote_sets(buffer) {
            if matches!(source, EmoteSource::Twitch(_)) { &mut l.own } else { &mut l.sets }.push(set);
        }
        l
    }

    /// Replaces the emotes a script provides for a channel (`Some("#channel")`) or globally; they
    /// are shown and completed in Twitch channels.
    pub fn set_script_emotes(&mut self, name: &str, channel: Option<&str>, emotes: Vec<(String, String)>) {
        let key = (name.to_ascii_lowercase(), channel.map(|c| c.to_ascii_lowercase()));
        let emotes = emotes.into_iter().filter(|(name, url)| !name.is_empty() && !url.is_empty()).collect();
        self.script_emotes.insert(key, EmoteSet { emotes });
        self.emote_gen += 1;
        self.dirty.lines = true;
    }

    /// Emote completions for `query` in a Twitch channel buffer, best first (none when the
    /// network has emote completion switched off).
    pub fn emote_completions(&self, buffer: BufferId, query: &str, limit: usize) -> Vec<EmoteEntry> {
        if !self.twitch_channel(buffer).is_some_and(|(n, ..)| n.cfg.emote_completion) {
            return Vec::new();
        }
        let sets = self.channel_emote_sets(buffer);
        let all: Vec<EmoteEntry> = sets.iter().flat_map(|(source, own, set)| set.entries(source, *own)).collect();
        crate::emotes::complete(all.iter(), query, limit)
    }
}
