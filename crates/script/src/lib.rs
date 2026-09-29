//! JavaScript/TypeScript scripting for schwätz, on an embedded QuickJS runtime.
//!
//! Every script file in the scripts folder gets its own context with a `schwaetz` API object.
//! Scripts never touch the model directly: API calls queue [`Action`]s that the host applies
//! after the script returns, which keeps borrowing simple and scripts unable to corrupt state.
//! Each call runs under a time budget (interrupt handler) and the runtime has a memory cap.
//! Network access is opt-in per script via a `// @grant http` header line.

mod ts;

pub use ts::{transpile, transpile_cached};

use rquickjs::prelude::{Opt, Rest};
use rquickjs::{Array, CatchResultExt, Coerced, Context, Ctx, Function, Object, Persistent, Runtime, Value};
use schwaetz_core::buffer::{Emote, LineFlags, LineKind, add_emote};
use schwaetz_core::services::{ScriptHost, ScriptInfo};
use schwaetz_core::{App, BufferId};
use schwaetz_proto::Message;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

const CALL_BUDGET: Duration = Duration::from_millis(250);
const MEMORY_LIMIT: usize = 64 << 20;
/// Scripts used to write to their own "scripts" buffer; that name still means the status buffer.
const LEGACY_SCRIPTS_BUFFER: &str = "scripts";

/// (request id, (status, body) or error).
type HttpResult = (u32, Result<(u16, String), String>);

/// Where a script wants output to go.
#[derive(Clone, Debug)]
enum Target {
    Active,
    Id(u32),
    Named { network: Option<String>, buffer: String },
}

enum Action {
    Print {
        target: Target,
        text: String,
    },
    Exec {
        target: Target,
        text: String,
    },
    Say {
        network: String,
        target: String,
        text: String,
        notice: bool,
    },
    Send {
        network: String,
        line: String,
    },
    Hide {
        buffer: u32,
        line: u64,
    },
    Decorate {
        buffer: u32,
        line: u64,
        emotes: Vec<(u32, u32, String, String)>,
    },
    /// Emotes offered for completion: provider, channel (`None` = global), (name, url).
    SetEmotes {
        provider: String,
        channel: Option<String>,
        emotes: Vec<(String, String)>,
    },
    Notify {
        title: String,
        body: String,
    },
    HttpGet {
        script: u32,
        url: String,
        cb: Persistent<Function<'static>>,
    },
    Error {
        script: u32,
        text: String,
    },
}

struct Handler {
    script: u32,
    event: String,
    f: Persistent<Function<'static>>,
}

struct Command {
    script: u32,
    name: String,
    help: String,
    f: Persistent<Function<'static>>,
}

struct Timer {
    script: u32,
    id: u32,
    due: i64,
    every: Option<i64>,
    f: Persistent<Function<'static>>,
}

#[derive(Clone, Default)]
struct NetSnapshot {
    name: String,
    nick: String,
    connected: bool,
    channels: Vec<String>,
}

#[derive(Default)]
struct Shared {
    actions: Vec<Action>,
    handlers: Vec<Handler>,
    commands: Vec<Command>,
    timers: Vec<Timer>,
    next_timer: u32,
    now: i64,
    networks: Vec<NetSnapshot>,
    active: (Option<String>, String, u32),
    storage: HashMap<u32, (PathBuf, BTreeMap<String, String>)>,
}

struct Script {
    id: u32,
    name: String,
    path: PathBuf,
    ctx: Context,
    mtime: Option<SystemTime>,
}

