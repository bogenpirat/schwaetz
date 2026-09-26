//! Network transport for schwätz.
//!
//! A single background thread runs a current-thread tokio runtime that owns every connection.
//! The UI thread talks to it through [`NetHandle`]: commands go in over a channel, events come
//! back over another, and a caller-supplied `wake` callback (e.g. `PostMessage`) is invoked —
//! coalesced — whenever new events are queued.
//!
//! Per connection the transport handles DNS, TCP, TLS (Windows certificate store via
//! `rustls-platform-verifier`, optional client certificates for CertFP/SASL EXTERNAL), line
//! framing and decoding, flood control, keepalive/lag measurement, and automatic reconnects with
//! jittered exponential backoff across multiple server addresses.

mod conn;
pub mod http;
mod tls;

use schwaetz_proto::Message;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;
use tokio::sync::mpsc;

pub use tls::ClientCert;

/// Identifies a network (connection slot) across reconnects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NetworkId(pub u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerAddr {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    /// Skip certificate verification (self-signed bouncers). Never the default.
    pub accept_invalid_certs: bool,
}

impl std::fmt::Display for ServerAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}{}", self.host, if self.tls { "+" } else { "" }, self.port)
    }
}

#[derive(Clone, Debug)]
pub struct FloodControl {
    /// Lines that may be sent back-to-back.
    pub burst: u32,
    /// One additional line is allowed per interval.
    pub interval: Duration,
}

impl Default for FloodControl {
    fn default() -> Self {
        FloodControl { burst: 5, interval: Duration::from_millis(2000) }
    }
}

impl FloodControl {
    /// Twitch allows 20 messages per 30 seconds for regular users.
    pub fn twitch() -> Self {
        FloodControl { burst: 20, interval: Duration::from_millis(1500) }
    }
}

#[derive(Clone, Debug)]
pub struct Reconnect {
    pub enabled: bool,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    /// Give up after this many consecutive failures (`None` = never).
    pub max_attempts: Option<u32>,
}

impl Default for Reconnect {
    fn default() -> Self {
        Reconnect {
            enabled: true,
            initial_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(300),
            max_attempts: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConnectParams {
    /// Tried in order; the list is cycled on reconnect.
    pub servers: Vec<ServerAddr>,
    pub client_cert: Option<ClientCert>,
    pub flood: FloodControl,
    pub reconnect: Reconnect,
    /// Send a lag-measuring PING after this much silence.
    pub ping_interval: Duration,
    /// Disconnect when nothing was received for this long.
    pub ping_timeout: Duration,
    pub connect_timeout: Duration,
}

impl Default for ConnectParams {
    fn default() -> Self {
        ConnectParams {
            servers: Vec::new(),
            client_cert: None,
            flood: FloodControl::default(),
            reconnect: Reconnect::default(),
            ping_interval: Duration::from_secs(60),
            ping_timeout: Duration::from_secs(120),
            connect_timeout: Duration::from_secs(20),
        }
    }
}

#[derive(Debug)]
pub enum NetCommand {
    /// Start (or replace) a connection; keeps reconnecting per the policy until `Disconnect`.
    Connect(NetworkId, Box<ConnectParams>),
    Send(NetworkId, Message),
    /// Send `QUIT` (if connected) and stop reconnecting.
    Disconnect(NetworkId, Option<String>),
    /// Drop the current connection (if any) and reconnect immediately, skipping backoff.
    ReconnectNow(NetworkId),
    /// Replace the server list/policy used for future attempts (e.g. after an STS upgrade).
    Update(NetworkId, Box<ConnectParams>),
    Shutdown,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NetEvent {
    Connecting {
        id: NetworkId,
        server: ServerAddr,
        attempt: u32,
    },
    Connected {
        id: NetworkId,
        server: ServerAddr,
        tls: Option<TlsInfo>,
    },
    Line {
        id: NetworkId,
        msg: Message,
    },
    Lag {
        id: NetworkId,
        ms: u64,
    },
    Disconnected {
        id: NetworkId,
        reason: String,
        retry_in: Option<Duration>,
    },
    /// Reconnecting stopped (user request or attempt limit).
    Stopped {
        id: NetworkId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsInfo {
    pub protocol: String,
    pub cipher: String,
}

/// Handle to the network thread.
pub struct NetHandle {
    tx: mpsc::UnboundedSender<NetCommand>,
    rx: std_mpsc::Receiver<NetEvent>,
    pending: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl NetHandle {
    /// Spawns the network thread. `wake` is called (at most once per drain) when events arrive.
    pub fn start(wake: impl Fn() + Send + Sync + 'static) -> std::io::Result<NetHandle> {
        let (tx, cmd_rx) = mpsc::unbounded_channel();
        let (ev_tx, rx) = std_mpsc::channel();
        let pending = Arc::new(AtomicBool::new(false));
        let sink = EventSink { tx: ev_tx, pending: pending.clone(), wake: Arc::new(wake) };
        let thread =
            std::thread::Builder::new().name("schwaetz-net".into()).stack_size(512 * 1024).spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .max_blocking_threads(4)
                    .build()
                    .expect("tokio runtime");
                rt.block_on(conn::run(cmd_rx, sink));
            })?;
        Ok(NetHandle { tx, rx, pending, thread: Some(thread) })
    }

    pub fn send(&self, cmd: NetCommand) {
        let _ = self.tx.send(cmd);
    }

    pub fn send_line(&self, id: NetworkId, msg: Message) {
        self.send(NetCommand::Send(id, msg));
    }

    /// Drains all queued events. Re-arms the wake callback first so no event is missed.
    pub fn drain(&self) -> impl Iterator<Item = NetEvent> + '_ {
        self.pending.store(false, Ordering::Release);
        self.rx.try_iter()
    }
}

impl Drop for NetHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(NetCommand::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Clone)]
pub(crate) struct EventSink {
    tx: std_mpsc::Sender<NetEvent>,
    pending: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl EventSink {
    pub(crate) fn emit(&self, ev: NetEvent) {
        if self.tx.send(ev).is_ok() && !self.pending.swap(true, Ordering::AcqRel) {
            (self.wake)();
        }
    }
}
