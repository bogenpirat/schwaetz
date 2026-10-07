//! The application model driven by simulated network events.

use schwaetz_core::{
    Activity, App, BufferKind, Config, ConnState, Effect, LineFlags, LineKind, NetworkConfig, NetworkKind,
};
use schwaetz_net::{NetCommand, NetEvent, NetworkId, ServerAddr};
use schwaetz_proto::Message;
use std::time::Duration;

const T0: i64 = 1_700_000_000_000;

struct Harness {
    app: App,
    net: NetworkId,
    now: i64,
}

impl Harness {
    fn new(kind: NetworkKind, autojoin: &[&str]) -> Harness {
        let mut cfg = Config::default();
        cfg.general.nick = "me".into();
        cfg.general.log_to_files = false;
        cfg.notifications.enabled = true;
        cfg.networks.push(NetworkConfig {
            name: "Test".into(),
            kind,
            servers: vec!["irc.test:6667".into()],
            autojoin: autojoin.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        });
        let mut app = App::new(cfg);
        app.focused = false;
        let net = *app.networks.keys().next().unwrap();
        Harness { app, net, now: T0 }
    }

    fn connect(&mut self) -> Vec<String> {
        self.app.connect(self.net);
        assert!(matches!(self.app.take_net_commands()[..], [NetCommand::Connect(..)]));
        let server = ServerAddr { host: "irc.test".into(), port: 6667, tls: false, accept_invalid_certs: false };
        self.app.on_net_event(NetEvent::Connected { id: self.net, server, tls: None }, self.now);
        self.sent()
    }

    fn register(&mut self) -> Vec<String> {
        self.connect();
        self.lines(&[
            ":srv 001 me :Welcome",
            ":srv 005 me CHANTYPES=# PREFIX=(ov)@+ :are supported",
            ":srv 376 me :End of MOTD",
        ]);
        self.sent()
    }

    fn lines(&mut self, lines: &[&str]) {
        for l in lines {
            self.now += 10;
            let msg = Message::parse(l).unwrap();
            self.app.on_net_event(NetEvent::Line { id: self.net, msg }, self.now);
        }
    }

    fn sent(&mut self) -> Vec<String> {
        self.app
            .take_net_commands()
            .into_iter()
            .filter_map(|c| match c {
                NetCommand::Send(_, m) => Some(m.to_line()),
                _ => None,
            })
            .collect()
    }

    fn buffer(&self, name: &str) -> schwaetz_core::BufferId {
        self.app.find_buffer(self.net, name).unwrap_or_else(|| panic!("no buffer {name}"))
    }
}

#[test]
fn connect_register_autojoin_and_chat() {
    let mut h = Harness::new(NetworkKind::Irc, &["#chan", "#keyed secret"]);
    let reg = h.connect();
    assert_eq!(
        reg,
        ["CAP LS 302", "NICK me", "USER me 0 * :schwätz user"]
            .map(|s| s.replace("USER me", &format!("USER {}", h.app.config.general.username)))
    );
    h.lines(&[":srv CAP * LS :server-time", ":srv CAP * ACK :server-time"]);
    h.sent();
    h.lines(&[":srv 001 me :Welcome", ":srv 376 me :End"]);
    assert_eq!(h.sent(), ["JOIN #keyed,#chan secret"]);
    assert_eq!(h.app.networks[&h.net].conn, ConnState::Ready);

    h.lines(&[":me!u@host JOIN #chan", ":srv 353 me = #chan :@op me", ":srv 366 me #chan :End"]);
    let chan = h.buffer("#chan");
    assert!(h.app.buffer(chan).unwrap().joined);

    h.lines(&[":op!o@h PRIVMSG #chan :hello everyone", ":op!o@h PRIVMSG #chan :me: are you there?"]);
    let b = h.app.buffer(chan).unwrap();
    let msgs: Vec<_> = b.lines.iter().filter(|l| l.kind == LineKind::Message).collect();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].prefix, Some('@'));
    assert!(msgs[1].flags.has(LineFlags::HIGHLIGHT));
    assert_eq!(b.activity, Activity::Highlight);
    assert_eq!(b.highlights, 1);
    assert!(h.app.take_effects().iter().any(|e| matches!(e, Effect::Notify { title, .. } if title == "op in #chan")));

    // Typing in the channel sends PRIVMSG and echoes locally.
    h.app.input(chan, "hi op");
    assert_eq!(h.sent(), ["PRIVMSG #chan :hi op"]);
    let last = h.app.buffer(chan).unwrap().lines.back().unwrap().clone();
    assert!(last.flags.has(LineFlags::OWN));
    assert_eq!(&*last.text, "hi op");
}

#[test]
fn join_command_switches_buffer_and_queries_open() {
    let mut h = Harness::new(NetworkKind::Irc, &[]);
    h.register();
    let sb = h.app.networks[&h.net].server_buffer;
    h.app.switch_to(sb);
    h.app.input(sb, "/join rust");
    assert_eq!(h.sent(), ["JOIN #rust"]);
    h.lines(&[":me!u@h JOIN #rust"]);
    assert_eq!(h.app.active, h.buffer("#rust"));

    h.lines(&[":friend!f@h PRIVMSG me :psst"]);
    let q = h.buffer("friend");
    assert_eq!(h.app.buffer(q).unwrap().kind, BufferKind::Query);
    assert_eq!(h.app.buffer(q).unwrap().activity, Activity::Highlight);

    // Private notices without a query go to the current context.
    h.lines(&[":NickServ!s@services NOTICE me :This nickname is registered"]);
    assert!(h.app.find_buffer(h.net, "NickServ").is_none());
}

#[test]
fn smart_filter_hides_inactive_users() {
    let mut h = Harness::new(NetworkKind::Irc, &[]);
    h.app.networks.get_mut(&h.net).unwrap().cfg.joins_parts = Some("smart".into());
    h.register();
    h.lines(&[
        ":me!u@h JOIN #c",
        ":lurker!l@h JOIN #c",
        ":talker!t@h JOIN #c",
        ":talker!t@h PRIVMSG #c :hi",
        ":talker!t@h PART #c",
        ":lurker!l@h QUIT :bye",
    ]);
    let b = h.app.buffer(h.buffer("#c")).unwrap();
    let find = |kind: LineKind, nick: &str| b.lines.iter().find(|l| l.kind == kind && &*l.nick == nick).unwrap().flags;
    assert!(find(LineKind::Join, "lurker").has(LineFlags::FILTERED));
    assert!(!find(LineKind::Part, "talker").has(LineFlags::FILTERED));
    assert!(find(LineKind::Quit, "lurker").has(LineFlags::FILTERED));
}

#[test]
fn joins_parts_default_to_hidden_on_twitch_only() {
    let mut h = Harness::new(NetworkKind::Irc, &[]);
    h.register();
    h.lines(&[":me!u@h JOIN #c", ":lurker!l@h JOIN #c"]);
    let b = h.app.buffer(h.buffer("#c")).unwrap();
    assert!(
        b.lines.iter().any(|l| l.kind == LineKind::Join && &*l.nick == "lurker" && !l.flags.has(LineFlags::FILTERED))
    );

    let mut h = Harness::new(NetworkKind::Twitch, &[]);
    h.connect();
    h.lines(&[
        ":tmi.twitch.tv 001 me :hi",
        ":tmi.twitch.tv 376 me :>",
        ":me!me@me.tmi.twitch.tv JOIN #chan",
        ":bob!bob@bob.tmi.twitch.tv JOIN #chan",
    ]);
    let b = h.app.buffer(h.buffer("#chan")).unwrap();
    assert!(b.lines.iter().any(|l| l.kind == LineKind::Join && &*l.nick == "bob" && l.flags.has(LineFlags::FILTERED)));
}

#[test]
fn netsplit_quits_are_folded() {
    let mut h = Harness::new(NetworkKind::Irc, &[]);
    h.register();
    h.lines(&[
        ":me!u@h JOIN #c",
        ":a!a@h JOIN #c",
        ":b!b@h JOIN #c",
        ":a!a@h QUIT :hub.example.net leaf.example.net",
        ":b!b@h QUIT :hub.example.net leaf.example.net",
    ]);
    let b = h.app.buffer(h.buffer("#c")).unwrap();
    let splits: Vec<_> = b.lines.iter().filter(|l| l.kind == LineKind::Netsplit).collect();
    assert_eq!(splits.len(), 1);
    assert!(splits[0].text.ends_with("a, b"), "{}", splits[0].text);
}

