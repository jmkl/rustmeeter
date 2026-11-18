# Whistle Recorder

A small Windows-friendly Rust CLI for recording short voice-command samples and appending Cactus Whistle JSONL manifest entries.

## Hotkeys

- `r` — start recording
- `s` — stop recording and save a mono 16-bit PCM WAV file
- `t` — type the transcript for the saved recording; press Enter to append it to `train.jsonl`
- `q` — quit

The program uses the default input device. It records at the device's native sample rate and writes mono WAV. Cactus's fine-tuning pipeline can resample supported audio to its expected sample rate.

## Requirements

- Rust stable
- A working microphone / Windows audio input device

## Run

```powershell
cargo run --release
```

## Output

```text
whistle-recorder/
├── audio/
│   ├── recording_....wav
│   └── ...
└── train.jsonl
```

Each appended line has this shape:

```json
{"audio":"audio/recording_....wav","text":"matiin lampu","language":"id"}
```

The manifest is appended only after you enter a non-empty transcript. Audio is saved when you press `s`.

## Suggested workflow

1. Press `r` and speak one command.
2. Press `s` to save the WAV.
3. Press `t`, type exactly what you said, then press Enter.
4. Repeat.

Record different natural phrasings and speakers. Keep separate validation and test recordings; don't train on the same recordings you use to evaluate the model. When ready, split the manifest into `train.jsonl`, `validation.jsonl`, and `test.jsonl` as required by your Whistle fine-tuning run.

## Notes

- The default input device is selected automatically.
- Audio is downmixed to mono.
- WAV output is 16-bit PCM.
- Transcript text is JSON-escaped automatically.
- This starter project writes to `train.jsonl`; change `MANIFEST` in `src/main.rs` if you want another output filename.
