//! Runtime on/off control for live captions, shared by every frontend (CLI, GUI).
//!
//! The Cactus engine is a process-wide singleton that can only be loaded once, so the controller
//! loads the model lazily on the first `on()` and keeps it for later on/off cycles.
//! Frontends only choose what happens to each caption line via the callback
//! (CLI: print it; GUI: push into a channel and request a repaint).

use crate::captions::{Caption, CaptionLogger, SharedTranscriber};
use crate::mixer::Mixer;
use anyhow::{Result, anyhow, bail};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Called on the caption thread for every transcribed utterance.
pub type CaptionCallback = Arc<dyn Fn(&Caption) + Send + Sync>;

pub struct CaptionController {
    model: PathBuf,
    log: PathBuf,
    lang: Option<String>,
    threads: Option<u32>,
    on_caption: CaptionCallback,
    asr: Option<SharedTranscriber>,
    logger: Option<CaptionLogger>,
    strip: Option<usize>,
}

impl CaptionController {
    pub fn new(
        model: PathBuf,
        log: PathBuf,
        lang: Option<String>,
        threads: Option<u32>,
        on_caption: CaptionCallback,
    ) -> Self {
        Self {
            model,
            log,
            lang,
            threads,
            on_caption,
            asr: None,
            logger: None,
            strip: None,
        }
    }

    #[cfg(feature = "cactus")]
    fn load_asr(&mut self) -> Result<SharedTranscriber> {
        use crate::cactus::Whistle;
        use crate::captions::share_transcriber;
        if let Some(a) = &self.asr {
            return Ok(a.clone());
        }
        let w = Whistle::load(&self.model, self.lang.as_deref())?;
        if let Some(n) = self.threads {
            w.set_threads(n);
        }
        let a = share_transcriber(w);
        self.asr = Some(a.clone());
        Ok(a)
    }

    #[cfg(not(feature = "cactus"))]
    fn load_asr(&mut self) -> Result<SharedTranscriber> {
        bail!("this build has no Cactus engine: rebuild with default features (see README)")
    }

    /// Start captioning a strip (1-based). `None` = the first mic strip.
    /// Instant: no audio restart. Loads the model on first use (may take a moment).
    /// Returns the 1-based strip number that is now being captioned.
    pub fn on(&mut self, mixer: &Mixer, strip: Option<usize>) -> Result<usize> {
        if self.logger.is_some() {
            bail!("captions are already on (turn them off first to change strip)");
        }
        let strips = mixer.strips();
        let idx = match strip {
            Some(n) => n
                .checked_sub(1)
                .filter(|i| *i < strips.len())
                .ok_or_else(|| anyhow!("no strip {n}"))?,
            None => strips
                .iter()
                .position(|s| !s.source.loopback)
                .ok_or_else(|| anyhow!("no mic strip to caption; pass a strip number"))?,
        };
        let s = &strips[idx];
        let label = if s.source.loopback { "desktop" } else { "me" };
        let asr = self.load_asr()?;
        let tap = mixer
            .subscribe(s.id)
            .ok_or_else(|| anyhow!("strip vanished"))?;
        let cb = self.on_caption.clone();
        let logger = CaptionLogger::start(
            tap,
            label.to_string(),
            self.log.clone(),
            asr,
            move |c: &Caption| (*cb)(c),
        )?;
        self.logger = Some(logger);
        self.strip = Some(idx + 1);
        Ok(idx + 1)
    }

    /// Stop captioning. Returns false if it was not running.
    pub fn off(&mut self) -> bool {
        self.strip = None;
        self.logger.take().is_some()
    }

    /// 1-based strip number being captioned, if running.
    pub fn active_strip(&self) -> Option<usize> {
        self.logger.as_ref().and(self.strip)
    }

    pub fn model_loaded(&self) -> bool {
        self.asr.is_some()
    }

    pub fn log_path(&self) -> &Path {
        &self.log
    }

    pub fn model_path(&self) -> &Path {
        &self.model
    }
}
