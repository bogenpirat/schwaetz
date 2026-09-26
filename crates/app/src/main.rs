//! schwätz — a fast, native IRC client for Windows.

#![windows_subsystem = "windows"]

use schwaetz_core::{Config, NetworkConfig, Paths};
use schwaetz_ui::Services;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut paths = Paths::detect();
    let mut url = None;
    let mut separate = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--profile" => {
                if let Some(dir) = it.next() {
                    paths = Paths::in_dir(dir);
                    separate = true;
                }
            }
            "--new-instance" => separate = true,
            a if a.starts_with("irc://") || a.starts_with("ircs://") => url = Some(a.to_owned()),
            _ => {}
        }
    }
    // Single instance: hand links/activation to the running window.
    if !separate && schwaetz_ui::win::forward_to_running_instance(url.as_deref().unwrap_or("!show")) {
        return;
    }
    if let Err(e) = paths.ensure() {
        schwaetz_ui::win::error_box("schwätz", &format!("Cannot create data folders: {e}"));
        return;
    }
    install_panic_hook(paths.crash_dir());

    let mut notes = Vec::new();
    let first_run = !paths.config_file().exists();
    let mut config = match Config::load(&paths.config_file()) {
        Ok(c) => c,
        Err(e) => {
            notes.push(format!("Could not read settings ({e}); using defaults. Your file was left untouched."));
            Config::default()
        }
    };
    if first_run {
        config.networks.push(NetworkConfig {
            name: "Libera.Chat".into(),
            servers: vec!["irc.libera.chat:+6697".into()],
            auto_connect: false,
            ..Default::default()
        });
        let _ = config.save(&paths.config_file());
        notes.push(
            "Created a default configuration with Libera.Chat — right-click it in the sidebar and choose Connect."
                .into(),
        );
    }
    if let Some(u) = url {
        apply_irc_url(&mut config, &u);
    }

    let services = Services { history: None, scripts: None };
    if let Err(e) = schwaetz_ui::run(config, paths, services, notes) {
        schwaetz_ui::win::error_box("schwätz", &format!("schwätz could not start:\n\n{e}"));
    }
}

/// `irc://host[:port]/#channel` → connect to that server (configured network if one matches).
fn apply_irc_url(config: &mut Config, url: &str) {
    let Some((host, port, tls)) = NetworkConfig::parse_server(url) else { return };
    let channel =
        url.split_once("://").and_then(|(_, r)| r.split_once('/')).map(|(_, c)| c.trim_start_matches('/').to_owned());
    let channel =
        channel.filter(|c| !c.is_empty()).map(|c| if c.starts_with(['#', '&']) { c } else { format!("#{c}") });
    let existing = config.networks.iter_mut().find(|n| {
        n.servers.iter().any(|s| NetworkConfig::parse_server(s).is_some_and(|(h, _, _)| h.eq_ignore_ascii_case(&host)))
    });
    match existing {
        Some(n) => {
            n.auto_connect = true;
            if let Some(c) = channel
                && !n.autojoin.iter().any(|a| a.split_whitespace().next() == Some(c.as_str()))
            {
                n.autojoin.push(c);
            }
        }
        None => config.networks.push(NetworkConfig {
            name: host.clone(),
            servers: vec![format!("{host}:{}{port}", if tls { "+" } else { "" })],
            auto_connect: true,
            autojoin: channel.into_iter().collect(),
            ..Default::default()
        }),
    }
}

/// Writes a crash report and tells the user where it is.
fn install_panic_hook(dir: PathBuf) {
    std::panic::set_hook(Box::new(move |info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let t = schwaetz_core::time::now_ms();
        let report = format!("schwätz {} crashed\n\n{info}\n\n{bt}\n", env!("CARGO_PKG_VERSION"));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("crash-{t}.txt"));
        let _ = std::fs::write(&path, &report);
        schwaetz_ui::win::error_box(
            "schwätz crashed",
            &format!(
                "Sorry — schwätz hit an unexpected error and has to close.\n\nA report was saved to:\n{}",
                path.display()
            ),
        );
    }));
}
