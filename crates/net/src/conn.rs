//! Connection tasks: connect, read/write loop, flood control, keepalive and reconnect.

use crate::{ConnectParams, EventSink, NetCommand, NetEvent, NetworkId, ServerAddr, TlsInfo, tls};
use schwaetz_proto::decode::decode;
use schwaetz_proto::{Message, MessageRef};
use std::collections::{HashMap, VecDeque};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, sleep_until, timeout};

/// Tags may take up to 8191 bytes plus the 512-byte message body.
const MAX_LINE: usize = 8191 + 512 + 2;
const LAG_INTERVAL: Duration = Duration::from_secs(30);
const KEEPALIVE_TICK: Duration = Duration::from_secs(5);
/// A connection that stayed up this long resets the backoff.
const STABLE_AFTER: Duration = Duration::from_secs(60);

trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}
type BoxStream = Box<dyn AsyncStream>;

enum ConnCmd {
    Send(Message),
    Stop(Option<String>),
    ReconnectNow,
    Update(Box<ConnectParams>),
}

pub(crate) async fn run(mut cmd_rx: mpsc::UnboundedReceiver<NetCommand>, sink: EventSink) {
    let mut conns: HashMap<NetworkId, (mpsc::UnboundedSender<ConnCmd>, JoinHandle<()>)> = HashMap::new();
    while let Some(cmd) = cmd_rx.recv().await {
        conns.retain(|_, (_, task)| !task.is_finished());
        match cmd {
            NetCommand::Connect(id, params) => {
                if let Some((old, task)) = conns.remove(&id) {
                    let _ = old.send(ConnCmd::Stop(None));
                    let _ = task.await;
                }
                let (tx, rx) = mpsc::unbounded_channel();
                let task = tokio::spawn(connection(id, *params, rx, sink.clone()));
                conns.insert(id, (tx, task));
            }
            NetCommand::Send(id, msg) => {
                if let Some((c, _)) = conns.get(&id) {
                    let _ = c.send(ConnCmd::Send(msg));
                }
            }
            NetCommand::Disconnect(id, quit) => {
                if let Some((c, _)) = conns.get(&id) {
                    let _ = c.send(ConnCmd::Stop(quit));
                }
            }
            NetCommand::ReconnectNow(id) => {
                if let Some((c, _)) = conns.get(&id) {
                    let _ = c.send(ConnCmd::ReconnectNow);
                }
            }
            NetCommand::Update(id, params) => {
                if let Some((c, _)) = conns.get(&id) {
                    let _ = c.send(ConnCmd::Update(params));
                }
            }
            NetCommand::Shutdown => break,
        }
    }
    // Give connections a moment to deliver their QUIT.
    for (c, _) in conns.values() {
        let _ = c.send(ConnCmd::Stop(None));
    }
    let tasks: Vec<_> = conns.into_values().map(|(_, t)| t).collect();
    let _ = timeout(Duration::from_millis(1500), async {
        for t in tasks {
            let _ = t.await;
        }
    })
    .await;
}

enum Outcome {
    /// Stop reconnecting (user request / channel closed).
    Stop,
    /// Connection lost or failed; `established` tells whether we were ever connected.
    Lost { reason: String, established_for: Option<Duration>, immediate: bool },
}