#[test]
fn disconnect_marks_channels_and_reconnect_rejoins() {
    let mut h = Harness::new(NetworkKind::Irc, &[]);
    h.register();
    h.lines(&[":me!u@h JOIN #c"]);
    h.app.on_net_event(
        NetEvent::Disconnected { id: h.net, reason: "Ping timeout".into(), retry_in: Some(Duration::from_secs(4)) },
        h.now,
    );
    let c = h.buffer("#c");
    assert!(!h.app.buffer(c).unwrap().joined);
    assert!(h.app.buffer(c).unwrap().lines.back().unwrap().text.contains("Reconnecting in 4s"));
    assert_eq!(h.app.networks[&h.net].conn, ConnState::Connecting);
    let rejoin = h.register();
    assert_eq!(rejoin, ["JOIN #c"]);
}

#[test]
fn history_gap_fill_dedupes() {
    let mut h = Harness::new(NetworkKind::Irc, &[]);
    h.connect();
    h.lines(&[
        ":srv CAP * LS :batch server-time draft/chathistory",
        ":srv CAP * ACK :batch server-time draft/chathistory",
    ]);
    h.lines(&[":srv 001 me :hi", ":srv 376 me :end"]);
    h.sent();
    h.lines(&[":me!u@h JOIN #c"]);
    assert_eq!(h.sent(), ["CHATHISTORY LATEST #c * 100"]);
    h.lines(&[
        ":srv BATCH +1 chathistory #c",
        "@batch=1;msgid=a;time=2023-11-14T22:00:00.000Z :x!x@h PRIVMSG #c :old one",
        "@batch=1;msgid=b;time=2023-11-14T22:00:01.000Z :x!x@h PRIVMSG #c :old two",
        ":srv BATCH -1",
        // The same message again (e.g. overlapping gap-fill) is dropped.
        ":srv BATCH +2 chathistory #c",
        "@batch=2;msgid=b;time=2023-11-14T22:00:01.000Z :x!x@h PRIVMSG #c :old two",
        ":srv BATCH -2",
    ]);
    let b = h.app.buffer(h.buffer("#c")).unwrap();
    let msgs: Vec<_> = b.lines.iter().filter(|l| l.kind == LineKind::Message).collect();
    assert_eq!(msgs.len(), 2);
    assert!(msgs.iter().all(|l| l.flags.has(LineFlags::HISTORY)));
    // History never notifies.
    assert!(!h.app.take_effects().iter().any(|e| matches!(e, Effect::Notify { .. })));
}

#[test]
fn notifications_switch_globally_and_per_network() {
    assert!(!Config::default().notifications.enabled);
    let mut h = Harness::new(NetworkKind::Irc, &["#c"]);
    h.register();
    h.lines(&[":me!u@h JOIN #c"]);
    h.app.take_effects();
    let notified = |h: &mut Harness| h.app.take_effects().iter().any(|e| matches!(e, Effect::Notify { .. }));

    h.lines(&[":x!x@h PRIVMSG #c :me: one"]);
    assert!(notified(&mut h));

    h.app.config.notifications.enabled = false;
    h.lines(&[":x!x@h PRIVMSG #c :me: two"]);
    assert!(!notified(&mut h));

    h.app.config.notifications.enabled = true;
    h.app.networks.get_mut(&h.net).unwrap().cfg.notifications = false;
    h.lines(&[":x!x@h PRIVMSG #c :me: three"]);
    let effects = h.app.take_effects();
    assert!(!effects.iter().any(|e| matches!(e, Effect::Notify { .. })));
    // The taskbar still flashes.
    assert!(effects.iter().any(|e| matches!(e, Effect::FlashTaskbar)));
}

#[test]
fn commands_and_aliases() {
    let mut h = Harness::new(NetworkKind::Irc, &[]);
    h.register();
    h.lines(&[":me!u@h JOIN #c", ":srv 353 me = #c :me bob", ":srv 366 me #c :End", ":bob!b@bobhost JOIN #c"]);
    let c = h.buffer("#c");
    h.sent();
    h.app.input(c, "/op bob");
    h.app.input(c, "/kickban bob spam");
    h.app.input(c, "/me waves");
    h.app.input(c, "/topic new topic");
    h.app.input(c, "/knock #other please");
    assert_eq!(
        h.sent(),
        [
            "MODE #c +o bob",
            "MODE #c +b *!*@bobhost",
            "KICK #c bob spam",
            "PRIVMSG #c :\u{1}ACTION waves\u{1}",
            "TOPIC #c :new topic",
            "KNOCK #other please",
        ]
    );
    h.app.input(c, "/alias greet /msg $1 hello $2-");
    h.app.input(c, "/greet bob how are you");
    assert_eq!(h.sent(), ["PRIVMSG bob :hello how are you"]);
    h.app.input(c, "//not a command");
    assert_eq!(h.sent(), ["PRIVMSG #c :/not a command"]);
}

#[test]
fn set_command_edits_config() {
    let mut h = Harness::new(NetworkKind::Irc, &[]);
    let s = h.app.status_buffer;
    h.app.input(s, "/set appearance.font_size 15.5");
    h.app.input(s, "/set general.smart_filter_secs 60");
    h.app.input(s, "/set notifications.sound yes");
    h.app.input(s, "/set highlight.words rust, irc");
    assert_eq!(h.app.config.appearance.font_size, 15.5);
    assert_eq!(h.app.config.general.smart_filter_secs, 60);
    assert!(h.app.config.notifications.sound);
    assert_eq!(h.app.config.highlight.words, ["rust", "irc"]);
    h.app.input(s, "/set nope.nothing 1");
    let last = h.app.buffer(s).unwrap().lines.back().unwrap().clone();
    assert_eq!(last.kind, LineKind::Error);
}

#[test]
fn paste_confirmation() {
    let mut h = Harness::new(NetworkKind::Irc, &[]);
    h.register();
    h.lines(&[":me!u@h JOIN #c"]);
    let c = h.buffer("#c");
    h.sent();
    h.app.input(c, "1\n2\n3\n4\n5");
    assert!(h.sent().is_empty());
    let eff = h.app.take_effects();
    let Some(Effect::ConfirmPaste { text, lines, .. }) = eff.iter().find(|e| matches!(e, Effect::ConfirmPaste { .. }))
    else {
        panic!()
    };
    assert_eq!(*lines, 5);
    h.app.input_confirmed(c, &text.clone());
    assert_eq!(h.sent().len(), 5);
}

#[test]
fn znc_network_import() {
    let mut h = Harness::new(NetworkKind::Znc, &[]);
    h.register();
    let sb = h.app.networks[&h.net].server_buffer;
    h.app.input(sb, "/znc import");
    assert_eq!(h.sent(), ["PRIVMSG *status ListNetworks"]);
    h.lines(&[
        ":*status!znc@znc.in PRIVMSG me :+--------+-------+",
        ":*status!znc@znc.in PRIVMSG me :| Network | OnIRC |",
        ":*status!znc@znc.in PRIVMSG me :+--------+-------+",
        ":*status!znc@znc.in PRIVMSG me :| libera | Yes |",
        ":*status!znc@znc.in PRIVMSG me :| oftc | No |",
        ":*status!znc@znc.in PRIVMSG me :+--------+-------+",
    ]);
    let eff = h.app.take_effects();
    assert!(eff.iter().any(
        |e| matches!(e, Effect::ZncNetworks { names, .. } if names == &vec!["libera".to_string(), "oftc".to_string()])
    ));
}

