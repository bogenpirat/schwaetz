# Configuration

Settings are stored in `config.toml` (`%APPDATA%\schwaetz`, or `data\` in portable mode). Most can
be changed in the settings dialog (**Ctrl+,**) or with `/set section.key value`
(e.g. `/set appearance.font_size 15`); `/set` without arguments lists everything.

Configuration, themes and scripts live in `%APPDATA%\schwaetz`, history and caches in
`%LOCALAPPDATA%\schwaetz`. Put an empty `portable.txt` next to `schwaetz.exe` to keep everything in
a `data` folder beside it instead. Passwords and tokens never go into the config file: they are
stored in the Windows Credential Manager (see [Secrets](#networks) below).

## Networks

```toml
[[network]]
name = "Libera.Chat"
servers = ["irc.libera.chat:+6697"]   # "+" = TLS; several servers are tried in order
autojoin = ["#rust", "#secret key"]
perform = ["/mode $nick +x"]           # run after connecting
sasl = "scram-sha-256"                 # none | plain | scram-sha-256 | external
sasl_username = "me"
auto_connect = true
```

| Key | Meaning |
|-----|---------|
| `kind` | `irc`, `znc`, `soju` or `twitch` |
| `nick`, `username`, `realname`, `alt_nicks` | Override the global identity |
| `server_password` | A server password is stored in Credential Manager |
| `znc_user`, `znc_network` | ZNC login (`PASS user/network:password`) |
| `bouncer_netid` | soju: bind to one upstream network |
| `client_cert` | PEM file for CertFP / SASL EXTERNAL |
| `accept_invalid_certs` | Accept self-signed certificates (not recommended) |
| `reconnect`, `reconnect_max_attempts` | Automatic reconnects (exponential backoff) |
| `rejoin_on_kick`, `sasl_required`, `previews`, `notifications` | As named |
| `flood_burst`, `flood_interval_ms` | Flood control: burst size, then one line per interval |
| `live_check_secs` | Twitch: seconds between live checks (default 120, at least 30) |
| `twitch_colors` | Twitch: show the name colors users picked in the chat, exactly as Twitch sends them (your own included; default on), whatever `colored_nicks` says |
| `twitch_popout` | Twitch: "Open stream" (topic bar button, channel menu) opens the popout player instead of the channel page |
| `twitch_live_first` | Twitch: list live channels before offline ones in the sidebar, each group in your arranged order |
| `unread_badges` | Unread badges for this network: `all`, `highlights` or `none` (unset: as in `[appearance]`) |
| `joins_parts` | Joins, parts, quits and nick changes of others: `all`, `smart` (only for people who spoke within `smart_filter_secs`) or `none` (unset: `none` on Twitch, `all` elsewhere) |
| `channel_order` | Sidebar order of the channels; drag channels in the sidebar to arrange them (empty = alphabetical, unlisted channels go last) |

Secrets: `/secret <network> sasl <password>`, `/secret <network> pass <password>`,
`/secret Twitch twitch oauth:<token>`, `/secret Twitch twitch-api <token>` — or the network dialog. They go to the Windows Credential
Manager (`schwaetz:<network>:…`).

**Twitch:** use `kind = "twitch"`, server `irc.chat.twitch.tv:+6697`. With a token you can chat;
with a nick like `justinfan12345` and no token you can read anonymously.

**Twitch live status:** sign in with Twitch (network dialog → Twitch → *Sign in with Twitch*,
or `/twitch login`): your browser opens a Twitch page with the code filled in, and after you
click *Authorize* the app picks up the tokens by itself. They are kept in the Credential Manager
and refreshed automatically at least once a day (Twitch expires unused refresh tokens after 30
days); `/twitch status` shows the account and `/twitch logout` signs out. Alternatively store a
Helix API token yourself (*Manual API token*, or `/secret <network> twitch-api <token>`; any
user token works, no scopes needed). Joined channels
are then checked for being live: after connecting once all autojoin channels are joined, right
away for channels you join later, and every `live_check_secs` after that. The stream title and
game show in the topic bar (`🔴 Live · Game — Title · 1,234 viewers` or `Offline · Game — Title`),
and going live, going offline and title or game changes are printed in the channel.

**Twitch emotes:** 7TV, BetterTTV and FrankerFaceZ emotes (each channel's and the global ones)
show inline in Twitch channels like Twitch's own; each provider can be switched off in the
network settings (`emotes_7tv`, `emotes_bttv`, `emotes_ffz`). With the Twitch sign-in, your own
messages show your Twitch emotes too (Twitch doesn't mark them in what you send).

**Twitch emote completion** (`emote_completion`, on by default): in a Twitch channel, typing `:`
followed by a letter or digit at the start of a word lists matching emotes above the input (↑/↓,
PgUp/PgDn to move, Tab or Enter to insert, Esc to close; clicking an entry inserts it too). Order:
the channel's Twitch emotes you may use (follower, subscriber tiers), then the channel's 7TV, FFZ
and BTTV emotes, then global ones. Each entry names its source; Twitch emotes by kind (`sub`,
`follower`, `bits`, `global` …). Twitch emotes need the Twitch sign-in (it asks for permission to
read your emotes; sign-ins from before this was added need to sign in once more); without it only
Twitch's global emotes are offered, if any API token is available. Emotes are loaded once per
connection, so emotes from a new subscription or follow are offered after reconnecting.

**ZNC:** `/znc <command>` talks to `*status`; `/znc import` offers to add your other ZNC networks.

## General (`[general]`)

`nick`, `alt_nicks`, `username`, `realname`, `quit_message`, `part_message`,
`scrollback_lines` (in memory; older lines load from history), `smart_filter_secs` (for a
network's `joins_parts = "smart"`), `ctcp_replies`, `log_to_files`, `history_days` (0 = keep forever),
`confirm_paste_lines`, `remember_channels`, `minimize_to_tray`, `close_to_tray`,
`copy_on_select` (on by default: text you select in the chat goes to the clipboard right away,
no Ctrl+C needed).

With `remember_channels` (on by default) each network's `autojoin` list ("Join on connect") follows
what you do: channels you join are added, and channels you leave with `/part` or by closing their
window are removed. Being kicked or `/cycle` keeps a channel on the list.

## Appearance (`[appearance]`)

`theme` (`system`, `dark`, `light`, or the name of a file in `themes\`), `font`, `font_size`,
`ui_font`, `timestamp_format` (`%H %M %S %d %m %Y %y %A %B`), `nick_column`,
`nick_column_auto` (fit the nick column to the longest name in the buffer, badges included, up
to 40% of the chat's width; on by default), `nick_column_width` (the column's width in
characters when `nick_column_auto` is off), `show_nicklist`, `colored_nicks`, `show_mirc_colors`, `unread_badges`, `mica`,
`gpu_acceleration` (off by default: the CPU rasterizer is fast enough for text and saves the GPU
driver's ~50 MB). `colored_nicks` colors names from the theme's palette: in the member list, and
in the chat for names without a Twitch color (see the network setting `twitch_colors`).
`unread_badges` sets the counts next to buffers in the sidebar: `all` (new messages, the
default), `highlights` (only highlights are counted) or `none`; networks can override it.

### Themes

A theme file (`themes\mytheme.toml`) overrides any subset of the built-in colors:

```toml
base = "dark"            # or "light"
nick_colors = ["#f38ba8", "#fab387", "#a6e3a1", "#89b4fa"]   # before any [table]

[colors]
accent = "#ff79c6"
chat_bg = "#1e1e2e"
highlight_bg = "#f9e2af22"   # #rrggbbaa
```

Color keys: `backdrop`, `backdrop_opaque`, `sidebar_fg`, `sidebar_dim`, `sidebar_header`,
`sidebar_selected`, `sidebar_hover`, `chat_bg`, `text`, `text_dim`, `timestamp`, `accent`,
`accent_fg`, `highlight_bg`, `highlight_bar`, `selection`, `link`, `error`, `join`, `part`,
`notice`, `own_nick`, `border`, `input_bg`, `topic_bg`, `nicklist_bg`, `badge_bg`, `badge_fg`,
`badge_highlight`, `scrollbar`, `unread_marker`, `overlay_scrim`, `panel_bg`, `online`,
`connecting`, `offline`, `live` (the sidebar dot for live Twitch channels). `mirc = [...]` overrides
colors 0–15.

## Highlights, ignores, aliases

```toml
[highlight]
words = ["rust", "schwätz"]
patterns = ['\bdeploy(ed|ing)?\b']   # case-insensitive regular expressions
exclude_nicks = ["*bot"]

[[ignore]]
mask = "spammer!*@*"
types = ["msg", "notice"]           # msg notice action ctcp invite join part quit nick tagmsg all

[aliases]
j = "/join $1-"
ns = "/msg NickServ $1-"
```

Commands: `/ignore`, `/unignore`, `/highlight add|del`, `/alias`, `/unalias`.
Alias variables: `$1`…`$9`, `$1-` (rest), `$nick`, `$channel`, `$network`.

## Notifications, previews

`[notifications]`: `enabled` (desktop notifications; off by default), `on_highlight`, `on_private`,
`when_focused`, `flash_taskbar`. A network's `notifications = false` keeps it quiet even when
they are on.
Per buffer: `/notify all|default|highlights|mute` or the sidebar menu.

`[previews]`: `enabled` (off by default), `auto_load` (otherwise click "Show preview"),
`allow_hosts` (only auto-load from these domains), `max_bytes`, `max_dimension`.
Only HTTPS is fetched.

`[scripts]`: `disabled` (script file names without extension that are switched off; see
[scripting](scripting.md)).

## Files

| Path | Contents |
|------|----------|
| `config.toml` | Settings |
| `themes\*.toml` | Themes |
| `scripts\*.js`, `*.ts` | Scripts (plus `schwaetz.d.ts`, `tsconfig.json`) |
| `history.sqlite` | Message history (full-text searchable with `/search`) |
| `logs\<network>\<buffer>\<date>.log` | Plain-text logs |
| `session.toml` | Window placement and last active buffer |
| `crashes\` | Crash reports |
