//! Transport behaviour against a local TCP server.

use schwaetz_net::{ConnectParams, FloodControl, NetCommand, NetEvent, NetHandle, NetworkId, Reconnect, ServerAddr};
use schwaetz_proto::Message;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

const ID: NetworkId = NetworkId(1);

fn params(port: u16) -> Box<ConnectParams> {
    Box::new(ConnectParams {
        servers: vec![ServerAddr { host: "127.0.0.1".into(), port, tls: false, accept_invalid_certs: false }],
        reconnect: Reconnect { initial_delay: Duration::from_millis(50), ..Default::default() },
        ..Default::default()
    })
}

fn wait_for(net: &NetHandle, what: &str, mut pred: impl FnMut(&NetEvent) -> bool) -> Vec<NetEvent> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        for ev in net.drain() {
            let done = pred(&ev);
            seen.push(ev);
            if done {
                return seen;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}; saw {seen:?}");
}

fn accept(listener: &TcpListener) -> (BufReader<TcpStream>, TcpStream) {
    let (s, _) = listener.accept().unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    (BufReader::new(s.try_clone().unwrap()), s)
}

fn read_line(r: &mut BufReader<TcpStream>) -> String {
    let mut l = String::new();
    r.read_line(&mut l).unwrap();
    l.trim_end().to_owned()
}

#[test]
fn ping_pong_lines_and_sending() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let net = NetHandle::start(|| {}).unwrap();
    net.send(NetCommand::Connect(ID, params(port)));
    let (mut r, mut w) = accept(&listener);
    wait_for(&net, "connected", |e| matches!(e, NetEvent::Connected { .. }));

    w.write_all(b"PING :tok123\r\n:srv 001 me :Welcome\r\n").unwrap();
    assert_eq!(read_line(&mut r), "PONG tok123");
    let evs = wait_for(&net, "welcome line", |e| matches!(e, NetEvent::Line { .. }));
    let line = evs.iter().find_map(|e| match e {
        NetEvent::Line { msg, .. } => Some(msg.clone()),
        _ => None,
    });
    assert_eq!(line.unwrap().command, "001");

    // Latin-1 fallback decoding.
    w.write_all(b":a!b@c PRIVMSG #x :gr\xfc\xdfe\r\n").unwrap();
    let evs = wait_for(&net, "latin1 line", |e| matches!(e, NetEvent::Line { .. }));
    assert!(evs.iter().any(|e| matches!(e, NetEvent::Line { msg, .. } if msg.arg(1) == "grüße")));

    net.send_line(ID, Message::new("PRIVMSG", ["#x", "hello world"]));
    assert_eq!(read_line(&mut r), "PRIVMSG #x :hello world");

    net.send(NetCommand::Disconnect(ID, Some("bye".into())));
    assert_eq!(read_line(&mut r), "QUIT bye");
    wait_for(&net, "stopped", |e| matches!(e, NetEvent::Stopped { .. }));
}

#[test]
fn reconnects_after_server_close() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let net = NetHandle::start(|| {}).unwrap();
    net.send(NetCommand::Connect(ID, params(port)));
    let (_r, w) = accept(&listener);
    wait_for(&net, "connected", |e| matches!(e, NetEvent::Connected { .. }));
    drop(w);
    drop(_r);
    let evs = wait_for(&net, "disconnect", |e| matches!(e, NetEvent::Disconnected { .. }));
    assert!(matches!(evs.last(), Some(NetEvent::Disconnected { retry_in: Some(_), .. })));
    let (_r2, _w2) = accept(&listener);
    wait_for(&net, "reconnected", |e| matches!(e, NetEvent::Connected { .. }));
}

#[test]
fn connection_refused_backs_off_and_can_be_forced() {
    // Bind then drop to get a port with nothing listening.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let net = NetHandle::start(|| {}).unwrap();
    let mut p = params(port);
    p.reconnect = Reconnect { initial_delay: Duration::from_secs(60), ..Default::default() };
    net.send(NetCommand::Connect(ID, p));
    let evs = wait_for(&net, "failure", |e| matches!(e, NetEvent::Disconnected { .. }));
    match evs.last() {
        Some(NetEvent::Disconnected { retry_in: Some(d), .. }) => assert!(*d >= Duration::from_secs(40)),
        other => panic!("{other:?}"),
    }
    // A forced reconnect skips the long backoff.
    net.send(NetCommand::ReconnectNow(ID));
    wait_for(&net, "second attempt", |e| matches!(e, NetEvent::Connecting { attempt: 2, .. }));
    net.send(NetCommand::Disconnect(ID, None));
    wait_for(&net, "stopped", |e| matches!(e, NetEvent::Stopped { .. }));
}

#[test]
fn max_attempts_stops() {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let net = NetHandle::start(|| {}).unwrap();
    let mut p = params(port);
    p.reconnect.max_attempts = Some(2);
    p.reconnect.initial_delay = Duration::from_millis(10);
    net.send(NetCommand::Connect(ID, p));
    wait_for(&net, "stopped", |e| matches!(e, NetEvent::Stopped { .. }));
}

#[test]
fn flood_control_throttles() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let net = NetHandle::start(|| {}).unwrap();
    let mut p = params(port);
    p.flood = FloodControl { burst: 2, interval: Duration::from_millis(200) };
    net.send(NetCommand::Connect(ID, p));
    let (mut r, _w) = accept(&listener);
    wait_for(&net, "connected", |e| matches!(e, NetEvent::Connected { .. }));
    let start = Instant::now();
    for i in 0..5 {
        net.send_line(ID, Message::new("PRIVMSG", ["#x".to_string(), format!("m{i}")]));
    }
    for i in 0..5 {
        assert_eq!(read_line(&mut r), format!("PRIVMSG #x m{i}"));
        if i == 1 {
            assert!(start.elapsed() < Duration::from_millis(150), "burst should be immediate");
        }
    }
    // 3 throttled lines at 200ms each.
    assert!(start.elapsed() >= Duration::from_millis(550), "{:?}", start.elapsed());
}

#[test]
fn wake_is_coalesced() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let wakes = Arc::new(AtomicUsize::new(0));
    let w2 = wakes.clone();
    let net = NetHandle::start(move || {
        w2.fetch_add(1, Ordering::SeqCst);
    })
    .unwrap();
    net.send(NetCommand::Connect(ID, params(port)));
    let (_r, mut w) = accept(&listener);
    std::thread::sleep(Duration::from_millis(200));
    let before = wakes.load(Ordering::SeqCst);
    let mut burst = String::new();
    for i in 0..500 {
        burst.push_str(&format!(":a!b@c PRIVMSG #x :{i}\r\n"));
    }
    w.write_all(burst.as_bytes()).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    // Without draining, at most one more wake happens no matter how many lines arrived.
    assert!(wakes.load(Ordering::SeqCst) - before <= 1);
    let n = net.drain().filter(|e| matches!(e, NetEvent::Line { .. })).count();
    assert_eq!(n, 500);
}
