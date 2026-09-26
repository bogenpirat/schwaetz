// Type declarations for schwätz scripts.
//
// Put this file (and the generated tsconfig.json) next to your scripts so your editor can
// type-check them. schwätz itself only strips types; it never type-checks.

/** Identifies a buffer: pass an event object, `{ network, buffer }`, a bufferId, or a buffer name. */
type BufferRef = { network?: string | null; buffer?: string; bufferId?: number } | string | number;

interface LineEvent {
  network: string | null;
  buffer: string;
  bufferId: number;
  /** Line id, used with `hide` and `decorate`. */
  id: number;
  kind:
    | "message" | "action" | "notice" | "join" | "part" | "quit" | "kick" | "nick" | "mode" | "topic"
    | "invite" | "status" | "error" | "server" | "motd" | "ctcp" | "system" | "netsplit";
  nick: string;
  /** Text including mIRC formatting codes. */
  text: string;
  /** Text with formatting removed. Offsets for `decorate` refer to this string. */
  plain: string;
  /** Unix milliseconds. */
  time: number;
  own: boolean;
  highlight: boolean;
  /** Replayed from history (chathistory / bouncer playback), not live. */
  history: boolean;
  msgid: string | null;
}

interface InputEvent {
  network: string | null;
  buffer: string;
  bufferId: number;
  text: string;
}

interface RawEvent {
  network: string;
  command: string;
  params: string[];
  source: string | null;
  nick: string | null;
  tags: Record<string, string>;
  line: string;
}

interface CommandContext {
  /** Everything after the command name. */
  args: string;
  argv: string[];
  network: string | null;
  buffer: string;
  bufferId: number;
}

interface NetworkInfo {
  name: string;
  nick: string;
  connected: boolean;
  channels: string[];
}

interface Emote {
  /** UTF-16 offsets into `LineEvent.plain` (JavaScript string indices). */
  start: number;
  end: number;
  /** Image URL (https). */
  url: string;
  name?: string;
}

interface HttpResponse {
  /** HTTP status, or 0 when the request failed (see `error`). */
  status: number;
  body?: string;
  error?: string;
}

declare namespace schwaetz {
  const version: string;
  const scriptName: string;

  /** All lines added to any buffer. */
  function on(event: "line", handler: (line: LineEvent) => void): void;
  /** Messages, actions and notices only. */
  function on(event: "message", handler: (line: LineEvent) => void): void;
  /** Text typed into a buffer. Return `true` to consume it (it won't be sent). */
  function on(event: "input", handler: (input: InputEvent) => boolean | void): void;
  /** Every protocol line received from a server. */
  function on(event: "raw", handler: (msg: RawEvent) => void): void;

  /** Registers `/name`. */
  function command(name: string, handler: (ctx: CommandContext) => void, help?: string): void;

  /** Prints a local line (defaults to the active buffer). */
  function print(text: unknown, target?: BufferRef): void;
  /** Runs input as if typed, e.g. `exec("/join #rust", { network: "Libera.Chat", buffer: "Libera.Chat" })`. */
  function exec(input: string, target?: BufferRef): void;
  function say(network: string, target: string, text: string): void;
  function notice(network: string, target: string, text: string): void;
  /** Sends a raw protocol line. */
  function send(network: string, line: string): void;
  /** Removes a line from its buffer. */
  function hide(line: LineEvent): void;
  /** Shows inline images over parts of a line (e.g. third-party emotes). */
  function decorate(line: LineEvent, emotes: Emote[]): void;
  /** Desktop notification (respects focus and mute settings). */
  function notify(title: string, body?: string): void;
  function networks(): NetworkInfo[];
  function active(): { network: string | null; buffer: string; bufferId: number };
  /** Current time in Unix milliseconds (as seen by the client). */
  function now(): number;
  function log(...args: unknown[]): void;

  function setTimeout(fn: () => void, ms?: number): number;
  function setInterval(fn: () => void, ms: number): number;
  function clearTimer(id: number): void;

  /** Per-script string storage, persisted in `<script>.storage.toml`. */
  namespace storage {
    function get(key: string): string | undefined;
    function set(key: string, value?: string): void;
  }

  /** HTTPS GET. Requires a `// @grant http` line in the script's first 30 lines. */
  namespace http {
    function get(url: string, callback: (response: HttpResponse) => void): void;
  }
}

declare function setTimeout(fn: () => void, ms?: number): number;
declare function setInterval(fn: () => void, ms: number): number;
declare function clearTimeout(id: number): void;
declare function clearInterval(id: number): void;
declare const console: { log(...a: unknown[]): void; info(...a: unknown[]): void; warn(...a: unknown[]): void; error(...a: unknown[]): void };