pub struct Host {
    rt: Runtime,
    shared: Rc<RefCell<Shared>>,
    scripts: Vec<Script>,
    dir: PathBuf,
    cache: Option<PathBuf>,
    deadline: Rc<Cell<Option<Instant>>>,
    next_id: u32,
    last_scan: i64,
    http_tx: mpsc::Sender<HttpResult>,
    http_rx: mpsc::Receiver<HttpResult>,
    pending_http: HashMap<u32, (u32, Persistent<Function<'static>>)>,
    next_req: u32,
    /// Outcome of the last load of each file, so failed scripts are only retried after they
    /// change and the settings page can show why.
    status: HashMap<PathBuf, FileStatus>,
    /// Latest runtime error per script name.
    last_error: RefCell<HashMap<String, String>>,
}

struct FileStatus {
    mtime: Option<SystemTime>,
    error: Option<String>,
    network: bool,
}

/// Example scripts bundled into the executable (offered on the settings page).
const EXAMPLES: &[(&str, &str)] = &[
    ("classic.js", include_str!("../../../scripts/examples/classic.js")),
    ("highlights.ts", include_str!("../../../scripts/examples/highlights.ts")),
];

fn stem(p: &Path) -> &str {
    p.file_stem().and_then(|s| s.to_str()).unwrap_or("script")
}

/// A script host message in the status buffer.
fn status_line(app: &mut App, kind: LineKind, text: &str) {
    let b = app.status_buffer;
    app.print_flagged(b, kind, "", text, LineFlags::SCRIPT);
}

fn grants_http(source: &str) -> bool {
    source.lines().take(30).any(|l| l.trim_start().starts_with("//") && l.contains("@grant http"))
}

fn script_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            (n.ends_with(".js") || n.ends_with(".ts")) && !n.ends_with(".d.ts") && !n.starts_with('_')
        })
        .collect();
    v.sort();
    v
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl Host {
    /// Creates the host. Scripts are loaded from `dir`; transpiled TypeScript is cached in `cache`.
    pub fn new(dir: PathBuf, cache: Option<PathBuf>) -> Result<Host, String> {
        let rt = Runtime::new().map_err(|e| e.to_string())?;
        rt.set_memory_limit(MEMORY_LIMIT);
        rt.set_max_stack_size(512 * 1024);
        let deadline: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
        let d = deadline.clone();
        rt.set_interrupt_handler(Some(Box::new(move || d.get().is_some_and(|t| Instant::now() > t))));
        let (http_tx, http_rx) = mpsc::channel();
        Ok(Host {
            rt,
            shared: Rc::new(RefCell::new(Shared::default())),
            scripts: Vec::new(),
            dir,
            cache,
            deadline,
            next_id: 1,
            last_scan: 0,
            http_tx,
            http_rx,
            pending_http: HashMap::new(),
            next_req: 1,
            status: HashMap::new(),
            last_error: RefCell::new(HashMap::new()),
        })
    }

    /// Runs queued promise jobs (async functions, `.then` callbacks) under the time budget.
    fn run_jobs(&self) {
        self.deadline.set(Some(Instant::now() + CALL_BUDGET));
        for _ in 0..10_000 {
            if !self.rt.is_job_pending() || self.rt.execute_pending_job().is_err() {
                break;
            }
        }
        self.deadline.set(None);
    }

    pub fn script_names(&self) -> Vec<String> {
        self.scripts.iter().map(|s| s.name.clone()).collect()
    }

    fn snapshot(&self, app: &App, now: i64) {
        let mut sh = self.shared.borrow_mut();
        sh.now = now;
        sh.networks = app
            .networks
            .values()
            .map(|n| NetSnapshot {
                name: n.display_name().to_owned(),
                nick: n.session.nick().to_owned(),
                connected: n.conn == schwaetz_core::ConnState::Ready,
                channels: n.session.channels().map(|c| c.name.clone()).collect(),
            })
            .collect();
        let b = app.active_buffer();
        let net = b.network.and_then(|n| app.network(n)).map(|n| n.display_name().to_owned());
        sh.active = (net, b.name.clone(), b.id.0);
    }

    /// Script files that are switched on.
    fn enabled_files(&self, app: &App) -> Vec<PathBuf> {
        script_files(&self.dir).into_iter().filter(|p| app.config.scripts.is_enabled(stem(p))).collect()
    }

    fn load_all(&mut self, app: &mut App) {
        for path in self.enabled_files(app) {
            self.load(app, &path);
        }
        app.extra_commands = self.shared.borrow().commands.iter().map(|c| (c.name.clone(), c.help.clone())).collect();
    }

    fn unload(&mut self, id: u32) {
        let mut sh = self.shared.borrow_mut();
        sh.handlers.retain(|h| h.script != id);
        sh.commands.retain(|c| c.script != id);
        sh.timers.retain(|t| t.script != id);
        sh.storage.remove(&id);
        drop(sh);
        self.pending_http.retain(|_, (s, _)| *s != id);
        self.scripts.retain(|s| s.id != id);
    }

    fn load(&mut self, app: &mut App, path: &Path) {
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("script").to_owned();
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => return self.failed(app, path, &name, &format!("cannot read: {e}"), false),
        };
        let js = if path.extension().is_some_and(|e| e == "ts") {
            match transpile_cached(&source, path, self.cache.as_deref()) {
                Ok(js) => js,
                Err(e) => return self.failed(app, path, &name, &e, grants_http(&source)),
            }
        } else {
            source.clone()
        };
        let http = grants_http(&source);
        let ctx = match Context::full(&self.rt) {
            Ok(c) => c,
            Err(e) => return self.failed(app, path, &name, &e.to_string(), http),
        };
        let id = self.next_id;
        self.next_id += 1;
        let storage_path = self.dir.join(format!("{name}.storage.toml"));
        let stored: BTreeMap<String, String> =
            std::fs::read_to_string(&storage_path).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default();
        self.shared.borrow_mut().storage.insert(id, (storage_path, stored));
        let shared = self.shared.clone();
        let deadline = self.deadline.clone();
        let result = ctx.with(|ctx| -> Result<(), String> {
            install_api(&ctx, id, &name, shared, http).map_err(|e| e.to_string())?;
            deadline.set(Some(Instant::now() + CALL_BUDGET * 4));
            let r = ctx.eval::<(), _>(js.as_bytes()).catch(&ctx).map_err(|e| e.to_string());
            deadline.set(None);
            r
        });
        self.run_jobs();
        let mtime = mtime(path);
        self.scripts.push(Script { id, name: name.clone(), path: path.to_owned(), ctx, mtime });
        self.status.insert(path.to_owned(), FileStatus { mtime, error: result.as_ref().err().cloned(), network: http });
        self.last_error.borrow_mut().remove(&name);
        match result {
            Ok(()) => {
                let msg = if http {
                    format!("Script {name} loaded (network access granted)")
                } else {
                    format!("Script {name} loaded")
                };
                status_line(app, LineKind::Status, &msg);
            }
            Err(e) => {
                self.report(app, &name, &e);
                self.unload(id);
            }
        }
        self.apply(app);
    }

    /// A script could not be loaded: remember why (until the file changes) and say so.
    fn failed(&mut self, app: &mut App, path: &Path, name: &str, error: &str, network: bool) {
        self.status.insert(path.to_owned(), FileStatus { mtime: mtime(path), error: Some(error.to_owned()), network });
        self.report(app, name, error);
    }

    fn report(&self, app: &mut App, script: &str, text: &str) {
        if !script.is_empty() {
            self.last_error.borrow_mut().insert(script.to_owned(), text.to_owned());
        }
        // Error lines show no nick column, so name the script in the text.
        let text = if script.is_empty() { text.to_owned() } else { format!("Script {script}: {text}") };
        status_line(app, LineKind::Error, &text);
    }

    /// Calls `f` from script `sid` with an argument built in its context. Returns the result
    /// coerced to bool (for "consume" semantics).
    fn call<F>(&self, sid: u32, f: &Persistent<Function<'static>>, build: F) -> bool
    where
        F: for<'js> FnOnce(&Ctx<'js>) -> rquickjs::Result<Value<'js>>,
    {
        let Some(script) = self.scripts.iter().find(|s| s.id == sid) else { return false };
        let name = script.name.clone();
        let out = script.ctx.with(|ctx| {
            let r = (|| -> rquickjs::Result<bool> {
                let func = f.clone().restore(&ctx)?;
                let arg = build(&ctx)?;
                self.deadline.set(Some(Instant::now() + CALL_BUDGET));
                let v: Value = func.call((arg,))?;
                Ok(v.as_bool().unwrap_or(false))
            })()
            .catch(&ctx);
            self.deadline.set(None);
            r.map_err(|e| e.to_string())
        });
        self.run_jobs();
        match out {
            Ok(b) => b,
            Err(e) => {
                let text = if e.contains("interrupted") {
                    format!("took longer than {CALL_BUDGET:?} and was stopped")
                } else {
                    e
                };
                self.shared.borrow_mut().actions.push(Action::Error { script: sid, text: format!("{name}: {text}") });
                false
            }
        }
    }

    fn handlers_for(&self, event: &str) -> Vec<(u32, Persistent<Function<'static>>)> {
        self.shared.borrow().handlers.iter().filter(|h| h.event == event).map(|h| (h.script, h.f.clone())).collect()
    }

    fn resolve(&self, app: &mut App, t: &Target) -> Option<BufferId> {
        match t {
            Target::Active => Some(app.active),
            Target::Id(i) => app.buffer(BufferId(*i)).map(|b| b.id),
            Target::Named { network, buffer } => {
                // Without a network the name refers to a client-side buffer; "scripts" (and "") is
                // the status buffer.
                let Some(n) = network else {
                    if buffer.is_empty() || buffer == LEGACY_SCRIPTS_BUFFER {
                        return Some(app.status_buffer);
                    }
                    return Some(app.ensure_special(buffer));
                };
                let net = app.network_by_name(n)?;
                Some(app.buffer_for(net, buffer))
            }
        }
    }

    /// Applies queued actions (repeatedly, since actions can trigger more script calls).
    fn apply(&mut self, app: &mut App) {
        for _ in 0..8 {
            let actions = std::mem::take(&mut self.shared.borrow_mut().actions);
            if actions.is_empty() {
                break;
            }
            for a in actions {
                match a {
                    Action::Print { target, text } => {
                        let b = self.resolve(app, &target).unwrap_or(app.status_buffer);
                        app.print_flagged(b, LineKind::Status, "", &text, LineFlags::SCRIPT);
                    }
                    Action::Exec { target, text } => {
                        let Some(b) = self.resolve(app, &target) else { continue };
                        if !self.run_command(app, b, &text) {
                            app.input(b, &text);
                        }
                    }
                    Action::Say { network, target, text, notice } => {
                        if let Some(net) = app.network_by_name(&network) {
                            let kind = if notice {
                                schwaetz_client::ChatKind::Notice
                            } else {
                                schwaetz_client::ChatKind::Privmsg
                            };
                            app.send_chat(net, kind, &target, &text, schwaetz_proto::Tags::new());
                        }
                    }
                    Action::Send { network, line } => {
                        if let (Some(net), Ok(m)) = (app.network_by_name(&network), Message::parse(&line)) {
                            app.send_raw(net, m);
                        }
                    }
                    Action::Hide { buffer, line } => app.remove_line(BufferId(buffer), line),
                    Action::SetEmotes { provider, channel, emotes } => {
                        app.set_script_emotes(&provider, channel.as_deref(), emotes)
                    }
                    Action::Decorate { buffer, line, emotes } => {
                        if let Some(b) = app.buffer_mut(BufferId(buffer))
                            && let Some(l) = b.lines.iter_mut().find(|l| l.id == line)
                        {
                            let plain = schwaetz_proto::format::strip(&l.text);
                            let u16_to_byte = |u: u32| -> u32 {
                                let mut units = 0u32;
                                for (i, c) in plain.char_indices() {
                                    if units >= u {
                                        return i as u32;
                                    }
                                    units += c.len_utf16() as u32;
                                }
                                plain.len() as u32
                            };
                            let extra = l.extra_mut();
                            for (s, e, url, name) in emotes {
                                let (start, end) = (u16_to_byte(s), u16_to_byte(e));
                                add_emote(&mut extra.emotes, Emote { start, end, url, name });
                            }
                            b.generation += 1;
                            app.dirty.lines = true;
                        }
                    }
                    Action::Notify { title, body } => {
                        let b = app.active;
                        app.notify(b, title, body);
                    }
                    Action::HttpGet { script, url, cb } => {
                        let req = self.next_req;
                        self.next_req += 1;
                        self.pending_http.insert(req, (script, cb));
                        let tx = self.http_tx.clone();
                        std::thread::spawn(move || {
                            let r = schwaetz_net::http::get(&url, 4 << 20, false)
                                .map(|r| (r.status, String::from_utf8_lossy(&r.body).into_owned()));
                            let _ = tx.send((req, r));
                        });
                    }
                    Action::Error { script, text } => {
                        let name =
                            self.scripts.iter().find(|s| s.id == script).map(|s| s.name.clone()).unwrap_or_default();
                        self.report(app, &name, &text);
                    }
                }
            }
        }
        // Persist storage changes.
        let sh = self.shared.borrow();
        for (path, map) in sh.storage.values() {
            if let Ok(text) = toml::to_string(map)
                && std::fs::read_to_string(path).ok().as_deref() != Some(text.as_str())
                && !(map.is_empty() && !path.exists())
            {
                let _ = std::fs::write(path, text);
            }
        }
    }

    /// Runs a script-registered `/command`. Returns false if no script handles it.
    fn run_command(&mut self, app: &mut App, buffer: BufferId, text: &str) -> bool {
        let Some(rest) = text.strip_prefix('/') else { return false };
        let (name, args) = rest.split_once(' ').unwrap_or((rest, ""));
        let name = name.to_lowercase();
        let cmd = self.shared.borrow().commands.iter().find(|c| c.name == name).map(|c| (c.script, c.f.clone()));
        let Some((sid, f)) = cmd else { return false };
        let (net, bname) = match app.buffer(buffer) {
            Some(b) => (b.network.and_then(|n| app.network(n)).map(|n| n.display_name().to_owned()), b.name.clone()),
            None => (None, String::new()),
        };
        let args = args.to_owned();
        self.call(sid, &f, move |ctx| {
            let o = Object::new(ctx.clone())?;
            o.set("args", args.as_str())?;
            let words: Vec<&str> = args.split_whitespace().collect();
            o.set("argv", words)?;
            o.set("network", net)?;
            o.set("buffer", bname)?;
            o.set("bufferId", buffer.0)?;
            Ok(o.into_value())
        });
        true
    }
}

