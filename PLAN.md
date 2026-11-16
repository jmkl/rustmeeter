# PLAN: wiring the RustMeeter core to a GUI

Status: GUI library **not chosen yet**. Sections 1-7 are library-agnostic and can be done now.
Section 8 is the decision guide. Section 9 is the order of work.

## 1. Goal

A desktop window that does what the CLI prompt does today, plus things the CLI is bad at:

- faders, mute buttons, A/B route toggles, 3-band EQ knobs per strip
- live level meters for every strip and bus
- device pickers for bus A / bus B, and "add strip" (mic or loopback)
- captions panel: on/off, which strip, live scrolling transcript
- remember everything between runs

Non-goals for the first GUI: virtual driver, voice commands (Needle), per-app capture, multi-window.

## 2. What the core already gives (public API)

```
Mixer (src/mixer.rs)                      keep it on ONE thread (cpal streams are not Send)
  new()                                   add_strip(source, routes) -> StripId
  start() / stop() / is_running()         remove_strip(id) -> bool
  apply()  (rebuild streams if dirty)     set_bus_device(bus, Option<String>)
  strips() / strip(id) / bus(i)           subscribe(id) -> Option<Tap>   (no restart)

Params (src/params.rs)                    Arc<Params> per strip and per bus: Send + Sync, atomics
  gain_db()/set_gain_db(db)               -60..+12, clamped
  muted()/set_muted(bool)
  eq_db(band)/set_eq_db(band, db)         band 0 low, 1 mid, 2 high, +-12 dB (strips only)
  routed(bus)/set_routed(bus, bool)       strips only
  level() / level_db()                    post-fader peak, decays in the audio thread

Devices (src/devices.rs)                  list_inputs(), list_outputs(), default_input_name(),
                                          default_output_name(), InputSource { name, loopback }

Captions                                  CaptionLogger::start(tap, label, path, SharedTranscriber, on_caption)
                                          Whistle (feature "cactus"), share_transcriber(), Caption { time, label, text }
```

Two kinds of change, and the GUI must treat them differently:

| Kind | Examples | Cost | How |
|---|---|---|---|
| Live | gain, mute, EQ, routing, meters, captions on/off | instant, no glitch | write/read `Params` atomics, `subscribe()` |
| Structural | add/remove strip, change bus device | rebuilds streams, brief audio glitch, can fail | edit `Mixer`, then `apply()` |

## 3. Architecture

```
                 +----------------------- frontend (GUI crate / bin) ------------------+
                 |  widgets  <--read-- Arc<Params> (atomics)  --write-->  widgets      |
 user  --------> |  intents (add/remove/bus/captions)  ------------>  Session          |
                 |  transcript list  <------- mpsc channel <-------  caption callback |
                 +----------------------------------------------------------------------+
                                           |
                       rustmeeter lib:  Session { Mixer, Captions, Settings }
                                           |
                    Mixer -> engine (cpal input/output threads, lock-free rings)
                    Tap -> CaptionLogger thread -> Whistle (Cactus) -> log file
```

Rules that fall out of this:

1. The audio threads never talk to the GUI. They only read/write atomics.
2. The GUI never touches cpal. It only calls `Mixer` / `Params` / `Session`.
3. Anything that arrives from another thread (caption lines, errors) goes through an
   `mpsc` channel that the GUI drains once per frame/tick.

## 4. Core work to do first (small, library-independent)

Do these before or alongside the first GUI spike. Each is a self-contained PR.

| # | Gap | Why the GUI needs it | Sketch |
|---|---|---|---|
| C1 **(done)** | **Move `Captions` controller from `main.rs` into the lib** (now `CaptionController`, `src/caption_controller.rs`) | Today on/off/status logic lives in the CLI. The GUI must not re-implement it (model loads once; singleton). | `pub struct CaptionController { new(model, log, lang, threads), on(&Mixer, Option<usize>), off(), is_on() -> Option<usize>, subscribe_lines() -> Receiver<Caption> }`. CLI becomes a thin user of it. |
| C2 | **`Settings` (serde JSON) + load/save** | Remember strips, routes, gains, EQ, bus devices, captions config. | New `settings.rs`: `Settings { buses, strips: Vec<StripSettings>, captions }`; `Mixer::to_settings()` and `Mixer::apply_settings(&Settings)`. File: `%APPDATA%\rustmeeter\settings.json`. Missing device at load -> skip that strip and report a warning, never fail the whole load. |
| C3 **(done)** | **Event handler** (`Mixer::set_event_handler`, `Event::{Started, Error}`; callback-based like captions, so a GUI forwards events into its own channel) | Surface errors (stream failed, device vanished, transcribe error) without `eprintln!`. | `enum Event { Error(String), Warning(String), Caption(Caption), Restarted }` and `Mixer::events() -> Receiver<Event>`. Engine error callbacks send into it. |
| C4 **(done)** | **`Mixer::snapshot()` -> plain data** (`MixerView`, `StripView`, `BusView`) | Message-passing GUIs (and tests) want an owned, `Send` view instead of borrowing the Mixer. | `MixerView { running, buses: [BusView; 2], strips: Vec<StripView> }` with ids, labels, routes, gain, mute, eq. Meters stay on `Params`. |
| C5 | **Bus EQ (optional)** | Natural request: "EQ what I hear" on bus A. | `Params` already has `eq_db`; apply it in `BusMixer::render`. |
| C6 | **Device refresh helper** | Headsets get unplugged. | `list_*` on a button and on a slow timer (e.g. 5 s), never per frame. Later: detect a missing device in `apply()` errors. |
| C7 | **`start()` off the UI thread (optional)** | Opening streams can take 100+ ms and would freeze the window. | Fine for the spike. If it hurts, move the whole Mixer onto an "audio owner" thread (pattern B below). |

