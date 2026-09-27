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
  commands to stdout), `mock_chat` (a REPL that writes lines to
  `/tmp/tpn-bot.fifo`, read by `messaging::connect_to_mock`, which is not
  connected in `main.rs` at the moment).
- Tests: `direnv exec . cargo test`. Tests marked `#[ignore = "manual test"]`
  need real config/credentials.
- Logging: `tracing`; filter from `RUST_LOG`, default `limatoukka=info`.

## Runtime dependencies

- Valkey (Redis) at `config.valkey`: all persistent state.
- Elasticsearch: chat log (`[elastic]`) and command stats (`[stats]`).
- Twitch: IRC (`twitch-irc`) for chat, Helix + EventSub websocket
  (`twitch_api`). User tokens are in the system keyring (service
  `limatoukka-the-twitch-bot`). If there is no token, an OAuth flow starts
  with a local axum server on `redirect-url`.
- YouTube Data API v3 (music player), ntfy (Ko-fi webhooks as topic `kofi`).
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
  the `Injector` with all services, and starts the main `select!` loop:
  chat messages → `Runner::process_message`; Noita events, EventSub events
  (rewards, subs, raids, ads, stream on/off) and Ko-fi events → handler
  functions in this file. Redemption reward titles (e.g. `"Buy 5 charges"`)
  are matched as strings.
- Hot restart: at start, a new instance publishes `bot-restart` over Valkey
  pub/sub (`services/ipc.rs`), and the old instance stops. Interrupts go
  across instances on the `interrupt` channel.
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
    (charges), `free_for = <PermissionLevel>`, and bare `CommandTag`s
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
- `defs/`: the chat commands, by area (betting, economy, macros, moderation,
  sounds/music, stats, data, util).
- `integration/`: external APIs (Twitch Helix/OAuth, EventSub, YouTube,
  ntfy, Ko-fi payload, websocket, justfile runner).
- `services/noita.rs`: polls the Noita process with `noita-engine-reader`
  and emits `NoitaEvent`s (death, win, items, pillars from
  `data/pillar-names.yml`, etc.).
- `services/display.rs`, `status_wall.rs`, `music.rs` (+ `music-player.html`):
  axum/maud HTTP servers for OBS browser sources, on `browser-source-bind`
  and `music-player-bind`.

## Data and conventions

- Valkey key prefixes: `storage:<key>`, `caches:<cache>:<key>` (TTL, e.g.
  `caches:names:<uid>` for display names), `charges:<uid>`,
  `gate:<user>:<key>`, `macros:<owner>`, `vars:<owner>`,
  `kick:begone:<uid>`. Settings are storage keys such as
  `storage:setting:stop`.
- `Charges` are stored as integer thousandths (`Charges::whole(1)` = 1000).
  The display format drops trailing fraction zeroes and adds `⚡︎`
  (`1.2⚡︎`). The `top-charges` recipe copies this format. The bot account
  (`Limatoukka`) is the burn destination and is not part of the economy.
- `PermissionLevel` is ordered, `Caster` (7) is the highest.
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