#[test]
fn twitch_moderation_marks_lines() {
    let mut h = Harness::new(NetworkKind::Twitch, &[]);
    h.connect();
    h.lines(&[
        ":tmi.twitch.tv CAP * ACK :twitch.tv/tags twitch.tv/commands twitch.tv/membership",
        ":tmi.twitch.tv 001 me :Welcome",
        ":tmi.twitch.tv 376 me :>",
    ]);
    h.lines(&[
        ":me!me@me.tmi.twitch.tv JOIN #streamer",
        "@display-name=Troll;color=#FF0000;id=m1 :troll!troll@troll.tmi.twitch.tv PRIVMSG #streamer :spam spam",
        "@display-name=Nice;id=m2;emotes=25:0-4 :nice!nice@nice.tmi.twitch.tv PRIVMSG #streamer :Kappa hello",
        "@ban-duration=60 :tmi.twitch.tv CLEARCHAT #streamer :troll",
        "@slow=30 :tmi.twitch.tv ROOMSTATE #streamer",
    ]);
    let b = h.app.buffer(h.buffer("#streamer")).unwrap();
    let troll = b.lines.iter().find(|l| &*l.nick == "troll").unwrap();
    assert!(troll.flags.has(LineFlags::DELETED));
    assert_eq!(troll.extra.as_ref().unwrap().color, Some(0xff0000));
    let nice = b.lines.iter().find(|l| &*l.nick == "nice").unwrap();
    let emotes = &nice.extra.as_ref().unwrap().emotes;
    assert_eq!(emotes.len(), 1);
    assert!(emotes[0].url.contains("/25/"));
    assert!(b.lines.iter().any(|l| l.kind == LineKind::System && l.text.contains("timed out for 1m 0s")));
    let (_, topic) = h.app.topic_for(h.buffer("#streamer"));
    assert!(topic.contains("slow 30s"));
}

#[test]
fn ctcp_version_reply_rate_limited() {
    let mut h = Harness::new(NetworkKind::Irc, &[]);
    h.register();
    for _ in 0..6 {
        h.lines(&[":curious!c@h PRIVMSG me :\u{1}VERSION\u{1}"]);
    }
    let replies = h.sent();
    assert_eq!(replies.len(), 4);
    assert!(replies[0].starts_with("NOTICE curious :\u{1}VERSION schwätz"));
}

#[test]
fn ignored_users_are_dropped() {
    let mut h = Harness::new(NetworkKind::Irc, &[]);
    h.register();
    let s = h.app.status_buffer;
    h.app.input(s, "/ignore spammer msg");
    h.lines(&[":me!u@h JOIN #c", ":spammer!s@h PRIVMSG #c :buy now", ":spammer!s@h NOTICE #c :notice passes"]);
    let b = h.app.buffer(h.buffer("#c")).unwrap();
    assert!(!b.lines.iter().any(|l| l.text.contains("buy now")));
    assert!(b.lines.iter().any(|l| l.text.contains("notice passes")));
}

#[test]
fn joined_channels_are_remembered() {
    let mut h = Harness::new(NetworkKind::Irc, &["#old"]);
    h.register();
    h.lines(&[":me!u@h JOIN #old", ":me!u@h JOIN #new", ":srv 324 me #new +k pw", ":me!u@h PART #old"]);
    let cfg = &h.app.config.networks[0];
    assert_eq!(cfg.autojoin, ["#new"]);
    assert!(h.app.take_effects().iter().any(|e| matches!(e, Effect::SaveConfig)));
    // Disabled: nothing changes.
    h.app.config.general.remember_channels = false;
    h.lines(&[":me!u@h JOIN #third"]);
    assert_eq!(h.app.config.networks[0].autojoin, ["#new"]);
}

#[test]
fn twitch_own_lines_carry_the_badges_of_their_channel() {
    let mut h = Harness::new(NetworkKind::Twitch, &[]);
    h.connect();
    h.lines(&[
        ":tmi.twitch.tv CAP * ACK :twitch.tv/tags twitch.tv/commands",
        ":tmi.twitch.tv 001 me :hi",
        ":tmi.twitch.tv 376 me :>",
        "@badges=premium/1;display-name=Me :tmi.twitch.tv GLOBALUSERSTATE",
        ":me!me@me.tmi.twitch.tv JOIN #mine",
        "@badges=broadcaster/1,premium/1;display-name=Me :tmi.twitch.tv USERSTATE #mine",
        ":me!me@me.tmi.twitch.tv JOIN #other",
        "@badges=subscriber/6;badge-info=subscriber/8;display-name=Me :tmi.twitch.tv USERSTATE #other",
        ":me!me@me.tmi.twitch.tv JOIN #fresh",
    ]);
    let badges = |h: &mut Harness, chan: &str| {
        let b = h.buffer(chan);
        h.app.input(b, "hi");
        h.app.buffer(b).unwrap().lines.back().unwrap().extra.clone().unwrap().badges
    };
    assert_eq!(badges(&mut h, "#mine"), ["broadcaster/1", "premium/1"]);
    assert_eq!(badges(&mut h, "#other"), ["subscriber/6"]);
    // Still the right ones after writing elsewhere, and the global ones before a USERSTATE.
    assert_eq!(badges(&mut h, "#mine"), ["broadcaster/1", "premium/1"]);
    assert_eq!(badges(&mut h, "#fresh"), ["premium/1"]);
}

#[test]
fn replies_use_the_right_tag_and_show_context() {
    // Twitch: reply-parent-msg-id, local echo resolves the parent.
    let mut h = Harness::new(NetworkKind::Twitch, &[]);
    h.connect();
    h.lines(&[
        ":tmi.twitch.tv CAP * ACK :twitch.tv/tags twitch.tv/commands",
        ":tmi.twitch.tv 001 me :hi",
        ":tmi.twitch.tv 376 me :>",
    ]);
    h.lines(&[
        ":me!me@me.tmi.twitch.tv JOIN #chan",
        "@id=abc;display-name=Alice :alice!alice@alice.tmi.twitch.tv PRIVMSG #chan :original words",
    ]);
    let c = h.buffer("#chan");
    assert!(h.app.can_reply(c));
    h.sent();
    h.app.reply(c, "abc", "my answer");
    assert_eq!(h.sent(), ["@reply-parent-msg-id=abc PRIVMSG #chan :my answer"]);
    let own = h.app.buffer(c).unwrap().lines.back().unwrap().clone();
    let (parent, nick, text) = own.extra.unwrap().reply_to.unwrap();
    assert_eq!((parent.as_str(), nick.as_str(), text.as_str()), ("abc", "Alice", "original words"));

    // IRC with message-tags: +draft/reply.
    let mut h = Harness::new(NetworkKind::Irc, &[]);
    h.connect();
    h.lines(&[":srv CAP * LS :message-tags", ":srv CAP * ACK :message-tags"]);
    h.lines(&[":srv 001 me :hi", ":srv 376 me :end", ":me!u@h JOIN #c"]);
    let c = h.buffer("#c");
    h.sent();
    h.app.reply(c, "m1", "sure");
    assert_eq!(h.sent(), ["@+draft/reply=m1 PRIVMSG #c sure"]);
}

#[test]
fn twitch_hides_hostmasks_and_has_no_queries() {
    let mut h = Harness::new(NetworkKind::Twitch, &[]);
    h.app.networks.get_mut(&h.net).unwrap().cfg.joins_parts = Some("all".into());
    h.connect();
    h.lines(&[
        ":tmi.twitch.tv CAP * ACK :twitch.tv/tags twitch.tv/commands twitch.tv/membership",
        ":tmi.twitch.tv 001 me :hi",
        ":tmi.twitch.tv 376 me :>",
        ":me!me@me.tmi.twitch.tv JOIN #chan",
        ":bob!bob@bob.tmi.twitch.tv JOIN #chan",
        ":bob!bob@bob.tmi.twitch.tv PART #chan",
    ]);
    let c = h.buffer("#chan");
    let texts: Vec<String> = h.app.buffer(c).unwrap().lines.iter().map(|l| l.text.to_string()).collect();
    assert!(texts.iter().any(|t| t == "bob has joined"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "bob has left #chan"), "{texts:?}");
    assert_eq!(h.app.twitch_profile_url(c, "Bob").as_deref(), Some("https://www.twitch.tv/bob"));

    let before = h.app.buffers().len();
    h.app.input(c, "/query bob");
    assert_eq!(h.app.buffers().len(), before, "no query buffer on Twitch");

    // Regular IRC keeps the mask.
    let mut h = Harness::new(NetworkKind::Irc, &[]);
    h.connect();
    h.lines(&[":srv 001 me :hi", ":srv 376 me :end", ":me!u@h JOIN #c", ":bob!b@example.org JOIN #c"]);
    let c = h.buffer("#c");
    assert!(h.app.buffer(c).unwrap().lines.iter().any(|l| &*l.text == "bob (b@example.org) has joined"));
    assert_eq!(h.app.twitch_profile_url(c, "bob"), None);
}

fn live_requests(app: &mut App) -> Vec<Vec<String>> {
    app.take_effects()
        .into_iter()
        .filter_map(|e| match e {
            Effect::TwitchLive(r) => Some(r.logins),
            _ => None,
        })
        .collect()
}