## 5. Wiring patterns

Pick by GUI style, not by taste. The core supports both.

### 5.1 Pattern A: Mixer lives in the UI thread (immediate-mode GUIs, e.g. egui)

Simplest. `Mixer` is a field of the app struct; widgets read and write `Params` directly.

```rust
struct App { session: Session, intents: Vec<Intent>, transcript: Vec<Caption>, error: Option<String> }

fn frame(&mut self, ui) {
    // 1. drain cross-thread messages
    while let Ok(ev) = self.session.events.try_recv() { /* Caption -> transcript.push, Error -> self.error */ }

    // 2. draw: read atomics every frame, write only on change
    for s in self.session.mixer.strips() {
        let p = &s.params;
        let mut g = p.gain_db();
        if vertical_slider(ui, &mut g, MIN_DB..=MAX_DB).changed() { p.set_gain_db(g); }
        meter(ui, p.level_db());
        // mute, A/B toggles, 3 EQ sliders: same pattern
        if ui.button("Remove").clicked() { self.intents.push(Intent::Remove(s.id)); }
    }

    // 3. apply structural intents AFTER drawing (never mutate the strip list while iterating it)
    for i in self.intents.drain(..) { match i { Intent::Remove(id) => { self.session.mixer.remove_strip(id); } ... } }
    if let Err(e) = self.session.mixer.apply() { self.error = Some(format!("{e:#}")); }

    // 4. keep meters alive
    request_repaint_after(33 ms);
}
```

Notes: `apply()` is a no-op unless the config is dirty, so calling it every frame is fine.
After a failed `apply()` the config stays dirty, so a "Retry" button just calls it again.

### 5.2 Pattern B: audio owner thread + messages (Elm-style / declarative / web GUIs: iced, Slint, Tauri)

These frameworks want an owned model and messages. Put the `Mixer` on a dedicated thread that creates it
itself (it is not `Send`), and talk to it with a channel.

```
GUI thread                                audio-owner thread
  Command::AddStrip{..}   -- mpsc -->      owns Mixer + CaptionController
  Command::SetBus{..}                      runs commands, calls apply(), sends Event::State(MixerView)
  Command::Captions{on,strip}
  <-- Event::{State, Caption, Error} --
  slider drag -> params.set_gain_db()      (direct, via the shared Arc<Params>: no channel for live controls)
  timer tick (30 Hz) -> read params.level_db() for meters
```

Key point: **live controls bypass the channel.** The GUI gets the `Arc<Params>` clones from the latest
`MixerView` and writes atomics directly, so faders stay instant. Only structural changes and captions
go through `Command`s.

## 6. Screen mapping (widget -> API)

| Widget | Read | Write |
|---|---|---|
| Strip fader (vertical) | `params.gain_db()` | `params.set_gain_db(db)` |
| Strip mute | `params.muted()` | `params.set_muted(b)` |
| Strip A / B toggles | `params.routed(0/1)` | `params.set_routed(bus, b)` |
| Strip EQ low/mid/high | `params.eq_db(band)` | `params.set_eq_db(band, db)` |
| Strip / bus meter | `params.level_db()` at ~30 Hz | - |
| Strip title | `strip.source.label()` | - |
| Add strip | `list_inputs()` (cached, refresh button) | `mixer.add_strip(src, routes)` + `apply()` |
| Remove strip | - | `mixer.remove_strip(id)` + `apply()` |
| Bus device dropdown | `mixer.bus(i).device`, `list_outputs()` | `mixer.set_bus_device(i, dev)` + `apply()` |
| Bus master fader / mute / meter | `mixer.bus(i).params` | same setters |
| Captions toggle + strip picker | `CaptionController::is_on()` | `.on(&mixer, strip)` / `.off()` |
| Transcript view | drained `Caption`s (time, label, text) | - (file logging is done by the logger) |
| Status line | `mixer.is_running()`, last `Event::Error` | - |

## 7. Rules for any frontend

