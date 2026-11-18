//! Cactus engine FFI (`cactus.h`): the whole C API — text agents (Needle 3), speech (Whistle)
//! and vision (Mirage).
//!
//! The engine is one process-global runtime. `cactus_load` *merges* every `.cact` you hand it, so
//! a binary can hold `needle3.cact` and `whistle.cact` at the same time; `Models` reports which
//! families are loaded. There is still only one conversation, so callers must serialize access
//! (captions already do that through `SharedTranscriber`).
//!
//! `Cactus` is the raw handle and exposes every entry point; `Whistle`, `Agent` and `Mirage` are
//! thin per-model facades. `Whistle` implements `Transcriber`, which is what the caption
//! pipeline uses.
//!
//! Model buffers are leaked on purpose: the engine may keep pointing into them.

/// Anything that turns 16 kHz mono i16 audio into text (Cactus Whistle, a stub, ...).
pub trait Transcriber: Send {
    fn transcribe(&mut self, pcm16k: &[i16]) -> anyhow::Result<String>;
}
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::ffi::{CStr, CString, c_char, c_int, c_uchar, c_ulonglong};
use std::ops::BitOr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

unsafe extern "C" {
    fn cactus_load(cact: *const c_uchar, n: c_ulonglong) -> c_int;
    fn cactus_models() -> c_int;
    fn cactus_last_error() -> *const c_char;
    fn cactus_init(
        system_prompt: *const c_char,
        tools_json: *const c_char,
        tool_index_path: *const c_char,
    ) -> c_int;
    fn cactus_complete(
        input: *const c_char,
        pcm: *const f32,
        samples: c_int,
        max_new_tokens: c_int,
        out: *mut c_char,
        out_capacity: c_int,
    ) -> c_int;
    fn cactus_set_audio(language: *const c_char, keywords: *const c_char, word_timestamps: c_int);
    fn cactus_set_threads(threads: c_int);
    fn cactus_reset();
    fn cactus_transcribe(
        pcm: *const f32,
        samples: c_int,
        language: *const c_char,
        keywords: *const c_char,
        word_timestamps: c_int,
        out: *mut c_char,
        out_capacity: c_int,
    ) -> c_int;
    fn cactus_stream_transcribe_process(
        pcm: *const f32,
        samples: c_int,
        language: *const c_char,
        keywords: *const c_char,
        out: *mut c_char,
        out_capacity: c_int,
    ) -> c_int;
    fn cactus_stream_transcribe_stop(out: *mut c_char, out_capacity: c_int) -> c_int;
    fn cactus_describe(
        rgb: *const c_uchar,
        width: c_int,
        height: c_int,
        options: *const c_char,
        language: *const c_char,
        keywords: *const c_char,
        out: *mut c_char,
        out_capacity: c_int,
    ) -> c_int;
    fn cactus_embed(
        input: *const c_char,
        pcm: *const f32,
        samples: c_int,
        out: *mut f32,
        out_capacity: c_int,
    ) -> c_int;
}

/// Model families the loaded engine can serve. From `cactus.h`; `cactus_models()` returns the
/// union of the families of every `.cact` loaded so far.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Models(u8);

impl Models {
    pub const TEXT: Models = Models(1);
    pub const SPEECH: Models = Models(2);
    pub const VISION: Models = Models(4);

    pub fn contains(self, other: Models) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for Models {
    type Output = Models;
    fn bitor(self, rhs: Models) -> Models {
        Models(self.0 | rhs.0)
    }
}

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

/// `None` becomes a null pointer; a NUL byte inside the string is rejected rather than truncated.
fn opt_cstring(value: Option<&str>) -> Result<Option<CString>> {
    value
        .map(|v| CString::new(v).with_context(|| format!("cactus: NUL byte in {v:?}")))
        .transpose()
}

fn ptr(text: &Option<CString>) -> *const c_char {
    text.as_ref().map_or(std::ptr::null(), |c| c.as_ptr())
}

/// Read a NUL-terminated result buffer after a non-negative return code.
fn finish(out: &mut [u8], rc: c_int) -> Result<String> {
    if rc < 0 {
        bail!("cactus call failed: {}", last_error());
    }
    let end = out.iter().position(|&b| b == 0).unwrap_or(out.len());
    Ok(String::from_utf8_lossy(&out[..end]).into_owned())
}

/// Model archives already handed to `cactus_load` (the engine is global; loading one twice is
/// wasteful). Keyed by path, so a different archive at the same path is not re-read.
static LOADED: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);
/// `cactus_load` is not documented as thread-safe; keep loads serialized.
static LOAD_LOCK: Mutex<()> = Mutex::new(());

