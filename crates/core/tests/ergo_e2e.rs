//! End-to-end tests: the real model and network stack against a live Ergo server.
//!
//! Skipped unless `SCHWAETZ_ERGO_PORT` is set (see `tests/ergo/start.ps1`; CI starts a server).

use schwaetz_core::secrets::{self, SecretKind};
use schwaetz_core::{App, Config, ConnState, LineFlags, LineKind, NetworkConfig};
use schwaetz_net::{NetCommand, NetHandle, NetworkId};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

fn port() -> Option<u16> {
    std::env::var("SCHWAETZ_ERGO_PORT").ok()?.parse().ok()
}

fn unique(prefix: &str) -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros();
    format!("{prefix}{}", t % 10_000_000)
}

struct Client {
    app: App,
    net: NetHandle,
    id: NetworkId,
}

impl Client {
    fn new(nick: &str, port: u16, tweak: impl FnOnce(&mut NetworkConfig)) -> Client {
        let mut cfg = Config::default();
        cfg.general.nick = nick.into();
        cfg.general.log_to_files = false;
        let mut n = NetworkConfig {
            name: format!("e2e-{nick}"),
            servers: vec![format!("127.0.0.1:{port}")],
            ..Default::default()
        };
        tweak(&mut n);
        cfg.networks.push(n);
        let mut app = App::new(cfg);
        app.focused = false;
        let id = *app.networks.keys().next().unwrap();
        let net = NetHandle::start(|| {}).unwrap();
        app.connect(id);
        let mut c = Client { app, net, id };
        c.pump_until("registration", |a, id| a.network(id).is_some_and(|n| n.conn == ConnState::Ready));
        c
    }

    fn pump(&mut self) {
        for cmd in self.app.take_net_commands() {
            self.net.send(cmd);
        }
        let now = schwaetz_core::time::now_ms();
        let evs: Vec<_> = self.net.drain().collect();
        for ev in evs {
            self.app.on_net_event(ev, now);
        }
        self.app.tick(now);
        for cmd in self.app.take_net_commands() {
            self.net.send(cmd);
        }
        let _ = self.app.take_effects();
    }

    fn pump_until(&mut self, what: &str, mut done: impl FnMut(&App, NetworkId) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            self.pump();
            if done(&self.app, self.id) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let all: Vec<String> =
            self.app.buffers().iter().flat_map(|b| b.lines.iter().map(|l| format!("{}: {}", b.name, l.text))).collect();
        let texts = &all[all.len().saturating_sub(25)..];
        panic!("timed out waiting for {what}; lines: {texts:#?}");
    }

    fn input(&mut self, buffer_name: &str, text: &str) {
        let b = if buffer_name.is_empty() {
            self.app.network(self.id).unwrap().server_buffer
        } else {
            self.app.find_buffer(self.id, buffer_name).unwrap()
        };
        self.app.input(b, text);
        self.pump();
    }

    fn lines(&self, buffer: &str) -> Vec<String> {
        self.app
            .find_buffer(self.id, buffer)
            .and_then(|b| self.app.buffer(b))
            .map(|b| b.lines.iter().filter(|l| l.kind.is_message()).map(|l| l.text.to_string()).collect())
            .unwrap_or_default()
    }
}

/// A bare-bones second user.
struct Raw {
    w: TcpStream,
}

impl Raw {
    fn connect(port: u16, nick: &str) -> Raw {
        let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut r = Raw { w: s.try_clone().unwrap() };
        r.send(&format!("NICK {nick}"));
        r.send(&format!("USER {nick} 0 * :{nick}"));
        let mut rd = BufReader::new(s);
        let mut line = String::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            line.clear();
            if rd.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            if line.contains(" 376 ") || line.contains(" 422 ") {
                break;
            }
            if let Some(p) = line.strip_prefix("PING ") {
                r.send(&format!("PONG {}", p.trim()));
            }
        }
        std::thread::spawn(move || {
            let mut l = String::new();
            while rd.read_line(&mut l).unwrap_or(0) > 0 {
                l.clear();
            }
        });
        r
    }

    fn send(&mut self, line: &str) {
        let _ = self.w.write_all(format!("{line}\r\n").as_bytes());
    }
}