fn kind_name(k: LineKind) -> &'static str {
    match k {
        LineKind::Message => "message",
        LineKind::Action => "action",
        LineKind::Notice => "notice",
        LineKind::Join => "join",
        LineKind::Part => "part",
        LineKind::Quit => "quit",
        LineKind::Kick => "kick",
        LineKind::Nick => "nick",
        LineKind::Mode => "mode",
        LineKind::Topic => "topic",
        LineKind::Invite => "invite",
        LineKind::Status => "status",
        LineKind::Error => "error",
        LineKind::Server => "server",
        LineKind::Motd => "motd",
        LineKind::Ctcp => "ctcp",
        LineKind::System => "system",
        LineKind::Netsplit => "netsplit",
    }
}

fn parse_target<'js>(v: Option<Value<'js>>) -> Target {
    let Some(v) = v else { return Target::Active };
    if let Some(s) = v.as_string().and_then(|s| s.to_string().ok()) {
        return Target::Named { network: None, buffer: s };
    }
    if let Some(n) = v.as_int() {
        return Target::Id(n as u32);
    }
    if let Some(o) = v.as_object() {
        if let Ok(Some(id)) = o.get::<_, Option<u32>>("bufferId") {
            return Target::Id(id);
        }
        let network = o.get::<_, Option<String>>("network").ok().flatten();
        if let Ok(Some(buffer)) = o.get::<_, Option<String>>("buffer") {
            return Target::Named { network, buffer };
        }
    }
    Target::Active
}