async fn connection(
    id: NetworkId,
    mut params: ConnectParams,
    mut rx: mpsc::UnboundedReceiver<ConnCmd>,
    sink: EventSink,
) {
    let mut failures: u32 = 0;
    let mut server_idx = 0usize;
    loop {
        if params.servers.is_empty() {
            sink.emit(NetEvent::Disconnected { id, reason: "No server configured".into(), retry_in: None });
            sink.emit(NetEvent::Stopped { id });
            return;
        }
        let server = params.servers[server_idx % params.servers.len()].clone();
        sink.emit(NetEvent::Connecting { id, server: server.clone(), attempt: failures + 1 });

        let connect = timeout(params.connect_timeout, connect(&server, params.client_cert.as_ref()));
        let outcome = tokio::select! {
            r = connect => match r {
                Err(_) => Outcome::Lost { reason: "Connection timed out".into(), established_for: None, immediate: false },
                Ok(Err(e)) => Outcome::Lost { reason: e, established_for: None, immediate: false },
                Ok(Ok((stream, tls))) => {
                    sink.emit(NetEvent::Connected { id, server: server.clone(), tls });
                    session(id, stream, &mut params, &mut rx, &sink).await
                }
            },
            cmd = recv_control(&mut rx) => match cmd {
                Control::Stop => Outcome::Stop,
                Control::ReconnectNow => Outcome::Lost { reason: "Reconnecting".into(), established_for: None, immediate: true },
                Control::Update(p) => {
                    params = *p;
                    Outcome::Lost { reason: "Server settings changed".into(), established_for: None, immediate: true }
                }
            },
        };

        let (reason, immediate) = match outcome {
            Outcome::Stop => {
                sink.emit(NetEvent::Disconnected { id, reason: "Disconnected".into(), retry_in: None });
                sink.emit(NetEvent::Stopped { id });
                return;
            }
            Outcome::Lost { reason, established_for, immediate } => {
                match established_for {
                    Some(d) if d >= STABLE_AFTER => failures = 0,
                    Some(_) => failures += 1,
                    None => {
                        failures += 1;
                        server_idx += 1;
                    }
                }
                (reason, immediate)
            }
        };

        let rc = &params.reconnect;
        if !rc.enabled || rc.max_attempts.is_some_and(|m| failures >= m) {
            sink.emit(NetEvent::Disconnected { id, reason, retry_in: None });
            sink.emit(NetEvent::Stopped { id });
            return;
        }
        let delay = if immediate { Duration::ZERO } else { backoff(rc.initial_delay, rc.max_delay, failures) };
        sink.emit(NetEvent::Disconnected { id, reason, retry_in: Some(delay) });
        if delay.is_zero() {
            continue;
        }
        let deadline = Instant::now() + delay;
        loop {
            tokio::select! {
                _ = sleep_until(deadline) => break,
                cmd = recv_control(&mut rx) => match cmd {
                    Control::Stop => {
                        sink.emit(NetEvent::Stopped { id });
                        return;
                    }
                    Control::ReconnectNow => break,
                    Control::Update(p) => params = *p,
                },
            }
        }
    }
}

enum Control {
    Stop,
    ReconnectNow,
    Update(Box<ConnectParams>),
}

/// Waits for a control command while not connected; outgoing lines are dropped.
async fn recv_control(rx: &mut mpsc::UnboundedReceiver<ConnCmd>) -> Control {
    loop {
        match rx.recv().await {
            None => return Control::Stop,
            Some(ConnCmd::Stop(_)) => return Control::Stop,
            Some(ConnCmd::ReconnectNow) => return Control::ReconnectNow,
            Some(ConnCmd::Update(p)) => return Control::Update(p),
            Some(ConnCmd::Send(_)) => {}
        }
    }
}

/// Exponential backoff with ±20% jitter.
fn backoff(initial: Duration, max: Duration, failures: u32) -> Duration {
    let exp = initial.saturating_mul(1u32 << failures.saturating_sub(1).min(16));
    let base = exp.min(max).as_millis() as u64;
    let mut r = [0u8; 2];
    let _ = getrandom::fill(&mut r);
    let jitter = (u16::from_le_bytes(r) as u64 % 401) as i64 - 200; // -200..=200 per mille
    Duration::from_millis((base as i64 + base as i64 * jitter / 1000).max(0) as u64)
}

