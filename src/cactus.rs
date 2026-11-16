//! Cactus engine FFI (cactus.h, engine 4.1.1; 4.0.0 works too, minus set_threads). Only the speech part is wrapped: load + transcribe.
//! The C API is a process-wide singleton (no handles), so only one `Whistle` can exist and it
//! should be used from one thread at a time (CaptionLogger owns it on its own thread).

use crate::captions::Transcriber;
use anyhow::{Context, Result, bail};
use std::ffi::{CStr, CString, c_char, c_int, c_uchar, c_ulonglong};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

unsafe extern "C" {
    fn cactus_load(cact: *const c_uchar, n: c_ulonglong) -> c_int;
    fn cactus_models() -> c_int;
    fn cactus_last_error() -> *const c_char;
    fn cactus_set_threads(threads: c_int);
    fn cactus_transcribe(
        pcm: *const f32,
        samples: c_int,
        language: *const c_char,
        keywords: *const c_char,
        word_timestamps: c_int,
        out: *mut c_char,
        out_capacity: c_int,
    ) -> c_int;
}

const CACTUS_SPEECH: c_int = 2;
static LOADED: AtomicBool = AtomicBool::new(false);

fn last_error() -> String {
    unsafe {
        let p = cactus_last_error();
        if p.is_null() {
            "unknown error".into()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

/// Whistle speech recognition through the Cactus C engine.
pub struct Whistle {
    language: Option<CString>,
    out: Vec<u8>,
}

impl Whistle {
    /// `model`: path to whistle.cact. `language`: e.g. "en", or None to auto-detect.
    pub fn load(model: &Path, language: Option<&str>) -> Result<Self> {
        let bytes =
            std::fs::read(model).with_context(|| format!("cannot read {}", model.display()))?;
        if LOADED.swap(true, SeqCst) {
            bail!("Cactus engine is already loaded in this process");
        }
        // The engine may keep pointing into this buffer, so it lives for the whole process.
        let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        let rc = unsafe { cactus_load(bytes.as_ptr(), bytes.len() as c_ulonglong) };
        let models = unsafe { cactus_models() };
        if rc < 0 || models & CACTUS_SPEECH == 0 {
            LOADED.store(false, SeqCst);
            bail!(
                "cactus_load failed (rc={rc}, models={models}): {}",
                last_error()
            );
        }
        Ok(Self {
            language: language.map(|l| CString::new(l)).transpose()?,
            out: vec![0u8; 64 * 1024],
        })
    }
}

impl Whistle {
    /// Limit the CPU threads the engine uses (engine >= 4.1.1). Call after `load`.
    pub fn set_threads(&self, threads: u32) {
        unsafe { cactus_set_threads(threads as c_int) }
    }
}

/// Pull the transcript out of the engine's output. Output is expected to be JSON; if the
/// schema isn't what we guess, fall back to the raw string so nothing is silently lost.
fn extract_text(raw: &str) -> String {
    let raw = raw.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
        for key in ["text", "response", "audio_text", "transcript"] {
            if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
                return s.trim().to_string();
            }
        }
    }
    raw.to_string()
}

impl Transcriber for Whistle {
    fn transcribe(&mut self, pcm16k: &[i16]) -> Result<String> {
        let pcm: Vec<f32> = pcm16k.iter().map(|&s| s as f32 / 32768.0).collect();
        self.out[0] = 0;
        let lang = self
            .language
            .as_ref()
            .map_or(std::ptr::null(), |c| c.as_ptr());
        let n = unsafe {
            cactus_transcribe(
                pcm.as_ptr(),
                pcm.len() as c_int,
                lang,
                std::ptr::null(), // keywords (format not documented in the header; unused)
                0,                // word timestamps off
                self.out.as_mut_ptr() as *mut c_char,
                self.out.len() as c_int,
            )
        };
        if n < 0 {
            bail!("cactus_transcribe failed: {}", last_error());
        }
        let end = self
            .out
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.out.len());
        Ok(extract_text(&String::from_utf8_lossy(&self.out[..end])))
    }
}
