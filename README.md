# schwätz

A fast, native IRC client for Windows, written in Rust.

- Native Win32 UI rendered with Direct2D/DirectWrite — small single `.exe`, low memory, instant start.
- RFC 1459/2812, Modern IRC and IRCv3 (CAP 302, SASL PLAIN/EXTERNAL/SCRAM-SHA-256, batches,
  chathistory, echo-message, labeled-response, multiline, read markers, …).
- Bouncer aware: ZNC (`*playback`, self-message, network import) and soju (bouncer-networks).
- Twitch chat dialect (badges, emotes, moderation events).
- Scriptable in JavaScript or TypeScript.

> Status: under active development. See the milestones in `docs/`.

## Building

Requires Rust (stable, MSVC toolchain) and Visual Studio Build Tools with the C++ workload.

```powershell
cargo build --release
.\target\release\schwaetz.exe
```

If `cargo` reports `link.exe not found` even though Build Tools are installed, load the MSVC
environment first: `. .\scripts\dev-env.ps1`.

Run the same checks as CI with `.\scripts\check.ps1`.

## Layout

| Crate            | Purpose                                                              |
|------------------|----------------------------------------------------------------------|
| `crates/proto`   | IRC wire format: parsing, tags, formatting codes, casemapping, ISUPPORT |
| `crates/client`  | Sans-IO session: registration, CAP/SASL, state tracking, IRCv3        |
| `crates/net`     | Transport: TCP/TLS, flood control, keepalive, reconnect               |
| `crates/core`    | Application model: buffers, commands, config, highlights, Twitch      |
| `crates/store`   | SQLite history + full-text search, text logs                          |
| `crates/media`   | Link previews and inline images                                       |
| `crates/script`  | JavaScript/TypeScript scripting host                                  |
| `crates/ui`      | Win32 + Direct2D user interface                                       |
| `crates/app`     | The `schwaetz.exe` binary                                             |

## License

MIT. The IRC parser conformance vectors in `tests/fixtures` come from
[ircdocs/parser-tests](https://github.com/ircdocs/parser-tests) (CC0).