fn install_api<'js>(
    ctx: &Ctx<'js>,
    id: u32,
    name: &str,
    shared: Rc<RefCell<Shared>>,
    http: bool,
) -> rquickjs::Result<()> {
    let g = ctx.globals();
    let api = Object::new(ctx.clone())?;
    api.set("version", env!("CARGO_PKG_VERSION"))?;
    api.set("scriptName", name)?;

    let sh = shared.clone();
    api.set(
        "on",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, event: String, f: Function<'js>| {
            sh.borrow_mut().handlers.push(Handler { script: id, event, f: Persistent::save(&ctx, f) });
        })?,
    )?;
    let sh = shared.clone();
    api.set(
        "command",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, name: String, f: Function<'js>, help: Opt<String>| {
            let name = name.trim_start_matches('/').to_lowercase();
            let mut s = sh.borrow_mut();
            s.commands.retain(|c| c.name != name);
            s.commands.push(Command {
                script: id,
                name,
                help: help.0.unwrap_or_default(),
                f: Persistent::save(&ctx, f),
            });
        })?,
    )?;
    let sh = shared.clone();
    api.set(
        "print",
        Function::new(ctx.clone(), move |text: Coerced<String>, target: Opt<Value<'js>>| {
            sh.borrow_mut().actions.push(Action::Print { target: parse_target(target.0), text: text.0 });
        })?,
    )?;
    let sh = shared.clone();
    api.set(
        "exec",
        Function::new(ctx.clone(), move |text: String, target: Opt<Value<'js>>| {
            sh.borrow_mut().actions.push(Action::Exec { target: parse_target(target.0), text });
        })?,
    )?;
    let sh = shared.clone();
    api.set(
        "say",
        Function::new(ctx.clone(), move |network: String, target: String, text: Coerced<String>| {
            sh.borrow_mut().actions.push(Action::Say { network, target, text: text.0, notice: false });
        })?,
    )?;
    let sh = shared.clone();
    api.set(
        "notice",
        Function::new(ctx.clone(), move |network: String, target: String, text: Coerced<String>| {
            sh.borrow_mut().actions.push(Action::Say { network, target, text: text.0, notice: true });
        })?,
    )?;
    let sh = shared.clone();
    api.set(
        "send",
        Function::new(ctx.clone(), move |network: String, line: String| {
            sh.borrow_mut().actions.push(Action::Send { network, line });
        })?,
    )?;
    let sh = shared.clone();
    api.set(
        "hide",
        Function::new(ctx.clone(), move |line: Object<'js>| -> rquickjs::Result<()> {
            let buffer: u32 = line.get("bufferId")?;
            let id: u64 = line.get::<_, f64>("id")? as u64;
            sh.borrow_mut().actions.push(Action::Hide { buffer, line: id });
            Ok(())
        })?,
    )?;
    let sh = shared.clone();
    api.set(
        "decorate",
        Function::new(ctx.clone(), move |line: Object<'js>, emotes: Vec<Object<'js>>| -> rquickjs::Result<()> {
            let buffer: u32 = line.get("bufferId")?;
            let id: u64 = line.get::<_, f64>("id")? as u64;
            let mut list = Vec::new();
            for e in emotes {
                list.push((
                    e.get("start")?,
                    e.get("end")?,
                    e.get("url")?,
                    e.get::<_, Option<String>>("name")?.unwrap_or_default(),
                ));
            }
            sh.borrow_mut().actions.push(Action::Decorate { buffer, line: id, emotes: list });
            Ok(())
        })?,
    )?;
    let sh = shared.clone();
    api.set(
        "setEmotes",
        Function::new(
            ctx.clone(),
            move |provider: Coerced<String>,
                  channel: Option<Coerced<String>>,
                  emotes: Vec<Object<'js>>|
                  -> rquickjs::Result<()> {
                let mut list = Vec::with_capacity(emotes.len());
                for e in emotes {
                    list.push((e.get::<_, Coerced<String>>("name")?.0, e.get::<_, Coerced<String>>("url")?.0));
                }
                let channel = channel.map(|c| c.0).filter(|c| !c.is_empty());
                sh.borrow_mut().actions.push(Action::SetEmotes { provider: provider.0, channel, emotes: list });
                Ok(())
            },
        )?,
    )?;
    let sh = shared.clone();
    api.set(
        "notify",
        Function::new(ctx.clone(), move |title: Coerced<String>, body: Opt<Coerced<String>>| {
            sh.borrow_mut()
                .actions
                .push(Action::Notify { title: title.0, body: body.0.map(|b| b.0).unwrap_or_default() });
        })?,
    )?;
    let sh = shared.clone();
    api.set(
        "networks",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>| -> rquickjs::Result<Array<'js>> {
            let arr = Array::new(ctx.clone())?;
            for (i, n) in sh.borrow().networks.iter().enumerate() {
                let o = Object::new(ctx.clone())?;
                o.set("name", n.name.as_str())?;
                o.set("nick", n.nick.as_str())?;
                o.set("connected", n.connected)?;
                o.set("channels", n.channels.clone())?;
                arr.set(i, o)?;
            }
            Ok(arr)
        })?,
    )?;
    let sh = shared.clone();
    api.set(
        "active",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>| -> rquickjs::Result<Object<'js>> {
            let s = sh.borrow();
            let o = Object::new(ctx)?;
            o.set("network", s.active.0.clone())?;
            o.set("buffer", s.active.1.as_str())?;
            o.set("bufferId", s.active.2)?;
            Ok(o)
        })?,
    )?;
    let sh = shared.clone();
    api.set("now", Function::new(ctx.clone(), move || sh.borrow().now as f64)?)?;

    // Timers.
    let add_timer = |repeat: bool| {
        let sh = shared.clone();
        move |ctx: Ctx<'js>, f: Function<'js>, ms: Opt<f64>| -> u32 {
            let mut s = sh.borrow_mut();
            s.next_timer += 1;
            let tid = s.next_timer;
            let ms = ms.0.unwrap_or(0.0).max(if repeat { 100.0 } else { 0.0 }) as i64;
            let due = s.now + ms;
            s.timers.push(Timer {
                script: id,
                id: tid,
                due,
                every: repeat.then_some(ms),
                f: Persistent::save(&ctx, f),
            });
            tid
        }
    };
    let set_timeout = Function::new(ctx.clone(), add_timer(false))?;
    let set_interval = Function::new(ctx.clone(), add_timer(true))?;
    let sh = shared.clone();
    let clear = Function::new(ctx.clone(), move |tid: u32| {
        sh.borrow_mut().timers.retain(|t| !(t.script == id && t.id == tid));
    })?;
    g.set("setTimeout", set_timeout.clone())?;
    g.set("setInterval", set_interval.clone())?;
    g.set("clearTimeout", clear.clone())?;
    g.set("clearInterval", clear.clone())?;
    api.set("setTimeout", set_timeout)?;
    api.set("setInterval", set_interval)?;
    api.set("clearTimer", clear)?;

    // Per-script string storage, persisted next to the script.
    let storage = Object::new(ctx.clone())?;
    let sh = shared.clone();
    storage.set(
        "get",
        Function::new(ctx.clone(), move |key: String| {
            sh.borrow().storage.get(&id).and_then(|(_, m)| m.get(&key).cloned())
        })?,
    )?;
    let sh = shared.clone();
    storage.set(
        "set",
        Function::new(ctx.clone(), move |key: String, value: Opt<Coerced<String>>| {
            if let Some((_, m)) = sh.borrow_mut().storage.get_mut(&id) {
                match value.0 {
                    Some(v) => m.insert(key, v.0),
                    None => m.remove(&key),
                };
            }
        })?,
    )?;
    api.set("storage", storage)?;

    // Logging.
    let sh = shared.clone();
    let log = Function::new(ctx.clone(), move |args: Rest<Coerced<String>>| {
        let text = args.0.into_iter().map(|a| a.0).collect::<Vec<_>>().join(" ");
        sh.borrow_mut().actions.push(Action::Print {
            target: Target::Named { network: None, buffer: LEGACY_SCRIPTS_BUFFER.into() },
            text,
        });
    })?;
    api.set("log", log.clone())?;
    let console = Object::new(ctx.clone())?;
    console.set("log", log.clone())?;
    console.set("info", log.clone())?;
    console.set("warn", log.clone())?;
    console.set("error", log)?;
    g.set("console", console)?;

    // Network access (opt-in).
    let http_obj = Object::new(ctx.clone())?;
    let sh = shared.clone();
    http_obj.set(
        "get",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, url: String, cb: Function<'js>| -> rquickjs::Result<()> {
            if !http {
                return Err(rquickjs::Exception::throw_message(
                    &ctx,
                    "network access not granted: add a `// @grant http` line to the script header",
                ));
            }
            sh.borrow_mut().actions.push(Action::HttpGet { script: id, url, cb: Persistent::save(&ctx, cb) });
            Ok(())
        })?,
    )?;
    api.set("http", http_obj)?;

    g.set("schwaetz", api)?;
    Ok(())
}