#[test]
fn chat_roundtrip_and_multiline() {
    let Some(port) = port() else { return eprintln!("SCHWAETZ_ERGO_PORT not set; skipping") };
    let chan = unique("#e2e");
    let nick = unique("sw");
    let mut c = Client::new(&nick, port, |_| {});
    c.input("", &format!("/join {chan}"));
    c.pump_until("join", |a, id| a.find_buffer(id, &chan).and_then(|b| a.buffer(b)).is_some_and(|b| b.joined));

    let bob = unique("bob");
    let mut raw = Raw::connect(port, &bob);
    raw.send(&format!("JOIN {chan}"));
    raw.send(&format!("PRIVMSG {chan} :{nick}: hello from bob"));
    // A multiline batch from the other side.
    raw.send(&format!("BATCH +m draft/multiline {chan}"));
    raw.send(&format!("@batch=m PRIVMSG {chan} :first"));
    raw.send(&format!("@batch=m PRIVMSG {chan} :second"));
    raw.send("BATCH -m");
    c.pump_until("bob's messages", |a, id| {
        a.find_buffer(id, &chan)
            .and_then(|b| a.buffer(b))
            .is_some_and(|b| b.lines.iter().any(|l| &*l.text == "first\nsecond"))
    });
    let b = c.app.buffer(c.app.find_buffer(c.id, &chan).unwrap()).unwrap();
    let hl = b.lines.iter().find(|l| l.text.contains("hello from bob")).unwrap();
    assert!(hl.flags.has(LineFlags::HIGHLIGHT));

    // Our own message comes back through echo-message exactly once.
    c.input(&chan, "reply from the client");
    c.pump_until("echo", |a, id| {
        a.find_buffer(id, &chan)
            .and_then(|b| a.buffer(b))
            .is_some_and(|b| b.lines.iter().any(|l| &*l.text == "reply from the client"))
    });
    std::thread::sleep(Duration::from_millis(300));
    c.pump();
    assert_eq!(c.lines(&chan).iter().filter(|t| *t == "reply from the client").count(), 1);
}

#[test]
fn reconnect_rejoins_and_fills_the_gap() {
    let Some(port) = port() else { return eprintln!("SCHWAETZ_ERGO_PORT not set; skipping") };
    let chan = unique("#gap");
    let nick = unique("gap");
    let mut c = Client::new(&nick, port, |_| {});
    c.input("", &format!("/join {chan}"));
    c.pump_until("join", |a, id| a.find_buffer(id, &chan).and_then(|b| a.buffer(b)).is_some_and(|b| b.joined));
    let mut raw = Raw::connect(port, &unique("talk"));
    raw.send(&format!("JOIN {chan}"));
    raw.send(&format!("PRIVMSG {chan} :before"));
    c.pump_until("before", |a, id| {
        a.find_buffer(id, &chan).and_then(|b| a.buffer(b)).is_some_and(|b| b.lines.iter().any(|l| &*l.text == "before"))
    });

    // Drop the connection and talk while we're away.
    c.net.send(NetCommand::ReconnectNow(c.id));
    raw.send(&format!("PRIVMSG {chan} :while away 1"));
    raw.send(&format!("PRIVMSG {chan} :while away 2"));
    c.pump_until("gap fill", |a, id| {
        a.find_buffer(id, &chan)
            .and_then(|b| a.buffer(b))
            .is_some_and(|b| b.joined && b.lines.iter().any(|l| &*l.text == "while away 2"))
    });
    std::thread::sleep(Duration::from_millis(500));
    c.pump();
    let lines = c.lines(&chan);
    for t in ["before", "while away 1", "while away 2"] {
        assert_eq!(lines.iter().filter(|l| *l == t).count(), 1, "{t} in {lines:?}");
    }
}

#[test]
fn sasl_plain_and_scram() {
    let Some(port) = port() else { return eprintln!("SCHWAETZ_ERGO_PORT not set; skipping") };
    let nick = unique("acct");
    let password = "correct-horse-battery";
    // Register the account.
    let mut c = Client::new(&nick, port, |_| {});
    c.input("", &format!("/msg NickServ REGISTER {password}"));
    c.pump_until("account", |a, id| a.network(id).is_some_and(|n| n.session.account().is_some()));
    // The password never appears in the model.
    assert!(c.app.buffers().iter().all(|b| b.lines.iter().all(|l| !l.text.contains(password))));
    drop(c);

    for (mech, label) in [
        (schwaetz_core::config::SaslMechanism::Plain, "plain"),
        (schwaetz_core::config::SaslMechanism::ScramSha256, "scram"),
    ] {
        let net_name = format!("e2e-{nick}-{label}");
        secrets::set(&net_name, SecretKind::Sasl, password);
        let mut cfg = Config::default();
        cfg.general.nick = format!("{nick}{}", &label[..1]);
        cfg.networks.push(NetworkConfig {
            name: net_name.clone(),
            servers: vec![format!("127.0.0.1:{port}")],
            sasl: mech,
            sasl_username: Some(nick.clone()),
            sasl_required: true,
            ..Default::default()
        });
        let mut app = App::new(cfg);
        let id = *app.networks.keys().next().unwrap();
        let net = NetHandle::start(|| {}).unwrap();
        app.connect(id);
        let mut cl = Client { app, net, id };
        cl.pump_until(label, |a, id| {
            a.network(id).is_some_and(|n| n.conn == ConnState::Ready && n.session.account() == Some(nick.as_str()))
        });
        let sb = cl.app.network(id).unwrap().server_buffer;
        assert!(
            cl.app
                .buffer(sb)
                .unwrap()
                .lines
                .iter()
                .any(|l| l.kind == LineKind::Status && l.text.contains("SASL authentication successful"))
        );
        secrets::delete(&net_name, SecretKind::Sasl);
    }
}
