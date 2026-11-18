//! Live caption logger: Tap -> 16 kHz mono -> VAD -> Transcriber -> log file + callback.

use crate::params::AtomicF32;
use crate::{Segmenter, Tap};
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Local};
use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Anything that turns 16 kHz mono i16 audio into text (Cactus Whistle, a stub, ...).
pub trait Transcriber: Send {
    fn transcribe(&mut self, pcm16k: &[i16]) -> Result<String>;
}

/// A transcriber that outlives individual `CaptionLogger`s. The Cactus engine is a process-wide
/// singleton that can only be loaded once, so the model is loaded once and shared across
/// captions-on / captions-off cycles.
pub type SharedTranscriber = Arc<Mutex<Box<dyn Transcriber>>>;

pub fn share_transcriber(t: impl Transcriber + 'static) -> SharedTranscriber {
    Arc::new(Mutex::new(Box::new(t)))
}

#[derive(Clone, Default)]
pub struct Caption {
    pub time: DateTime<Local>,
    pub label: String,
    pub text: String,
}

/// Where a running caption session is right now. Cheap to poll, so a UI can show it live.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CaptionState {
    /// Not captioning.
    Idle,
    /// The tap is open and the VAD has not heard anything worth transcribing.
    Listening,
    /// Voice detected; an utterance is being collected.
    Speech,
    /// The model is busy with a finished utterance.
    Transcribing,
}

impl CaptionState {
    pub fn label(self) -> &'static str {
        match self {
            CaptionState::Idle => "idle",
            CaptionState::Listening => "listening",
            CaptionState::Speech => "hearing speech",
            CaptionState::Transcribing => "transcribing",
        }
    }
}

/// How many level samples the graph keeps. The logger ticks every 20 ms, so this is ~2 s.
const HISTORY: usize = 96;

/// Live state of a caption session: what the tap is hearing and what the transcriber is doing.
///
/// Written by the caption thread, polled by whichever UI is attached (`CaptionLogger::status`).
/// Everything is an atomic except the level history, which is a short ring behind a mutex; the
/// caption thread never holds it while transcribing, so a slow repaint cannot stall the pipeline.
pub struct CaptionStatus {
    level: AtomicF32, // current RMS, linear 0..1, with decay
    history: Mutex<VecDeque<f32>>,
    speech: AtomicBool,
    busy: AtomicBool,
    utterances: AtomicU32,
    last_ms: AtomicU32, // duration of the last transcription
    error: Mutex<Option<String>>,
}

impl Default for CaptionStatus {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptionStatus {
    pub fn new() -> Self {
        Self {
            level: AtomicF32::new(0.0),
            history: Mutex::new(VecDeque::with_capacity(HISTORY)),
            speech: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            utterances: AtomicU32::new(0),
            last_ms: AtomicU32::new(0),
            error: Mutex::new(None),
        }
    }

    /// Current tap level, linear peak-ish RMS in 0..1, with a short decay so the graph reads well.
    pub fn level(&self) -> f32 {
        self.level.get()
    }

    /// The last ~2 s of levels, oldest first, each in 0..1. Empty only before the first tick.
    pub fn history(&self) -> Vec<f32> {
        self.history
            .lock()
            .map(|h| h.iter().copied().collect())
            .unwrap_or_default()
    }

    pub fn speech(&self) -> bool {
        self.speech.load(Relaxed)
    }

    pub fn busy(&self) -> bool {
        self.busy.load(Relaxed)
    }

    /// Utterances that produced text since captions were started.
    pub fn utterances(&self) -> u32 {
        self.utterances.load(Relaxed)
    }

    /// Duration of the last transcription, in milliseconds. 0 before the first one.
    pub fn last_ms(&self) -> u32 {
        self.last_ms.load(Relaxed)
    }

    /// Last transcriber error, kept until the next successful transcription.
    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|e| e.clone())
    }

    pub fn state(&self) -> CaptionState {
        if self.busy() {
            CaptionState::Transcribing
        } else if self.speech() {
            CaptionState::Speech
        } else {
            CaptionState::Listening
        }
    }

    /// One caption-thread tick: a chunk of 16 kHz audio and whether the VAD is mid-utterance.
    fn observe(&self, rms: f32, speech: bool) {
        let level = rms.max(self.level.get() * 0.8).clamp(0.0, 1.0);
        self.level.set(level);
        self.speech.store(speech, Relaxed);
        if let Ok(mut h) = self.history.lock() {
            h.push_back(level);
            while h.len() > HISTORY {
                h.pop_front();
            }
        }
    }
}