impl ScriptHost for Host {
    fn on_lines(&mut self, app: &mut App, lines: &[(BufferId, u64)]) {
        let line_handlers = self.handlers_for("line");
        let msg_handlers = self.handlers_for("message");
        if line_handlers.is_empty() && msg_handlers.is_empty() {
            return;
        }
        self.snapshot(app, app.now);
        for &(bid, lid) in lines {
            let Some(b) = app.buffer(bid) else { continue };
            let Some(l) = b.lines.iter().rev().find(|l| l.id == lid) else { continue };
            if l.flags.has(LineFlags::SCRIPT) {
                continue;
            }
            let net = b.network.and_then(|n| app.network(n)).map(|n| n.display_name().to_owned());
            let data = (
                net,
                b.name.clone(),
                bid.0,
                lid,
                kind_name(l.kind),
                l.nick.to_string(),
                l.text.to_string(),
                schwaetz_proto::format::strip(&l.text),
                l.time,
                l.flags,
                l.msgid().map(str::to_owned),
            );
            let is_msg = l.kind.is_message();
            let targets: Vec<_> =
                line_handlers.iter().chain(if is_msg { msg_handlers.iter() } else { [].iter() }).cloned().collect();
            for (sid, f) in targets {
                let d = data.clone();
                self.call(sid, &f, move |ctx| {
                    let o = Object::new(ctx.clone())?;
                    o.set("network", d.0)?;
                    o.set("buffer", d.1)?;
                    o.set("bufferId", d.2)?;
                    o.set("id", d.3 as f64)?;
                    o.set("kind", d.4)?;
                    o.set("nick", d.5)?;
                    o.set("text", d.6)?;
                    o.set("plain", d.7)?;
                    o.set("time", d.8 as f64)?;
                    o.set("own", d.9.has(LineFlags::OWN))?;
                    o.set("highlight", d.9.has(LineFlags::HIGHLIGHT))?;
                    o.set("history", d.9.has(LineFlags::HISTORY))?;
                    o.set("msgid", d.10)?;
                    Ok(o.into_value())
                });
            }
        }
        self.apply(app);
    }