fn live_result(h: &Harness, statuses: &[(&str, bool, &str)]) -> schwaetz_core::helix::LiveResult {
    schwaetz_core::helix::LiveResult {
        network: h.net,
        client_id: Some("cid".into()),
        ids: Default::default(),
        unauthorized: false,
        result: Ok(statuses
            .iter()
            .map(|(login, live, title)| {
                let info = schwaetz_core::helix::StreamInfo {
                    live: *live,
                    title: title.to_string(),
                    game: "Just Chatting".into(),
                    viewers: if *live { 1234 } else { 0 },
                    ..Default::default()
                };
                (login.to_string(), info)
            })
            .collect()),
    }
}

#[test]
fn twitch_live_checks_follow_autojoin_manual_joins_and_interval() {
    let mut h = Harness::new(NetworkKind::Twitch, &["#alpha", "#beta"]);
    h.app.set_twitch_api_token(h.net, Some("oauth:tok".into()));
    h.connect();
    h.lines(&[
        ":tmi.twitch.tv CAP * ACK :twitch.tv/tags twitch.tv/commands",
        ":tmi.twitch.tv 001 me :hi",
        ":tmi.twitch.tv 376 me :>",
    ]);
    assert!(live_requests(&mut h.app).is_empty(), "waits for the autojoin channels");

    // The first check runs once every autojoin channel is joined, for all of them.
    h.lines(&[":me!me@me.tmi.twitch.tv JOIN #alpha"]);
    assert!(live_requests(&mut h.app).is_empty());
    h.lines(&[":me!me@me.tmi.twitch.tv JOIN #beta"]);
    assert_eq!(live_requests(&mut h.app), [vec!["alpha".to_string(), "beta".to_string()]]);

    // A manual join while that check runs is queued, then checked on its own.
    h.lines(&[":me!me@me.tmi.twitch.tv JOIN #gamma"]);
    assert!(live_requests(&mut h.app).is_empty());
    let r = live_result(&h, &[("alpha", true, "hello chat"), ("beta", false, "old title")]);
    h.app.on_live_result(r);
    assert_eq!(live_requests(&mut h.app), [vec!["gamma".to_string()]]);

    // Title and game show where a topic would be, and the first result is printed like one.
    let alpha = h.buffer("#alpha");
    assert_eq!(h.app.topic_for(alpha).1, "🔴 Live · Just Chatting — hello chat · 1,234 viewers");
    assert_eq!(h.app.topic_for(h.buffer("#beta")).1, "Offline · Just Chatting — old title");
    let last = |h: &Harness, b| h.app.buffer(b).unwrap().lines.back().unwrap().text.to_string();
    assert_eq!(last(&h, alpha), "Stream: 🔴 Live · Just Chatting — hello chat · 1,234 viewers");

    // A manual join when idle is checked immediately.
    h.app.on_live_result(live_result(&h, &[("gamma", false, "")]));
    h.lines(&[":me!me@me.tmi.twitch.tv JOIN #delta"]);
    assert_eq!(live_requests(&mut h.app), [vec!["delta".to_string()]]);
    h.app.on_live_result(live_result(&h, &[]));

    // Periodic re-check of every joined channel after the configured interval (default 120 s).
    h.app.tick(h.now + 60_000);
    assert!(live_requests(&mut h.app).is_empty());
    h.app.tick(h.now + 121_000);
    assert_eq!(live_requests(&mut h.app).len(), 1);

    // Going offline is announced; viewer-count changes alone are not.
    h.app.on_live_result(live_result(&h, &[("alpha", false, "hello chat")]));
    assert_eq!(last(&h, alpha), "alpha went offline");

    // Errors are reported once.
    let err = |h: &Harness| schwaetz_core::helix::LiveResult {
        network: h.net,
        client_id: None,
        ids: Default::default(),
        result: Err("the API token is invalid or expired".into()),
        unauthorized: true,
    };
    h.app.tick(h.now + 400_000);
    h.app.on_live_result(err(&h));
    h.app.tick(h.now + 600_000);
    h.app.on_live_result(err(&h));
    let sb = h.app.network(h.net).unwrap().server_buffer;
    let n = h.app.buffer(sb).unwrap().lines.iter().filter(|l| l.text.contains("Twitch API")).count();
    assert_eq!(n, 1);
}

#[test]
fn twitch_without_api_token_never_checks() {
    let mut h = Harness::new(NetworkKind::Twitch, &["#alpha"]);
    h.app.set_twitch_api_token(h.net, Some(String::new()));
    h.connect();
    h.lines(&[":tmi.twitch.tv 001 me :hi", ":tmi.twitch.tv 376 me :>", ":me!me@me.tmi.twitch.tv JOIN #alpha"]);
    h.app.tick(h.now + 1_000_000);
    assert!(live_requests(&mut h.app).is_empty());
}

#[test]
fn twitch_sign_in_feeds_live_checks_and_refreshes_on_401() {
    use schwaetz_core::twitch_auth::{AuthRequest, AuthResponse};
    let mut h = Harness::new(NetworkKind::Twitch, &["#alpha"]);
    h.app.use_memory_secrets();
    h.app.networks.get_mut(&h.net).unwrap().cfg.twitch_client_id = Some("cid".into());
    h.app.reload_twitch_auth(h.net);
    h.connect();
    h.lines(&[":tmi.twitch.tv 001 me :hi", ":tmi.twitch.tv 376 me :>", ":me!me@me.tmi.twitch.tv JOIN #alpha"]);
    assert!(live_requests(&mut h.app).is_empty(), "no token, no checks");

    let auth_requests = |app: &mut App| -> Vec<AuthRequest> {
        app.take_effects()
            .into_iter()
            .filter_map(|e| match e {
                Effect::TwitchAuth { request, .. } => Some(request),
                _ => None,
            })
            .collect()
    };

    // Sign in: device code → browser → poll → tokens.
    h.app.twitch_sign_in(h.net);
    let start = auth_requests(&mut h.app).remove(0);
    assert_eq!(start, AuthRequest::StartDevice { client_id: "cid".into() });
    let device = AuthResponse::Device {
        device_code: "dc".into(),
        user_code: "ABCDEFGH".into(),
        verification_uri: "https://www.twitch.tv/activate?public=true&device-code=ABCDEFGH".into(),
        expires_in: 1800,
        interval: 5,
    };
    h.app.on_auth_result(h.net, start, device);
    assert!(h.app.take_effects().iter().any(|e| matches!(e, Effect::OpenUrl(u) if u.contains("ABCDEFGH"))));
    assert_eq!(h.app.twitch_auth(h.net).unwrap().user_code(), Some("ABCDEFGH"));
    h.app.tick(h.now + 6_000);
    let poll = auth_requests(&mut h.app).remove(0);
    let tokens = AuthResponse::Tokens {
        access: "acc1".into(),
        refresh: "ref1".into(),
        expires_in: 14_400,
        login: "alice".into(),
    };
    h.app.on_auth_result(h.net, poll, tokens);
    assert_eq!(h.app.twitch_auth(h.net).unwrap().login(), Some("alice"));

    // Signing in starts live checks with the new token and the app's client ID.
    let reqs: Vec<_> = h
        .app
        .take_effects()
        .into_iter()
        .filter_map(|e| match e {
            Effect::TwitchLive(r) => Some(r),
            _ => None,
        })
        .collect();
    assert_eq!(reqs.len(), 1);
    assert_eq!((reqs[0].token.as_str(), reqs[0].client_id.as_deref()), ("acc1", Some("cid")));
    assert_eq!(reqs[0].logins, ["alpha"]);

    // The tokens survive a restart (stored, then loaded again).
    h.app.reload_twitch_auth(h.net);
    assert_eq!(h.app.twitch_auth(h.net).unwrap().access_token(), Some("acc1"));

    // A 401 from the API refreshes right away; the new token is used for the next check.
    let unauthorized = schwaetz_core::helix::LiveResult {
        network: h.net,
        client_id: None,
        ids: Default::default(),
        result: Err("the API token is invalid or expired".into()),
        unauthorized: true,
    };
    h.app.on_live_result(unauthorized);
    h.app.tick(h.now + 7_000);
    let refresh = auth_requests(&mut h.app).remove(0);
    assert_eq!(refresh, AuthRequest::Refresh { client_id: "cid".into(), refresh_token: "ref1".into() });
    let tokens = AuthResponse::Tokens {
        access: "acc2".into(),
        refresh: "ref2".into(),
        expires_in: 14_400,
        login: String::new(),
    };
    h.app.on_auth_result(h.net, refresh, tokens);
    let next: Vec<_> = h
        .app
        .take_effects()
        .into_iter()
        .filter_map(|e| match e {
            Effect::TwitchLive(r) => Some(r.token),
            _ => None,
        })
        .collect();
    assert_eq!(next, ["acc2"]);
    assert_eq!(h.app.twitch_auth(h.net).unwrap().login(), Some("alice"), "login kept across refreshes");

    // Signing out revokes and forgets the tokens.
    h.app.twitch_sign_out(h.net);
    assert!(matches!(&auth_requests(&mut h.app)[..], [AuthRequest::Revoke { token, .. }] if token == "acc2"));
    h.app.reload_twitch_auth(h.net);
    assert_eq!(h.app.twitch_auth(h.net).unwrap().access_token(), None);
}

