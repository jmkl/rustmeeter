# AGENTS.md

Guidance for agents working in this workspace. Background and architecture: [`DOCS.md`](DOCS.md).
User-facing setup/usage: [`README.md`](README.md).

## What this is

A Windows Voicemeeter-style mixer in Rust (`rustmeeter-core`) with a CLI and a WinUI 3 GUI over it,
plus Cactus model glue (`cactus-core`: Whistle speech → live captions, Needle 3 text → tool calls)
and a Whistle fine-tuning recorder.

## First, check the state of the tree

This repo is frequently mid-refactor. Before editing:

```
git status --short
git log --oneline -5
```

Recent sessions have left untracked new modules and deleted docs; do not assume `HEAD` matches
what is on disk, and do not "restore" deleted files without asking.

## Layout (where things live)

| Path | Role |
|---|---|
| `crates/rustmeeter-core/src/` | The library. Only this crate contains audio/engine code. |
| `.../mixer.rs` | `Mixer` high-level API + `MixerView`/`StripView`/`BusView`, feedback detection. |
| `.../params.rs` | Lock-free `Params` (gain/mute/EQ/routes/meter) shared with audio threads. |
| `.../engine.rs` | Internal cpal streams, resampling, EQ biquads, bus mixing. |
| `.../eq.rs` | EQ model, coefficient design, response curve. |
| `.../tap.rs`, `vad.rs`, `captions.rs`, `caption_controller.rs` | Caption pipeline. |
| `.../devices.rs` | Device discovery and cable-name matching. |
| `.../settings.rs` | `Conf`/`Settings`/`StripSettings`/`BusSettings`; `%APPDATA%\rustmeeter\config.json`, migration, atomic save. |
| `.../service.rs` | `Service`: owns `Mixer` + captions + tool worker + `Conf`; restore, debounced saves, API methods. |
| `.../toolcall/` | Needle worker (`ToolCallHandle`), `run_once`, `Tool`/`Registry`, `tools.rs` handlers. |
| `.../ipc.rs` | Line-delimited JSON over TCP on `127.0.0.1`; `Server` + `dispatch`. |
| `.../wav.rs` | Small WAV → 16 kHz mono decoder used by `transcribe`. |
| `crates/cactus-core/src/lib.rs` | FFI to `libcactus.a`; `Cactus`, `Whistle`, `Agent`, `Mirage`. |
| `crates/rustmeeter/src/` | GUI only; no audio/settings/tool logic. `app/mod.rs` wraps `Service` and pumps IPC. |
| `crates/rustmeeter-cli/src/main.rs` | CLI + REPL; `serve` = headless `Service` + IPC. |
| `crates/recorder/src/main.rs` | Dataset recorder. |

Frontends must use only the public `rustmeeter_core` API. Never put engine logic in the GUI/CLI.

## Build & test

Target is pinned to `x86_64-pc-windows-gnullvm` in `.cargo/config.toml` (llvm-mingw linker), and
`cactus-core/build.rs` requires `cactus/libcactus.a`. Without it the build panics; override with
`CACTUS_LIB_DIR` / `LLVM_MINGW_DIR` / `CACTUS_CXX_LIB`.

**Always address binaries by package.** The GUI (`rustmeeter` package) emits `rustmeeter.exe`;
the CLI package emits `rsmtr.exe` (its `[[bin]]` in `crates/rustmeeter-cli/Cargo.toml`).

```
cargo build --release -p rustmeeter-cli
cargo run --release -p rustmeeter-cli --bin rsmtr -- list
cargo build --release -p rustmeeter
cargo test  -p rustmeeter-core        # unit tests, needs libcactus.a
cargo test  -p rustmeeter-core --no-default-features   # same tests, no native engine
```

`cargo test -p rustmeeter-core` covers device naming, EQ responses, params sanitising, feedback
detection, settings round-trips, WAV decoding, tool triggers and the service's write-comparison.
Add unit tests next to the code you change (this codebase does that consistently). There are no
integration tests that need real audio devices.

The GUI depends on the sibling checkout `E:\_CODE\rust\windows-rs` via path dependencies; a fresh
clone of only this repo will not build `-p rustmeeter`.

