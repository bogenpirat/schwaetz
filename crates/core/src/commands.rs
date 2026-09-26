//! User input: messages, `/commands` and aliases.

use crate::app::{App, ConnState, Effect};
use crate::buffer::{BufferId, BufferKind, LineKind, NotifyLevel};
use crate::completion::Candidates;
use crate::config::{IgnoreRule, NetworkConfig, NetworkKind};
use schwaetz_client::ChatKind;
use schwaetz_net::NetworkId;
use schwaetz_proto::{Message, Tags, ctcp, mask};

pub struct CommandInfo {
    pub name: &'static str,
    pub usage: &'static str,
    pub help: &'static str,
}

macro_rules! cmds {
    ($($name:literal, $usage:literal, $help:literal;)*) => {
        pub const COMMANDS: &[CommandInfo] = &[$(CommandInfo { name: $name, usage: $usage, help: $help }),*];
    };
}

cmds! {
    "alias", "/alias [name [expansion]]", "Define an alias; $1..$9, $1- (rest), $nick, $channel, $network expand. Without arguments, list aliases.";
    "away", "/away [message]", "Mark yourself away (remembered across reconnects). Without a message, same as /back.";
    "back", "/back", "Clear away status.";
    "ban", "/ban <nick|mask>", "Ban a user from the current channel.";
    "buffer", "/buffer <name|number>", "Switch to a buffer by (partial) name or sidebar position.";
    "clear", "/clear", "Clear the current buffer.";
    "close", "/close", "Close the current buffer (parts channels).";
    "connect", "/connect <network|host[:[+]port]> [nick]", "Connect to a configured network or an ad-hoc server.";
    "ctcp", "/ctcp <target> <command> [args]", "Send a CTCP request.";
    "cycle", "/cycle [reason]", "Part and rejoin the current channel.";
    "dehop", "/dehop <nicks…>", "Remove half-operator status.";
    "deop", "/deop <nicks…>", "Remove operator status.";
    "devoice", "/devoice <nicks…>", "Remove voice.";
    "disconnect", "/disconnect [message]", "Disconnect from the current network and stop reconnecting.";
    "echo", "/echo <text>", "Print text locally.";
    "help", "/help [command]", "Show help.";
    "highlight", "/highlight [add|del <word>]", "Manage highlight words.";
    "history", "/history [count]", "Fetch recent history from the server (CHATHISTORY).";
    "hop", "/hop <nicks…>", "Give half-operator status.";
    "ignore", "/ignore [mask [types…]]", "Ignore a user (types: msg notice action ctcp invite join part quit nick all). Without arguments, list ignores.";
    "invite", "/invite <nick> [#channel]", "Invite a user.";
    "join", "/join <#channel[,#other]> [keys]", "Join channels.";
    "kick", "/kick <nick> [reason]", "Kick a user from the current channel.";
    "kickban", "/kickban <nick> [reason]", "Ban and kick a user.";
    "lastlog", "/lastlog <text>", "Show lines of the current buffer containing text.";
    "list", "/list [filter]", "Open the channel list.";
    "me", "/me <action>", "Send an action.";
    "mode", "/mode [target] <modes> [args]", "Change channel or user modes.";
    "monitor", "/monitor [+nick|-nick|list|clear]", "Get notified when users come online.";
    "msg", "/msg <target> <text>", "Send a private message.";
    "names", "/names [#channel]", "List users in a channel.";
    "nick", "/nick <nick>", "Change your nickname.";
    "notice", "/notice <target> <text>", "Send a notice.";
    "notify", "/notify [all|default|highlights|mute]", "Set the notification level for the current buffer.";
    "op", "/op <nicks…>", "Give operator status.";
    "part", "/part [#channel] [reason]", "Leave a channel.";
    "ping", "/ping <nick>", "Measure round-trip time to a user (CTCP PING).";
    "query", "/query <nick> [text]", "Open a private conversation.";
    "quit", "/quit [message]", "Disconnect from all networks and exit.";
    "quote", "/quote <raw line>", "Send a raw line to the server.";
    "raw", "/raw <raw line>", "Send a raw line to the server.";
    "rawlog", "/rawlog", "Toggle a per-network buffer showing raw protocol traffic (passwords masked).";
    "reconnect", "/reconnect", "Reconnect to the current network now.";
    "reload", "/reload", "Reload scripts.";
    "say", "/say <text>", "Send text as a message (even if it starts with /).";
    "search", "/search [-n] <text>", "Search the message history (all networks, or -n for the current one).";
    "server", "/server <host[:[+]port]>", "Connect to a server.";
    "set", "/set [key [value]]", "Show or change a setting, e.g. /set appearance.font_size 14.";
    "setname", "/setname <realname>", "Change your real name (IRCv3 setname).";
    "topic", "/topic [#channel] [text]", "Show or set the topic.";
    "unalias", "/unalias <name>", "Remove an alias.";
    "unban", "/unban <mask>", "Remove a ban.";
    "unignore", "/unignore <mask>", "Remove an ignore.";
    "version", "/version [nick]", "Request a user's client version (or the server's).";
    "voice", "/voice <nicks…>", "Give voice.";
    "whois", "/whois <nick>", "Show information about a user.";
    "whowas", "/whowas <nick>", "Show information about a user who left.";
    "znc", "/znc <command>", "Send a command to ZNC's *status (e.g. /znc ListNetworks). /znc import offers to add your other ZNC networks.";
}