#[test]
fn join_on_connect_list_follows_manual_joins_and_parts() {
    let mut h = Harness::new(NetworkKind::Irc, &["#keep"]);
    h.register();
    h.lines(&[":me!u@h JOIN #keep"]);
    let list = |h: &Harness| h.app.config.networks[0].autojoin.clone();

    // Joining adds; the confirmation of a /part removes (and /part removes right away).
    h.app.input(h.buffer("#keep"), "/join #new");
    h.lines(&[":me!u@h JOIN #new"]);
    assert_eq!(list(&h), ["#keep", "#new"]);
    let new = h.buffer("#new");
    h.app.input(new, "/part");
    assert_eq!(list(&h), ["#keep"]);
    h.lines(&[":me!u@h PART #new"]);
    assert_eq!(list(&h), ["#keep"]);

    // Closing a joined channel removes it, even though its buffer is gone before the echo.
    h.lines(&[":me!u@h JOIN #closeme"]);
    assert_eq!(list(&h), ["#keep", "#closeme"]);
    h.app.input(h.buffer("#closeme"), "/close");
    h.lines(&[":me!u@h PART #closeme"]);
    assert_eq!(list(&h), ["#keep"]);

    // A kick is not the user leaving; closing the buffer afterwards is.
    h.lines(&[":me!u@h JOIN #kicked", ":op!o@h KICK #kicked me :bye"]);
    assert_eq!(list(&h), ["#keep", "#kicked"]);
    h.app.input(h.buffer("#kicked"), "/close");
    assert_eq!(list(&h), ["#keep"]);

    // /cycle parts and rejoins: the channel stays.
    h.app.input(h.buffer("#keep"), "/cycle");
    h.lines(&[":me!u@h PART #keep", ":me!u@h JOIN #keep"]);
    assert_eq!(list(&h), ["#keep"]);
    assert!(h.app.take_effects().iter().any(|e| matches!(e, Effect::SaveConfig)), "changes are saved");
}

#[test]
fn twitch_stream_link_follows_the_popout_setting() {
    let mut h = Harness::new(NetworkKind::Twitch, &[]);
    h.connect();
    h.lines(&[":tmi.twitch.tv 001 me :hi", ":tmi.twitch.tv 376 me :>", ":me!me@me.tmi.twitch.tv JOIN #SomeStreamer"]);
    let c = h.buffer("#SomeStreamer");
    assert_eq!(h.app.twitch_stream_url(c).as_deref(), Some("https://www.twitch.tv/somestreamer"));
    h.app.networks.get_mut(&h.net).unwrap().cfg.twitch_popout = true;
    assert_eq!(
        h.app.twitch_stream_url(c).as_deref(),
        Some("https://player.twitch.tv/?channel=somestreamer&parent=twitch.tv&player=popout")
    );
    // Only Twitch channels have a stream.
    let server = h.app.network(h.net).unwrap().server_buffer;
    assert_eq!(h.app.twitch_stream_url(server), None);
    let mut irc = Harness::new(NetworkKind::Irc, &[]);
    irc.register();
    irc.lines(&[":me!u@h JOIN #chan"]);
    assert_eq!(irc.app.twitch_stream_url(irc.buffer("#chan")), None);
}

fn channel_names(h: &Harness) -> Vec<String> {
    h.app
        .sidebar_order()
        .into_iter()
        .filter_map(|id| h.app.buffer(id))
        .filter(|b| b.kind == BufferKind::Channel)
        .map(|b| b.name.clone())
        .collect()
}

#[test]
fn channels_can_be_arranged_and_twitch_can_list_live_first() {
    let mut h = Harness::new(NetworkKind::Twitch, &[]);
    h.connect();
    h.lines(&[":tmi.twitch.tv 001 me :hi", ":tmi.twitch.tv 376 me :>"]);
    for c in ["#delta", "#alpha", "#charlie", "#bravo"] {
        h.lines(&[&format!(":me!me@me.tmi.twitch.tv JOIN {c}")]);
    }
    assert_eq!(channel_names(&h), ["#alpha", "#bravo", "#charlie", "#delta"], "alphabetical until arranged");

    // Drag #delta above #bravo, then #alpha to the end.
    h.app.move_channel(h.buffer("#delta"), Some(h.buffer("#bravo")));
    assert_eq!(channel_names(&h), ["#alpha", "#delta", "#bravo", "#charlie"]);
    h.app.move_channel(h.buffer("#alpha"), None);
    assert_eq!(channel_names(&h), ["#delta", "#bravo", "#charlie", "#alpha"]);
    assert_eq!(h.app.config.networks[0].channel_order, ["#delta", "#bravo", "#charlie", "#alpha"], "saved");
    assert!(h.app.take_effects().iter().any(|e| matches!(e, Effect::SaveConfig)));

    // Channels joined later go after the arranged ones.
    h.lines(&[":me!me@me.tmi.twitch.tv JOIN #echo"]);
    assert_eq!(channel_names(&h), ["#delta", "#bravo", "#charlie", "#alpha", "#echo"]);

    // Live first: live channels, then offline ones, each in the arranged order.
    h.app.networks.get_mut(&h.net).unwrap().cfg.twitch_live_first = true;
    let live = |h: &Harness, logins: &[&str]| schwaetz_core::helix::LiveResult {
        network: h.net,
        client_id: None,
        ids: Default::default(),
        result: Ok(logins
            .iter()
            .map(|l| {
                let info = schwaetz_core::helix::StreamInfo { live: true, ..Default::default() };
                (l.to_string(), info)
            })
            .collect()),
        unauthorized: false,
    };
    h.app.on_live_result(live(&h, &["alpha", "charlie"]));
    assert_eq!(channel_names(&h), ["#charlie", "#alpha", "#delta", "#bravo", "#echo"]);
    // Arranging still works within the groups.
    h.app.move_channel(h.buffer("#alpha"), Some(h.buffer("#charlie")));
    assert_eq!(channel_names(&h), ["#alpha", "#charlie", "#delta", "#bravo", "#echo"]);
}

#[test]
fn networks_can_be_arranged() {
    let mut h = Harness::new(NetworkKind::Irc, &[]);
    for name in ["Second", "Third"] {
        h.app.upsert_network(
            None,
            NetworkConfig { name: name.into(), servers: vec!["irc.x:6667".into()], ..Default::default() },
        );
    }
    let names = |h: &Harness| -> Vec<String> { h.app.network_order().iter().map(|n| n.cfg.name.clone()).collect() };
    let server =
        |h: &Harness, name: &str| h.app.network_order().iter().find(|n| n.cfg.name == name).unwrap().server_buffer;
    assert_eq!(names(&h), ["Test", "Second", "Third"]);
    h.app.take_effects();

    // Drag Third above Test, then Test to the end.
    h.app.move_network(server(&h, "Third"), Some(server(&h, "Test")));
    assert_eq!(names(&h), ["Third", "Test", "Second"]);
    h.app.move_network(server(&h, "Test"), None);
    assert_eq!(names(&h), ["Third", "Second", "Test"]);
    let saved: Vec<&str> = h.app.config.networks.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(saved, ["Third", "Second", "Test"], "saved");
    assert!(h.app.take_effects().iter().any(|e| matches!(e, Effect::SaveConfig)));

    // The sidebar follows: each server buffer before its network's other buffers.
    let order = h.app.sidebar_order();
    let pos = |id| order.iter().position(|x| *x == id).unwrap();
    assert!(
        pos(server(&h, "Third")) < pos(server(&h, "Second")) && pos(server(&h, "Second")) < pos(server(&h, "Test"))
    );

    // Dropping a network where it already is changes nothing.
    h.app.move_network(server(&h, "Second"), Some(server(&h, "Test")));
    assert!(h.app.take_effects().iter().all(|e| !matches!(e, Effect::SaveConfig)));
}

