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
    h.app.input(s, "/set general.show_joins_parts all");
    h.app.input(s, "/set notifications.sound yes");
    h.app.input(s, "/set highlight.words rust, irc");
    assert_eq!(h.app.config.appearance.font_size, 15.5);
    assert_eq!(h.app.config.general.show_joins_parts, "all");
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
    h.app.config.general.show_joins_parts = "all".into();
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
    h.app.config.general.show_joins_parts = "all".into();
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
