//! Live caption logger: Tap -> 16 kHz mono -> VAD -> Transcriber -> log file + callback.

use crate::{Segmenter, Tap};
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Local};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

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

#[derive(Clone)]
pub struct Caption {
    pub time: DateTime<Local>,
    pub label: String,
    pub text: String,
}

/// Dropping a `CaptionLogger` stops captioning (and releases the tap).
pub struct CaptionLogger {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
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
        let handle = thread::spawn(move || {
            let mut seg = Segmenter::new();
            let mut pcm: Vec<i16> = vec![];
            while !stop2.load(Relaxed) {
                pcm.clear();
                tap.read_16k(&mut pcm);
                for u in seg.push(&pcm) {
                    let res = asr
                        .lock()
                        .map_err(|_| anyhow!("transcriber lock poisoned"))
                        .and_then(|mut t| t.transcribe(&u.samples));
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
                            on_caption(&cap);
                        }
                        Ok(_) => {}
                        Err(e) => eprintln!("transcribe error: {e:#}"),
                    }
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        Ok(Self {
            stop,
            handle: Some(handle),
        })
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
