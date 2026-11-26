# RustMeeter / Automatools — system documentation

Reference for what this workspace is and how the pieces fit. Contributor workflow lives in
[`AGENTS.md`](AGENTS.md); user-facing setup and CLI usage live in [`README.md`](README.md).

## 1. What it is

A small Voicemeeter-style audio mixer for Windows, written in Rust, plus the AI glue around it:

- Mix real inputs (microphones) and **WASAPI loopback taps** (any output device used as a source).
- Per strip: gain, mute, **parametric EQ** (up to 10 bands), A/B routing, level meter.
- Two output buses (A, B), each to any output device, with master gain/mute/meter.
- **Live captions**: a passive pre-fader copy of one strip is cut into utterances by an energy VAD
  and transcribed in-process by the Cactus **Whistle** speech model.
- **Tool calling**: each finished caption can be sent to the Cactus **Needle 3** text agent, whose
  function calls are dispatched to handlers registered in this repo.

Everything user-visible is a *frontend* over one library. The heavy lifting (audio engine, settings,
caption/tool workers, and the local API) lives in `rustmeeter-core`; the GUI and CLI are thin
frontends. The GUI and `rustmeeter-cli serve` expose a JSON-over-TCP API on loopback for external
scripts (see §5).

## 2. Workspace map

```
Cargo.toml                  workspace: 5 members, edition 2024, resolver 2
.cargo/config.toml          forces target x86_64-pc-windows-gnullvm + llvm-mingw linker
crates/
  cactus-core/              FFI wrapper around libcactus.a (Whistle / Needle / Mirage)
  rustmeeter-core/          engine + service + settings + toolcall + local IPC (everything shared)
  rustmeeter/               GUI frontend (windows-reactor / WinUI 3), no audio or settings logic
  rustmeeter-cli/           CLI frontend (`list`, `run` + REPL, `serve` = headless IPC)
  recorder/                 standalone Whistle fine-tuning dataset recorder
finetune/                   sample datasets produced by recorder (gitignored)
cactus/                     libcactus.a + *.cact models (gitignored)
```

### Crate roles and dependencies

| Crate | Kind | Depends on | Notes |
|---|---|---|---|
| `cactus-core` | lib `cactus_core` | `libcactus.a` (native) | Always links the native engine via `build.rs`. |
| `rustmeeter-core` | lib `rustmeeter_core` | cpal, ringbuf, biquad, chrono, anyhow, regex, serde, serde_json; optional `cactus-core` | Feature `cactus` (default) pulls in the models. Settings, toolcall and IPC are always available. |
| `rustmeeter` | bin `rustmeeter` | `rustmeeter-core`, windows-reactor/canvas/core/text | GUI. |
| `rustmeeter-cli` | bin `rustmeeter` | `rustmeeter-core` | CLI. |
| `recorder` | bin `recorder` | cpal, crossterm, hound | Independent of the mixer. |

**Binary name collision.** `rustmeeter` (GUI) and `rustmeeter-cli` both produce a binary named
`rustmeeter`, so a plain `cargo build --release` writes both to the same
`target/<triple>/release/rustmeeter.exe` and one overwrites the other. Build and run them by
package (`-p rustmeeter` / `-p rustmeeter-cli`) rather than relying on the output filename.

## 3. Core engine (`rustmeeter-core`)

Public API is re-exported from `src/lib.rs`: `Mixer`, `Strip`, `Bus`, `MixerView`/`StripView`/`BusView`,
`Params`, `Tap`, `Event`/`EventHandler`, `EqSettings`/`EqBand`/`EqKind`, the device helpers,
and (with `cactus`) `CaptionController`, `CaptionLogger`, `CaptionStatus`, `Transcriber`,
`SharedTranscriber`, `Agent`, `Whistle`, `Mirage`.

The frontend-facing additions are:

- `settings` — `Conf`, `Settings`, `StripSettings`, `BusSettings`, `app_config_dir`. The JSON
  document, migration and atomic write live here, not in the GUI.
- `Service` — owns `Mixer`, `CaptionController`, `ToolCallHandle` and `Conf`, restores the saved
  rack/buses, debounces settings writes, and exposes `add_strip`/`remove_strip`,
  `captions_on`/`captions_off`, `set_toolcall_enabled`, `toolcall(text)` and `transcribe(path)`.
  `ServiceCallbacks` routes events/captions/tool reports back to the frontend.
- `toolcall` — `ToolCallHandle` (caption worker), `run_once` (synchronous), `Tool`/`Registry`,
  built-in handler `switch_light`.