#[test]
fn live_chat_lines_keep_their_raw_form() {
    let mut h = Harness::new(NetworkKind::Twitch, &[]);
    h.connect();
    h.lines(&[
        ":tmi.twitch.tv CAP * ACK :twitch.tv/tags twitch.tv/commands",
        ":tmi.twitch.tv 001 me :hi",
        ":tmi.twitch.tv 376 me :>",
        ":me!me@me.tmi.twitch.tv JOIN #chan",
        "@badge-info=;color=#FF4500;display-name=Alice;id=abc;tmi-sent-ts=1 :alice!alice@alice.tmi.twitch.tv PRIVMSG #chan :hello there",
        r"@msg-id=raid;system-msg=Bob\sis\sraiding :tmi.twitch.tv USERNOTICE #chan",
    ]);
    let b = h.app.buffer(h.buffer("#chan")).unwrap();
    let raws: Vec<String> =
        b.lines.iter().filter_map(|l| l.extra.as_ref()?.raw.as_deref().map(str::to_owned)).collect();
    assert_eq!(raws.len(), 2, "{raws:?}");
    let msg = schwaetz_proto::Message::parse(&raws[0]).unwrap();
    assert_eq!(msg.command, "PRIVMSG");
    assert_eq!(msg.tags.value("display-name"), Some("Alice"));
    assert_eq!(msg.tags.value("color"), Some("#FF4500"));
    assert!(raws[1].contains("USERNOTICE") && raws[1].contains(r"system-msg=Bob\sis\sraiding"));
    // Join and status lines have none.
    assert!(
        b.lines.iter().filter(|l| l.kind == LineKind::Join).all(|l| l.extra.as_ref().is_none_or(|e| e.raw.is_none()))
    );
}

#[test]
fn twitch_topic_keeps_status_game_and_viewers_apart_from_the_title() {
    let mut h = Harness::new(NetworkKind::Twitch, &[]);
    h.connect();
    h.lines(&[
        ":tmi.twitch.tv 001 me :hi",
        ":tmi.twitch.tv 376 me :>",
        ":me!me@me.tmi.twitch.tv JOIN #xqc",
        "@followers-only=10;room-id=1;slow=0;subs-only=0 :tmi.twitch.tv ROOMSTATE #xqc",
    ]);
    let info = schwaetz_core::helix::StreamInfo {
        live: true,
        title: "a long title".into(),
        game: "Just Chatting".into(),
        viewers: 45123,
        ..Default::default()
    };
    h.app.on_live_result(schwaetz_core::helix::LiveResult {
        network: h.net,
        client_id: None,
        ids: Default::default(),
        result: Ok(vec![("xqc".into(), info)]),
        unauthorized: false,
    });
    let t = h.app.topic_parts(h.buffer("#xqc"));
    assert!(t.lead.starts_with('[') && t.lead.ends_with("🔴 Live · Just Chatting — "), "{t:?}");
    assert_eq!(t.body, "a long title");
    assert_eq!(t.tail, " · 45,123 viewers");
    let (_, joined) = h.app.topic_for(h.buffer("#xqc"));
    assert_eq!(joined, format!("{}{}{}", t.lead, t.body, t.tail));
}

/// The emote fetches requested since the last call.
fn emote_requests(h: &mut Harness) -> Vec<schwaetz_core::emotes::EmoteRequest> {
    h.app
        .take_effects()
        .into_iter()
        .filter_map(|e| match e {
            Effect::FetchEmotes(r) => Some(r),
            _ => None,
        })
        .collect()
}

fn emote_jobs(h: &mut Harness) -> Vec<Vec<schwaetz_core::emotes::Job>> {
    emote_requests(h).into_iter().map(|r| r.jobs).collect()
}

fn emote_result(
    h: &Harness,
    connection: u64,
    sets: Vec<(schwaetz_core::emotes::SetKey, &[&str])>,
) -> schwaetz_core::emotes::EmoteResult {
    let sets = sets
        .into_iter()
        .map(|(key, names)| (key, names.iter().map(|n| (n.to_string(), format!("https://e/{n}"))).collect()))
        .collect();
    schwaetz_core::emotes::EmoteResult { network: h.net, connection, sets, twitch_limited: false }
}

#[test]
fn emotes_are_fetched_once_per_connection_and_ordered() {
    use schwaetz_core::emote_providers::Provider;
    use schwaetz_core::emotes::{Job, SetKey};
    let provider = |provider, room: Option<&str>| Job::Provider { provider, room_id: room.map(str::to_owned) };
    let mut h = Harness::new(NetworkKind::Twitch, &[]);
    h.app.set_twitch_api_token(h.net, Some("tok".into()));
    h.connect();
    h.lines(&[":tmi.twitch.tv 001 me :hi", ":tmi.twitch.tv 376 me :>"]);
    // On connecting: the user's Twitch emotes (the same in every channel) apart from the rest.
    let reqs = emote_requests(&mut h);
    let connection = reqs[0].connection;
    let jobs: Vec<_> = reqs.into_iter().map(|r| r.jobs).collect();
    assert_eq!(jobs, [vec![Job::TwitchUser], Provider::ALL.map(|p| provider(p, None)).to_vec()]);

    // A joined channel's provider sets are fetched right away (they show in its chat) …
    h.lines(&[":me!me@me.tmi.twitch.tv JOIN #xqc", "@room-id=71092938 :tmi.twitch.tv ROOMSTATE #xqc"]);
    assert_eq!(emote_jobs(&mut h), [Provider::ALL.map(|p| provider(p, Some("71092938"))).to_vec()]);
    // … its follower emotes when it is looked at, once per connection.
    let c = h.buffer("#xqc");
    h.app.switch_to(c);
    assert_eq!(emote_jobs(&mut h), [vec![Job::TwitchFollower { room_id: "71092938".into() }]]);
    h.app.switch_to(c);
    assert!(emote_jobs(&mut h).is_empty());

    let twitch = |owner: &str| {
        let emote_type = if owner == "0" { "globals" } else { "subscriptions" };
        SetKey::Twitch { owner: owner.into(), emote_type: emote_type.into() }
    };
    let r = emote_result(
        &h,
        connection,
        vec![
            (twitch("71092938"), &["xqcL"]),
            (twitch("0"), &["LUL"]),
            (twitch("123"), &["xqcOther"]),
            (SetKey::Provider { provider: Provider::SevenTv, room: Some("71092938".into()) }, &["LULW"]),
            (SetKey::Provider { provider: Provider::Bttv, room: None }, &["LULE"]),
            (SetKey::Provider { provider: Provider::SevenTv, room: Some("999".into()) }, &["LULother"]),
        ],
    );
    h.app.on_emote_result(r);
    h.app.set_script_emotes("mine", Some("#xqc"), vec![("Scripted".into(), "https://s/Scripted".into())]);

    let names =
        |h: &Harness, q: &str| h.app.emote_completions(c, q, 50).into_iter().map(|e| e.name).collect::<Vec<_>>();
    assert_eq!(names(&h, "lu"), ["LULW", "LUL", "LULE"], "7TV channel, then Twitch global, then BTTV global");
    assert_eq!(names(&h, "x"), ["xqcL", "xqcOther"], "the channel's Twitch emotes first");
    assert_eq!(h.app.emote_completions(c, "Scripted", 1)[0].label(), "mine");
    let labels = |q: &str| h.app.emote_completions(c, q, 50).into_iter().map(|e| e.label()).collect::<Vec<_>>();
    assert_eq!(labels("x"), ["Twitch · sub", "Twitch · sub"], "subscriber emotes, the channel's and others'");
    assert_eq!(labels("LUL")[1], "Twitch · global");
    // Only Twitch channels complete emotes.
    let server = h.app.network(h.net).unwrap().server_buffer;
    assert!(h.app.emote_completions(server, "lu", 50).is_empty());

    // In the chat: the providers' and scripts' sets; Twitch emotes in our own lines only.
    let l = h.app.emote_lookup(c);
    assert_eq!(l.get("LULW", false), Some("https://e/LULW"));
    assert_eq!(l.get("Scripted", false), Some("https://s/Scripted"));
    assert_eq!(l.get("LULother", false), None, "another channel's set");
    assert_eq!((l.get("xqcL", false), l.get("xqcL", true)), (None, Some("https://e/xqcL")));

    // A reconnect starts over; results from the old connection are dropped.
    h.app.on_net_event(NetEvent::Disconnected { id: h.net, reason: "gone".into(), retry_in: None }, h.now);
    assert!(names(&h, "x").is_empty());
    h.app.on_emote_result(emote_result(&h, connection, vec![(twitch("0"), &["stale"])]));
    assert!(names(&h, "stale").is_empty());
    h.connect();
    h.lines(&[":tmi.twitch.tv 001 me :hi", ":tmi.twitch.tv 376 me :>"]);
    let reqs = emote_requests(&mut h);
    assert!(reqs.iter().all(|r| r.connection != connection));
    assert_eq!(reqs[0].jobs, [Job::TwitchUser]);
    // Rejoining the channel being looked at fetches its sets and follower emotes again.
    h.lines(&[":me!me@me.tmi.twitch.tv JOIN #xqc", "@room-id=71092938 :tmi.twitch.tv ROOMSTATE #xqc"]);
    let jobs = emote_jobs(&mut h).concat();
    assert!(jobs.contains(&Job::TwitchFollower { room_id: "71092938".into() }) && jobs.len() == 4, "{jobs:?}");
}