/// Root-mean-square of a 16 kHz chunk, linear 0..1.
fn rms(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples
        .iter()
        .map(|&s| (f32::from(s) / 32768.0).powi(2))
        .sum();
    (sum / samples.len() as f32).sqrt()
}

/// Dropping a `CaptionLogger` stops captioning (and releases the tap).
pub struct CaptionLogger {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    status: Arc<CaptionStatus>,
}

impl CaptionLogger {
    pub fn start(
        mut tap: Tap,
        label: String,
        log_path: PathBuf,
        asr: SharedTranscriber,
        on_caption: impl Fn(&Caption) + Send + 'static,
    ) -> Result<Self> {
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("cannot open {}", log_path.display()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let status = Arc::new(CaptionStatus::new());
        let status2 = status.clone();
        let handle = thread::spawn(move || {
            let status = status2;
            let mut seg = Segmenter::new();
            let mut pcm: Vec<i16> = vec![];
            while !stop2.load(Relaxed) {
                pcm.clear();
                tap.read_16k(&mut pcm);
                let utterances = seg.push(&pcm);
                // Do this before transcribing: it is what tells the UI the tap is alive.
                status.observe(rms(&pcm), seg.in_speech());
                for u in utterances {
                    status.busy.store(true, Relaxed);
                    let started = Instant::now();
                    let res = asr
                        .lock()
                        .map_err(|_| anyhow!("transcriber lock poisoned"))
                        .and_then(|mut t| t.transcribe(&u.samples));
                    status
                        .last_ms
                        .store(started.elapsed().as_millis() as u32, Relaxed);
                    status.busy.store(false, Relaxed);
                    match res {
                        Ok(text) if !text.is_empty() => {
                            let cap = Caption {
                                time: u.started_at,
                                label: label.clone(),
                                text,
                            };
                            let _ = writeln!(
                                log,
                                "[{}] [{}] {}",
                                cap.time.format("%Y-%m-%d %H:%M:%S"),
                                cap.label,
                                cap.text
                            );
                            let _ = log.flush();
                            status.utterances.fetch_add(1, Relaxed);
                            if let Ok(mut e) = status.error.lock() {
                                *e = None;
                            }
                            on_caption(&cap);
                        }
                        Ok(_) => {}
                        Err(e) => {
                            eprintln!("transcribe error: {e:#}");
                            if let Ok(mut slot) = status.error.lock() {
                                *slot = Some(format!("{e:#}"));
                            }
                        }
                    }
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        Ok(Self {
            stop,
            handle: Some(handle),
            status,
        })
    }

    /// Live state of this session (tap level, VAD, transcriber). Clone it into a UI.
    pub fn status(&self) -> Arc<CaptionStatus> {
        self.status.clone()
    }
}

impl Drop for CaptionLogger {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_indicator_prefers_working_over_hearing() {
        let s = CaptionStatus::new();
        s.observe(0.0, false);
        assert_eq!(s.state(), CaptionState::Listening);
        s.observe(0.2, true);
        assert_eq!(s.state(), CaptionState::Speech);
        // The model now takes over: the dot must say so, even though the VAD still holds speech.
        s.busy.store(true, Relaxed);
        assert_eq!(s.state(), CaptionState::Transcribing);
        assert_eq!(s.state().label(), "transcribing");
    }

    #[test]
    fn the_graph_keeps_a_fixed_window_and_decays() {
        let s = CaptionStatus::new();
        s.observe(0.5, false);
        assert_eq!(s.level(), 0.5);
        // Silence only decays the level; it does not snap to zero, so the graph reads smoothly.
        s.observe(0.0, false);
        assert!((s.level() - 0.4).abs() < 1e-6);
        assert_eq!(s.history().len(), 2);
        for _ in 0..HISTORY * 2 {
            s.observe(0.0, false);
        }
        assert_eq!(s.history().len(), HISTORY);
    }

    #[test]
    fn a_transcribed_line_clears_an_older_error() {
        let s = CaptionStatus::new();
        *s.error.lock().unwrap() = Some("cactus fell over".to_string());
        assert!(s.error().is_some());
        // What the logger does on a successful transcription.
        s.utterances.fetch_add(1, Relaxed);
        *s.error.lock().unwrap() = None;
        assert_eq!(s.error(), None);
        assert_eq!(s.utterances(), 1);
    }
}
