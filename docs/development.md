# Development

## Building

Requires Rust (stable, MSVC toolchain) and Visual Studio Build Tools with the C++ workload.

```powershell
cargo build --release
.\target\release\schwaetz.exe
```

## Checks and tests

`.\scripts\check.ps1` runs the same checks as CI (format, clippy, tests). End-to-end tests run
against a local [Ergo](https://ergo.chat) server, which the script downloads on first use:

```powershell
$ergo = .\tests\ergo\start.ps1 -Port 16667
$env:SCHWAETZ_ERGO_PORT = 16667; cargo test --workspace
```

Other helpers: `scripts\send.ps1` (send input to a running instance), `scripts\screenshot.ps1`,
`scripts\flood.ps1` (load test).

## Twitch sign-in

"Sign in with Twitch" uses the Twitch application whose client ID is compiled in from
`SCHWAETZ_TWITCH_CLIENT_ID`, set in `.cargo\config.toml` (so local and CI builds get it without
extra setup). The application must be registered with the **Public** client type: Twitch only
offers public clients the device code flow, which needs no client secret. To build against another
application, set the variable in your environment (`$env:SCHWAETZ_TWITCH_CLIENT_ID = "..."`),
which takes precedence; an empty value builds without Twitch sign-in. The OAuth endpoints can be
checked with `cargo test -p schwaetz-core --test twitch_live -- --ignored --nocapture`.

## Imgur uploads

Image uploads use the Imgur application whose Client-ID is compiled in from
`SCHWAETZ_IMGUR_CLIENT_ID` (`.cargo\config.toml`, or the environment, which takes precedence).
It is empty by default; then the ID is read at the first upload from the script of imgur.com
(see `crates/core/src/imgur.rs`). Check against the real service with
`cargo test -p schwaetz-core --test imgur_live -- --ignored --nocapture`; the upload test publishes
a small image for a moment and only runs with `$env:SCHWAETZ_IMGUR_UPLOAD_TEST = "1"`.

## Releases

The **Release** workflow (Actions → Release → Run workflow) builds, tests and publishes x86_64 and
arm64 zips with checksums. Its tag must match the workspace version in `Cargo.toml`: bump
`[workspace.package] version`, build once so `Cargo.lock` follows, commit, push, then run it with
`v<version>` (pre-release suffixes like `v0.1.0-rc.1` are allowed).

## Licenses

schwätz is GPL-3.0-only, so every dependency must be under a compatible license. The Release
workflow runs [cargo-about](https://github.com/EmbarkStudios/cargo-about), which fails on any
license not listed in `about.toml`, and ships its output as `THIRD-PARTY-LICENSES.html`. To check
before releasing, run `cargo about generate about.hbs -o THIRD-PARTY-LICENSES.html`.

## Layout

| Crate            | Purpose                                                                 |
|------------------|-------------------------------------------------------------------------|
| `crates/proto`   | IRC wire format: parsing, tags, formatting codes, casemapping, ISUPPORT |
| `crates/client`  | Sans-IO session: registration, CAP/SASL, state tracking, IRCv3          |
| `crates/net`     | Transport: TCP/TLS, flood control, keepalive, reconnect, HTTPS helper   |
| `crates/core`    | Application model: buffers, commands, config, highlights, Twitch, ZNC   |
| `crates/store`   | SQLite history with full-text search, text logs                         |
| `crates/media`   | Image decoding (WIC) and link previews                                  |
| `crates/script`  | JavaScript/TypeScript scripting (QuickJS, oxc)                          |
| `crates/ui`      | Win32 + Direct2D user interface                                         |
| `crates/app`     | The `schwaetz.exe` binary                                               |