fn note_loaded(path: &Path) -> bool {
    let mut guard = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    let set = guard.get_or_insert_with(HashSet::new);
    !set.insert(path.to_path_buf())
}

/// The process-global Cactus engine.
///
/// One instance per thread of use is fine — they all talk to the same C runtime, so keep calls
/// serialized (a `Mutex<Cactus>` is enough).
pub struct Cactus {
    /// Result buffer for the text-producing entry points (64 KiB, the engine's own default).
    out: Vec<u8>,
    /// Embedding output. 8192 floats covers the published model sizes; `embed` checks the count.
    embed: Vec<f32>,
}

impl Default for Cactus {
    fn default() -> Self {
        Self::new()
    }
}

impl Cactus {
    pub fn new() -> Self {
        Self {
            out: vec![0u8; 64 * 1024],
            embed: vec![0.0; 8192],
        }
    }

    /// Merge a `.cact` archive into the engine (Needle 3, Whistle, Mirage, or a fine-tuned one).
    /// Returns the families the engine can now serve. Safe to call repeatedly: a path already
    /// loaded is skipped.
    pub fn load(&mut self, model: &Path) -> Result<Models> {
        let _guard = LOAD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if note_loaded(model) {
            return Ok(self.models());
        }
        let bytes =
            std::fs::read(model).with_context(|| format!("cannot read {}", model.display()))?;
        // The engine may keep pointing into this buffer, so it lives for the whole process.
        let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        let rc = unsafe { cactus_load(bytes.as_ptr(), bytes.len() as c_ulonglong) };
        if rc < 0 {
            bail!("cactus_load failed: {}", last_error());
        }
        Ok(self.models())
    }

    /// Families currently loaded (bitwise OR of [`Models::TEXT`], [`Models::SPEECH`],
    /// [`Models::VISION`]).
    pub fn models(&self) -> Models {
        Models(unsafe { cactus_models() }.max(0) as u8)
    }

    /// Bail unless every family in `needed` is loaded.
    pub fn require(&self, needed: Models) -> Result<()> {
        let have = self.models();
        if have.contains(needed) {
            Ok(())
        } else {
            bail!("cactus: the loaded model(s) do not provide {needed:?} (loaded: {have:?})");
        }
    }

    /// CPU thread count for models loaded *after* this call (`0` = all cores the process may use).
    /// Call it before [`Cactus::load`] for it to affect that model.
    pub fn set_threads(&self, threads: u32) {
        unsafe { cactus_set_threads(threads.min(c_int::MAX as u32) as c_int) }
    }

    /// Rewind the text conversation. Tools stay loaded.
    pub fn reset(&self) {
        unsafe { cactus_reset() }
    }

    /// Bind the system facts and tool schemas for the text model.
    ///
    /// `tools_json` is a JSON array of tool schemas (Needle's compact form or OpenAI-style
    /// `{"type": "function", "function": {...}}`). `tool_index_path` is reserved for engine
    /// tool-index archives; pass `None`.
    pub fn init(
        &self,
        system_prompt: Option<&str>,
        tools_json: Option<&str>,
        tool_index_path: Option<&str>,
    ) -> Result<u32> {
        let system = opt_cstring(system_prompt)?;
        let tools = opt_cstring(tools_json)?;
        let index = opt_cstring(tool_index_path)?;
        let rc = unsafe { cactus_init(ptr(&system), ptr(&tools), ptr(&index)) };
        if rc < 0 {
            bail!("cactus_init failed: {}", last_error());
        }
        Ok(rc as u32)
    }

    /// One text turn. `input` and `pcm` are mutually exclusive: give text, or a 16 kHz mono clip
    /// (the engine transcribes it first when a speech model is loaded).
    ///
    /// Returns the raw JSON envelope (`type`, `function_calls`, `confidence`, ...). An empty
    /// `function_calls` is the model's refusal, not an error.
    pub fn complete(
        &mut self,
        input: Option<&str>,
        pcm: Option<&[f32]>,
        max_new_tokens: u32,
    ) -> Result<String> {
        self.require(Models::TEXT)?;
        let text = opt_cstring(input)?;
        let samples = pcm.map_or(0, |p| p.len().min(c_int::MAX as usize) as c_int);
        self.out[0] = 0;
        let rc = unsafe {
            cactus_complete(
                ptr(&text),
                pcm.map_or(std::ptr::null(), |p| p.as_ptr()),
                samples,
                max_new_tokens.min(c_int::MAX as u32) as c_int,
                self.out.as_mut_ptr() as *mut c_char,
                self.out.len() as c_int,
            )
        };
        finish(&mut self.out, rc)
    }

