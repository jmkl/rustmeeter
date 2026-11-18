use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const AUDIO_DIR: &str = "audio";
const LANGUAGE: &str = "id";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DatasetSplit {
    Train,
    Validation,
    Test,
}

impl DatasetSplit {
    fn from_key(key: char) -> Option<Self> {
        match key {
            '1' => Some(Self::Train),
            '2' => Some(Self::Validation),
            '3' => Some(Self::Test),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Train => "train",
            Self::Validation => "validation",
            Self::Test => "test",
        }
    }

    fn manifest(self) -> String {
        format!("finetune\\{}.jsonl", self.name())
    }

    fn audio_dir(self) -> PathBuf {
        Path::new(AUDIO_DIR).join(self.name())
    }
}

#[derive(Serialize)]
struct ManifestEntry {
    audio: String,
    text: String,
    language: String,
}

struct PendingRecording {
    path: PathBuf,
    split: DatasetSplit,
}

struct Recorder {
    recording: Arc<AtomicBool>,
    samples: Arc<Mutex<Vec<i16>>>,
    sample_rate: u32,
    stream: Stream,
}

impl Recorder {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or("No default audio input device found")?;
        let device_name = device.name().unwrap_or_else(|_| "Unknown input".into());
        let supported = device.default_input_config()?;
        let sample_rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.into();

        let recording = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(Mutex::new(Vec::<i16>::new()));

        let stream = match sample_format {
            SampleFormat::F32 => {
                let rec = recording.clone();
                let out = samples.clone();
                device.build_input_stream(
                    &config,
                    move |data: &[f32], _| {
                        if rec.load(Ordering::Relaxed) {
                            append_mono(data, channels, &out, |v| {
                                (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
                            });
                        }
                    },
                    stream_error,
                    None,
                )?
            }
            SampleFormat::I16 => {
                let rec = recording.clone();
                let out = samples.clone();
                device.build_input_stream(
                    &config,
                    move |data: &[i16], _| {
                        if rec.load(Ordering::Relaxed) {
                            append_mono(data, channels, &out, |v| v);
                        }
                    },
                    stream_error,
                    None,
                )?
            }
            SampleFormat::U16 => {
                let rec = recording.clone();
                let out = samples.clone();
                device.build_input_stream(
                    &config,
                    move |data: &[u16], _| {
                        if rec.load(Ordering::Relaxed) {
                            append_mono(data, channels, &out, |v| (v as i32 - 32768) as i16);
                        }
                    },
                    stream_error,
                    None,
                )?
            }
            other => return Err(format!("Unsupported input sample format: {other:?}").into()),
        };

        stream.play()?;
        println!("Input device: {device_name}");
        println!("Input format: {sample_rate} Hz, {channels} channel(s), {sample_format:?}");

        Ok(Self {
            recording,
            samples,
            sample_rate,
            stream,
        })
    }

    fn start(&self) -> Result<(), &'static str> {
        if self.recording.load(Ordering::SeqCst) {
            return Err("Already recording");
        }
        self.samples.lock().map_err(|_| "Audio buffer poisoned")?.clear();
        self.recording.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&self) -> Result<Vec<i16>, &'static str> {
        if !self.recording.swap(false, Ordering::SeqCst) {
            return Err("Not currently recording");
        }
        // Let the audio callback finish its current buffer before taking the samples.
        std::thread::sleep(Duration::from_millis(80));
        let mut guard = self.samples.lock().map_err(|_| "Audio buffer poisoned")?;
        Ok(std::mem::take(&mut *guard))
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

fn append_mono<T, F>(data: &[T], channels: usize, output: &Arc<Mutex<Vec<i16>>>, convert: F)
where
    T: Copy,
    F: Fn(T) -> i16,
{
    if channels == 0 {
        return;
    }

    let mut mono = Vec::with_capacity(data.len() / channels);
    for frame in data.chunks_exact(channels) {
        let sum: i64 = frame.iter().map(|sample| convert(*sample) as i64).sum();
        mono.push((sum / channels as i64) as i16);
    }

    if let Ok(mut buffer) = output.lock() {
        buffer.extend(mono);
    }
}

fn stream_error(error: cpal::StreamError) {
    eprintln!("\nAudio stream error: {error}");
}

fn timestamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn save_wav(
    path: &Path,
    samples: &[i16],
    sample_rate: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for sample in samples {
        writer.write_sample(*sample)?;
    }
    writer.finalize()?;
    Ok(())
}