    fn on_input(&mut self, app: &mut App, buffer: BufferId, text: &str) -> bool {
        self.snapshot(app, app.now);
        if self.run_command(app, buffer, text) {
            self.apply(app);
            return true;
        }
        let handlers = self.handlers_for("input");
        let mut consumed = false;
        let (net, bname) = match app.buffer(buffer) {
            Some(b) => (b.network.and_then(|n| app.network(n)).map(|n| n.display_name().to_owned()), b.name.clone()),
            None => (None, String::new()),
        };
        for (sid, f) in handlers {
            let (n, b, t) = (net.clone(), bname.clone(), text.to_owned());
            consumed |= self.call(sid, &f, move |ctx| {
                let o = Object::new(ctx.clone())?;
                o.set("network", n)?;
                o.set("buffer", b)?;
                o.set("bufferId", buffer.0)?;
                o.set("text", t)?;
                Ok(o.into_value())
            });
            if consumed {
                break;
            }
        }
        self.apply(app);
        consumed
    }

    fn on_raw(&mut self, app: &mut App, network: &str, line: &Message) {
        let handlers = self.handlers_for("raw");
        if handlers.is_empty() {
            return;
        }
        self.snapshot(app, app.now);
        for (sid, f) in handlers {
            let (net, m) = (network.to_owned(), line.clone());
            self.call(sid, &f, move |ctx| {
                let o = Object::new(ctx.clone())?;
                o.set("network", net)?;
                o.set("command", m.command.as_str())?;
                o.set("params", m.params.clone())?;
                o.set("source", m.source.as_ref().map(|s| s.to_string()))?;
                o.set("nick", m.source_nick())?;
                let tags = Object::new(ctx.clone())?;
                for (k, v) in m.tags.iter() {
                    tags.set(k, v)?;
                }
                o.set("tags", tags)?;
                o.set("line", m.to_line())?;
                Ok(o.into_value())
            });
        }
        self.apply(app);
    }