async fn connect(
    server: &ServerAddr,
    cert: Option<&crate::ClientCert>,
) -> Result<(BoxStream, Option<TlsInfo>), String> {
    let tcp = TcpStream::connect((server.host.as_str(), server.port)).await.map_err(|e| e.to_string())?;
    let _ = tcp.set_nodelay(true);
    if !server.tls {
        return Ok((Box::new(tcp), None));
    }
    let config = tls::client_config(server.accept_invalid_certs, cert)?;
    let name =
        rustls_pki_types::ServerName::try_from(server.host.clone()).map_err(|e| format!("invalid host name: {e}"))?;
    let stream = tokio_rustls::TlsConnector::from(config).connect(name, tcp).await.map_err(|e| format!("TLS: {e}"))?;
    let conn = stream.get_ref().1;
    let info = TlsInfo {
        protocol: conn.protocol_version().map(|v| format!("{v:?}").replace('_', ".")).unwrap_or_default(),
        cipher: conn.negotiated_cipher_suite().map(|c| format!("{:?}", c.suite())).unwrap_or_default(),
    };
    Ok((Box::new(stream), Some(info)))
}

struct Bucket {
    tokens: f64,
    burst: f64,
    per_sec: f64,
    last: Instant,
}

impl Bucket {
    fn refill(&mut self) {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * self.per_sec).min(self.burst);
        self.last = now;
    }

    fn wait(&self) -> Duration {
        if self.tokens >= 1.0 { Duration::ZERO } else { Duration::from_secs_f64((1.0 - self.tokens) / self.per_sec) }
    }
}

async fn session(
    id: NetworkId,
    stream: BoxStream,
    params: &mut ConnectParams,
    rx: &mut mpsc::UnboundedReceiver<ConnCmd>,
    sink: &EventSink,
) -> Outcome {
    let started = Instant::now();
    let (rd, mut wr) = tokio::io::split(stream);
    let mut reader = BufReader::with_capacity(16 * 1024, rd);
    let mut buf = Vec::with_capacity(1024);
    let mut discarding = false;
    let mut queue: VecDeque<Vec<u8>> = VecDeque::new();
    let fc = &params.flood;
    let mut bucket = Bucket {
        tokens: fc.burst as f64,
        burst: fc.burst.max(1) as f64,
        per_sec: 1.0 / fc.interval.as_secs_f64().max(0.001),
        last: Instant::now(),
    };
    let mut last_rx = Instant::now();
    let mut lag_ping: Option<(String, Instant)> = None;
    let mut last_lag_ping = Instant::now();
    let mut keepalive = tokio::time::interval(KEEPALIVE_TICK);
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let lost =
        |reason: String, immediate: bool| Outcome::Lost { reason, established_for: Some(started.elapsed()), immediate };

    loop {
        let flood_wait = if queue.is_empty() { Duration::from_secs(3600) } else { bucket.wait() };
        tokio::select! {
            r = async { (&mut reader).take(MAX_LINE as u64).read_until(b'\n', &mut buf).await } => {
                match r {
                    Ok(0) if buf.is_empty() => return lost("Connection closed by server".into(), false),
                    Ok(_) => {
                        last_rx = Instant::now();
                        if buf.last() != Some(&b'\n') {
                            if buf.len() >= MAX_LINE {
                                // Oversized line: drop it up to the next newline.
                                buf.clear();
                                discarding = true;
                                continue;
                            }
                            if r.as_ref().is_ok_and(|n| *n == 0) {
                                return lost("Connection closed by server".into(), false);
                            }
                            continue;
                        }
                        if discarding {
                            discarding = false;
                            buf.clear();
                            continue;
                        }
                        let line = decode(&buf);
                        let line = line.trim_end_matches(['\r', '\n']);
                        if let Err(e) = handle_line(id, line, &mut wr, &mut lag_ping, sink).await {
                            return lost(e, false);
                        }
                        buf.clear();
                    }
                    Err(e) => return lost(e.to_string(), false),
                }
            }
            cmd = rx.recv() => match cmd {
                Some(ConnCmd::Send(msg)) => {
                    let mut line = msg.to_line();
                    line.push_str("\r\n");
                    queue.push_back(line.into_bytes());
                    if let Err(e) = flush(&mut wr, &mut queue, &mut bucket).await {
                        return lost(e, false);
                    }
                }
                Some(ConnCmd::Stop(quit)) => return quit_and_close(&mut wr, quit).await,
                None => return quit_and_close(&mut wr, None).await,
                Some(ConnCmd::ReconnectNow) => return lost("Reconnecting".into(), true),
                Some(ConnCmd::Update(p)) => *params = *p,
            },
            _ = sleep(flood_wait), if !queue.is_empty() => {
                if let Err(e) = flush(&mut wr, &mut queue, &mut bucket).await {
                    return lost(e, false);
                }
            }
            _ = keepalive.tick() => {
                let silent = last_rx.elapsed();
                if silent >= params.ping_timeout {
                    return lost(format!("Ping timeout ({} seconds)", silent.as_secs()), false);
                }
                let due = lag_ping.is_none() && (last_lag_ping.elapsed() >= LAG_INTERVAL || silent >= params.ping_interval);
                if due {
                    let token = format!("lag{}", started.elapsed().as_millis());
                    let line = format!("PING :{token}\r\n");
                    if let Err(e) = wr.write_all(line.as_bytes()).await {
                        return lost(e.to_string(), false);
                    }
                    lag_ping = Some((token, Instant::now()));
                    last_lag_ping = Instant::now();
                }
            }
        }
    }
}