fn append_manifest(
    audio_path: &Path,
    transcript: &str,
    split: DatasetSplit,
) -> Result<(), Box<dyn std::error::Error>> {
    let relative_audio = audio_path.to_string_lossy().replace('\\', "/");
    let entry = ManifestEntry {
        audio: relative_audio,
        text: transcript.to_owned(),
        language: LANGUAGE.to_owned(),
    };

    // Append mode is intentional: existing recordings are preserved across sessions/days.
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(split.manifest())?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(&mut writer, &entry)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn prompt_transcript(recording: &PendingRecording) -> Result<String, Box<dyn std::error::Error>> {
    disable_raw_mode()?;
    println!(
        "\nTranscript for {} [{}]:",
        recording.path.display(),
        recording.split.name()
    );
    print!("> ");
    io::stdout().flush()?;

    let mut transcript = String::new();
    io::stdin().read_line(&mut transcript)?;

    enable_raw_mode()?;
    Ok(transcript.trim().to_owned())
}

fn print_help(selected_split: DatasetSplit) {
    println!();
    println!("  [1] Select TRAIN split      -> train.jsonl");
    println!("  [2] Select VALIDATION split -> validation.jsonl");
    println!("  [3] Select TEST split       -> test.jsonl");
    println!("  [r] Start recording");
    println!("  [s] Stop recording, save WAV, then enter transcript");
    println!("  [t] Retry transcript entry if the previous transcript was empty");
    println!("  [h] Show this help");
    println!("  [q] Quit");
    println!("\n  Current split: {}", selected_split.name().to_uppercase());
    println!();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for split in [
        DatasetSplit::Train,
        DatasetSplit::Validation,
        DatasetSplit::Test,
    ] {
        fs::create_dir_all(split.audio_dir())?;
    }

    let recorder = Recorder::new()?;
    // Keep the stream alive for the lifetime of the program.
    let _stream_guard = &recorder.stream;

    let mut selected_split = DatasetSplit::Train;
    let mut recording_split: Option<DatasetSplit> = None;
    let mut pending_audio: Option<PendingRecording> = None;
    let mut recording_started_at: Option<u128> = None;

    print_help(selected_split);
    enable_raw_mode()?;
    println!("Ready. Press a hotkey...");

    loop {
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match key.code {
                    KeyCode::Char('1' | '2' | '3') => {
                        if recorder.recording.load(Ordering::SeqCst) || pending_audio.is_some() {
                            println!("\nFinish labeling the current recording before changing split.");
                            continue;
                        }
                        if let KeyCode::Char(ch) = key.code {
                            selected_split = DatasetSplit::from_key(ch).expect("matched split key");
                        }
                        println!(
                            "\nSelected {} -> {}. New recordings will append there.",
                            selected_split.name().to_uppercase(),
                            selected_split.manifest()
                        );
                    }
                    KeyCode::Char('r') | KeyCode::Char('R') => {
                        if pending_audio.is_some() {
                            println!("\nFinish the previous transcript with [t] before starting another.");
                            continue;
                        }
                        match recorder.start() {
                            Ok(()) => {
                                recording_started_at = Some(timestamp());
                                recording_split = Some(selected_split);
                                println!(
                                    "\nRecording [{}]... press [s] to stop.",
                                    selected_split.name().to_uppercase()
                                );
                            }
                            Err(message) => println!("\n{message}"),
                        }
                    }
                    KeyCode::Char('s') | KeyCode::Char('S') => match recorder.stop() {
                        Ok(samples) => {
                            let split = recording_split.take().unwrap_or(selected_split);
                            let name = format!(
                                "recording_{}.wav",
                                recording_started_at.take().unwrap_or_else(timestamp)
                            );
                            let directory = split.audio_dir();
                            fs::create_dir_all(&directory)?;
                            let path = directory.join(name);
                            save_wav(&path, &samples, recorder.sample_rate())?;
                            if samples.is_empty() {
                                fs::remove_file(&path)?;
                                println!("\nNo audio captured; discarded empty recording.");
                            } else {
                                let seconds = samples.len() as f64 / recorder.sample_rate() as f64;
                                let pending = PendingRecording { path: path.clone(), split };
                                pending_audio = Some(PendingRecording { path: path.clone(), split });
                                println!("\nSaved {} ({seconds:.1}s).", path.display());
                                // Immediately switch to line input so the user can transcribe
                                // the clip without pressing another hotkey.
                                match prompt_transcript(&pending) {
                                    Ok(transcript) if !transcript.is_empty() => {
                                        append_manifest(&pending.path, &transcript, pending.split)?;
                                        println!(
                                            "Appended to {}. Existing entries were preserved.",
                                            pending.split.manifest()
                                        );
                                        pending_audio = None;
                                    }
                                    Ok(_) => println!(
                                        "Empty transcript; recording kept. Press [t] to retry."
                                    ),
                                    Err(error) => {
                                        let _ = enable_raw_mode();
                                        return Err(error);
                                    }
                                }
                                println!("Ready. [1/2/3] split  [r] record  [s] stop  [t] retry transcript  [q] quit");
                            }
                        }
                        Err(message) => println!("\n{message}"),
                    },
                    KeyCode::Char('t') | KeyCode::Char('T') => {
                        if let Some(recording) = pending_audio.as_ref() {
                            let recording = PendingRecording {
                                path: recording.path.clone(),
                                split: recording.split,
                            };
                            match prompt_transcript(&recording) {
                                Ok(transcript) if !transcript.is_empty() => {
                                    append_manifest(&recording.path, &transcript, recording.split)?;
                                    println!(
                                        "Appended to {}. Existing entries were preserved.",
                                        recording.split.manifest()
                                    );
                                    pending_audio = None;
                                }
                                Ok(_) => println!(
                                    "Empty transcript; recording kept. Press [t] to retry."
                                ),
                                Err(error) => {
                                    let _ = enable_raw_mode();
                                    return Err(error);
                                }
                            }
                            println!("Ready. [1/2/3] split  [r] record  [s] stop  [t] transcript  [q] quit");
                        } else {
                            println!("\nNo saved recording to label. Press [r], then [s] first.");
                        }
                    }
                    KeyCode::Char('h') | KeyCode::Char('H') => print_help(selected_split),
                    KeyCode::Char('q') | KeyCode::Char('Q') => {
                        if recorder.recording.load(Ordering::SeqCst) {
                            let _ = recorder.stop();
                        }
                        disable_raw_mode()?;
                        println!("\nBye.");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(())
}