## Non-negotiable invariants

- **Keep `Mixer` on one thread.** It owns cpal streams and is not `Send`-safe by convention. The
  GUI keeps it on the UI thread inside `Rc<RefCell<SharedMixer>>`; the CLI keeps it on `main`.
- **`Params` stays lock-free for live controls.** Gain/mute/routes/level are atomics. EQ is an
  immutable `Arc<EqSettings>` swapped under a mutex and published through `eq_version`; the audio
  thread uses `try_eq()` and retries next buffer rather than blocking.
- **Structural changes go through `apply()`.** `add_strip`/`remove_strip`/`set_bus_device` mark
  dirty; never rebuild streams implicitly. A rebuild restarts audio (audible glitch) — that is why
  faders/routing/EQ must not require it.
- **Never block an audio callback.** Tap reads, level writes and event handlers are kept short;
  `EventHandler` runs on arbitrary threads.
- **Serialize Cactus access.** One process-global conversation. Use
  `rustmeeter_core::cactus_call_lock()` around any `Whistle`/`Agent` call, and do not load models
  twice (the wrapper guards by path already).
- **Captions read a tap, not the mix.** Pre-fader/pre-mute/pre-EQ by design; routing does not
  affect transcription.
- **IPC executes on the mixer's thread.** `ipc::Server` only does socket I/O on background threads;
  the frontend calls `poll` from the thread that owns the `Service` (GUI UI thread, or `serve`'s
  loop). Never move `Mixer` into the server. `toolcall`/`transcribe` are synchronous and block that
  poller by design; use a new IPC method if you need an async job.
- **IPC stays on loopback.** Bind `127.0.0.1` only, add methods in `ipc::handle` (not in the GUI),
  and keep every method expressible through the public `Service` API.

## Conventions

- Rust edition 2024, `anyhow::Result` in fallible code, `bail!`/`Context` for errors.
- GUI views are built with `windows_reactor` builders; reusable pieces go in `theme.rs`
  (`card`, `chip`, `badge`, `caption`, `spark`, `vmeter`), not inline.
- Messages flow to components as enums (`Message`, `CaptionWindow::Msg`); async work reports back
  through callbacks/channels and posts a message, never touches component state directly.
- Settings keys are **device labels**, not strip indices. Omitted fields mean defaults; empty
  entries are removed. Writes are debounced (~600 ms) and atomic (tmp + rename).
- Comments in this codebase explain *why*, at length, and often record a rejected alternative.
  Match that style; do not delete explanatory comments.
- Tool schemas live in `%APPDATA%\rustmeeter\config.json`; implementations live in
  `crates/rustmeeter-core/src/toolcall/tools.rs` and must be registered in `register()`.
  Triggers are case-insensitive regexes; no triggers means every caption is sent (avoid that in
  practice).

## Adding a Needle tool (the usual task)

1. Add a `Tool` impl in `crates/rustmeeter-core/src/toolcall/tools.rs` and register its name in
   `register()`.
2. Declare the same name plus schema (and `triggers`) under `"tools"` in `config.json`.
3. Remember the worker resets the conversation per caption; keep tools stateless across calls.
4. Check `toolcall.log` (next to `captions.log`) for the decision trace.

## Adding an IPC method

1. Add a branch in `crates/rustmeeter-core/src/ipc.rs::handle`.
2. Express it through `Service`/`Mixer`/`Params`; do not reach into frontend state.
3. Document the method in `DOCS.md` §5. Keep the request/response shape `id`/`ok`/`result|error`.
4. Test the pure parts (parser, WAV, settings) next to the code; socket behaviour is exercised by
   running `cargo run -p rustmeeter-cli -- serve --port 0` and a small client.

## Gotchas
- Feedback loops (`[Loopback] X` routed into a bus playing on X, or `CABLE Output` into
  `CABLE Input`) are dropped by the engine; surface them in the UI rather than silently muting.
- `recorder` writes relative `finetune\*.jsonl` paths — run it from the repository root.
- Model/`.cact`/`.a`/`finetune`/zip files are gitignored; do not commit them.
