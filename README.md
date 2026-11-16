# RustMeeter

A small Voicemeeter-style audio mixer for Windows, in Rust. No driver of its own: it mixes
real devices (and VB-Cable) and has a live speech-to-text caption logger built on Cactus Whistle.

- Strips: any mic, or **[Loopback]** of any output device (captures what that device plays)
- Per strip: gain, mute, 3-band EQ, routing to bus A and/or B, level meter
- Two output buses (A, B), each to any output device, with master gain/mute/meter
- **Live captions**: a passive copy of one strip is cut into utterances and transcribed
  in-process by Whistle into `captions.log`. Toggle on/off at runtime. Discord/OBS never notice.
- Two frontends over one clean library (`Mixer`, `Params`, `Tap`): a CLI and a windows-reactor GUI

## 1. One-time setup (Windows)

| What | Where it goes |
|---|---|
| Rust (stable, **1.85+**, project uses edition 2024) | `rustup update` |
| Rust target | `rustup target add x86_64-pc-windows-gnullvm` |
| **llvm-mingw** (x86_64, UCRT), unzip to `C:\tools\llvm-mingw` | github.com/mstorsjo/llvm-mingw/releases |
| Cactus engine `libcactus.a` (+ `cactus.h` for reference) | `cactus\libcactus.a` in this folder. From huggingface.co/Cactus-Compute/cactus -> `releases/4.1.1/windows-x86_64` |
| Whistle model `whistle.cact` | `cactus\whistle.cact` (huggingface.co/Cactus-Compute/whistle) |
| VB-Cable (only if other apps must receive your mixed audio as a mic) | vb-audio.com/Cable |

Why gnullvm? `libcactus.a` is a C++ static library. Linking it with MSVC or classic MinGW
(TDM-GCC: missing `libgcc_eh`) was painful; the llvm-mingw clang toolchain brings a matching
`libc++` / `libunwind`, which `build.rs` links **statically**, so the `.exe` has no DLL dependencies.

Project files that make this work (already set up, edit only if your paths differ):
- `.cargo/config.toml`: default target = `x86_64-pc-windows-gnullvm` and the llvm-mingw clang linker path
- `build.rs`: links `cactus\libcactus.a` + static `c++` + `unwind` (override: `CACTUS_LIB_DIR`,
  `LLVM_MINGW_DIR`; for another toolchain `CACTUS_CXX_LIB=stdc++`)

## 2. Build

```
cargo build --release
```
Output: `target\x86_64-pc-windows-gnullvm\release\rustmeeter.exe`

The GUI is a second binary behind the `gui` feature (windows-reactor):
```
cargo build --release --features gui
```
Output: `target\x86_64-pc-windows-gnullvm\release\rustmeeter-gui.exe` (see §7).

The `cactus` feature is **on by default** (captions available). No Cactus files / no C++ toolchain?
```
cargo build --release --no-default-features
```
The mixer then works as usual; `captions on` just says the build has no Cactus engine.

`task.nu` has shortcuts (`nu task.nu`: build / run / zip).

## 3. Run

```
rustmeeter list                       # show devices
rustmeeter run --bus-a "Speakers (Realtek(R) Audio)" --bus-b "CABLE Input" ^
               --mic "Headset Microphone (MH752)"
```
- Device names are case-insensitive substrings (or `default`); quote names with spaces.
- Strips are numbered in the order given. Defaults: `--mic` -> bus B, `--loopback` -> bus A.
  Override with a suffix: `--mic "Headset:AB"`.
- Typical Discord/OBS setup: bus A = your headphones, bus B = `CABLE Input`;
  in Discord/OBS choose `CABLE Output` as the microphone.

### Routing: what bus A and bus B are for

Each strip has two independent route buttons, **A** and **B**, and each bus is one output device.
The usual split is:

| Bus | Device | What it is for |
|---|---|---|
| A | your real output (headphones / speakers) | what **you** hear |
| B | `CABLE Input` (VB-Cable) | what **other apps** receive as a microphone |

They are deliberately separate directions. Discord/OBS record `CABLE Output`, which carries
exactly what bus B wrote into `CABLE Input`, while you keep listening to bus A on your headphones.

| Strip | → A | → B |
|---|---|---|
| a microphone | off — otherwise you hear yourself | **on** — so Discord/OBS hear you |
| `[Loopback] <the output you listen to>` (game / desktop audio) | **on** to monitor it | **on** only if the other side should hear it too |
| `[Loopback] CABLE Output` | off | off |

The defaults follow this rule: `--mic` and "Add input" for a mic route to **B only**, `--loopback`
and "Add input" for a loopback route to **A only**. Change them per strip with
`route <strip> <a|b> <on|off>` in the CLI, or with the A/B checkboxes on a strip card in the GUI.

