# Scripting

Scripts are `.js` or `.ts` files in the `scripts` folder (`%APPDATA%\schwaetz\scripts`). They are
loaded at startup and reloaded automatically when you save them; `/reload` reloads everything.
Messages and errors from scripts appear in the **Status** window (the button at the bottom of the
sidebar).

**Settings → Scripts** lists every script with its state (running, off, or why it failed, plus
its network access and commands). Switch scripts on or off there (this takes effect and is saved
immediately, as `[scripts] disabled` in `config.toml`), open the scripts folder, reload everything, or add the
bundled example scripts (they arrive switched off). A script that fails to load is retried when
you save the file again.

TypeScript is supported by stripping types (it is not type-checked at runtime). The scripts folder
contains `schwaetz.d.ts` and a `tsconfig.json`, so editors like VS Code type-check your scripts
as you write them. The full API is documented in [schwaetz.d.ts](schwaetz.d.ts).

## A first script

```ts
// scripts/hello.ts
schwaetz.command("hello", (ctx) => {
  schwaetz.say(ctx.network!, ctx.buffer, `Hello from ${schwaetz.scriptName}!`);
}, "Say hello");

schwaetz.on("message", (line) => {
  if (!line.own && line.plain.includes("schwätz")) {
    schwaetz.notify(`${line.nick} mentioned schwätz`, line.plain);
  }
});
```

## Events

| Event | Payload | Notes |
|-------|---------|-------|
| `line` | `LineEvent` | Every line added to any buffer |
| `message` | `LineEvent` | Messages, actions and notices |
| `input` | `InputEvent` | Return `true` to consume the input |
| `raw` | `RawEvent` | Every protocol line from a server |

`LineEvent.plain` is the text without formatting codes; offsets passed to `decorate` refer to it
(JavaScript string indices). `history` is true for lines replayed by the server.

## Doing things

`print`, `exec` (run input as if typed, including `/commands`), `say`, `notice`, `send` (raw
line), `hide`, `decorate` (inline images, e.g. emotes), `notify`, `networks`, `active`, `now`,
`log` / `console.log`, `setTimeout` / `setInterval` / `clearTimer`, `storage.get` / `storage.set`
(persisted per script in `<name>.storage.toml`).

Targets (`BufferRef`) can be an event object, `{ network, buffer }`, a buffer id, or a name.
Without a network, a name refers to a client-side buffer (created on demand), e.g.
`schwaetz.print("…", { buffer: "highlights" })`.

## Network access

`schwaetz.http.get(url, callback)` fetches HTTPS URLs (up to 4 MB) and is only available to
scripts that declare it in their first 30 lines:

```ts
// @grant http
```

Settings → Scripts (and the Status window, when they load) shows which scripts have network access.

## Limits

- Each call into a script (event, command, timer) may run for 250 ms; longer runs are stopped
  and reported. The runtime is capped at 64 MB.
- Scripts can't access files or start processes. They only see the model through their events
  and the functions above.
- Each script is a single file; `import` between script files is not supported yet.

## Examples

Settings → Scripts → *Add example scripts* copies them into your scripts folder (switched off),
or see `scripts/examples` in the repository: a highlight collector, third-party emotes for
Twitch (7TV/BTTV/FFZ), and classic `/slap`.
