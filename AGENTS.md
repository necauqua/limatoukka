# limatoukka

Personal Twitch chat bot of the `necauqua` channel (Rust, edition 2024).
It started as "Twitch Plays Noita" and is now a general-purpose bot: a chat
command language with macros and variables, a "charges" (⚡︎) economy, bets,
sounds, TTS, a YouTube music player, OBS browser sources, and Noita game-state
events read from process memory. The code is public for reference only, and
it is not made to be simple to run by other people.

## Build, run, test

- Dev shell: `flake.nix` (direnv `use flake`). It supplies `rustup`, `just`,
  `valkey`, `openssl`/`dbus` (on `LD_LIBRARY_PATH`), `nodejs`, `wasm-pack`.
  Run cargo from the host with `direnv exec . cargo ...`.
- `.cargo/config.toml` sets `--cfg tokio_unstable`.
- **Path dependencies outside this repository:**
  `../neca-cmd` (a `[patch.crates-io]` for `neca-cmd`, the chat command
  parser) and `../noita/noita-utility-box/noita-engine-reader`. The sandbox
  mounts only this folder, thus `cargo build` fails there. Use `unsafe-bash`,
  or a sandbox with a mount on `~/projects`.
- `twitch_api` is a git dependency on a pinned upstream commit (for the hype
  train V2 EventSub types, which are not in 0.8.0), with a
  `[patch.crates-io]` for `twitch_types` from the same commit so that only
  one `twitch_types` is in the build. Change both back to crates.io versions
  when a release has these types.
- Binaries: `limatoukka` (default, the bot), `docgen` (prints YAML docs of all
  commands to stdout), `mock_chat` (`src/bin/mock_chat/`: the bot with the
  chat in the terminal, see below).
- `mock_chat`: a `reedline` line editor; the lines are chat messages of the
  current user (the caster at start). Lines that start with `/` are
  terminal commands (`/help`): `/as <login> [level]` and simulated
  EventSub, Noita and Ko-fi events. All services are real (Twitch reads,
  Valkey, Elastic, Jev, Noita, sounds, TTS, music), except: chat I/O is the
  terminal, Twitch writes (title, shoutout, fulfill) only print, the chat
  box clear does not store `storage:chat-box:cleared-at`, there is
  no EventSub/ntfy/`bot-restart`, and the HTTP servers use other ports
  (`--browser-source-bind`, `--music-player-bind`). `--valkey <url>`
  selects another Valkey, `--in-memory` keeps the state in memory (no music
  player). Logs go above the prompt, default filter `limatoukka=warn`.
  History: `target/mock-chat-history.txt`.
- Tests: `direnv exec . cargo test`. Tests marked `#[ignore = "manual test"]`
  need real config/credentials.
- Logging: `tracing`; filter from `RUST_LOG`, default `limatoukka=info`.

## Runtime dependencies

- Valkey (Redis) at `config.valkey`: all persistent state.
- Elasticsearch: chat log (`[elastic]`) and command stats (`[stats]`).
  The chat log is written by our own archiver (twitch-archiver,
  `../twitch-archiver`). It stores each IRC message with its tags
  (`badges`/`badge-info` as objects, numbers as numbers, `display-name` as
  `name`, `/me` as `action: true`; empty tags are left out), and it sets
  `commands.count`, and `commands.only: true` for messages that have only
  commands; `_id` is the message id, `@timestamp` is `tmi-sent-ts`.
- Twitch: IRC (`twitch-irc`) for chat, Helix + EventSub websocket
  (`twitch_api`). User tokens are in the system keyring (service
  `limatoukka-the-twitch-bot`). If there is no token, an OAuth flow starts
  with a local axum server on `redirect-url`.