    /// Embed text (and/or a 16 kHz mono clip) into a vector. `cactus_embed` returns the number of
    /// floats written; a count at the buffer limit means the result was truncated.
    pub fn embed(&mut self, input: Option<&str>, pcm: Option<&[f32]>) -> Result<Vec<f32>> {
        self.require(Models::TEXT)?;
        let text = opt_cstring(input)?;
        let samples = pcm.map_or(0, |p| p.len().min(c_int::MAX as usize) as c_int);
        let rc = unsafe {
            cactus_embed(
                ptr(&text),
                pcm.map_or(std::ptr::null(), |p| p.as_ptr()),
                samples,
                self.embed.as_mut_ptr(),
                self.embed.len() as c_int,
            )
        };
        if rc < 0 {
            bail!("cactus_embed failed: {}", last_error());
        }
        let n = rc as usize;
        if n > self.embed.len() {
            bail!(
                "cactus_embed returned {n} floats, larger than the {} float buffer",
                self.embed.len()
            );
        }
        Ok(self.embed[..n].to_vec())
    }

    /// Language / keyword bias / word timestamps for transcriptions the engine runs for itself
    /// (for example inside [`Cactus::complete`] when it is handed audio).
    pub fn set_audio(&self, language: Option<&str>, keywords: Option<&str>, word_timestamps: bool) {
        let language = opt_cstring(language).ok().flatten();
        let keywords = opt_cstring(keywords).ok().flatten();
        unsafe { cactus_set_audio(ptr(&language), ptr(&keywords), word_timestamps as c_int) }
    }

    /// One-pass transcription of a 16 kHz mono clip. Returns the raw JSON (`text`, `words`, ...).
    pub fn transcribe(
        &mut self,
        pcm: &[f32],
        language: Option<&str>,
        keywords: Option<&str>,
        word_timestamps: bool,
    ) -> Result<String> {
        self.require(Models::SPEECH)?;
        let language = opt_cstring(language)?;
        let keywords = opt_cstring(keywords)?;
        let samples = pcm.len().min(c_int::MAX as usize) as c_int;
        self.out[0] = 0;
        let rc = unsafe {
            cactus_transcribe(
                pcm.as_ptr(),
                samples,
                ptr(&language),
                ptr(&keywords),
                word_timestamps as c_int,
                self.out.as_mut_ptr() as *mut c_char,
                self.out.len() as c_int,
            )
        };
        finish(&mut self.out, rc)
    }

    /// Feed one chunk (~1 s of 16 kHz mono) to the live transcriber. Returns the JSON for the
    /// committed text, `pending` tail, timings, ...; [`Cactus::stream_stop`] flushes the tail.
    pub fn stream_process(
        &mut self,
        pcm: &[f32],
        language: Option<&str>,
        keywords: Option<&str>,
    ) -> Result<String> {
        self.require(Models::SPEECH)?;
        let language = opt_cstring(language)?;
        let keywords = opt_cstring(keywords)?;
        let samples = pcm.len().min(c_int::MAX as usize) as c_int;
        self.out[0] = 0;
        let rc = unsafe {
            cactus_stream_transcribe_process(
                pcm.as_ptr(),
                samples,
                ptr(&language),
                ptr(&keywords),
                self.out.as_mut_ptr() as *mut c_char,
                self.out.len() as c_int,
            )
        };
        finish(&mut self.out, rc)
    }

    /// Flush the live transcriber's unconfirmed tail. End the stream with this.
    pub fn stream_stop(&mut self) -> Result<String> {
        self.require(Models::SPEECH)?;
        self.out[0] = 0;
        let rc = unsafe {
            cactus_stream_transcribe_stop(
                self.out.as_mut_ptr() as *mut c_char,
                self.out.len() as c_int,
            )
        };
        finish(&mut self.out, rc)
    }

    /// Describe a packed RGB image (vision model, `mirage.cact`). `options` is the settings JSON.
    pub fn describe(
        &mut self,
        rgb: &[u8],
        width: u32,
        height: u32,
        options: Option<&str>,
        language: Option<&str>,
        keywords: Option<&str>,
    ) -> Result<String> {
        self.require(Models::VISION)?;
        let need = width as usize * height as usize * 3;
        if rgb.len() < need {
            bail!(
                "cactus_describe: need {need} bytes of RGB for {width}x{height}, got {}",
                rgb.len()
            );
        }
        let options = opt_cstring(options)?;
        let language = opt_cstring(language)?;
        let keywords = opt_cstring(keywords)?;
        self.out[0] = 0;
        let rc = unsafe {
            cactus_describe(
                rgb.as_ptr(),
                width.min(c_int::MAX as u32) as c_int,
                height.min(c_int::MAX as u32) as c_int,
                ptr(&options),
                ptr(&language),
                ptr(&keywords),
                self.out.as_mut_ptr() as *mut c_char,
                self.out.len() as c_int,
            )
        };
        finish(&mut self.out, rc)
    }
}

