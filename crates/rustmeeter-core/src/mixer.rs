//! High-level API. This is the only thing a frontend (CLI, egui, MIDI, tray app...) needs.
//!
//! Model: the `Mixer` owns a list of strips and two buses. Structural changes
//! (add/remove strip, change a bus device) only edit the config and mark it dirty;
//! call `apply()` to rebuild the audio streams if running (a brief glitch).
//! Live controls (gain, mute, EQ, routing, meters) live in `Arc<Params>` and take effect
//! instantly with no rebuild: clone the Arc into any thread.
//!
//! `Mixer` itself holds cpal streams, which are not `Send` on every platform:
//! keep it on the thread that created it (the GUI's main thread).

use crate::devices::InputSource;
use crate::engine::{BusSpec, Engine, StripSpec};
use crate::eq::EqSettings;
use crate::events::{Event, EventHandler, default_handler};
use crate::params::{NUM_BUSES, Params};
use crate::tap::{Tap, TapShared};
use anyhow::Result;
use std::sync::Arc;

pub type StripId = u32;

pub struct Strip {
    pub id: StripId,
    pub source: InputSource,
    pub params: Arc<Params>,
    pub(crate) tap: Arc<TapShared>,
}

pub struct Bus {
    pub device: Option<String>,
    pub params: Arc<Params>,
}

/// Plain-data, `Send + Clone` picture of the mixer at one instant (for message-passing GUIs,
/// tests, settings export). Values are a point-in-time copy; for live controls and meters use
/// the `params` handles, which stay connected to the audio threads.
#[derive(Clone)]
pub struct StripView {
    pub id: StripId,
    pub label: String,
    pub loopback: bool,
    pub routes: [bool; NUM_BUSES],
    pub gain_db: f32,
    pub muted: bool,
    pub eq: Arc<EqSettings>,
    pub params: Arc<Params>,
}

#[derive(Clone)]
pub struct BusView {
    pub device: Option<String>,
    pub gain_db: f32,
    pub muted: bool,
    pub params: Arc<Params>,
}

#[derive(Clone)]
pub struct MixerView {
    pub running: bool,
    pub strips: Vec<StripView>,
    pub buses: [BusView; NUM_BUSES],
}

pub struct Mixer {
    strips: Vec<Strip>,
    buses: [Bus; NUM_BUSES],
    engine: Option<Engine>,
    dirty: bool,
    next_id: StripId,
    handler: EventHandler,
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

impl Mixer {
    pub fn new() -> Self {
        Self {
            strips: vec![],
            buses: [
                Bus {
                    device: None,
                    params: Arc::new(Params::new([false; NUM_BUSES])),
                },
                Bus {
                    device: None,
                    params: Arc::new(Params::new([false; NUM_BUSES])),
                },
            ],
            engine: None,
            dirty: false,
            next_id: 1,
            handler: default_handler(),
        }
    }

    // ----- structure (call apply() afterwards if the mixer is running) -----

    pub fn add_strip(&mut self, source: InputSource, routes: [bool; NUM_BUSES]) -> StripId {
        let id = self.next_id;
        self.next_id += 1;
        self.strips.push(Strip {
            id,
            source,
            params: Arc::new(Params::new(routes)),
            tap: TapShared::new(),
        });
        self.dirty = true;
        id
    }

    pub fn remove_strip(&mut self, id: StripId) -> bool {
        let before = self.strips.len();
        self.strips.retain(|s| s.id != id);
        let removed = self.strips.len() != before;
        self.dirty |= removed;
        removed
    }

    /// Get a passive tap of this strip's raw input (for captions, recording, wake-word, ...).
    /// Works while audio is running: no restart, no glitch. The tap works even if the strip is
    /// routed nowhere. Drop the returned `Tap` to switch it off again. One active tap per strip.
    pub fn subscribe(&self, id: StripId) -> Option<Tap> {
        let strip = self.strips.iter().find(|s| s.id == id)?;
        Some(Tap::attach(strip.tap.clone()))
    }

    pub fn set_bus_device(&mut self, bus: usize, device: Option<String>) {
        if self.buses[bus].device != device {
            self.buses[bus].device = device;
            self.dirty = true;
        }
    }

    // ----- queries -----

    pub fn strips(&self) -> &[Strip] {
        &self.strips
    }
    pub fn strip(&self, id: StripId) -> Option<&Strip> {
        self.strips.iter().find(|s| s.id == id)
    }
    pub fn bus(&self, bus: usize) -> &Bus {
        &self.buses[bus]
    }

    /// Where stream errors and lifecycle events go (default: errors to stderr). The handler can be
    /// called from audio/error threads. Set it before `start()`.
    pub fn set_event_handler(&mut self, handler: EventHandler) {
        self.handler = handler;
    }

    /// Owned copy of the current state (see `MixerView`).
    pub fn snapshot(&self) -> MixerView {
        MixerView {
            running: self.is_running(),
            strips: self
                .strips
                .iter()
                .map(|s| StripView {
                    id: s.id,
                    label: s.source.label(),
                    loopback: s.source.loopback,
                    routes: std::array::from_fn(|b| s.params.routed(b)),
                    gain_db: s.params.gain_db(),
                    muted: s.params.muted(),
                    eq: s.params.eq(),
                    params: s.params.clone(),
                })
                .collect(),
            buses: std::array::from_fn(|i| BusView {
                device: self.buses[i].device.clone(),
                gain_db: self.buses[i].params.gain_db(),
                muted: self.buses[i].params.muted(),
                params: self.buses[i].params.clone(),
            }),
        }
    }

    // ----- lifecycle -----

    pub fn is_running(&self) -> bool {
        self.engine.is_some()
    }

    /// (Re)build all streams from the current config and start audio.
    pub fn start(&mut self) -> Result<()> {
        self.engine = None; // drop old streams first
        let strips: Vec<StripSpec> = self
            .strips
            .iter()
            .map(|s| StripSpec {
                source: s.source.clone(),
                params: s.params.clone(),
                tap: Some(s.tap.clone()),
            })
            .collect();
        let buses = [
            BusSpec {
                device: self.buses[0].device.clone(),
                params: self.buses[0].params.clone(),
            },
            BusSpec {
                device: self.buses[1].device.clone(),
                params: self.buses[1].params.clone(),
            },
        ];
        self.engine = Some(Engine::start(&strips, &buses, &self.handler)?);
        self.dirty = false;
        (*self.handler)(&Event::Started);
        Ok(())
    }

    pub fn stop(&mut self) {
        self.engine = None;
    }

    /// If running and the structure changed, rebuild the streams. Cheap no-op otherwise.
    pub fn apply(&mut self) -> Result<()> {
        if self.engine.is_some() && self.dirty {
            self.start()?;
        }
        Ok(())
    }
}
