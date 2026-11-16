//! Lock-free parameters shared between the audio threads and any control surface (CLI, GUI, MIDI...).
//! `Params` is `Send + Sync`; clone the `Arc<Params>` freely and call the setters from any thread.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

pub const NUM_BUSES: usize = 2;
pub const EQ_BANDS: usize = 3;
pub const EQ_BAND_NAMES: [&str; EQ_BANDS] = ["low", "mid", "high"];
pub const MIN_DB: f32 = -60.0;
pub const MAX_DB: f32 = 12.0;
pub const EQ_RANGE_DB: f32 = 12.0;

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
    pub(crate) eq_db: [AtomicF32; EQ_BANDS], // low shelf 120 Hz, peak 1 kHz, high shelf 8 kHz
    pub(crate) routes: [AtomicBool; NUM_BUSES],
    pub(crate) level: AtomicF32, // linear peak, written by the audio thread
}

impl Params {
    pub fn new(routes: [bool; NUM_BUSES]) -> Self {
        Self {
            gain_db: AtomicF32::new(0.0),
            mute: AtomicBool::new(false),
            eq_db: [
                AtomicF32::new(0.0),
                AtomicF32::new(0.0),
                AtomicF32::new(0.0),
            ],
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

    /// and: 0 = low, 1 = mid, 2 = high.
    pub fn eq_db(&self, band: usize) -> f32 {
        self.eq_db[band].get()
    }
    pub fn set_eq_db(&self, band: usize, db: f32) {
        self.eq_db[band].set(db.clamp(-EQ_RANGE_DB, EQ_RANGE_DB));
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
