//! Session behaviour driven by recorded server transcripts.

use schwaetz_client::{ChatKind, Event, Phase, SaslConfig, Session, SessionConfig, Target, TwitchEvent};
use schwaetz_proto::{Message, Tags};

const NOW: i64 = 1_700_000_000_000;

fn session(cfg: SessionConfig) -> Session {
    let mut s = Session::new(cfg);
    s.on_connect(NOW);
    s
}

fn cfg() -> SessionConfig {
    SessionConfig { nick: "me".into(), username: "u".into(), realname: "Real Name".into(), ..Default::default() }
}

fn feed(s: &mut Session, lines: &[&str]) {
    for l in lines {
        s.on_message(Message::parse(l).unwrap_or_else(|e| panic!("{l}: {e}")), NOW);
    }
}

fn sent(s: &mut Session) -> Vec<String> {
    s.drain_outgoing().map(|m| m.to_line()).collect()
}

fn events(s: &mut Session) -> Vec<Event> {
    s.drain_events().map(|e| e.kind).collect()
}

fn register(s: &mut Session) {
    feed(
        s,
        &[
            ":srv 001 me :Welcome",
            ":srv 005 me CHANTYPES=# PREFIX=(qov)~@+ CASEMAPPING=rfc1459 :are supported",
            ":srv 376 me :End",
        ],
    );
}

#[test]
fn registration_with_sasl_plain() {
    let mut s = session(SessionConfig {
        sasl: Some(SaslConfig::Plain { username: "acct".into(), password: "pw".into() }),
        autojoin: vec![("#a".into(), None), ("#k".into(), Some("key".into()))],
        ..cfg()
    });
    assert_eq!(sent(&mut s), ["CAP LS 302", "NICK me", "USER u 0 * :Real Name"]);
    feed(
        &mut s,
        &[
            ":srv CAP * LS * :multi-prefix sasl=PLAIN,EXTERNAL server-time",
            ":srv CAP * LS :echo-message batch unknown-cap",
        ],
    );
    assert_eq!(sent(&mut s), ["CAP REQ :batch echo-message multi-prefix sasl server-time"]);
    feed(&mut s, &[":srv CAP * ACK :batch echo-message multi-prefix sasl server-time"]);
    assert_eq!(sent(&mut s), ["AUTHENTICATE PLAIN"]);
    feed(&mut s, &["AUTHENTICATE +"]);
    assert_eq!(sent(&mut s), ["AUTHENTICATE AGFjY3QAcHc="]);
    feed(&mut s, &[":srv 900 me me!u@h acct :You are now logged in", ":srv 903 me :SASL successful"]);
    assert_eq!(sent(&mut s), ["CAP END"]);
    assert_eq!(s.account(), Some("acct"));
    register(&mut s);
    assert_eq!(s.phase(), Phase::Ready);
    // Keyed channels first so keys line up.
    assert_eq!(sent(&mut s), ["JOIN #k,#a key"]);
    let evs = events(&mut s);
    assert!(evs.iter().any(|e| matches!(e, Event::SaslResult { success: true, .. })));
    assert!(evs.iter().any(|e| matches!(e, Event::Ready)));
}

#[test]
fn sasl_failure_continues_unless_required() {
    let mut s = session(SessionConfig {
        sasl: Some(SaslConfig::Plain { username: "a".into(), password: "b".into() }),
        ..cfg()
    });
    sent(&mut s);
    feed(
        &mut s,
        &[":srv CAP * LS :sasl", ":srv CAP * ACK :sasl", "AUTHENTICATE +", ":srv 904 me :SASL authentication failed"],
    );
    let out = sent(&mut s);
    assert_eq!(out.last().unwrap(), "CAP END");

    let mut s = session(SessionConfig {
        sasl: Some(SaslConfig::Plain { username: "a".into(), password: "b".into() }),
        sasl_required: true,
        ..cfg()
    });
    sent(&mut s);
    feed(&mut s, &[":srv CAP * LS :sasl", ":srv CAP * ACK :sasl", "AUTHENTICATE +", ":srv 904 me :failed"]);
    assert!(sent(&mut s).last().unwrap().starts_with("QUIT"));
}