- `wav` — a small WAV reader (`read_pcm16k`) used by `transcribe`.
- `ipc` — `Server` (background TCP accept threads + polled dispatch) and `dispatch`. See §5.

### Threading model

- **`Params`** is `Send + Sync` and lock-free (atomics for gain/mute/routes/level; a mutex-guarded
  `Arc<EqSettings>` snapshot plus an atomic version counter for EQ). Any thread may hold an
  `Arc<Params>` and call setters; changes take effect on the next audio buffer with no rebuild.
- **`Mixer`** owns the cpal streams and is **not** `Send`-safe by convention: keep it on the thread
  that created it (the GUI keeps it on the UI thread; the CLI on `main`).
- Structural edits (`add_strip`, `remove_strip`, `set_bus_device`) only mutate config and set
  `dirty`; `apply()` rebuilds the streams if running (a brief audible glitch).
- `Mixer::start()` drops old streams first, then builds new ones and fires `Event::Started`.
- Audio-thread callbacks reach the frontend through the `EventHandler` set with
  `set_event_handler`. The handler may run on any thread; keep it short.

### Signal path

```
input stream (cpal callback)
  ├─> TapShared ring (mono mix of L/R)   [only while a Tap is subscribed]
  └─> per-bus HeapRb<f32> (interleaved stereo, ~0.5 s)
        └─> Reader (resample by linear interpolation, EQ biquad chain, gain, mute)
              └─> BusMixer scratch (sum, bus gain, clamp, peak meter)
                    └─> output stream (cpal callback)
```

Details worth knowing:

- **Resampling** is linear interpolation between input samples; no anti-aliasing filter. Buffer
  bloat above 250 ms is drained down to ~50 ms (clock-drift control).
- **EQ** is applied per `(strip, bus)` reader. `Reader::refresh_eq` rebuilds coefficients only when
  `eq_version` changed, using `try_lock` so it never blocks the audio thread; filter state is kept
  for bands that stay in place so dragging a dot does not click.
- **Meters** are post-fader peaks with a 0.85/buffer decay, written by the audio thread into
  `Params::level`. Strip meters are enabled only for strips feeding the first bus with a device.
- **Feedback protection**: `is_feedback(source, bus_device)` drops a route whose source listens to
  the very device the bus plays into (including `CABLE Output` ↔ `CABLE Input`). Dropped routes are
  reported once via `Event::Warning`, and `MixerView::feedback_loops()` recomputes the same
  condition from live data so a GUI can explain it before audio starts.
- **Sample formats**: f32/i16/u16/i32 inputs and outputs are supported; anything else fails the
  stream build with a clear error.

### Device model (`devices.rs`)

- `InputSource { name, loopback }`; `label()` prefixes `[Loopback] ` for output taps.
- `list_inputs()` returns real inputs **plus** one loopback entry per output device.
- `fed_by(name, loopback)` answers "which render endpoint feeds this capture": itself for a
  loopback tap, the cable peer for a capture endpoint, `None` for a microphone.
- `capture_peer(render_name)` / `swap_io` flip the first whole-word `Input`/`Output` token
  (case variants handled, word boundaries enforced) so `CABLE Input (...)` ↔ `CABLE Output (...)`.
  This is deliberately pure string work so it can run per frame without touching COM.
- `same_device` compares trimmed, case-insensitively.

### Tap (`tap.rs`)

Every strip owns a `TapShared` ring (~8 s) from creation; it stays dormant (one atomic load per
buffer) until `Mixer::subscribe(id)` attaches a `Tap`. `Tap::read_16k` drains and box-downsamples
to 16 kHz mono i16. One active tap per strip; dropping the `Tap` disables it. Captions use this, so
they hear the raw pre-fader, pre-mute, pre-EQ signal regardless of routing.

### Parametric EQ (`eq.rs`)

`EqSettings { bypass, bands }`, max `EQ_MAX_BANDS = 10`. Kinds: HighPass, LowShelf, Bell,
HighShelf, LowPass. Ranges: 20 Hz–20 kHz, ±15 dB (pass filters ignore gain), Q 0.1–10.
`coefficients(fs)` builds `biquad` coefficients; `response_db(freq, fs)` computes magnitude
response in dB straight from those coefficients (used for the GUI curve, not for audio).
All setters clamp/sanitize; pass-filter gain is forced to 0.

### Live captions

Pipeline: `Tap` → `Segmenter` (VAD) → `Transcriber` → log line + callback.

- `vad.rs`: 20 ms frames, noise floor adapts while silent (`-80…-35 dB`, threshold = floor + 12 dB
  floored at −50 dB), 200 ms pre-roll, 30-frame (~600 ms) hangover, ignores blips shorter than
  200 ms, hard cap 25 s. Emits `Utterance { samples, started_at }`.