- Never hold a lock or block inside the widget code. Atomics only.
- Write a param **only when the widget reports a change** (otherwise the GUI fights the CLI or MIDI).
- Read params **every frame**, so any other control surface is reflected.
- Structural intents are queued and applied after drawing; show errors from `apply()`.
- Do not smooth meters again in the GUI (the audio thread already decays them); peak-hold is fine.
- Device lists are cached; refresh on button or slow timer only.
- Keep the GUI in its own binary behind a feature so the CLI build stays small:
  `[features] gui = ["dep:<gui-crate>"]`, `[[bin]] name = "rustmeeter-gui" required-features = ["gui"]`.
  If it grows, turn the repo into a workspace: `rustmeeter` (lib + cli) and `rustmeeter-gui`.

## 8. Choosing the GUI library (not decided)

Facts to weigh. Check current versions and docs before committing; GUI crates move fast.

| Option | Style | Fits pattern | Pros for this app | Cons / risks |
|---|---|---|---|---|
| **egui / eframe** | immediate-mode | A | Fastest to wire (read atomics each frame, no state sync). Easy vertical sliders. Pure Rust. Already used in this project's earlier draft (eframe 0.37 API checked). | Breaking API changes almost every release (we already hit this). Default look is "tool-like"; meters are custom-drawn. Repaints continuously for meters. |
| **iced** | Elm-style, retained | B | Clean message architecture, matches the owner-thread design. Pure Rust. | More boilerplate; vertical faders and meters need custom widgets or canvas; subscriptions needed for the 30 Hz meter tick. |
| **Slint** | declarative `.slint` markup | B | Good-looking, designer-style layout; light runtime. | Separate DSL to learn; check the license terms for your use; binding atomics to properties needs a timer. |
| **Tauri (web UI)** | HTML/JS frontend + Rust backend | B (IPC) | Best looks, easy to style meters/faders with CSS. Familiar web tooling. | Heavier; JS/Rust boundary for meters (poll or event stream); more moving parts for a mixer. |
| **ratatui (TUI)** | terminal UI | A | Zero graphics, fits the current CLI/nushell workflow, very small. Good intermediate step. | Not a "real" GUI; no mouse-drag faders unless you add mouse handling. |

How to decide quickly: do a **1-day spike** with the same four widgets in the top two candidates:
one strip with a vertical fader, a mute toggle, a live meter, and an "add strip" dropdown that calls `apply()`.
Pick the one where the meter is smooth at 30 Hz with low CPU, and the code feels least awkward.

Provisional recommendation (change it if the spike disagrees): **egui/eframe first**, because the core
is already shaped for it (Mixer on the UI thread, `Params` as atomics, `Mixer::apply()` as the only
structural call). If the API churn or look becomes a problem, the core is unchanged and pattern B (iced or
Slint) is a port of the thin frontend layer only. Pin the exact GUI crate version in `Cargo.toml`.

## 9. Milestones

| M | Deliverable | Done when |
|---|---|---|
| M0 | Core prep: C1 (CaptionController in lib, done), C4 (snapshot, done), C3 (events, done). **M0 complete.** | CLI uses `CaptionController`; `cargo build` and `--no-default-features` both pass |
| M1 | Spike in 2 libraries (section 8), pick one. **egui spike written in `src/gui/main.rs` (untested); second library not started** | meter + fader + add-strip working in each; decision written in this file |
| M2 | GUI MVP: strips row, 2 buses, meters, mute, A/B, EQ, bus device pickers, status/error line | Replaces the CLI for daily use of the mixer |
| M3 | Captions panel: toggle, strip picker, live transcript, "open log" button | Captions on/off from the GUI with no audio glitch |
| M4 | C2 settings save/load, remember window size | Restart restores the whole setup |
| M5 | Polish: device refresh/unplug handling (C6), hotkeys (global mute), tray icon, light/dark | Daily-driver quality |
| M6 | Stretch: bus EQ (C5), Needle voice commands, own virtual driver | Separate plans |

## 10. Risks and open questions

- **Audio glitch on structural changes.** `apply()` restarts all streams. If this annoys in daily use,
  make `apply()` incremental (only rebuild the changed strip/bus); that is core work, independent of the GUI.
- **Device vanishes at runtime** (headset unplugged): today `start()` just errors. Needs C3 plus a "device
  missing" state per strip rather than failing the whole mixer.
- **Cactus engine is a process-wide singleton**: only one transcriber, loaded once. The GUI must go through
  `CaptionController` and never load `Whistle` itself.
- **Meter repaint cost**: continuous repaint at 30 Hz is fine on desktop; consider pausing it when the
  window is minimized.
- **Not yet verified on real hardware**: transcribe output format from Cactus (`extract_text` in
  `src/cactus.rs`) and exclusive-mode behavior of other apps. Settle these before polishing the captions UI.
- **GUI library churn**: pin versions; keep the frontend layer thin (everything in sections 2-4 stays in the lib).