async fn quit_and_close(wr: &mut WriteHalf<BoxStream>, quit: Option<String>) -> Outcome {
    let line = match quit {
        Some(q) => Message::new("QUIT", [q]).to_line(),
        None => "QUIT".to_owned(),
    };
    let _ = timeout(Duration::from_secs(2), async {
        let _ = wr.write_all(format!("{line}\r\n").as_bytes()).await;
        let _ = wr.flush().await;
        let _ = wr.shutdown().await;
    })
    .await;
    Outcome::Stop
}

async fn handle_line(
    id: NetworkId,
    line: &str,
    wr: &mut WriteHalf<BoxStream>,
    lag_ping: &mut Option<(String, Instant)>,
    sink: &EventSink,
) -> Result<(), String> {
    let Ok(m) = MessageRef::parse(line) else { return Ok(()) };
    if m.command.eq_ignore_ascii_case("PING") {
        // Answer immediately, bypassing flood control, so a busy UI can never cause a timeout.
        let token = m.params().last().unwrap_or("");
        let reply = Message::new("PONG", [token]).to_line();
        wr.write_all(format!("{reply}\r\n").as_bytes()).await.map_err(|e| e.to_string())?;
        return Ok(());
    }
    if m.command.eq_ignore_ascii_case("PONG") {
        let token = m.params().last().unwrap_or("");
        if let Some((t, sent)) = lag_ping.as_ref()
            && t == token
        {
            sink.emit(NetEvent::Lag { id, ms: sent.elapsed().as_millis() as u64 });
            *lag_ping = None;
            return Ok(());
        }
    }
    sink.emit(NetEvent::Line { id, msg: m.to_owned() });
    Ok(())
}

async fn flush(
    wr: &mut WriteHalf<BoxStream>,
    queue: &mut VecDeque<Vec<u8>>,
    bucket: &mut Bucket,
) -> Result<(), String> {
    bucket.refill();
    let mut wrote = false;
    while bucket.tokens >= 1.0 {
        let Some(line) = queue.pop_front() else { break };
        wr.write_all(&line).await.map_err(|e| e.to_string())?;
        bucket.tokens -= 1.0;
        wrote = true;
    }
    if wrote {
        wr.flush().await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let i = Duration::from_secs(2);
        let m = Duration::from_secs(300);
        let d1 = backoff(i, m, 1).as_millis();
        assert!((1600..=2400).contains(&d1), "{d1}");
        let d4 = backoff(i, m, 4).as_millis();
        assert!((12_800..=19_200).contains(&d4), "{d4}");
        let d20 = backoff(i, m, 20).as_millis();
        assert!(d20 <= 360_000, "{d20}");
    }
}