    fn tick(&mut self, app: &mut App, now: i64) {
        self.snapshot(app, now);
        if self.last_scan == 0 {
            self.last_scan = now;
            self.load_all(app);
        } else if now - self.last_scan >= 2000 {
            self.last_scan = now;
            self.hot_reload(app);
        }
        // Timers.
        let due: Vec<(u32, u32, Persistent<Function<'static>>)> = {
            let mut s = self.shared.borrow_mut();
            let mut out = Vec::new();
            for t in s.timers.iter_mut().filter(|t| t.due <= now) {
                out.push((t.script, t.id, t.f.clone()));
                if let Some(every) = t.every {
                    t.due = now + every;
                }
            }
            s.timers.retain(|t| t.every.is_some() || t.due > now);
            out
        };
        for (sid, _, f) in due {
            self.call(sid, &f, |ctx| Ok(Value::new_undefined(ctx.clone())));
        }
        // HTTP completions.
        while let Ok((req, result)) = self.http_rx.try_recv() {
            let Some((sid, cb)) = self.pending_http.remove(&req) else { continue };
            self.call(sid, &cb, move |ctx| {
                let o = Object::new(ctx.clone())?;
                match result {
                    Ok((status, body)) => {
                        o.set("status", status)?;
                        o.set("body", body)?;
                    }
                    Err(e) => {
                        o.set("status", 0)?;
                        o.set("error", e)?;
                    }
                }
                Ok(o.into_value())
            });
        }
        self.apply(app);
    }