- `captions.rs`: `CaptionLogger` owns a thread that ticks every 20 ms, reads the tap, pushes to the
  segmenter, transcribes finished utterances under the shared transcriber mutex, appends
  `[timestamp] [label] text` to the log and flushes. `CaptionStatus` exposes live level/history,
  speech/busy flags, utterance count, last transcription time and the last error; the level history
  is a 96-sample ring (~2 s) and decays rather than snapping to zero.
- `caption_controller.rs`: `CaptionController` loads `whistle.cact` lazily on the first `on()` and
  keeps the loaded model for later on/off cycles (Cactus is process-wide and cannot reload).
  `on(mixer, strip)` picks a strip (1-based; `None` = first non-loopback strip) and subscribes a tap;
  `off()`, `active_strip()`, `status()`, `latest()`. Every successful session clears `latest`.
- With `--no-default-features`, `load_asr` bails with a message telling the user to rebuild.

### Cactus engine wrapper (`cactus-core`)

- `Cactus` is a thin safe handle over the C API: `load`, `models`, `require`, `set_threads`,
  `reset`, `init`, `complete`, `embed`, `set_audio`, `transcribe`, `stream_process`,
  `stream_stop`, `describe`.
- `Models` bitflags: TEXT=1, SPEECH=2, VISION=4; `cactus_load` merges archives, so one binary can
  hold several `.cact` files at once.
- The runtime is process-global with **one conversation shared by every facade**. All callers must
  serialize. `rustmeeter-core::cactus_call_lock()` provides that mutex and is taken around Whistle
  transcription and Needle completions.
- Model bytes are `Box::leak`ed deliberately (the engine keeps pointing into them), and a global
  `LOADED` set keyed by path prevents double-loading. `cactus_load` is serialized by `LOAD_LOCK`.
- Facades: `Whistle` (implements `Transcriber`, parses `text`/`response`/`audio_text`/`transcript`
  out of the JSON envelope, plus streaming), `Agent` (Needle 3 text/tools/embeddings), `Mirage`
  (vision describe).

## 4. GUI frontend (`crates/rustmeeter`)

windows-reactor (WinUI 3) application. It contains **no audio code** — only `rustmeeter-core`
types. Module layout:

```
src/main.rs        RustMeeterApp: tray icon, menu, window registration, icon extraction
src/app/mod.rs     SharedMixer wrapper around core Service, layout, message loop, IPC pump
src/app/toolbar.rs bus A/B pickers + "add source" combo
src/app/strips.rs  horizontally scrolling strip cards (route chips, mute, EQ entry, fader)
src/app/buses.rs   side panel with the two bus cards
src/app/status.rs  bottom bar: engine state, caption state dot + level graph, counters/errors
src/app/transcript.rs  current caption line (large, centred)
src/app/caption_window.rs  standalone always-on-top caption bar opened from the tray
src/app/eq/        full-window parametric EQ modal: Direct2D graph + band table
src/fader.rs       custom vertical channel fader (dB taper, 0.5 dB snap, segmented meter)
src/hslider.rs     compact horizontal slider for the EQ table
src/theme.rs       palette + reusable view builders (card, chip, badge, spark, vmeter)
src/component/      AcButton (hover/press states, rounded sides)
src/util.rs        display/domain helpers (colour maths, dB formatting)
src/messages.rs    Message enum sent to the UI thread
```

The GUI no longer owns `config.rs` or `toolcall/`; both moved to `rustmeeter-core`.

### Shared state

`SharedMixer` (`Rc<RefCell<>>`, single UI thread) is a thin wrapper: a core `Service` plus the single
`completion_slot` used to forward engine/caption/toolcall events into whichever window component
exists. It implements `Deref`/`DerefMut` to `Service`, so panels keep writing `shared.mixer`,
`shared.config`, `shared.captions`, `shared.toolcall`, etc. Closing a window drops only UI state;
audio keeps running. `MixerApp::drop` flushes settings, because the debounce timer will never fire
again after close.

`MixerApp` also owns the optional IPC `Server`. A separate 100 ms `Message::IpcTick` timer polls it
even when audio is stopped (the meter tick stops when idle), executing requests on the UI thread so
the `Mixer` never moves. `RUSTMEETER_IPC_PORT` overrides the port; `0` disables it.

### Settings file (`%APPDATA%\rustmeeter\config.json`)

Written atomically (`config.json.tmp` → rename) about 600 ms after the last change, and again on
exit. Hand-editable JSON; values are clamped on load, empty entries are dropped. Layout:

- `model_root` — folder holding `whistle.cact` / `needle3.cact`.
- `strip_order` — the rack, in order. Absent/`null` = no rack recorded yet (first run or an old
  file, migrated once from `strips`); `[]` = deliberately emptied.
- `strips` — per-source settings keyed by `InputSource::label()` (device name, `[Loopback] `
  prefixed), so a device comes back configured even after being removed. `eq`, `gain_db`, `muted`,
  `routes`; fields at defaults are omitted, and a fully-default entry removes its key.
- `buses` — per-bus `device`, `gain_db`, `muted`.
- `toolcall_enabled`, `tools` — Needle schemas passed verbatim to the engine; `triggers` regexes
  additionally gate which captions are sent to the model.

Also in that folder: `app.ico` (written by the GUI from its embedded asset), `captions.log`,
`toolcall.log`, and `config.broken.json` if the settings file had to be moved aside.
`Conf::load_from(root)` allows a portable/test directory; `Conf::write_icon(bytes)` lets a GUI
provide the icon without the core knowing what an icon is.

### Tool calling

`ToolCallHandle` owns a worker thread. Finished captions are queued by the caption callback
(`submit`, cheap and non-blocking). The worker:

1. skips text that matches none of the tools' `triggers` regexes (if any are declared);
2. lazily loads `needle3.cact`, `init`s the schemas, then `reset`s the conversation before every
   completion so captions stay independent turns;
3. runs the completion under `cactus_call_lock`;
4. parses `function_calls` and executes each through `Registry` (name → `Tool` impl).

Handlers live in `crates/rustmeeter-core/src/toolcall/tools.rs`; the schema lives in `config.json`.
A tool only runs if a handler is registered — currently only `switch_light` (shells out to
`tool wiz <command>`). Every decision is appended to `toolcall.log`. `toolcall::run_once` is the
synchronous path used by IPC; it ignores triggers because the caller explicitly asked for that line.

### Notable UI behaviours

- Meters tick at ~30 fps only while audio runs; the tick doubles as the settings-write heartbeat.
- Route chips show three states: normal, **stranded** (routed but bus has no device, amber) and
  **feedback** (would loop, red, route dropped by the engine).
- Bus cards say what the bus *means*: `WHAT YOU HEAR` for a real output, `MIC FOR APPS` for the
  render side of a virtual cable, naming the capture endpoint apps should select.
- The EQ editor is a modal with a Direct2D-rendered response curve (opaque surface paints well,
  grid, 0 dB line and labels; dots stay XAML so they keep hit-testing). Band table columns flex
  with the window width; Q and frequency sliders are logarithmic.
- The caption window never touches `completion_slot` (it is typed to the mixer's `Message` and
  single); it polls on a 33 ms timer while active and 250 ms when idle.

## 5. Local IPC (external scripts)

The GUI (on by default, port 7590) and `rustmeeter-cli serve` expose a line-oriented JSON API on
`127.0.0.1`. Nothing is bound to the network. Request/response shapes:

```json
{"id": 1, "method": "list_devices", "params": {}}
{"id": 1, "ok": true, "result": {"inputs": [...], "outputs": [...]}}
{"id": 1, "ok": false, "error": "no strip 3"}
```

`id` is echoed verbatim. Methods:

| Method | Params | Notes |
|---|---|---|
| `ping` | — | version + `pong`. |
| `list_devices` (`list_device`) | — | inputs with labels/loopback flag, outputs, defaults. |
| `status` / `snapshot` | — | mixer view: strips (id, routes, gain, mute, EQ, level), buses. |
| `enable_mixer` / `enablemixer` / `start` | — | start audio. |
| `disable_mixer` / `disablemixer` / `stop` | — | stop audio (also flushes settings). |
| `add_strip` | `name`, `loopback`, optional `routes` | returns the new strip id. |
| `remove_strip` | `id` or 1-based `strip` | keeps the device's settings on disk. |
| `set_bus` | `bus` (0/1 or `"a"`/`"b"`), `device` or null | rebuilds streams. |
| `set_gain` / `set_mute` | `bus` or `strip`/`id`, plus `db` / `muted` | live, no rebuild. |
| `set_route` | `strip`/`id`, `bus`, `on` | live. |
| `set_eq` | `strip`/`id`, `eq` (same JSON as the settings file) | live. |
| `captions_on` / `captions_off` | optional `strip` | loads Whistle lazily. |
| `set_toolcall` | `enabled` | persists the flag. |
| `toolcall` | `text` | runs Needle now; blocks while the model works. Returns calls and error. |
| `transcribe` | `path`, optional `language` | decodes a WAV (PCM/float, any rate) to 16 kHz and runs Whistle; blocks. |
| `save` | — | flush settings. |
| `shutdown` | — | stop audio; `serve` exits. |