#[test]
fn sasl_mechanism_not_offered() {
    let mut s = session(SessionConfig { sasl: Some(SaslConfig::External), ..cfg() });
    sent(&mut s);
    feed(&mut s, &[":srv CAP * LS :sasl=PLAIN"]);
    assert_eq!(sent(&mut s), ["CAP END"]);
}

#[test]
fn server_without_cap_support() {
    let mut s = session(cfg());
    sent(&mut s);
    feed(&mut s, &[":old 421 * CAP :Unknown command"]);
    register(&mut s);
    assert_eq!(s.phase(), Phase::Ready);
}

#[test]
fn nick_fallback_during_registration() {
    let mut s = session(SessionConfig { alt_nicks: vec!["me2".into()], ..cfg() });
    sent(&mut s);
    feed(&mut s, &[":srv 433 * me :Nickname is already in use"]);
    assert_eq!(sent(&mut s), ["NICK me2"]);
    feed(&mut s, &[":srv 433 * me2 :Nickname is already in use"]);
    assert_eq!(sent(&mut s), ["NICK me_"]);
    feed(&mut s, &[":srv 001 me_ :Welcome", ":srv 005 me_ MONITOR=100 :are supported", ":srv 376 me_ :End"]);
    assert_eq!(s.nick(), "me_");
    // Reclaim the primary nick via MONITOR.
    assert_eq!(sent(&mut s), ["MONITOR + me"]);
    feed(&mut s, &[":srv 731 me_ :me"]);
    assert_eq!(sent(&mut s), ["NICK me"]);
}

#[test]
fn channel_state_tracking() {
    let mut s = session(cfg());
    register(&mut s);
    feed(
        &mut s,
        &[
            ":me!u@host JOIN #chan",
            ":srv 353 me = #chan :~@alice +bob me",
            ":srv 366 me #chan :End of NAMES",
            ":srv 332 me #chan :the topic",
            ":carol!c@h JOIN #chan",
            ":alice!a@h MODE #chan +v-o carol bob",
            ":alice!a@h MODE #chan +kl secret 10",
            ":bob!b@h NICK robert",
            ":carol!c@h PART #chan :bye",
            ":alice!a@h QUIT :gone",
        ],
    );
    let ch = s.channel("#CHAN").expect("case-insensitive lookup");
    assert_eq!(ch.topic.as_deref(), Some("the topic"));
    assert_eq!(ch.key.as_deref(), Some("secret"));
    assert_eq!(ch.mode_string(), "+kl secret 10");
    let names: Vec<(String, Vec<char>)> =
        ch.sorted_members(s.isupport()).iter().map(|m| (m.nick.clone(), m.prefixes.clone())).collect();
    assert_eq!(names, vec![("robert".into(), vec!['+']), ("me".into(), vec![])]);
    let evs = events(&mut s);
    let quit = evs.iter().find_map(|e| match e {
        Event::Quit { channels, .. } => Some(channels.clone()),
        _ => None,
    });
    assert_eq!(quit, Some(vec!["#chan".to_string()]));
    assert!(evs.iter().any(|e| matches!(e, Event::Nick { old, new, .. } if old == "bob" && new == "robert")));
}

#[test]
fn kicked_removes_channel() {
    let mut s = session(cfg());
    register(&mut s);
    feed(&mut s, &[":me!u@h JOIN #c", ":op!o@h KICK #c me :out"]);
    assert!(s.channel("#c").is_none());
    assert!(events(&mut s).iter().any(|e| matches!(e, Event::Kick { own: true, .. })));
}

