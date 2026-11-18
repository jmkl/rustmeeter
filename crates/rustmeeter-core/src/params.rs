//! Lock-free parameters shared between the audio threads and any control surface (CLI, GUI, MIDI...).
//! `Params` is `Send + Sync`; clone the `Arc<Params>` freely and call the setters from any thread.

use crate::eq::EqSettings;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

pub const NUM_BUSES: usize = 2;
pub const MIN_DB: f32 = -60.0;
pub const MAX_DB: f32 = 12.0;

pub(crate) struct AtomicF32(AtomicU32);
impl AtomicF32 {
    pub(crate) fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    pub(crate) fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Relaxed))
    }
    pub(crate) fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Relaxed)
    }
}

/// Controls for one strip or one bus (buses ignore EQ and routing).
pub struct Params {
    pub(crate) gain_db: AtomicF32,
    pub(crate) mute: AtomicBool,
    // Parametric EQ. Shared as an immutable snapshot: writers (UI/CLI) swap in a new `Arc`; the audio
    // thread polls `eq_version` and, only when it changed, `try_lock`s the state (never blocks).
    eq_state: Mutex<(u32, Arc<EqSettings>)>,
    eq_version: AtomicU32,
    pub(crate) routes: [AtomicBool; NUM_BUSES],
    pub(crate) level: AtomicF32, // linear peak, written by the audio thread
}

impl Params {
    pub fn new(routes: [bool; NUM_BUSES]) -> Self {
        Self {
            gain_db: AtomicF32::new(0.0),
            mute: AtomicBool::new(false),
            eq_state: Mutex::new((0, Arc::new(EqSettings::default()))),
            eq_version: AtomicU32::new(0),
            routes: [AtomicBool::new(routes[0]), AtomicBool::new(routes[1])],
            level: AtomicF32::new(0.0),
        }
    }

    pub fn gain_db(&self) -> f32 {
        self.gain_db.get()
    }
    pub fn set_gain_db(&self, db: f32) {
        self.gain_db.set(db.clamp(MIN_DB, MAX_DB));
    }

    pub fn muted(&self) -> bool {
        self.mute.load(Relaxed)
    }
    pub fn set_muted(&self, m: bool) {
        self.mute.store(m, Relaxed)
    }

    /// Current parametric EQ (cheap: clones an `Arc`).
    pub fn eq(&self) -> Arc<EqSettings> {
        self.eq_state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .clone()
    }

    /// Replace the whole EQ. Settings are clamped (`EqSettings::sanitize`) and take effect on the
    /// next audio buffer, with no stream restart.
    pub fn set_eq(&self, mut settings: EqSettings) {
        settings.sanitize();
        let version = {
            let mut g = self.eq_state.lock().unwrap_or_else(|e| e.into_inner());
            g.0 = g.0.wrapping_add(1);
            g.1 = Arc::new(settings);
            g.0
        };
        self.eq_version.store(version, Relaxed);
    }

    /// Read-modify-write helper: `params.update_eq(|eq| eq.bypass = true)`.
    pub fn update_eq(&self, f: impl FnOnce(&mut EqSettings)) {
        let mut settings = (*self.eq()).clone();
        f(&mut settings);
        self.set_eq(settings);
    }

    pub(crate) fn eq_version(&self) -> u32 {
        self.eq_version.load(Relaxed)
    }

    /// Audio-thread read: `None` if a writer holds the lock right now (retry next buffer).
    pub(crate) fn try_eq(&self) -> Option<(u32, Arc<EqSettings>)> {
        let g = self.eq_state.try_lock().ok()?;
        Some((g.0, g.1.clone()))
    }

    pub fn routed(&self, bus: usize) -> bool {
        self.routes[bus].load(Relaxed)
    }
    pub fn set_routed(&self, bus: usize, on: bool) {
        self.routes[bus].store(on, Relaxed)
    }

    /// Post-fader peak, linear (0.0..1.0), with a short decay. Poll this for meters.
    pub fn level(&self) -> f32 {
        self.level.get()
    }
    /// Same as `level()` in dB, floored at -90.
    pub fn level_db(&self) -> f32 {
        (20.0 * self.level().max(1e-9).log10()).max(-90.0)
    }
}

pub(crate) fn db_to_lin(db: f32) -> f32 {
    if db <= MIN_DB {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}
