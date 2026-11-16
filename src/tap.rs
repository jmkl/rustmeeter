//! Tap: a passive copy of one strip's raw input (pre-gain, pre-mute, pre-EQ, regardless of routing).
//! It never affects what the buses/Discord receive.
//!
//! Every strip owns a tap buffer from the moment it is added. It is dormant (one atomic load per
//! audio buffer) until someone calls `Mixer::subscribe`, so a tap can be switched on and off while
//! audio is running, with no stream restart. One active `Tap` per strip at a time.

use ringbuf::{HeapConsumer, HeapProducer, HeapRb};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

pub const TAP_RATE: u32 = 16_000;

pub(crate) struct TapShared {
    pub(crate) enabled: AtomicBool,
    pub(crate) prod: Mutex<HeapProducer<f32>>, // audio thread: mono f32 at the device's native rate
    cons: Mutex<HeapConsumer<f32>>,            // reader side
    pub(crate) rate: AtomicU32,                // set by the engine when the stream opens
}

impl TapShared {
    pub(crate) fn new() -> Arc<Self> {
        let (p, c) = HeapRb::<f32>::new(48_000 * 8).split(); // ~8 s of headroom
        Arc::new(Self {
            enabled: AtomicBool::new(false),
            prod: Mutex::new(p),
            cons: Mutex::new(c),
            rate: AtomicU32::new(0),
        })
    }
}

pub struct Tap {
    shared: Arc<TapShared>,
    sum: f32,
    n: u32,
    phase: f64,
}

impl Tap {
    pub(crate) fn attach(shared: Arc<TapShared>) -> Tap {
        // Drop stale audio from a previous subscription, then start collecting.
        if let Ok(mut c) = shared.cons.lock() {
            while c.pop().is_some() {}
        }
        shared.enabled.store(true, Relaxed);
        Tap {
            shared,
            sum: 0.0,
            n: 0,
            phase: 0.0,
        }
    }

    /// Native sample rate of the captured device (0 until the mixer has started).
    pub fn source_rate(&self) -> u32 {
        self.shared.rate.load(Relaxed)
    }

    /// Drain everything captured so far, converted to 16 kHz mono i16 (box-filter downsample),
    /// appended to `out`.
    pub fn read_16k(&mut self, out: &mut Vec<i16>) {
        let rate = self.source_rate();
        if rate == 0 {
            return;
        }
        let ratio = rate as f64 / TAP_RATE as f64;
        let Ok(mut cons) = self.shared.cons.lock() else {
            return;
        };
        while let Some(x) = cons.pop() {
            self.sum += x;
            self.n += 1;
            self.phase += 1.0;
            if self.phase >= ratio {
                let v = self.sum / self.n as f32;
                out.push((v.clamp(-1.0, 1.0) * 32767.0) as i16);
                self.sum = 0.0;
                self.n = 0;
                self.phase -= ratio;
            }
        }
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        self.shared.enabled.store(false, Relaxed);
    }
}
