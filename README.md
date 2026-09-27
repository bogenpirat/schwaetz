# schwätz

A fast, native IRC client for Windows, written in Rust.

- **Small and light.** One ~8 MB executable, no runtime. About 8 MB of memory when idle and a
  first window in roughly 150 ms. It uses no CPU while nothing happens, and handles 1,000 msg/s
  floods comfortably.
- **Native UI.** Win32 with Direct2D/DirectWrite: crisp text, color emoji, Mica on Windows 11,
  dark/light themes that follow Windows, per-monitor DPI.
- **Modern IRC.** RFC 1459/2812, Modern IRC and IRCv3: CAP 302, SASL (PLAIN, EXTERNAL,
  SCRAM-SHA-256), batches, chathistory with automatic gap-fill, echo-message, labeled-response,
  multiline, read markers, message redaction, typing, replies and reactions, monitor, STS, WHOX, …
- **Bouncers.** ZNC (`*playback`, self-message, importing your other ZNC networks) and soju
  (`bouncer-networks`, chathistory, read markers).
- **Twitch.** Badges, user colors, inline emotes, sub/raid notices, timeouts and deletions,
  room modes, live status with stream title and game (with a Helix API token). Works
  anonymously (read-only) or with an OAuth token.
- **Comforts.** Quick switcher, tab completion, highlights, ignores, aliases, notifications, tray
  icon, searchable history with scroll-back, logs, link previews (opt-in), session restore, raw
  protocol log, channel list, network and settings dialogs.
- **Scriptable** in JavaScript or TypeScript — see [docs/scripting.md](docs/scripting.md).

## Getting started

Run `schwaetz.exe`. On first start it creates a configuration with Libera.Chat; right-click it in
the sidebar and choose **Connect**, or type `/connect irc.libera.chat`. Add networks with
**Add network…** (right-click "schwätz" in the sidebar) or `/network add`.

Useful keys: **Ctrl+J** quick switcher · **Alt+1…9** buffers · **Alt+A** next activity ·
**Ctrl+W** close · **Ctrl+,** settings · **Ctrl+B/I/U/K** formatting · **Tab** completion ·
**Shift+Enter** new line. `/help` lists all commands.

Passwords and tokens are stored in the Windows Credential Manager, never in the config file
(`/secret <network> sasl|pass|twitch|twitch-api <value>` or the network dialog).

Configuration, themes and scripts live in `%APPDATA%\schwaetz`, history and caches in
`%LOCALAPPDATA%\schwaetz`. Put an empty `portable.txt` next to the exe to keep everything in a
`data` folder beside it instead. Details: [docs/configuration.md](docs/configuration.md).

## Building

Requires Rust (stable, MSVC toolchain) and Visual Studio Build Tools with the C++ workload.

```powershell
cargo build --release
.\target\release\schwaetz.exe
```

If `cargo` reports `link.exe not found` although Build Tools are installed, load the MSVC
environment first: `. .\scripts\dev-env.ps1`.

"Sign in with Twitch" uses the Twitch application whose client ID is compiled in from
`SCHWAETZ_TWITCH_CLIENT_ID`, set in `.cargo\config.toml` (so local and CI builds get it without
extra setup). The application must be registered with the **Public** client type: Twitch only
offers public clients the device code flow, which needs no client secret. To build against another
application, set the variable in your environment (`$env:SCHWAETZ_TWITCH_CLIENT_ID = "..."`),
which takes precedence; an empty value builds without Twitch sign-in. The OAuth endpoints can be
checked with `cargo test -p schwaetz-core --test twitch_live -- --ignored --nocapture`.

`.\scripts\check.ps1` runs the same checks as CI (format, clippy, tests). End-to-end tests run
against a local [Ergo](https://ergo.chat) server:

```powershell
$ergo = .\tests\ergo\start.ps1 -Port 16667
$env:SCHWAETZ_ERGO_PORT = 16667; cargo test --workspace
```

Other helpers: `scripts\send.ps1` (send input to a running instance), `scripts\screenshot.ps1`,
`scripts\flood.ps1` (load test).

## Layout

| Crate            | Purpose                                                                |
|------------------|------------------------------------------------------------------------|
| `crates/proto`   | IRC wire format: parsing, tags, formatting codes, casemapping, ISUPPORT |
| `crates/client`  | Sans-IO session: registration, CAP/SASL, state tracking, IRCv3         |
| `crates/net`     | Transport: TCP/TLS, flood control, keepalive, reconnect, HTTPS helper  |
| `crates/core`    | Application model: buffers, commands, config, highlights, Twitch, ZNC  |
| `crates/store`   | SQLite history with full-text search, text logs                        |
| `crates/media`   | Image decoding (WIC) and link previews                                 |
| `crates/script`  | JavaScript/TypeScript scripting (QuickJS, oxc)                         |
| `crates/ui`      | Win32 + Direct2D user interface                                        |
| `crates/app`     | The `schwaetz.exe` binary                                              |

## License

MIT. The IRC parser conformance vectors in `tests/fixtures` come from
[ircdocs/parser-tests](https://github.com/ircdocs/parser-tests) (CC0).