#[test]
fn provider_emotes_follow_the_network_settings() {
    use schwaetz_core::emote_providers::Provider;
    use schwaetz_core::emotes::{Job, SetKey};
    // No API token: 7TV/FFZ/BTTV need none.
    let mut h = Harness::new(NetworkKind::Twitch, &[]);
    h.connect();
    h.lines(&[
        ":tmi.twitch.tv 001 me :hi",
        ":tmi.twitch.tv 376 me :>",
        ":me!me@me.tmi.twitch.tv JOIN #xqc",
        "@room-id=71092938 :tmi.twitch.tv ROOMSTATE #xqc",
    ]);
    let reqs = emote_requests(&mut h);
    assert!(reqs.iter().flat_map(|r| &r.jobs).all(|j| matches!(j, Job::Provider { .. })));
    let c = h.buffer("#xqc");
    let global = |p| SetKey::Provider { provider: p, room: None };
    let sets = vec![(global(Provider::SevenTv), &["Clap"][..]), (global(Provider::Bttv), &["KEKW"][..])];
    h.app.on_emote_result(emote_result(&h, reqs[0].connection, sets));
    assert_eq!(h.app.emote_lookup(c).find("Clap KEKW", false), [(0, 4, "https://e/Clap"), (5, 9, "https://e/KEKW")]);

    // Switched-off providers are neither shown nor completed; completion can be switched off.
    let mut cfg = h.app.networks[&h.net].cfg.clone();
    let name = cfg.name.clone();
    cfg.emotes_bttv = false;
    h.app.upsert_network(Some(&name), cfg.clone());
    assert_eq!(h.app.emote_lookup(c).get("KEKW", false), None);
    assert!(h.app.emote_completions(c, "KEKW", 5).is_empty());
    assert_eq!(h.app.emote_completions(c, "Clap", 5).len(), 1);
    cfg.emote_completion = false;
    h.app.upsert_network(Some(&name), cfg.clone());
    assert!(h.app.emote_completions(c, "Clap", 5).is_empty());
    assert_eq!(h.app.emote_lookup(c).get("Clap", false), Some("https://e/Clap"), "still shown");

    // Switching a provider back on fetches what was skipped, for every channel.
    let mut h2 = Harness::new(NetworkKind::Twitch, &[]);
    let mut cfg = h2.app.networks[&h2.net].cfg.clone();
    cfg.emotes_ffz = false;
    let name = cfg.name.clone();
    h2.app.upsert_network(Some(&name), cfg.clone());
    h2.connect();
    h2.lines(&[
        ":tmi.twitch.tv 001 me :hi",
        ":tmi.twitch.tv 376 me :>",
        ":me!me@me.tmi.twitch.tv JOIN #xqc",
        "@room-id=71092938 :tmi.twitch.tv ROOMSTATE #xqc",
    ]);
    assert!(emote_jobs(&mut h2).concat().iter().all(|j| !matches!(j, Job::Provider { provider: Provider::Ffz, .. })));
    cfg.emotes_ffz = true;
    h2.app.upsert_network(Some(&name), cfg);
    let ffz = |room: Option<&str>| Job::Provider { provider: Provider::Ffz, room_id: room.map(str::to_owned) };
    assert_eq!(emote_jobs(&mut h2).concat(), [ffz(None), ffz(Some("71092938"))]);
}

#[test]
fn emote_fetches_follow_provider_switches() {
    use schwaetz_core::emote_providers::Provider;
    use schwaetz_core::emotes::Job;
    let room = "71092938";
    let job = |provider, room: Option<&str>| Job::Provider { provider, room_id: room.map(str::to_owned) };
    let both = |ps: &[Provider]| -> Vec<Job> {
        let mut jobs: Vec<Job> = ps.iter().map(|&p| job(p, None)).collect();
        jobs.extend(ps.iter().map(|&p| job(p, Some(room))));
        jobs
    };
    let join = [":me!me@me.tmi.twitch.tv JOIN #xqc", "@room-id=71092938 :tmi.twitch.tv ROOMSTATE #xqc"];
    let ready = [":tmi.twitch.tv 001 me :hi", ":tmi.twitch.tv 376 me :>"];

    // All providers off: nothing is fetched from them; Twitch's own emotes still are.
    let mut h = Harness::new(NetworkKind::Twitch, &[]);
    h.app.set_twitch_api_token(h.net, Some("tok".into()));
    let mut cfg = h.app.networks[&h.net].cfg.clone();
    let name = cfg.name.clone();
    for p in Provider::ALL {
        cfg.set_emote_provider(p, false);
    }
    h.app.upsert_network(Some(&name), cfg.clone());
    h.connect();
    h.lines(&ready);
    h.lines(&join);
    h.app.switch_to(h.buffer("#xqc"));
    let jobs = emote_jobs(&mut h).concat();
    assert!(jobs.iter().all(|j| !matches!(j, Job::Provider { .. })), "{jobs:?}");
    assert!(jobs.contains(&Job::TwitchUser) && jobs.contains(&Job::TwitchFollower { room_id: room.into() }));

    // Switching two on fetches exactly their global and channel sets.
    cfg.emotes_7tv = true;
    cfg.emotes_bttv = true;
    h.app.upsert_network(Some(&name), cfg.clone());
    let mut got = emote_jobs(&mut h).concat();
    let mut want = both(&[Provider::SevenTv, Provider::Bttv]);
    got.sort_by_key(|j| format!("{j:?}"));
    want.sort_by_key(|j| format!("{j:?}"));
    assert_eq!(got, want);

    // Switching one off, or emote completion, fetches nothing.
    cfg.emotes_bttv = false;
    h.app.upsert_network(Some(&name), cfg.clone());
    assert!(emote_jobs(&mut h).is_empty());
    cfg.emote_completion = false;
    h.app.upsert_network(Some(&name), cfg.clone());
    assert!(emote_jobs(&mut h).is_empty());

    // Switching it back on during the same connection uses what was fetched already.
    cfg.emotes_bttv = true;
    h.app.upsert_network(Some(&name), cfg.clone());
    assert!(emote_jobs(&mut h).is_empty());

    // A reconnect fetches the providers switched on, and only those.
    cfg.emotes_bttv = false;
    h.app.upsert_network(Some(&name), cfg.clone());
    h.app.on_net_event(NetEvent::Disconnected { id: h.net, reason: "gone".into(), retry_in: None }, h.now);
    h.connect();
    h.lines(&ready);
    h.lines(&join);
    let jobs = emote_jobs(&mut h).concat();
    let providers: Vec<_> = jobs.into_iter().filter(|j| matches!(j, Job::Provider { .. })).collect();
    assert_eq!(providers, both(&[Provider::SevenTv]));

    // Channels joined while a provider is off get its sets once it is switched on.
    h.lines(&[":me!me@me.tmi.twitch.tv JOIN #other", "@room-id=42 :tmi.twitch.tv ROOMSTATE #other"]);
    assert_eq!(emote_jobs(&mut h).concat(), [job(Provider::SevenTv, Some("42"))]);
    cfg.emotes_ffz = true;
    h.app.upsert_network(Some(&name), cfg);
    let mut got = emote_jobs(&mut h).concat();
    got.sort_by_key(|j| format!("{j:?}"));
    let mut want = vec![job(Provider::Ffz, None), job(Provider::Ffz, Some(room)), job(Provider::Ffz, Some("42"))];
    want.sort_by_key(|j| format!("{j:?}"));
    assert_eq!(got, want);
}