#[test]
fn local_echo_without_echo_message() {
    let mut s = session(cfg());
    register(&mut s);
    sent(&mut s);
    events(&mut s);
    s.say(ChatKind::Privmsg, "#c", "hello", Tags::new(), NOW);
    assert_eq!(sent(&mut s), ["PRIVMSG #c hello"]);
    let evs = events(&mut s);
    match &evs[..] {
        [Event::Chat(c)] => {
            assert!(c.own);
            assert_eq!(c.target, Target::Channel { name: "#c".into(), status: None });
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn echo_message_suppresses_local_echo_and_routes_queries() {
    let mut s = session(cfg());
    feed(&mut s, &[":srv CAP * LS :echo-message", ":srv CAP * ACK :echo-message"]);
    register(&mut s);
    events(&mut s);
    s.say(ChatKind::Action, "friend", "waves", Tags::new(), NOW);
    assert!(events(&mut s).is_empty());
    assert!(sent(&mut s).contains(&"PRIVMSG friend :\u{1}ACTION waves\u{1}".to_string()));
    feed(&mut s, &[":me!u@h PRIVMSG friend :\u{1}ACTION waves\u{1}"]);
    match &events(&mut s)[..] {
        [Event::Chat(c)] => {
            assert_eq!(c.kind, ChatKind::Action);
            assert_eq!(c.text, "waves");
            assert_eq!(c.target, Target::Query { peer: "friend".into() });
            assert!(c.own);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn long_messages_are_split() {
    let mut s = session(cfg());
    register(&mut s);
    sent(&mut s);
    let text = "word ".repeat(200);
    s.say(ChatKind::Privmsg, "#c", text.trim_end(), Tags::new(), NOW);
    let out = sent(&mut s);
    assert!(out.len() >= 2);
    for l in &out {
        assert!(l.len() + 2 + 100 <= 512, "{} bytes", l.len());
    }
}

#[test]
fn multiline_send_and_receive() {
    let mut s = session(cfg());
    feed(
        &mut s,
        &[
            ":srv CAP * LS :batch draft/multiline=max-bytes=4096 echo-message",
            ":srv CAP * ACK :batch draft/multiline echo-message",
        ],
    );
    register(&mut s);
    sent(&mut s);
    s.say(ChatKind::Privmsg, "#c", "one\ntwo", Tags::new(), NOW);
    assert_eq!(
        sent(&mut s),
        ["BATCH +ml1 draft/multiline #c", "@batch=ml1 PRIVMSG #c one", "@batch=ml1 PRIVMSG #c two", "BATCH -ml1"]
    );
    events(&mut s);
    feed(
        &mut s,
        &[
            "@msgid=abc :srv BATCH +x draft/multiline #c",
            "@batch=x :bob!b@h PRIVMSG #c :hello",
            "@batch=x;draft/multiline-concat :bob!b@h PRIVMSG #c : world",
            "@batch=x :bob!b@h PRIVMSG #c :line two",
            ":srv BATCH -x",
        ],
    );
    match &events(&mut s)[..] {
        [Event::Chat(c)] => {
            assert_eq!(c.text, "hello world\nline two");
            assert_eq!(c.msgid.as_deref(), Some("abc"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn chathistory_batch_does_not_touch_state() {
    let mut s = session(cfg());
    register(&mut s);
    feed(&mut s, &[":me!u@h JOIN #c", ":srv 353 me = #c :me", ":srv 366 me #c :End"]);
    events(&mut s);
    feed(
        &mut s,
        &[
            ":srv BATCH +h chathistory #c",
            "@batch=h;time=2023-11-14T22:13:20.000Z;msgid=1 :old!o@h PRIVMSG #c :earlier",
            "@batch=h;time=2023-11-14T22:13:21.000Z :ghost!g@h JOIN #c",
            ":srv BATCH -h",
        ],
    );
    assert_eq!(s.channel("#c").unwrap().members.len(), 1);
    match &events(&mut s)[..] {
        [Event::History { target, events }] => {
            assert_eq!(target, "#c");
            assert_eq!(events.len(), 2);
            assert_eq!(events[0].time, 1_700_000_000_000);
            assert!(matches!(&events[0].kind, Event::Chat(c) if c.history && c.text == "earlier"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn labeled_response() {
    let mut s = session(cfg());
    feed(&mut s, &[":srv CAP * LS :labeled-response batch", ":srv CAP * ACK :batch labeled-response"]);
    register(&mut s);
    events(&mut s);
    sent(&mut s);
    let label = s.send_labeled(Message::new("WHOIS", ["bob"])).unwrap();
    assert_eq!(sent(&mut s), [format!("@label={label} WHOIS bob")]);
    feed(
        &mut s,
        &[
            &format!("@label={label} :srv BATCH +w labeled-response"),
            "@batch=w :srv 311 me bob b host * :Bob",
            "@batch=w :srv 318 me bob :End of WHOIS",
            ":srv BATCH -w",
        ],
    );
    let evs: Vec<_> = s.drain_events().collect();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].label.as_deref(), Some(label.as_str()));
    assert!(matches!(&evs[0].kind, Event::Whois(w) if w.realname.as_deref() == Some("Bob")));
}

#[test]
fn reconnect_rejoins_open_channels_not_autojoin() {
    let mut s = session(SessionConfig { autojoin: vec![("#auto".into(), None)], ..cfg() });
    register(&mut s);
    feed(&mut s, &[":me!u@h JOIN #auto", ":me!u@h JOIN #other", ":me!u@h PART #auto", ":srv 324 me #other +k pw"]);
    s.set_away(Some("brb".into()));
    sent(&mut s);
    s.on_disconnect();
    s.on_connect(NOW + 5000);
    sent(&mut s);
    register(&mut s);
    let out = sent(&mut s);
    assert_eq!(out, ["JOIN #other pw", "AWAY brb"]);
}

#[test]
fn sts_upgrade_and_policy() {
    let mut s = session(SessionConfig { tls: false, port: 6667, ..cfg() });
    feed(&mut s, &[":srv CAP * LS :sts=port=6697,duration=300"]);
    assert!(events(&mut s).iter().any(|e| matches!(e, Event::ReconnectRequested { tls_port: Some(6697) })));
    let mut s = session(SessionConfig { tls: true, port: 6697, ..cfg() });
    feed(&mut s, &[":srv CAP * LS :sts=duration=300"]);
    assert!(events(&mut s).iter().any(|e| matches!(e, Event::StsPolicy { port: 6697, duration: 300 })));
}

#[test]
fn znc_prefers_standard_caps() {
    let mut s = session(cfg());
    sent(&mut s);
    feed(
        &mut s,
        &[
            ":irc.znc.in CAP unknown-nick LS :server-time znc.in/server-time-iso znc.in/self-message znc.in/playback batch",
        ],
    );
    assert_eq!(sent(&mut s), ["CAP REQ :batch server-time znc.in/playback znc.in/self-message"]);
    feed(&mut s, &[":irc.znc.in CAP me ACK :batch server-time znc.in/playback znc.in/self-message"]);
    register(&mut s);
    assert!(s.is_znc());
    sent(&mut s);
    assert!(s.znc_playback(1_700_000_000_500));
    assert_eq!(sent(&mut s), ["PRIVMSG *playback :PLAY * 1700000000.500"]);
    // Self-messages from other clients appear as our own, routed to the right query.
    events(&mut s);
    feed(&mut s, &[":me!u@h PRIVMSG *status :ListNetworks"]);
    assert!(
        matches!(&events(&mut s)[..], [Event::Chat(c)] if c.own && c.target == Target::Query { peer: "*status".into() })
    );
}

#[test]
fn twitch_dialect() {
    let mut s =
        session(SessionConfig { twitch: true, password: Some("oauth:abc".into()), nick: "viewer".into(), ..cfg() });
    assert_eq!(
        sent(&mut s),
        ["CAP REQ :twitch.tv/tags twitch.tv/commands twitch.tv/membership", "PASS oauth:abc", "NICK viewer"]
    );
    feed(
        &mut s,
        &[
            ":tmi.twitch.tv CAP * ACK :twitch.tv/tags twitch.tv/commands twitch.tv/membership",
            ":tmi.twitch.tv 001 viewer :Welcome, GLHF!",
            ":tmi.twitch.tv 376 viewer :>",
        ],
    );
    assert_eq!(s.phase(), Phase::Ready);
    events(&mut s);
    feed(
        &mut s,
        &[
            "@badges=moderator/1;color=#FF0000;display-name=Streamer;emotes=25:0-4;id=m1;tmi-sent-ts=1700000000123 :streamer!streamer@streamer.tmi.twitch.tv PRIVMSG #streamer :Kappa hi",
            "@ban-duration=600 :tmi.twitch.tv CLEARCHAT #streamer :spammer",
            "@login=spammer;target-msg-id=m2 :tmi.twitch.tv CLEARMSG #streamer :bad words",
            "@msg-id=sub;system-msg=someone\\ssubscribed :tmi.twitch.tv USERNOTICE #streamer",
            ":tmi.twitch.tv RECONNECT",
        ],
    );
    let evs: Vec<_> = s.drain_events().collect();
    match &evs[0].kind {
        Event::Chat(c) => {
            assert_eq!(c.msgid.as_deref(), Some("m1"));
            assert_eq!(c.tags.get("display-name"), Some("Streamer"));
            assert_eq!(evs[0].time, 1_700_000_000_123);
        }
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(&evs[1].kind, Event::Twitch(TwitchEvent::ClearChat { nick: Some(n), duration_secs: Some(600), .. }) if n == "spammer")
    );
    assert!(matches!(&evs[2].kind, Event::Twitch(TwitchEvent::ClearMsg { target_msgid, .. }) if target_msgid == "m2"));
    assert!(
        matches!(&evs[3].kind, Event::Twitch(TwitchEvent::UserNotice { system_msg: Some(m), .. }) if m == "someone subscribed")
    );
    assert!(matches!(&evs[4].kind, Event::ReconnectRequested { tls_port: None }));
}

#[test]
fn ctcp_requests_and_standard_replies() {
    let mut s = session(cfg());
    register(&mut s);
    events(&mut s);
    feed(
        &mut s,
        &[":bob!b@h PRIVMSG me :\u{1}VERSION\u{1}", ":srv FAIL CHATHISTORY INVALID_TARGET #nope :No such target"],
    );
    let evs = events(&mut s);
    assert!(matches!(&evs[0], Event::CtcpRequest { command, .. } if command == "VERSION"));
    assert!(
        matches!(&evs[1], Event::StandardReply { code, context, .. } if code == "INVALID_TARGET" && context == &vec!["#nope".to_string()])
    );
}

#[test]
fn whox_on_join_updates_users() {
    let mut s = session(cfg());
    feed(&mut s, &[":srv 001 me :hi", ":srv 005 me WHOX :are supported", ":srv 376 me :end"]);
    feed(&mut s, &[":me!u@h JOIN #c", ":srv 353 me = #c :me bob", ":srv 366 me #c :End"]);
    assert!(sent(&mut s).contains(&"WHO #c %tcuhnfar,742".to_string()));
    events(&mut s);
    feed(&mut s, &[":srv 354 me 742 #c bu bhost bob G bobacct :Bob Real", ":srv 315 me #c :End of WHO"]);
    let bob = s.user("bob").unwrap();
    assert_eq!(bob.account.as_deref(), Some("bobacct"));
    assert_eq!(bob.away.as_deref(), Some(""));
    assert_eq!(bob.realname.as_deref(), Some("Bob Real"));
    // Our own WHO output is not echoed to the server buffer.
    assert!(events(&mut s).iter().all(|e| !matches!(e, Event::ServerText { .. })));
}

#[test]
fn chathistory_request_formats() {
    let mut s = session(cfg());
    feed(
        &mut s,
        &[":srv CAP * LS :draft/chathistory batch server-time", ":srv CAP * ACK :draft/chathistory batch server-time"],
    );
    register(&mut s);
    sent(&mut s);
    assert!(s.request_history("#c", Some(1_700_000_000_000), 100));
    assert_eq!(sent(&mut s), ["CHATHISTORY AFTER #c timestamp=2023-11-14T22:13:20.001Z 100"]);
}