**Do not route a `[Loopback] CABLE Output` into a bus.** It is the mixed result of bus B, so
routing it to A replays your own mix (you hear yourself twice) and routing it back to B builds a
feedback loop. If a strip is unrouted and you expected sound, check its two checkboxes first —
adding a mic gives you B on, A off.

Captions do not depend on any of this: they read a passive copy of the raw strip input, so a mic
routed only to B is still transcribed. "First mic" means the first strip that is not a loopback.

### Prompt commands (type at `>`)

| Command | Effect |
|---|---|
| `status` / `levels` | show everything / meters |
| `gain <strip\|a\|b> <db>`, `mute <strip\|a\|b> [on\|off]` | instant |
| `eq <strip> <low\|mid\|high> <db>` | instant |
| `route <strip> <a\|b> <on\|off>` | instant |
| `add <mic\|loop> <device>[:AB]`, `remove <strip>`, `bus <a\|b> <device\|none>` | restarts audio briefly |
| `captions on [strip]`, `captions off`, `captions` | live captions, instant, no audio restart |
| `help`, `quit` | |

### Live captions

```
rustmeeter run ... --mic "Headset Microphone (MH752)" --captions captions.log --whistle cactus\whistle.cact
```
- `--captions [FILE]` starts captions right away; otherwise start them any time with `captions on`.
- The model (`cactus\whistle.cact` by default, or `--whistle PATH`) loads on first use only.
- It captions the first `--mic` strip unless you pass a strip number (`captions on 2`).
- Each utterance (cut after ~0.6 s of silence) becomes a line:
  `[2026-11-16 12:03:41] [me] hello everyone`
- `--caption-lang en` forces a language (Whistle: en/de/fr/es/it/nl/pl; auto-detect otherwise),
  `--caption-threads 2` caps the transcriber's CPU use.
- Captioning a `[Loopback]` strip transcribes the other people in a call. Check consent rules
  before logging other people's voices.

## 4. Troubleshooting

- **I hear my own voice in the headphones.** Run `status`. A strip routed to `A` that is a
  `[Loopback] CABLE Input` plays your mic back. The VB-Cable installer can make `CABLE Input` the
  Windows default output, and `--loopback default` then picks it up. Fix: set the Windows default
  output back to your speakers, or `route <that strip> a off`, or `remove <that strip>`.
  (With `--mic` given first, the mic is strip 1 and the loopback strip 2, so it is usually
  `route 2 a off`; `route 1 b off` would instead stop sending your mic to Discord.)
- **Echo / howl.** A loopback of device X routed to a bus that plays on X captures its own output.
  The common case is a `[Loopback] CABLE Output` strip: see §3 Routing for the intended A/B table.