/// Pull the transcript out of a transcription result. Output is expected to be JSON; if the
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

/// Whistle speech recognition: `Transcriber` for the caption pipeline, plus the streaming API.
pub struct Whistle {
    engine: Cactus,
    language: Option<String>,
}

impl Whistle {
    /// `model`: path to `whistle.cact`. `language`: e.g. "en", or `None` to auto-detect.
    pub fn load(model: &Path, language: Option<&str>) -> Result<Self> {
        let mut engine = Cactus::new();
        engine.load(model)?;
        engine.require(Models::SPEECH)?;
        Ok(Self {
            engine,
            language: language.map(str::to_owned),
        })
    }

    /// CPU threads for models loaded after this call. Call before [`Whistle::load`] for it to
    /// affect this model.
    pub fn set_threads(&self, threads: u32) {
        self.engine.set_threads(threads)
    }

    /// Language / keywords / word timestamps for the engine's own transcriptions.
    pub fn set_audio(&self, language: Option<&str>, keywords: Option<&str>, word_timestamps: bool) {
        self.engine.set_audio(language, keywords, word_timestamps)
    }

    /// Live transcription: feed ~1 s chunks, end with [`Whistle::stream_stop`].
    pub fn stream_process(&mut self, pcm16k: &[i16], keywords: Option<&str>) -> Result<String> {
        let pcm = to_f32(pcm16k);
        let language = self.language.as_deref();
        self.engine.stream_process(&pcm, language, keywords)
    }

    pub fn stream_stop(&mut self) -> Result<String> {
        self.engine.stream_stop()
    }
}

impl Transcriber for Whistle {
    fn transcribe(&mut self, pcm16k: &[i16]) -> Result<String> {
        let pcm = to_f32(pcm16k);
        let raw = self.engine.transcribe(
            &pcm,
            self.language.as_deref(),
            None,  // keywords: not exposed by the Transcriber trait
            false, // word timestamps off
        )?;
        Ok(extract_text(&raw))
    }
}

/// Needle 3 text agent: tool calling, structured extraction, embeddings.
pub struct Agent {
    engine: Cactus,
}

impl Agent {
    /// `model`: path to `needle3.cact` (or a fine-tuned `.cact`).
    pub fn load(model: &Path) -> Result<Self> {
        let mut engine = Cactus::new();
        engine.load(model)?;
        engine.require(Models::TEXT)?;
        Ok(Self { engine })
    }

    /// Bind the system facts and tool schemas. See [`Cactus::init`].
    pub fn init(
        &self,
        system_prompt: Option<&str>,
        tools_json: Option<&str>,
        tool_index_path: Option<&str>,
    ) -> Result<u32> {
        self.engine.init(system_prompt, tools_json, tool_index_path)
    }

    /// One turn. Pass text, or a 16 kHz mono clip and let the engine transcribe it first (needs a
    /// speech model loaded too). Returns the raw JSON envelope.
    pub fn complete(
        &mut self,
        input: Option<&str>,
        pcm: Option<&[f32]>,
        max_new_tokens: u32,
    ) -> Result<String> {
        self.engine.complete(input, pcm, max_new_tokens)
    }

    /// Rewind the conversation, keeping the tools.
    pub fn reset(&self) {
        self.engine.reset()
    }

    /// Embed text or audio into a vector.
    pub fn embed(&mut self, input: Option<&str>, pcm: Option<&[f32]>) -> Result<Vec<f32>> {
        self.engine.embed(input, pcm)
    }
}

/// Mirage vision: describe an image (needs `mirage.cact`).
pub struct Mirage {
    engine: Cactus,
}

impl Mirage {
    pub fn load(model: &Path) -> Result<Self> {
        let mut engine = Cactus::new();
        engine.load(model)?;
        engine.require(Models::VISION)?;
        Ok(Self { engine })
    }

    pub fn describe(
        &mut self,
        rgb: &[u8],
        width: u32,
        height: u32,
        options: Option<&str>,
        language: Option<&str>,
        keywords: Option<&str>,
    ) -> Result<String> {
        self.engine
            .describe(rgb, width, height, options, language, keywords)
    }
}

fn to_f32(pcm16k: &[i16]) -> Vec<f32> {
    pcm16k.iter().map(|&s| s as f32 / 32768.0).collect()
}