    fn reload(&mut self, app: &mut App) {
        let ids: Vec<u32> = self.scripts.iter().map(|s| s.id).collect();
        for id in ids {
            self.unload(id);
        }
        self.status.clear();
        self.last_error.borrow_mut().clear();
        self.load_all(app);
        let n = self.scripts.len();
        status_line(app, LineKind::Status, &format!("{n} script(s) loaded from {}", self.dir.display()));
    }

    fn commands(&self) -> Vec<String> {
        self.shared.borrow().commands.iter().map(|c| c.name.clone()).collect()
    }

    fn list(&self, app: &App) -> Vec<ScriptInfo> {
        let shared = self.shared.borrow();
        script_files(&self.dir)
            .into_iter()
            .map(|path| {
                let name = stem(&path).to_owned();
                let running = self.scripts.iter().find(|s| s.path == path);
                let status = self.status.get(&path);
                let network = match status {
                    Some(s) => s.network,
                    None => std::fs::read_to_string(&path).is_ok_and(|s| grants_http(&s)),
                };
                let error = status
                    .and_then(|s| s.error.clone())
                    .filter(|_| running.is_none())
                    .or_else(|| running.and_then(|_| self.last_error.borrow().get(&name).cloned()));
                let commands = running
                    .map(|s| shared.commands.iter().filter(|c| c.script == s.id).map(|c| c.name.clone()).collect())
                    .unwrap_or_default();
                ScriptInfo {
                    enabled: app.config.scripts.is_enabled(&name),
                    file: path.file_name().and_then(|f| f.to_str()).unwrap_or_default().to_owned(),
                    name,
                    running: running.is_some(),
                    error,
                    network,
                    commands,
                }
            })
            .collect()
    }

    fn refresh(&mut self, app: &mut App) {
        self.last_scan = app.now.max(1);
        self.hot_reload(app);
    }

    fn dir(&self) -> PathBuf {
        self.dir.clone()
    }

    fn install_examples(&mut self) -> Result<Vec<String>, String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let mut added = Vec::new();
        for (file, source) in EXAMPLES {
            let path = self.dir.join(file);
            if !path.exists() {
                std::fs::write(&path, source).map_err(|e| format!("{}: {e}", path.display()))?;
                added.push(stem(&path).to_owned());
            }
        }
        Ok(added)
    }
}

impl Host {
    fn hot_reload(&mut self, app: &mut App) {
        let files = self.enabled_files(app);
        let mut changed = false;
        // Removed, switched off or modified scripts.
        let stale: Vec<u32> = self
            .scripts
            .iter()
            .filter(|s| !files.contains(&s.path) || mtime(&s.path) != s.mtime)
            .map(|s| s.id)
            .collect();
        for id in stale {
            self.unload(id);
            changed = true;
        }
        for f in files {
            // A script that failed is retried once its file changes, not on every scan.
            let failed_unchanged = self.status.get(&f).is_some_and(|s| s.error.is_some() && s.mtime == mtime(&f));
            if !failed_unchanged && !self.scripts.iter().any(|s| s.path == f) {
                self.load(app, &f);
                changed = true;
            }
        }
        if changed {
            app.extra_commands =
                self.shared.borrow().commands.iter().map(|c| (c.name.clone(), c.help.clone())).collect();
        }
    }
}

impl Drop for Host {
    /// Every JS handle must be released before the runtime that owns it.
    fn drop(&mut self) {
        {
            let mut s = self.shared.borrow_mut();
            s.handlers.clear();
            s.commands.clear();
            s.timers.clear();
            s.actions.clear();
        }
        self.pending_http.clear();
        self.scripts.clear();
        self.rt.run_gc();
    }
}

/// Type declarations for script authors (written into the scripts folder).
pub const TYPES: &str = include_str!("../../../docs/schwaetz.d.ts");

/// Creates the scripts folder with type declarations and a tsconfig for editor support.
pub fn prepare_dir(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let dts = dir.join("schwaetz.d.ts");
    if std::fs::read_to_string(&dts).ok().as_deref() != Some(TYPES) {
        let _ = std::fs::write(&dts, TYPES);
    }
    let tsconfig = dir.join("tsconfig.json");
    if !tsconfig.exists() {
        let _ = std::fs::write(
            &tsconfig,
            "{\n  \"compilerOptions\": {\n    \"target\": \"ES2022\",\n    \"lib\": [\"ES2022\"],\n    \"strict\": true,\n    \"noEmit\": true,\n    \"isolatedModules\": false,\n    \"types\": []\n  },\n  \"include\": [\"*.ts\"]\n}\n",
        );
    }
}