- **`libcactus.a not found`** at build: put it in `cactus\` or set `CACTUS_LIB_DIR`; or `--no-default-features`.
- **`cannot find -lgcc_eh`**: you are on the GNU target with TDM-GCC. Use the gnullvm setup above.
- **Captions look like raw JSON**: the engine's output format is parsed in `src/cactus.rs`
  (`extract_text`, looks for `text`/`response`/`audio_text`/`transcript`). Adjust the key there.
- **Captions cut too early/late or pick up noise**: constants at the top of `src/vad.rs`.

## 5. How it is built

```
src/main.rs      CLI (uses only the public library API)
src/lib.rs       public API: Mixer, Params, Tap, CaptionLogger, device helpers
src/mixer.rs     Mixer: strips + 2 buses, start / stop / apply
src/params.rs    lock-free controls (gain, mute, EQ, routing, meters) shared with audio threads
src/devices.rs   device discovery (cpal / WASAPI)
src/engine.rs    internal: cpal streams, resampling, EQ, mixing (input -> per-bus ring -> output)
src/tap.rs       passive per-strip copy of the raw input; dormant until subscribed
src/vad.rs       energy-based utterance segmenter (16 kHz mono)
src/events.rs    Event + EventHandler: stream errors / lifecycle reported to any frontend
src/gui/main.rs  windows-reactor GUI (feature `gui`), a thin frontend over the library
src/captions.rs  Tap -> VAD -> Transcriber -> log file
src/caption_controller.rs  runtime captions on/off, lazy one-time model load (used by CLI and any GUI)
src/cactus.rs    FFI to libcactus (feature `cactus`): Whistle implements Transcriber
build.rs         links Cactus + static libc++/libunwind
```
Key ideas:
- Live controls are atomics (`Arc<Params>`): instant, any thread. Structural changes
  (`add_strip`, `remove_strip`, `set_bus_device`) mark the config dirty; `Mixer::apply()` rebuilds streams.
- `Mixer` holds cpal streams (not `Send`): keep it on one thread (the GUI keeps it on the UI thread).
- **Runtime captions toggle**: whether Cactus is *linked* is a build-time choice (the feature), but
  captioning itself is runtime. Every strip owns a tap buffer from creation; it only records while a
  `Tap` is subscribed (`mixer.subscribe(id)`), so `captions on/off` needs no stream restart.
  The Cactus engine is a process-wide singleton that can't be reloaded, so the model is loaded once
  and shared (`SharedTranscriber`) across on/off cycles.
- One active `Tap` per strip.

## 6. Ideas / next steps

- Streaming transcription (`cactus_stream_transcribe_process`) for captions while still speaking
- Needle 3 (tool calling) on the transcript: voice commands like "mute my mic" (wake word first)
- Save/load settings (JSON), GUI polish (per-strip width, transcript auto-scroll), per-app capture
- Own virtual audio driver (SysVAD fork) instead of VB-Cable

## 7. GUI (optional, feature `gui`)

```
cargo run --release --features gui --bin rustmeeter-gui
```

windows-reactor (WinUI 3). Like the CLI it is only a frontend: it talks to `Mixer`, `Params`,
`Mixer::snapshot()`, `CaptionController` and `Mixer::set_event_handler()`, and contains no audio
code. It starts with bus A = your default output and your default mic as strip 1 (→ bus B); wire
the rest up with the device pickers and "Add input".

- **Toolbar**: Start/Stop, "Refresh devices", bus A and bus B device pickers (both include `None`),
  "Add input" (mics and `[Loopback]` sources), and the caption controls.
- **Strip cards**: source name, A/B route checkboxes, 3-band EQ sliders (±12 dB), fader with a dB
  readout, mute, level meter, remove.
- **Bus cards**: bus name, device, and the same fader (gain, mute, meter).
- **Transcript**: the last 500 captions, also written to `captions.log`; errors show up in an info
  bar above.

Meters are refreshed by a 33 ms timer. Faders and route checkboxes write the `Params` atomics
directly, so they are instant; adding/removing a strip or changing a bus device rebuilds the
streams (`Mixer::apply()`), which is an audible restart, just like `add`/`bus` in the CLI.

Caveats:
- The transcript does not auto-scroll — scroll it yourself. reactor's `ScrollViewer` has no
  scroll-to-end call.
- The first "Start captions" blocks the window while the Whistle model loads. That is a one-time
  cost per process; stopping and starting again is instant.
- Strip cards have a fixed width, so with many strips the cards area scrolls both ways.





Note for me:
Your routing is *almost* right, but look at strip 2's A/B boxes: **neither is checked**, so the headset mic currently goes nowhere.

What the screenshot says:

| Element | Value | Meaning |
|---|---|---|
| Bus A | Speakers (Realtek) | what you hear |
| Bus B | CABLE Input (VB-Audio Virtual Cable) | what other apps can record |
| Strip 1 | CABLE Output (VB-…), **A** | that virtual-cable capture feeds Bus A (your speakers) |
| Strip 2 | Headset Microphone, **nothing** | unrouted → inaudible everywhere |

For the standard "mic into Discord/OBS, desktop to my headphones" setup:

- **Bus A** = your real output (Speakers/Headset).
- **Bus B** = `CABLE Input` → then in Discord/OBS you pick **`CABLE Output`** as the microphone.
- **Mic strip** → check **B only** (not A, unless you want to hear yourself).
- **Monitor loopback** (e.g. `[Loopback] Speakers`) → check **A only**, if you want the desktop audio in your ears.
- `[Loopback] CABLE Output` as a *source* only makes sense if you want to monitor what the cable is carrying; if a program is also playing that into the cable, routing it to A can create a feedback loop.

Two smaller notes:

- Newly added mics default to B and loopback sources default to A (`AddInput` in `main.rs`), so strip 2 should have arrived with B checked — it looks like B was clicked off. Re-check B; if a freshly added strip comes up unchecked again, tell me and I'll debug the checkbox binding.
- Captions: with strip 1 being the virtual-cable capture, "first mic" correctly resolves to **strip 2** — that's the one Whisper should transcribe. Captions also don't need any A/B routing, the tap is pre-fader.

Minor UI stuff I can tidy if you want: the strip cards are a bit narrow (the EQ sliders and the B box on strip 2 sit right on the card edge), and the transcript doesn't auto-scroll (reactor's `ScrollViewer` exposes no scroll-to-end API).