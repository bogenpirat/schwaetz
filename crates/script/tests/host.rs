use schwaetz_core::services::ScriptHost;
use schwaetz_core::{App, Config, LineKind};
use schwaetz_script::Host;
use std::path::PathBuf;

fn setup(name: &str, files: &[(&str, &str)]) -> (Host, App, PathBuf) {
    let dir = std::env::temp_dir().join(format!("schwaetz-scripts-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (f, src) in files {
        std::fs::write(dir.join(f), src).unwrap();
    }
    let host = Host::new(dir.clone(), Some(dir.join("cache"))).unwrap();
    let mut app = App::new(Config::default());
    app.track_new_lines = true;
    (host, app, dir)
}

fn texts(app: &App, buffer: &str) -> Vec<String> {
    app.buffers()
        .iter()
        .find(|b| b.name == buffer)
        .map(|b| b.lines.iter().map(|l| l.text.to_string()).collect())
        .unwrap_or_default()
}

fn pump(host: &mut Host, app: &mut App) {
    let lines = app.take_new_lines();
    host.on_lines(app, &lines);
}

#[test]
fn commands_events_and_typescript() {
    let (mut host, mut app, _dir) = setup(
        "basic",
        &[
            (
                "greet.js",
                r#"
                schwaetz.command("greet", (ctx) => { schwaetz.print("hello " + ctx.argv[0], ctx); }, "Greets someone");
                schwaetz.on("line", (l) => { if (l.plain === "ping") schwaetz.print("pong", l); });
                schwaetz.on("input", (i) => i.text === "swallow me");
                "#,
            ),
            (
                "typed.ts",
                r#"
                interface Counter { n: number }
                const c: Counter = { n: 0 };
                enum Mode { A = 1, B = 2 }
                schwaetz.command("count", (): void => { c.n += Mode.B; schwaetz.print(`count=${c.n}`); });
                "#,
            ),
        ],
    );
    host.tick(&mut app, 1_000);
    let status = app.status_buffer;
    assert!(app.extra_commands.iter().any(|(n, h)| n == "greet" && h == "Greets someone"));

    assert!(host.on_input(&mut app, status, "/greet world"));
    assert!(texts(&app, "schwätz").contains(&"hello world".to_string()));

    assert!(host.on_input(&mut app, status, "/count"));
    assert!(host.on_input(&mut app, status, "/count"));
    assert!(texts(&app, "schwätz").contains(&"count=4".to_string()));

    assert!(host.on_input(&mut app, status, "swallow me"));
    assert!(!host.on_input(&mut app, status, "keep me"));

    app.print(status, LineKind::Status, "", "ping");
    pump(&mut host, &mut app);
    assert!(texts(&app, "schwätz").contains(&"pong".to_string()));
}

#[test]
fn timers_storage_and_errors() {
    let (mut host, mut app, dir) = setup(
        "timers",
        &[
            (
                "t.js",
                r#"
                let n = Number(schwaetz.storage.get("n") || "0");
                setTimeout(() => { schwaetz.print("once"); }, 500);
                const id = setInterval(() => { n++; schwaetz.storage.set("n", n); if (n >= 3) clearInterval(id); }, 100);
                schwaetz.command("spin", () => { while (true) {} });
                schwaetz.command("boom", () => { throw new Error("kaboom"); });
                schwaetz.command("net", () => { schwaetz.http.get("https://example.com", () => {}); });
                "#,
            ),
            ("broken.js", "this is not javascript"),
        ],
    );
    host.tick(&mut app, 1_000);
    let scripts = texts(&app, "scripts");
    assert!(scripts.iter().any(|t| t.contains("SyntaxError") || t.contains("expecting")), "{scripts:?}");

    for t in [1_200, 1_400, 1_600, 1_800] {
        host.tick(&mut app, t);
    }
    let status = app.status_buffer;
    assert!(texts(&app, "schwätz").contains(&"once".to_string()));
    let stored = std::fs::read_to_string(dir.join("t.storage.toml")).unwrap();
    assert!(stored.contains("n = \"3\""), "{stored}");

    let start = std::time::Instant::now();
    assert!(host.on_input(&mut app, status, "/spin"));
    assert!(start.elapsed() < std::time::Duration::from_secs(2), "runaway loop must be interrupted");
    assert!(texts(&app, "scripts").iter().any(|t| t.contains("stopped")));

    assert!(host.on_input(&mut app, status, "/boom"));
    assert!(texts(&app, "scripts").iter().any(|t| t.contains("kaboom")));

    assert!(host.on_input(&mut app, status, "/net"));
    assert!(texts(&app, "scripts").iter().any(|t| t.contains("@grant http")));
}

#[test]
fn hot_reload_picks_up_changes() {
    let (mut host, mut app, dir) =
        setup("reload", &[("a.js", r#"schwaetz.command("v", () => schwaetz.print("v1"));"#)]);
    host.tick(&mut app, 1_000);
    let status = app.status_buffer;
    host.on_input(&mut app, status, "/v");
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(dir.join("a.js"), r#"schwaetz.command("v", () => schwaetz.print("v2"));"#).unwrap();
    // Make sure the modification time differs on coarse filesystems.
    let f = std::fs::File::options().write(true).open(dir.join("a.js")).unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5)).unwrap();
    host.tick(&mut app, 4_000);
    host.on_input(&mut app, status, "/v");
    let t = texts(&app, "schwätz");
    assert!(t.contains(&"v1".to_string()) && t.contains(&"v2".to_string()), "{t:?}");
}