#[test]
fn at_mentions_complete_as_mentions_on_twitch_only() {
    let mut irc = Harness::new(NetworkKind::Irc, &[]);
    irc.register();
    irc.lines(&[":me!u@h JOIN #c", ":irc.test 353 me = #c :me alice", ":irc.test 366 me #c :End"]);
    let c = irc.buffer("#c");
    assert_eq!(irc.app.complete(c, "@al", 3, false), Some(("alice: ".to_owned(), 7)));

    let mut tw = Harness::new(NetworkKind::Twitch, &[]);
    tw.connect();
    tw.lines(&[
        ":tmi.twitch.tv 001 me :hi",
        ":tmi.twitch.tv 376 me :>",
        ":me!me@me.tmi.twitch.tv JOIN #xqc",
        ":me.tmi.twitch.tv 353 me = #xqc :me alice",
        ":me.tmi.twitch.tv 366 me #xqc :End",
    ]);
    let c = tw.buffer("#xqc");
    assert_eq!(tw.app.complete(c, "@al", 3, false), Some(("@alice ".to_owned(), 7)));
    assert_eq!(tw.app.complete(c, "al", 2, false), Some(("alice: ".to_owned(), 7)), "without @ as before");
}

#[test]
fn visited_buffers_can_be_gone_back_and_forward_through() {
    let mut h = Harness::new(NetworkKind::Irc, &[]);
    h.register();
    h.lines(&[":me!u@h JOIN #a", ":me!u@h JOIN #b", ":me!u@h JOIN #c"]);
    let (a, b, c) = (h.buffer("#a"), h.buffer("#b"), h.buffer("#c"));
    let start = h.app.active;
    for id in [a, b, c] {
        h.app.switch_to(id);
    }
    h.app.switch_to(c);
    assert!(h.app.go_back(false));
    assert_eq!(h.app.active, b, "switching to the active buffer is no visit");
    assert!(h.app.go_back(false));
    assert_eq!(h.app.active, a);
    assert!(h.app.go_back(true));
    assert_eq!(h.app.active, b);
    assert!(h.app.go_back(true));
    assert_eq!(h.app.active, c);
    assert!(!h.app.go_back(true), "nothing ahead");
    assert_eq!(h.app.active, c);

    // A new visit drops what was ahead.
    h.app.go_back(false);
    h.app.go_back(false);
    h.app.switch_to(c);
    assert!(!h.app.go_back(true));
    assert!(h.app.go_back(false));
    assert_eq!(h.app.active, a);

    // Closed buffers are skipped.
    h.app.switch_to(b);
    h.app.switch_to(c);
    h.app.close_buffer(b);
    assert!(h.app.go_back(false));
    assert_eq!(h.app.active, a);
    while h.app.go_back(false) {}
    assert_eq!(h.app.active, start, "back to where it started");
}

#[test]
fn only_messages_make_a_buffer_active() {
    let mut tw = Harness::new(NetworkKind::Twitch, &[]);
    tw.connect();
    tw.lines(&[
        ":tmi.twitch.tv 001 me :hi",
        ":tmi.twitch.tv 376 me :>",
        ":me!me@me.tmi.twitch.tv JOIN #xqc",
        "@emote-only=0;followers-only=10;room-id=71092938;slow=0;subs-only=0 :tmi.twitch.tv ROOMSTATE #xqc",
        "@msg-id=host_on :tmi.twitch.tv NOTICE #xqc :Now hosting someone.",
    ]);
    let c = tw.buffer("#xqc");
    assert!(!tw.app.buffer(c).unwrap().lines.is_empty());
    assert_eq!(tw.app.buffer(c).unwrap().activity, Activity::None, "joining leaves it idle");
    tw.lines(&["@display-name=Alice :alice!alice@alice.tmi.twitch.tv PRIVMSG #xqc :hello"]);
    assert_eq!(tw.app.buffer(c).unwrap().activity, Activity::Messages);

    let mut irc = Harness::new(NetworkKind::Irc, &[]);
    irc.register();
    irc.lines(&[":me!u@h JOIN #c", ":me!u@h JOIN #d"]);
    let c = irc.buffer("#c");
    irc.lines(&[
        ":alice!a@h JOIN #c",
        ":alice!a@h PART #c :bye",
        ":bob!b@h TOPIC #c :new topic",
        ":bob!b@h MODE #c +m",
    ]);
    assert_eq!(irc.app.buffer(c).unwrap().activity, Activity::None, "events leave it idle");
    irc.lines(&[":bob!b@h PRIVMSG #c :hi"]);
    assert_eq!(irc.app.buffer(c).unwrap().activity, Activity::Messages);
}

/// The badge lists requested since the last call, as (room id, refresh).
fn badge_requests(h: &mut Harness) -> Vec<(Option<String>, bool)> {
    h.app
        .take_effects()
        .into_iter()
        .filter_map(|e| match e {
            Effect::FetchBadges(r) => Some((r.room_id, r.refresh)),
            _ => None,
        })
        .collect()
}

fn badge_result(room: Option<&str>, badges: &[&str]) -> schwaetz_core::badges::BadgeResult {
    let image = |id: &str| schwaetz_core::helix::BadgeImage {
        title: id.into(),
        urls: [1, 2, 3].map(|s| format!("https://static-cdn.jtvnw.net/badges/v1/{id}/{s}")),
    };
    schwaetz_core::badges::BadgeResult {
        room_id: room.map(str::to_owned),
        badges: Some(badges.iter().map(|id| (id.to_string(), image(id))).collect()),
    }
}

#[test]
fn badge_lists_are_loaded_once_and_refreshed_for_unknown_badges() {
    let mut h = Harness::new(NetworkKind::Twitch, &[]);
    h.app.set_twitch_api_token(h.net, Some("tok".into()));
    h.connect();
    h.lines(&[":tmi.twitch.tv 001 me :hi", ":tmi.twitch.tv 376 me :>"]);
    assert_eq!(badge_requests(&mut h), [(None, false)], "the global list on connecting");
    h.lines(&[":me!me@me.tmi.twitch.tv JOIN #xqc", "@room-id=71092938 :tmi.twitch.tv ROOMSTATE #xqc"]);
    assert_eq!(badge_requests(&mut h), [(Some("71092938".into()), false)], "the channel's once its id is known");
    h.lines(&["@room-id=71092938 :tmi.twitch.tv ROOMSTATE #xqc"]);
    assert!(badge_requests(&mut h).is_empty(), "once");

    h.app.on_badge_result(badge_result(None, &["moderator/1", "subscriber/0"]));
    h.app.on_badge_result(badge_result(Some("71092938"), &["subscriber/0", "subscriber/12"]));
    let c = h.buffer("#xqc");
    let l = h.app.badge_lookup(c).unwrap();
    assert!(l.get("subscriber/0").unwrap().urls[0].contains("subscriber/0"));
    assert_eq!(l.get("moderator/1").unwrap().title, "moderator/1", "global ones too");
    assert!(l.get("vip/1").is_none());

    // Known badges need nothing; a new subscriber tier fetches the channel's list again, a new
    // global badge the global one, each once.
    let msg = |badges: &str| format!("@badges={badges};room-id=71092938 :u!u@u.tmi.twitch.tv PRIVMSG #xqc :hi");
    h.lines(&[&msg("moderator/1,subscriber/12")]);
    assert!(badge_requests(&mut h).is_empty());
    h.lines(&[&msg("subscriber/24")]);
    assert_eq!(badge_requests(&mut h), [(Some("71092938".into()), true)]);
    h.lines(&[&msg("subscriber/36,newevent/1")]);
    assert_eq!(badge_requests(&mut h), [(None, true)]);
    h.lines(&[&msg("newevent/2")]);
    assert!(badge_requests(&mut h).is_empty());

    // Switched off: symbols instead, nothing fetched.
    let mut cfg = h.app.networks[&h.net].cfg.clone();
    let name = cfg.name.clone();
    cfg.badge_images = false;
    h.app.upsert_network(Some(&name), cfg);
    assert!(h.app.badge_lookup(c).is_none());
}