The socket threads only read/write; `Server::poll` executes requests on the thread that owns the
`Mixer` (the GUI UI thread, or `serve`'s main loop). `toolcall` and `transcribe` therefore block
that poller; they are deliberate, synchronous calls rather than shortcuts around the audio model.

## 6. CLI frontend (`crates/rustmeeter-cli`)

`rsmtr list` prints inputs/outputs. `rsmtr run [--bus-a DEV] [--bus-b DEV]
[--mic DEV[:AB]]... [--loopback DEV[:AB]]... [--captions [FILE]] [--whistle PATH]
[--caption-strip N] [--caption-lang CODE] [--caption-threads N]` starts audio and drops into a
line-oriented REPL (`status`/`s`, `levels`/`l`, `gain`, `mute`, `eq ...`, `route`, `add`,
`remove`, `bus`, `captions ...`, `help`, `quit`). Device names are case-insensitive exact matches
first, then substrings; `default` means the Windows default device. `--mic` defaults to bus B,
`--loopback` to bus A, overridable with `:A`/`:B`/`:AB`. Structural commands call `Mixer::apply()`
and briefly restart audio. Captions print each line to stdout as well as the log file.

`rsmtr serve [--port N] [--no-start]` is the headless equivalent of the GUI: it opens the
saved rack from `config.json` via `Service::open`, starts audio, and serves the same IPC API.

## 7. Recorder (`crates/recorder`)

Standalone crossterm TUI for building Whistle fine-tuning datasets. Hotkeys: `1/2/3` pick the
split, `r` record, `s` stop+save+prompt for transcript, `t` retry transcript, `h` help, `q` quit.
Records the default input device downmixed to mono 16-bit PCM WAV at its native rate, into
`audio/{train,validation,test}/recording_<ms>.wav`, and appends
`{"audio":"...","text":"...","language":"id"}` to `finetune/<split>.jsonl` only after a non-empty
transcript. Paths are **relative to the current directory**, so run it from the repository root.
`finetune/` holds sample data: `whistle.cact`, LoRA (`whistle_lora.safetensors`) and full
(`whistle.safetensors`) checkpoints, plus train/validation/test JSONL manifests.

## 8. Building and running

Environment prerequisites (see README §1):

- Rust stable 1.85+, target `x86_64-pc-windows-gnullvm`.
- llvm-mingw (default `C:\tools\llvm-mingw`) for the linker and static libc++/libunwind.
- `cactus/libcactus.a`; models `cactus/whistle.cact` (captions), optionally `needle3.cact`.
- The sibling checkout `E:\_CODE\rust\windows-rs` (windows-reactor, windows-canvas, windows-core,
  windows-text are path dependencies, not vendored).

```
cargo build --release                     # everything (see binary-name caveat above)
cargo build --release -p rustmeeter-cli   # CLI only
cargo build --release -p rustmeeter       # GUI only
cargo test  -p rustmeeter-core            # unit tests (needs libcactus.a)
cargo run   --release -p rustmeeter-cli --bin rsmtr -- list
```

`cargo-core/build.rs` panics with remediation hints if `libcactus.a` is missing; override the
folder with `CACTUS_LIB_DIR`, the toolchain dir with `LLVM_MINGW_DIR`, or the C++ runtime with
`CACTUS_CXX_LIB=stdc++`. Build without the engine at all with `--no-default-features` (captions
then report that the build has no Cactus).

## 9. Known rough edges

- **Binary name collision** between the GUI and CLI packages (§2).
- **IPC is polled on the owner thread**: `toolcall`/`transcribe` are synchronous and block the GUI
  UI thread while the model runs. A future version can give the IPC server its own command queue
  or run the slow methods on a worker with a completion method.
- **IPC has no authentication**: it binds `127.0.0.1` only, but any local process can connect.
  Set `RUSTMEETER_IPC_PORT=0` to disable it in the GUI.
- **One IPC owner per port**: starting `serve` while the GUI is running fails to bind unless a
  different `--port` is given.
- **Working tree is mid-refactor**: check `git status` before assuming the committed state.
- **`task.nu`** still invokes the GUI-named binary with CLI arguments
  (`rustmeeter.exe run ...`), which only works if the last build happened to be the CLI.
- `recorder` hardcodes `LANGUAGE = "id"` and uses backslash-relative manifest paths.
- Audio is resampled by linear interpolation and meters decay rather than follow peaks exactly;
  these are deliberate simplicity trade-offs.