impl App {
    /// Handles text from the input box. Multi-line pastes may require confirmation.
    pub fn input(&mut self, buffer: BufferId, text: &str) {
        let text = text.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            return;
        }
        let lines = text.lines().count();
        let limit = self.config.general.confirm_paste_lines;
        if limit > 0 && lines > limit && !text.starts_with('/') {
            self.effect(Effect::ConfirmPaste { buffer, text: text.to_owned(), lines });
            return;
        }
        self.input_confirmed(buffer, text);
    }

    /// Sends input without the paste check (after the user confirmed).
    pub fn input_confirmed(&mut self, buffer: BufferId, text: &str) {
        if let Some(b) = self.buffer_mut(buffer) {
            b.input.push(text);
            b.input.draft.clear();
        }
        self.completer.reset();
        if text.contains('\n') && !text.lines().any(|l| l.starts_with('/') && !l.starts_with("//")) {
            // A plain multi-line message: one send so multiline batches can be used.
            self.say(buffer, ChatKind::Privmsg, text);
            return;
        }
        for line in text.lines() {
            self.input_line(buffer, line);
        }
    }

    /// One line of input: a command or a message.
    pub fn input_line(&mut self, buffer: BufferId, line: &str) {
        if let Some(rest) = line.strip_prefix('/')
            && !rest.starts_with('/')
        {
            let (cmd, args) = rest.split_once(' ').unwrap_or((rest, ""));
            self.command(buffer, &cmd.to_lowercase(), args.trim_start(), 0);
        } else {
            let text = line.strip_prefix('/').filter(|r| r.starts_with('/')).unwrap_or(line);
            self.say(buffer, ChatKind::Privmsg, text);
        }
    }

    fn say(&mut self, buffer: BufferId, kind: ChatKind, text: &str) {
        let Some(b) = self.buffer(buffer) else { return };
        let (net, name, bkind) = (b.network, b.name.clone(), b.kind);
        match (net, bkind) {
            (Some(net), BufferKind::Channel | BufferKind::Query) => self.send_chat(net, kind, &name, text, Tags::new()),
            _ => self.status(
                buffer,
                LineKind::Error,
                "This buffer is not a channel or query. Use /msg <target> <text> or /quote.",
            ),
        }
    }

    /// Sends PRIVMSG/NOTICE/ACTION via the session (handles splitting, multiline and local echo).
    pub fn send_chat(&mut self, net: NetworkId, kind: ChatKind, target: &str, text: &str, tags: Tags) {
        let now = self.now;
        let Some(n) = self.networks.get_mut(&net) else { return };
        if n.conn != ConnState::Ready {
            let b = self.find_buffer(net, target).unwrap_or(self.active);
            self.status(b, LineKind::Error, "Not connected — message not sent.");
            return;
        }
        n.session.say(kind, target, text, tags, now);
        self.flush(net);
    }

    /// Sends a reply to a message (`+draft/reply`) where supported.
    pub fn reply(&mut self, buffer: BufferId, msgid: &str, text: &str) {
        let Some(b) = self.buffer(buffer) else { return };
        let (Some(net), name) = (b.network, b.name.clone()) else { return };
        let mut tags = Tags::new();
        tags.insert("+draft/reply", msgid);
        self.send_chat(net, ChatKind::Privmsg, &name, text, tags);
    }

    /// Reacts to a message (`+draft/react`).
    pub fn react(&mut self, buffer: BufferId, msgid: &str, reaction: &str) {
        let Some(b) = self.buffer(buffer) else { return };
        let (Some(net), name) = (b.network, b.name.clone()) else { return };
        let mut tags = Tags::new();
        tags.insert("+draft/react", reaction);
        tags.insert("+draft/reply", msgid);
        if let Some(n) = self.networks.get_mut(&net) {
            if !n.session.tagmsg(&name, tags) {
                self.status(buffer, LineKind::Error, "This server does not support reactions.");
            }
            self.flush(net);
        }
    }

    /// Sends a typing notification (`+typing`) for the buffer, if supported.
    pub fn typing(&mut self, buffer: BufferId, state: &str) {
        let Some(b) = self.buffer(buffer) else { return };
        let (Some(net), name, kind) = (b.network, b.name.clone(), b.kind) else { return };
        if !matches!(kind, BufferKind::Channel | BufferKind::Query) {
            return;
        }
        if let Some(n) = self.networks.get_mut(&net)
            && n.conn == ConnState::Ready
            && !n.is_twitch()
        {
            let mut tags = Tags::new();
            tags.insert("+typing", state);
            n.session.tagmsg(&name, tags);
            self.flush(net);
        }
    }

    pub fn complete(
        &mut self,
        buffer: BufferId,
        input: &str,
        cursor: usize,
        backwards: bool,
    ) -> Option<(String, usize)> {
        let b = self.buffer(buffer)?;
        let mut nicks: Vec<String> = Vec::new();
        let mut channels: Vec<String> = Vec::new();
        if let Some(net) = b.network.and_then(|n| self.networks.get(&n)) {
            let cm = net.session.casemapping();
            if let Some(ch) = net.session.channel(&b.name) {
                let mut members: Vec<(&str, i64)> = ch
                    .members
                    .values()
                    .map(|m| (m.nick.as_str(), b.last_spoke.get(cm.fold(&m.nick).as_ref()).copied().unwrap_or(0)))
                    .collect();
                members.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
                nicks = members.into_iter().filter(|(n, _)| !net.session.is_me(n)).map(|(n, _)| n.to_owned()).collect();
            } else if b.kind == BufferKind::Query {
                nicks.push(b.name.clone());
            }
            if b.name.eq_ignore_ascii_case("*status") {
                nicks.extend(crate::znc::STATUS_COMMANDS.iter().map(|s| s.to_string()));
            }
            channels = net.session.channels().map(|c| c.name.clone()).collect();
            channels.sort();
        }
        let mut commands: Vec<String> = COMMANDS.iter().map(|c| c.name.to_owned()).collect();
        commands.extend(self.config.aliases.keys().cloned());
        commands.extend(self.extra_commands.iter().map(|(n, _)| n.clone()));
        commands.sort();
        commands.dedup();
        let c = Candidates { nicks: &nicks, channels: &channels, commands: &commands };
        self.completer.complete(input, cursor, &c, backwards)
    }

    fn net_of(&self, buffer: BufferId) -> Option<NetworkId> {
        self.buffer(buffer).and_then(|b| b.network)
    }

    fn require_net(&mut self, buffer: BufferId) -> Option<NetworkId> {
        let net = self.net_of(buffer);
        if net.is_none() {
            self.status(buffer, LineKind::Error, "This command needs a network; switch to a network's buffer first.");
        }
        net
    }

    /// The channel of the buffer, or the first argument if it's a channel name.
    fn channel_arg<'a>(&self, buffer: BufferId, args: &'a str) -> (Option<String>, &'a str) {
        let net = self.net_of(buffer).and_then(|n| self.networks.get(&n));
        let first = args.split_whitespace().next().unwrap_or("");
        if let Some(n) = net
            && n.session.is_channel(first)
        {
            return (Some(first.to_owned()), args[first.len()..].trim_start());
        }
        let b = self.buffer(buffer);
        (b.filter(|b| b.kind == BufferKind::Channel).map(|b| b.name.clone()), args)
    }

    fn raw(&mut self, net: NetworkId, msg: Message, buffer: BufferId) {
        let Some(n) = self.networks.get_mut(&net) else { return };
        if n.conn == ConnState::Disconnected {
            self.status(buffer, LineKind::Error, "Not connected.");
            return;
        }
        if let Some(label) = n.session.send_labeled(msg) {
            n.label_buffers.insert(label, buffer);
            if n.label_buffers.len() > 64 {
                let first = n.label_buffers.keys().next().cloned().unwrap();
                n.label_buffers.remove(&first);
            }
        }
        self.flush(net);
    }

    pub(crate) fn command(&mut self, buffer: BufferId, cmd: &str, args: &str, depth: u8) {
        if let Some(exp) = self.config.aliases.get(cmd).cloned() {
            if depth > 4 {
                self.status(buffer, LineKind::Error, "Alias recursion too deep.");
                return;
            }
            let expanded = self.expand_alias(buffer, &exp, args);
            for line in expanded.lines() {
                match line.strip_prefix('/') {
                    Some(rest) if !rest.starts_with('/') => {
                        let (c, a) = rest.split_once(' ').unwrap_or((rest, ""));
                        self.command(buffer, &c.to_lowercase(), a.trim_start(), depth + 1);
                    }
                    _ => self.say(buffer, ChatKind::Privmsg, line),
                }
            }
            return;
        }
        let mut words = args.split_whitespace();
        let arg1 = words.next().unwrap_or("");
        let rest_after1 = args[arg1.len()..].trim_start();
        match cmd {
            "help" => self.help(buffer, arg1),
            "echo" => self.status(buffer, LineKind::Status, args),
            "clear" => {
                if let Some(b) = self.buffer_mut(buffer) {
                    b.clear();
                }
                self.dirty.lines = true;
            }
            "close" | "wc" => {
                let (kind, name, net, joined) = match self.buffer(buffer) {
                    Some(b) => (b.kind, b.name.clone(), b.network, b.joined),
                    None => return,
                };
                if kind == BufferKind::Channel
                    && joined
                    && let Some(net) = net
                {
                    let reason =
                        if args.is_empty() { self.config.general.part_message.clone() } else { args.to_owned() };
                    let mut p = vec![name];
                    if !reason.is_empty() {
                        p.push(reason);
                    }
                    self.raw(net, Message::new("PART", p), buffer);
                }
                self.close_buffer(buffer);
            }
            "join" | "j" => {
                let Some(net) = self.require_net(buffer) else { return };
                if arg1.is_empty() {
                    return self.usage(buffer, "join");
                }
                let is_channel = |c: &str| self.networks[&net].session.is_channel(c);
                let chans: Vec<String> =
                    arg1.split(',').map(|c| if is_channel(c) { c.to_owned() } else { format!("#{c}") }).collect();
                let n = self.networks.get_mut(&net).unwrap();
                n.pending_joins.extend(chans.iter().cloned());
                // Switch right away to buffers that already exist.
                if let Some(existing) = self.find_buffer(net, &chans[0]) {
                    self.switch_to(existing);
                }
                let mut p = vec![chans.join(",")];
                if !rest_after1.is_empty() {
                    p.push(rest_after1.to_owned());
                }
                self.raw(net, Message::new("JOIN", p), buffer);
            }
            "part" | "leave" => {
                let Some(net) = self.require_net(buffer) else { return };
                let (chan, reason) = self.channel_arg(buffer, args);
                let Some(chan) = chan else { return self.usage(buffer, "part") };
                let reason =
                    if reason.is_empty() { self.config.general.part_message.clone() } else { reason.to_owned() };
                let mut p = vec![chan];
                if !reason.is_empty() {
                    p.push(reason);
                }
                self.raw(net, Message::new("PART", p), buffer);
            }
            "cycle" | "rejoin" => {
                let Some(net) = self.require_net(buffer) else { return };
                let (chan, reason) = self.channel_arg(buffer, args);
                let Some(chan) = chan else { return self.usage(buffer, "cycle") };
                let key = self.networks[&net].session.channel(&chan).and_then(|c| c.key.clone());
                let mut p = vec![chan.clone()];
                if !reason.is_empty() {
                    p.push(reason.to_owned());
                }
                self.raw(net, Message::new("PART", p), buffer);
                let mut j = vec![chan];
                j.extend(key);
                self.raw(net, Message::new("JOIN", j), buffer);
            }
            "msg" | "privmsg" | "notice" => {
                let Some(net) = self.require_net(buffer) else { return };
                if arg1.is_empty() || rest_after1.is_empty() {
                    return self.usage(buffer, cmd);
                }
                let kind = if cmd == "notice" { ChatKind::Notice } else { ChatKind::Privmsg };
                self.send_chat(net, kind, arg1, rest_after1, Tags::new());
                // Private notices/messages to targets without a buffer are echoed here.
                let is_chan = self.networks[&net].session.is_channel(arg1);
                if self.find_buffer(net, arg1).is_none() && !is_chan {
                    self.status(buffer, LineKind::Status, format!("-> {arg1}: {rest_after1}"));
                }
            }
            "query" | "q" => {
                let Some(net) = self.require_net(buffer) else { return };
                if arg1.is_empty() {
                    return self.usage(buffer, "query");
                }
                let id = self.ensure_buffer(net, BufferKind::Query, arg1);
                self.switch_to(id);
                if !rest_after1.is_empty() {
                    self.send_chat(net, ChatKind::Privmsg, arg1, rest_after1, Tags::new());
                }
            }
            "me" | "action" => {
                if args.is_empty() {
                    return self.usage(buffer, "me");
                }
                self.say(buffer, ChatKind::Action, args);
            }
            "say" => self.say(buffer, ChatKind::Privmsg, args),
            "ctcp" => {
                let Some(net) = self.require_net(buffer) else { return };
                let mut it = rest_after1.splitn(2, ' ');
                let command = it.next().unwrap_or("").to_uppercase();
                if arg1.is_empty() || command.is_empty() {
                    return self.usage(buffer, "ctcp");
                }
                let params = it.next().unwrap_or("");
                let params =
                    if command == "PING" && params.is_empty() { self.now.to_string() } else { params.to_owned() };
                self.raw(net, Message::new("PRIVMSG", [arg1.to_owned(), ctcp::encode(&command, &params)]), buffer);
            }
            "ping" => self.command(buffer, "ctcp", &format!("{arg1} PING"), depth),
            "version" => {
                if arg1.is_empty() {
                    let Some(net) = self.require_net(buffer) else { return };
                    self.raw(net, Message::new("VERSION", Vec::<String>::new()), buffer);
                } else {
                    self.command(buffer, "ctcp", &format!("{arg1} VERSION"), depth);
                }
            }
            "nick" => {
                let Some(net) = self.require_net(buffer) else { return };
                if arg1.is_empty() {
                    return self.usage(buffer, "nick");
                }
                let n = self.networks.get_mut(&net).unwrap();
                n.session.config_mut().nick = arg1.to_owned();
                if n.conn == ConnState::Disconnected {
                    n.cfg.nick = Some(arg1.to_owned());
                    self.status(buffer, LineKind::Status, format!("Will use nick {arg1} when connecting."));
                } else {
                    self.raw(net, Message::new("NICK", [arg1]), buffer);
                }
            }
            "topic" => {
                let Some(net) = self.require_net(buffer) else { return };
                let (chan, text) = self.channel_arg(buffer, args);
                let Some(chan) = chan else { return self.usage(buffer, "topic") };
                let mut p = vec![chan];
                if !text.is_empty() {
                    p.push(text.to_owned());
                }
                self.raw(net, Message::new("TOPIC", p), buffer);
            }
            "mode" => {
                let Some(net) = self.require_net(buffer) else { return };
                let mut p: Vec<String> = args.split_whitespace().map(str::to_owned).collect();
                let s = &self.networks[&net].session;
                let first_is_target = p.first().is_some_and(|f| s.is_channel(f) || s.is_me(f));
                if !first_is_target {
                    let (chan, _) = self.channel_arg(buffer, "");
                    let target = chan.unwrap_or_else(|| self.networks[&net].session.nick().to_owned());
                    p.insert(0, target);
                }
                self.raw(net, Message::new("MODE", p), buffer);
            }
            "op" | "deop" | "voice" | "devoice" | "hop" | "dehop" => {
                let Some(net) = self.require_net(buffer) else { return };
                let (chan, nicks) = self.channel_arg(buffer, args);
                let Some(chan) = chan else { return self.usage(buffer, cmd) };
                let nicks: Vec<&str> = nicks.split_whitespace().collect();
                if nicks.is_empty() {
                    return self.usage(buffer, cmd);
                }
                let (sign, m) = match cmd {
                    "op" => ('+', 'o'),
                    "deop" => ('-', 'o'),
                    "voice" => ('+', 'v'),
                    "devoice" => ('-', 'v'),
                    "hop" => ('+', 'h'),
                    _ => ('-', 'h'),
                };
                let per = self.networks[&net].session.isupport().modes.clamp(1, 12);
                for chunk in nicks.chunks(per) {
                    let mut p = vec![chan.clone(), format!("{sign}{}", m.to_string().repeat(chunk.len()))];
                    p.extend(chunk.iter().map(|s| s.to_string()));
                    self.raw(net, Message::new("MODE", p), buffer);
                }
            }
            "kick" | "k" | "kickban" | "kb" => {
                let Some(net) = self.require_net(buffer) else { return };
                let (chan, rest) = self.channel_arg(buffer, args);
                let Some(chan) = chan else { return self.usage(buffer, "kick") };
                let (nick, reason) = rest.split_once(' ').unwrap_or((rest, ""));
                if nick.is_empty() {
                    return self.usage(buffer, "kick");
                }
                if cmd.starts_with("kickb") || cmd == "kb" {
                    let m = self.ban_mask(net, nick);
                    self.raw(net, Message::new("MODE", [chan.clone(), "+b".into(), m]), buffer);
                }
                let mut p = vec![chan, nick.to_owned()];
                if !reason.is_empty() {
                    p.push(reason.to_owned());
                }
                self.raw(net, Message::new("KICK", p), buffer);
            }
            "ban" | "unban" => {
                let Some(net) = self.require_net(buffer) else { return };
                let (chan, rest) = self.channel_arg(buffer, args);
                let Some(chan) = chan else { return self.usage(buffer, cmd) };
                let target = rest.split_whitespace().next().unwrap_or("");
                if target.is_empty() {
                    // Without arguments, list bans.
                    self.raw(net, Message::new("MODE", [chan, "b".into()]), buffer);
                    return;
                }
                let m = if cmd == "ban" { self.ban_mask(net, target) } else { target.to_owned() };
                let sign = if cmd == "ban" { "+b" } else { "-b" };
                self.raw(net, Message::new("MODE", [chan, sign.into(), m]), buffer);
            }
            "invite" => {
                let Some(net) = self.require_net(buffer) else { return };
                let (chan, _) = if rest_after1.is_empty() {
                    self.channel_arg(buffer, "")
                } else {
                    (Some(rest_after1.to_owned()), "")
                };
                let (Some(chan), false) = (chan, arg1.is_empty()) else { return self.usage(buffer, "invite") };
                self.raw(net, Message::new("INVITE", [arg1.to_owned(), chan]), buffer);
            }
            "whois" | "wi" | "whowas" | "who" => {
                let Some(net) = self.require_net(buffer) else { return };
                let target = if arg1.is_empty() {
                    match self.buffer(buffer) {
                        Some(b) if b.kind == BufferKind::Query => b.name.clone(),
                        _ => return self.usage(buffer, cmd),
                    }
                } else {
                    arg1.to_owned()
                };
                let command = match cmd {
                    "whowas" => "WHOWAS",
                    "who" => "WHO",
                    _ => "WHOIS",
                };
                // "/whois nick nick" asks the user's server for idle time.
                let p = if cmd == "whois" && rest_after1 == "+" { vec![target.clone(), target] } else { vec![target] };
                self.raw(net, Message::new(command, p), buffer);
            }
            "names" => {
                let Some(net) = self.require_net(buffer) else { return };
                let (chan, _) = self.channel_arg(buffer, args);
                let Some(chan) = chan else { return self.usage(buffer, "names") };
                self.raw(net, Message::new("NAMES", [chan]), buffer);
            }
            "list" => {
                let Some(net) = self.require_net(buffer) else { return };
                if let Some(n) = self.networks.get_mut(&net) {
                    n.channel_list.clear();
                    n.channel_list_complete = false;
                }
                let p: Vec<String> = if args.is_empty() { vec![] } else { vec![args.to_owned()] };
                self.raw(net, Message::new("LIST", p), buffer);
                self.effect(Effect::ChannelList(net));
            }
            "away" | "back" => {
                let Some(net) = self.require_net(buffer) else { return };
                let msg = if cmd == "back" || args.is_empty() { None } else { Some(args.to_owned()) };
                if let Some(n) = self.networks.get_mut(&net) {
                    n.session.set_away(msg);
                }
                self.flush(net);
            }
            "quote" | "raw" => {
                let Some(net) = self.require_net(buffer) else { return };
                match Message::parse(args) {
                    Ok(m) => self.raw(net, m, buffer),
                    Err(_) => self.usage(buffer, "quote"),
                }
            }
            "setname" => {
                let Some(net) = self.require_net(buffer) else { return };
                if !self.networks[&net].session.has_cap("setname") {
                    return self.status(
                        buffer,
                        LineKind::Error,
                        "This server does not support changing the real name.",
                    );
                }
                self.raw(net, Message::new("SETNAME", [args]), buffer);
            }
            "monitor" => {
                let Some(net) = self.require_net(buffer) else { return };
                let p: Vec<String> = match arg1 {
                    "" | "list" | "l" => vec!["L".into()],
                    "clear" | "c" => vec!["C".into()],
                    "status" | "s" => vec!["S".into()],
                    a if a.starts_with('-') => vec!["-".into(), a[1..].replace(' ', ",")],
                    a => vec!["+".into(), a.trim_start_matches('+').to_owned()],
                };
                self.raw(net, Message::new("MONITOR", p), buffer);
            }
            "history" => {
                let Some(net) = self.require_net(buffer) else { return };
                let name = self.buffer(buffer).map(|b| b.name.clone()).unwrap_or_default();
                let count = arg1.parse().unwrap_or(100);
                let ok = self.networks.get_mut(&net).is_some_and(|n| n.session.request_history(&name, None, count));
                if ok {
                    self.flush(net);
                } else {
                    self.status(buffer, LineKind::Error, "This server does not provide history (CHATHISTORY).");
                }
            }
            "znc" => {
                let Some(net) = self.require_net(buffer) else { return };
                if arg1.eq_ignore_ascii_case("import") {
                    self.networks.get_mut(&net).unwrap().znc_collect = Some(Vec::new());
                    self.raw(net, Message::new("PRIVMSG", ["*status", "ListNetworks"]), buffer);
                } else if args.is_empty() {
                    self.usage(buffer, "znc");
                } else {
                    self.raw(net, Message::new("PRIVMSG", ["*status", args]), buffer);
                }
            }
            "connect" | "server" => self.cmd_connect(buffer, args.trim(), arg1, rest_after1),
            "disconnect" => {
                let Some(net) = self.require_net(buffer) else { return };
                self.disconnect(net, if args.is_empty() { None } else { Some(args.to_owned()) });
            }
            "reconnect" => {
                let Some(net) = self.require_net(buffer) else { return };
                self.reconnect_now(net);
            }
            "quit" | "exit" => {
                self.quit_all(if args.is_empty() { None } else { Some(args.to_owned()) });
                self.effect(Effect::Quit);
            }
            "notify" => {
                let level = match arg1 {
                    "all" => NotifyLevel::All,
                    "highlights" | "highlight" => NotifyLevel::HighlightsOnly,
                    "mute" | "none" | "off" => NotifyLevel::Mute,
                    "default" | "" => NotifyLevel::Default,
                    _ => return self.usage(buffer, "notify"),
                };
                if let Some(b) = self.buffer_mut(buffer) {
                    b.notify = level;
                }
                self.status(buffer, LineKind::Status, format!("Notification level: {level:?}"));
                self.dirty.sidebar = true;
            }
            "ignore" => {
                if arg1.is_empty() {
                    let list: Vec<String> =
                        self.config.ignores.iter().map(|i| format!("{} ({})", i.mask, i.types.join(" "))).collect();
                    let text = if list.is_empty() {
                        "No ignores.".to_owned()
                    } else {
                        format!("Ignoring: {}", list.join(", "))
                    };
                    return self.status(buffer, LineKind::Status, text);
                }
                let types: Vec<String> = words.map(str::to_owned).collect();
                let rule = IgnoreRule {
                    mask: mask::normalize(arg1),
                    types: if types.is_empty() { vec!["all".into()] } else { types },
                    network: None,
                    channel: None,
                };
                self.status(buffer, LineKind::Status, format!("Now ignoring {} ({})", rule.mask, rule.types.join(" ")));
                self.config.ignores.push(rule);
                self.apply_config();
                self.effect(Effect::SaveConfig);
            }
            "unignore" => {
                let m = mask::normalize(arg1);
                let before = self.config.ignores.len();
                self.config.ignores.retain(|i| mask::normalize(&i.mask) != m);
                let removed = before - self.config.ignores.len();
                self.status(buffer, LineKind::Status, format!("Removed {removed} ignore(s) for {m}"));
                self.apply_config();
                self.effect(Effect::SaveConfig);
            }
            "highlight" => match arg1 {
                "add" if !rest_after1.is_empty() => {
                    self.config.highlight.words.push(rest_after1.to_owned());
                    self.apply_config();
                    self.effect(Effect::SaveConfig);
                    self.status(buffer, LineKind::Status, format!("Highlighting \"{rest_after1}\""));
                }
                "del" | "remove" if !rest_after1.is_empty() => {
                    self.config.highlight.words.retain(|w| !w.eq_ignore_ascii_case(rest_after1));
                    self.apply_config();
                    self.effect(Effect::SaveConfig);
                }
                _ => {
                    let words = self.config.highlight.words.join(", ");
                    self.status(buffer, LineKind::Status, format!("Highlight words: {words}"));
                }
            },
            "alias" => {
                if arg1.is_empty() {
                    let list: Vec<String> = self.config.aliases.iter().map(|(k, v)| format!("/{k} → {v}")).collect();
                    for l in list {
                        self.status(buffer, LineKind::Status, l);
                    }
                } else if rest_after1.is_empty() {
                    let v = self.config.aliases.get(arg1).cloned().unwrap_or_else(|| "(not defined)".into());
                    self.status(buffer, LineKind::Status, format!("/{arg1} → {v}"));
                } else {
                    self.config.aliases.insert(arg1.trim_start_matches('/').to_lowercase(), rest_after1.to_owned());
                    self.effect(Effect::SaveConfig);
                    self.status(buffer, LineKind::Status, format!("Alias /{arg1} defined"));
                }
            }
            "unalias" => {
                if self.config.aliases.remove(arg1.trim_start_matches('/')).is_some() {
                    self.effect(Effect::SaveConfig);
                    self.status(buffer, LineKind::Status, format!("Alias /{arg1} removed"));
                }
            }
            "set" => self.cmd_set(buffer, arg1, rest_after1),
            "lastlog" => {
                if args.is_empty() {
                    return self.usage(buffer, "lastlog");
                }
                let needle = args.to_lowercase();
                let found: Vec<String> = self
                    .buffer(buffer)
                    .map(|b| {
                        b.lines
                            .iter()
                            .filter(|l| schwaetz_proto::format::strip(&l.text).to_lowercase().contains(&needle))
                            .map(|l| {
                                let t = crate::time::format("%H:%M", crate::time::local(l.time));
                                format!("[{t}] <{}> {}", l.nick, schwaetz_proto::format::strip(&l.text))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.status(buffer, LineKind::Status, format!("Lastlog: {} match(es) for \"{args}\"", found.len()));
                for f in found.into_iter().rev().take(50).rev() {
                    self.status(buffer, LineKind::Server, f);
                }
            }
            "search" => {
                if args.is_empty() {
                    return self.usage(buffer, "search");
                }
                // `/search -n text` limits the search to the current network.
                match args.strip_prefix("-n ") {
                    Some(q) => {
                        let net = self.network_of(buffer).map(|n| n.display_name().to_owned());
                        self.search(q.trim(), net);
                    }
                    None => self.search(args, None),
                }
            }
            "reload" => self.effect(Effect::ReloadScripts),
            "buffer" | "b" | "goto" => {
                if args.is_empty() {
                    return self.usage(buffer, "buffer");
                }
                let order = self.sidebar_order();
                let target = if let Ok(n) = args.parse::<usize>() {
                    order.get(n.saturating_sub(1)).copied()
                } else {
                    let want = args.to_lowercase();
                    let name_of = |id: &BufferId| self.buffer(*id).map(|b| b.name.to_lowercase()).unwrap_or_default();
                    order
                        .iter()
                        .find(|id| name_of(id) == want)
                        .or_else(|| {
                            order.iter().find(|id| {
                                name_of(id).trim_start_matches(['#', '&']) == want.trim_start_matches(['#', '&'])
                            })
                        })
                        .or_else(|| order.iter().find(|id| name_of(id).contains(&want)))
                        .copied()
                };
                match target {
                    Some(id) => self.switch_to(id),
                    None => self.status(buffer, LineKind::Error, format!("No buffer matching \"{args}\"")),
                }
            }
            "rawlog" => {
                self.rawlog = !self.rawlog;
                let state = if self.rawlog { "on — see the \"raw log\" buffer of each network" } else { "off" };
                self.status(buffer, LineKind::Status, format!("Raw protocol log {state}"));
            }
            _ => {
                // Unknown commands go to the server verbatim (e.g. /knock, /oper, /stats, /cs …).
                if let Some(net) = self.net_of(buffer) {
                    let line = format!("{} {args}", cmd.to_uppercase());
                    match Message::parse(line.trim_end()) {
                        Ok(m) => self.raw(net, m, buffer),
                        Err(_) => self.status(buffer, LineKind::Error, format!("Unknown command /{cmd}")),
                    }
                } else {
                    self.status(buffer, LineKind::Error, format!("Unknown command /{cmd}. Try /help."));
                }
            }
        }
    }

    fn usage(&mut self, buffer: BufferId, cmd: &str) {
        let u = COMMANDS.iter().find(|c| c.name == cmd).map(|c| c.usage).unwrap_or("");
        self.status(buffer, LineKind::Error, format!("Usage: {u}"));
    }

    fn help(&mut self, buffer: BufferId, topic: &str) {
        let topic = topic.trim_start_matches('/');
        if topic.is_empty() {
            let names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
            self.status(buffer, LineKind::Status, format!("Commands: {}", names.join(" ")));
            if !self.extra_commands.is_empty() {
                let s: Vec<&str> = self.extra_commands.iter().map(|(n, _)| n.as_str()).collect();
                self.status(buffer, LineKind::Status, format!("Script commands: {}", s.join(" ")));
            }
            self.status(
                buffer,
                LineKind::Status,
                "Use /help <command> for details. Unknown commands are sent to the server.",
            );
            return;
        }
        match COMMANDS.iter().find(|c| c.name == topic) {
            Some(c) => {
                self.status(buffer, LineKind::Status, c.usage);
                self.status(buffer, LineKind::Status, c.help);
            }
            None => match self.extra_commands.iter().find(|(n, _)| n == topic).map(|(_, h)| h.clone()) {
                Some(h) => self.status(buffer, LineKind::Status, format!("/{topic} (script): {h}")),
                None => self.status(buffer, LineKind::Error, format!("No help for /{topic}")),
            },
        }
    }

    fn ban_mask(&self, net: NetworkId, target: &str) -> String {
        if target.contains(['!', '@', '*']) {
            return mask::normalize(target);
        }
        match self.networks[&net].session.user(target).and_then(|u| u.host.clone()) {
            Some(host) => format!("*!*@{host}"),
            None => format!("{target}!*@*"),
        }
    }

    fn expand_alias(&self, buffer: BufferId, exp: &str, args: &str) -> String {
        let words: Vec<&str> = args.split_whitespace().collect();
        let b = self.buffer(buffer);
        let net = b.and_then(|b| b.network).and_then(|n| self.networks.get(&n));
        let mut out = exp.to_owned();
        for i in (1..=9).rev() {
            let rest = words.get(i - 1..).map(|w| w.join(" ")).unwrap_or_default();
            out = out.replace(&format!("${i}-"), &rest);
            out = out.replace(&format!("${i}"), words.get(i - 1).copied().unwrap_or(""));
        }
        out = out.replace("$nick", net.map(|n| n.session.nick()).unwrap_or(""));
        out =
            out.replace("$channel", b.filter(|b| b.kind == BufferKind::Channel).map(|b| b.name.as_str()).unwrap_or(""));
        out = out.replace("$network", net.map(|n| n.display_name()).unwrap_or(""));
        out
    }

    fn cmd_connect(&mut self, buffer: BufferId, full: &str, target: &str, nick: &str) {
        if target.is_empty() {
            match self.net_of(buffer) {
                Some(net) => self.connect(net),
                None => self.usage(buffer, "connect"),
            }
            return;
        }
        // A configured network by (possibly multi-word) name?
        if let Some(net) = self
            .networks
            .values()
            .find(|n| n.cfg.name.eq_ignore_ascii_case(full) || n.cfg.name.eq_ignore_ascii_case(target))
            .map(|n| n.id)
        {
            self.connect(net);
            self.switch_to(self.networks[&net].server_buffer);
            return;
        }
        let host = NetworkConfig::parse_server(target)
            .map(|(h, _, _)| h)
            .filter(|h| h.contains(['.', ':']) || h.eq_ignore_ascii_case("localhost"));
        let Some(host) = host else {
            return self.status(
                buffer,
                LineKind::Error,
                format!(
                    "No network named \"{full}\" and not a server address. Use /connect host[:port] or a network name."
                ),
            );
        };
        if let Some(net) = self
            .networks
            .values()
            .find(|n| {
                n.cfg
                    .servers
                    .iter()
                    .any(|s| NetworkConfig::parse_server(s).is_some_and(|(h, _, _)| h.eq_ignore_ascii_case(&host)))
            })
            .map(|n| n.id)
        {
            self.connect(net);
            self.switch_to(self.networks[&net].server_buffer);
            return;
        }
        let cfg = NetworkConfig {
            name: host.clone(),
            kind: if host.ends_with("twitch.tv") { NetworkKind::Twitch } else { NetworkKind::Irc },
            servers: vec![target.to_owned()],
            auto_connect: false,
            nick: (!nick.is_empty()).then(|| nick.to_owned()),
            ..Default::default()
        };
        self.config.networks.push(cfg.clone());
        self.effect(Effect::SaveConfig);
        let id = self.add_network(cfg);
        self.switch_to(self.networks[&id].server_buffer);
        self.connect(id);
    }

    /// `/set section.key value` edits the config through its TOML representation, so every
    /// setting is reachable without bespoke code.
    fn cmd_set(&mut self, buffer: BufferId, key: &str, value: &str) {
        let mut doc = match toml::Value::try_from(&self.config) {
            Ok(v) => v,
            Err(e) => return self.status(buffer, LineKind::Error, e.to_string()),
        };
        if key.is_empty() {
            for section in ["general", "appearance", "notifications", "highlight", "previews"] {
                if let Some(t) = doc.get(section).and_then(|v| v.as_table()) {
                    for (k, v) in t {
                        self.status(buffer, LineKind::Status, format!("{section}.{k} = {v}"));
                    }
                }
            }
            return;
        }
        let path: Vec<&str> = key.split('.').collect();
        let mut cur = &mut doc;
        for p in &path[..path.len() - 1] {
            match cur.get_mut(*p) {
                Some(v) => cur = v,
                None => return self.status(buffer, LineKind::Error, format!("Unknown setting {key}")),
            }
        }
        let last = path[path.len() - 1];
        let Some(slot) = cur.get_mut(last) else {
            return self.status(buffer, LineKind::Error, format!("Unknown setting {key}"));
        };
        if value.is_empty() {
            let v = slot.to_string();
            return self.status(buffer, LineKind::Status, format!("{key} = {v}"));
        }
        let new = match slot {
            toml::Value::String(_) => toml::Value::String(value.trim_matches('"').to_owned()),
            toml::Value::Boolean(_) => match value {
                "true" | "on" | "yes" | "1" => toml::Value::Boolean(true),
                "false" | "off" | "no" | "0" => toml::Value::Boolean(false),
                _ => return self.status(buffer, LineKind::Error, format!("{key} expects true/false")),
            },
            toml::Value::Integer(_) => match value.parse() {
                Ok(n) => toml::Value::Integer(n),
                Err(_) => return self.status(buffer, LineKind::Error, format!("{key} expects a whole number")),
            },
            toml::Value::Float(_) => match value.parse() {
                Ok(n) => toml::Value::Float(n),
                Err(_) => return self.status(buffer, LineKind::Error, format!("{key} expects a number")),
            },
            toml::Value::Array(_) => toml::Value::Array(
                value
                    .split(',')
                    .map(|s| toml::Value::String(s.trim().to_owned()))
                    .filter(|v| v.as_str() != Some(""))
                    .collect(),
            ),
            _ => return self.status(buffer, LineKind::Error, format!("{key} cannot be set with /set")),
        };
        *slot = new;
        match doc.try_into::<crate::config::Config>() {
            Ok(c) => {
                self.config = c;
                self.apply_config();
                self.effect(Effect::SaveConfig);
                self.status(buffer, LineKind::Status, format!("{key} = {value}"));
            }
            Err(e) => self.status(buffer, LineKind::Error, format!("Invalid value: {e}")),
        }
    }
}