- YouTube Data API v3 (music player), ntfy (Ko-fi webhooks as topic `kofi`).
- TypeSafe API (the Jev model, `[jev]`, `services/jev.rs`): typed
  noul/choice/score questions about a state. It only classifies and does
  not generate text. Used by `ask` (`defs/util.rs`): one request with
  the question, the asker and the recent chat from Elasticsearch (without
  the bot's messages) as the state, and two nouls: "is it a yes/no
  question" and "is the answer yes". Code maps the probability of yes to
  5 bands (Yes … No). Jev reads unknown claims as more likely false, so
  most answers to subjective or future questions are "Probably no".
- `justfile` recipes that the bot calls through `setsid just <recipe>`
  (`src/integration/justfile.rs`): `is-game-running`, `aws-tts` (AWS Polly +
  `pw-play`), `upload-large-reply` (rsync to necauq.ua), `play-sound`.
  Recipes below `### Manual stuff` are for the owner only (`when-ping`,
  `top-charges`).

## Configuration

- `Config::load()` merges `conf/config.toml` (tracked) and
  `conf/config.private.toml` (gitignored, has all secrets). The paths are
  relative, so run from the repository root.
- `src/config.rs` is the real schema. Keep the commented sample of the
  private values in `conf/config.toml` in sync with it.
- Never put secrets in tracked files. The repository is public.

## Architecture (`src/`)

- `main.rs`: connects Valkey/Twitch, subscribes to EventSub events, builds
  the `Injector` (`bot::services`, shared with `mock_chat`), and starts the
  main `select!` loop: chat messages → `Runner::process_message`; Noita
  events, EventSub events (converted to `events::ChannelEvent`) and Ko-fi
  events → the handlers in `events.rs`. Redemption reward titles (e.g.
  `"Buy 5 charges"`) are matched as strings.
- Hot restart: at start, a new instance publishes `bot-restart` over Valkey
  pub/sub (`services/ipc.rs`), and the old instance stops. Interrupts go
  across instances on the `interrupt` channel.
- Stop: Ctrl-C and `die` pause the channel point rewards (not on a hot
  restart). A system shutdown, SIGTERM and SIGHUP also interrupt the
  running commands first. For the shutdown, the bot holds a logind
  `delay` inhibitor lock (`integration/logind.rs`, zbus): logind waits
  for it (at most `InhibitDelayMaxSec`, 5 s) while Valkey and the network
  still run; the lock is released after the deinitialization.
- `services/`: one trait per service, with Valkey/Elastic/real
  implementations plus in-memory/Noop/Mock implementations.
  - `Injector` (`services/mod.rs`) is a `TypeId` → `Arc<dyn Trait>` map.
  - `injector_getter!(Trait::getter)` makes a `TraitExt` trait so that
    `ctx.getter()` works. With `{ expr }`, `expr` is the default that is used
    when no implementation is registered. Tests rely on these defaults.
- `context/`: `AppContext` (Injector + messaging + interrupts + quit) →
  `MessageContext` → `EvalContext` (owner, macro args, locals, nesting
  depth) → `CommandContext` (command metadata + parsed command). Each derefs
  to its parent.
- `commands/`:
  - `#[command(...)]` proc-macro (`macros/`) on an
    `async fn(ctx: CommandContext, args...) -> CommandResult` registers a
    `NativeCommand` through `inventory`. The name is the fn name with
    `_` → `-`. Doc comments become the help/docgen text.
  - Attributes: `permission = <PermissionLevel>`, `global_gate`/
    `sender_gate = 5s` (durations), `shortcode = x` (must be unique), `cost`
    (charges), `free_for = <PermissionLevel>`, `free_if = <fn>` (an
    `async fn(&CommandContext) -> anyhow::Result<bool>`; `true` makes this
    use free; docgen writes `free_if: true`), and bare `CommandTag`s
    (`Hidden`, `NoWall`, `NoitaControl`, `NoitaData`, `GlobalMacroExempt`).
  - Argument types implement `CommandArg`/`ArgExtractor` (`commands/args.rs`):
    `String`, numbers, `bool`, `Duration`, `Chatter`, `ChargesAmount`
    (exact, `all` or `<n>%`; the command resolves it against a balance),
    `HoldTime<DEF, MAX>`, `InRange<A, B>`, `Required<T>`, `RestOfArgs`,
    `RawScript`, `Script`, etc.
  - `runner.rs` evaluates a parsed `neca_cmd::Statement`: `|`-separated
    groups run in parallel, commands in a group run in sequence. It does the
    lookup of commands and user/global macros, permissions, gates, costs, and
    variable expansion. Macro recursion limit is 3. `fail!(...)` returns a
    user-visible precondition error. The last error per user is stored in
    `storage:last-error:<uid>`.
  - `main.rs` removes `NoitaControl` commands before it gives the command
    map to `Runner`, so they are disabled.
- `defs/`: the chat commands, by area (betting, economy, macros,
  moderation, sounds/music, stats, data, util).
- `integration/`: external APIs (Twitch Helix/OAuth, EventSub, YouTube,
  ntfy, Ko-fi payload, websocket, justfile runner).
- `services/noita.rs`: polls the Noita process with `noita-engine-reader`
  and emits `NoitaEvent`s (death, win, items, pillars from
  `data/pillar-names.yml`, etc.).
- `services/display.rs`, `status_wall.rs`, `music/` (+ `music-player.css`/`.js`):
  axum/maud HTTP servers for OBS browser sources, on `browser-source-bind`
  and `music-player-bind`. All the pages on `browser-source-bind`
  (display keys, event list, chat box) use one server.
- `services/music/`: the music player. The queue is in Valkey
  (`music:requests`, then the rotating shuffled `music:playlist`;
  `music:history` head is the current song). A `Song` has a `Media`
  (what it is: a YouTube video or a file in the `[music] folder`); the
  server picks a `Playback` for the page (YouTube player or `<audio>`
  with a `/media/...` URL, from `ServeDir`). A disk cache of YouTube songs
  can plug in there. `reset` (↺) takes the first song of the YouTube
  playlist as the intro. The playlist is the rest of the YouTube playlist,
  the folder (`library.rs`, tags with `lofty`, else `Artist - Title` file
  names) with `[music] mode = "replace"` (default), or both, shuffled
  together, with `mode = "both"`. Requests are always YouTube.
- `services/live/` (+ `live.js`): the live updates of all the pages, over
  a WebSocket at `<page>/events` (JSON `{event, data}`; `live(path,
  handlers)` in the page connects again after a disconnect).
  `live::with_snapshot` gives each page its full state first and after it
  lags behind (display keys: `html`, event list: `list`, chat box: `reset`,
  music player: `state`, which makes the page load `/state`). Do not use
  SSE: Chromium (also OBS) has a maximum of 6 HTTP/1.1 connections to one
  host for all the browser sources, and each SSE stream holds one, so with
  6 pages other requests (e.g. the chat box images) wait forever.
- `services/event_list/` (+ `event-list.css`/`.js`): the event list
  browser source at `/event-list` on `browser-source-bind` (follows, subs,
  gift subs, cheers, Power-ups, raids, charity and Ko-fi donations). It
  replaces a Streamlabs widget. `handle_channel_event`/`handle_kofi_event`
  add the entries. The page shows the 3 newest; each `list` event has the
  full shown list, so a reloaded or reconnected page is always correct.
  Valkey keeps the entries as shown: a new event of the same user and kind
  as a shown entry (bits, which include Power-ups; gift subs; Ko-fi
  donations and charity donations in the same currency; anonymous users
  are one user) merges into it. The entry keeps its id, gets the sum and
  moves to the top; the page moves it there and counts the number up.
  Twitch often sends a resub as a sub first: a resub replaces a shown sub
  of the same user in the same way (the page crossfades the text, by
  `data-kind`), and a sub of a user with a shown resub is not added.
  Bits have the color of the Twitch cheermote tier of the sum.
- `services/chat_box/` (+ `chat-box.css`/`.js`): the chat browser sources
  `/chat` (the chat) and `/chat/last` (only the last message), on
  `browser-source-bind`. They replace the Streamlabs chat box and use its
  HTML structure and custom CSS. The messages come from a separate
  anonymous IRC connection (so the bot's own messages are there), with
  `CLEARMSG`/`CLEARCHAT` deletions. The last 100 are in memory; at start
  they come from the chat log (`ChatLogService::recent_irc` rebuilds the
  IRC messages), after `storage:chat-box:cleared-at`, which
  `ChatBoxService::clear` sets at stream start. The pages get the full list
  on each (re)connect (`reset`), then `message`/`replace`/`delete`/
  `delete-user`/`clear`. Emotes, badges and cheermotes: Twitch (Helix), 7TV
  (REST v3 for the sets, EventAPI v3 for live set and cosmetic changes,
  GraphQL v4 for the badges, paints and personal emotes of each chatter,
  cached for 1 h),
  BTTV, FFZ (with the global sets of some users) and Chatterino badges; all
  refreshed every 30 min and at stream start. A message never waits for
  the data of its chatter (7TV) or shared chat channel (`cache.rs`): it is
  shown with the cached data, and when the data was not fresh, it is
  fetched in the background and the message is sent again as `replace`.
  Failed fetches are kept for 1 min. Also: replies (the leading
  mention is removed), mentions, `USERNOTICE`s (parsed as a `PRIVMSG`
  with the same tags in `message.rs`, because twitch-irc drops the
  notices with unusual `msg-param-*` tags), highlights and Power-ups,
  shared chat (badges and avatar of the source channel), Twemoji, BTTV/FFZ
  modifiers. `chat-box.js` sizes the emote layers, so that zero-width
  emotes are as high as the emote. Images go through
  `/chat/img/<host>/<path>`, a disk cache in
  `$XDG_CACHE_HOME/limatoukka/chat-images` (`images.rs`). In `mock_chat`
  the chat box shows the terminal chat. Manual tests: `real_preview`
  writes `target/chat-box-preview.html` from the real data and backlog,
  `edge_cases_preview` writes `target/chat-box-edge-cases.html` from fake
  messages with edge cases, `live_channel` serves the chat of any channel
  (`CHAT_BOX_CHANNEL`, on `localhost:7458`) until it is stopped.

## Data and conventions

- Valkey key prefixes: `storage:<key>`, `caches:<cache>:<key>` (TTL, e.g.
  `caches:names:<uid>` for display names), `charges:<uid>`,
  `gate:<user>:<key>`, `macros:<owner>`, `vars:<owner>`,
  `kick:begone:<uid>`, `event-list` (JSON entries, newest first, at most
  50; follows of one user are listed once per 24 h with
  `caches:event-list-follow:<uid>`). Settings are storage keys such as
  `storage:setting:stop`.
- `Charges` are stored as integer thousandths (`Charges::whole(1)` = 1000).
  The display format drops trailing fraction zeroes and adds `⚡︎`
  (`1.2⚡︎`). The `top-charges` recipe copies this format. The bot account
  (`Limatoukka`) is the burn destination and is not part of the economy.
- `PermissionLevel` is ordered, `Caster` (7) is the highest.
- A module that has non-Rust files (JS, CSS, data for `include_str!`) is a
  folder, `<module>/mod.rs`, with these files beside `mod.rs` (as
  `chat_box/`, `event_list/`, `music/`, `live/`). Do not put them
  loose in `services/`.
- Chat text is intentionally informal/memey (Twitch emotes such as `ICANT`,
  `POGGIES`). Keep this tone for new bot messages.

## Other folders

- `macros/`: the `#[command]` proc-macro crate (own `Cargo.lock`).
- `lexer/`: a WASM + webpack demo that shows how `neca-cmd` parses a chat
  message (`npm run build`/`serve`). It is a separate crate, not a workspace
  member; it also patches `neca-cmd` to `../../neca-cmd`.
- `sounds/`: only `_meta.yml` (sound ids → file variants, volume, rarity,
  message) and `.gitignore` are tracked. The `.ogg` files are not tracked.
- `lisp/`: a **separate nested jj repository** (a Lisp dialect, `.tpl`
  files, with LSP and a Zed extension). It is not part of this repository
  and not a dependency of the bot. Do not track its files here.
- `.luarc.json`: Lua LSP globals, a leftover.

## Version control

- jj, colocated with git. Auto-tracking is disabled: `jj file track` new
  files.
- Commit titles: conventional commits with a scope for the area, e.g.
  `fix(music): ...`, `chore(justfile): ...`.
