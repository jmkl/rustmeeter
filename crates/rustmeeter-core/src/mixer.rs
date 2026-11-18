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

use crate::devices::{InputSource, fed_by, same_device};
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
    /// Display label: the device name, prefixed with `[Loopback]` for output taps.
    pub label: String,
    /// Raw device name, without the label decoration.
    pub name: String,
    pub loopback: bool,
    /// Render device that physically feeds this source: itself for a loopback tap, the render side
    /// for a virtual cable's capture side, `None` for a microphone.
    pub fed_by: Option<String>,
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

impl MixerView {
    /// `(strip, bus)` pairs where the strip listens to the very device that bus plays into: a round
    /// trip through the sound card. Two names means a virtual cable (`CABLE Output` is fed by
    /// `CABLE Input`). The engine drops these routes when it builds streams; this is how a frontend
    /// can explain why before the user hears anything.
    pub fn feedback_loops(&self) -> Vec<(StripId, usize)> {
        let mut loops = vec![];
        for strip in &self.strips {
            let Some(fed) = &strip.fed_by else { continue };
            for (bus, b) in self.buses.iter().enumerate() {
                if strip.routes[bus] && b.device.as_deref().is_some_and(|d| same_device(fed, d)) {
                    loops.push((strip.id, bus));
                }
            }
        }
        loops
    }

    /// Capture endpoint that hears this bus, when the bus is the render side of a virtual cable.
    /// A frontend should still check that the name exists in its capture-device list.
    pub fn capture_peer(&self, bus: usize) -> Option<String> {
        self.buses[bus]
            .device
            .as_deref()
            .and_then(crate::devices::capture_peer)
    }
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
                    name: s.source.name.clone(),
                    loopback: s.source.loopback,
                    fed_by: fed_by(&s.source.name, s.source.loopback),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eq::EqSettings;
    use crate::params::NUM_BUSES;

    const CABLE_IN: &str = "CABLE Input (VB-Audio Virtual Cable)";
    const CABLE_OUT: &str = "CABLE Output (VB-Audio Virtual Cable)";

    fn strip(name: &str, loopback: bool, routes: [bool; NUM_BUSES]) -> StripView {
        StripView {
            id: 1,
            label: format!("{}{name}", if loopback { "[Loopback] " } else { "" }),
            name: name.to_string(),
            loopback,
            fed_by: fed_by(name, loopback),
            routes,
            gain_db: 0.0,
            muted: false,
            eq: Arc::new(EqSettings::default()),
            params: Arc::new(Params::new(routes)),
        }
    }

    fn bus(device: Option<&str>) -> BusView {
        BusView {
            device: device.map(str::to_string),
            gain_db: 0.0,
            muted: false,
            params: Arc::new(Params::new([false; NUM_BUSES])),
        }
    }

    fn view(strip: StripView, a: Option<&str>, b: Option<&str>) -> MixerView {
        MixerView {
            running: false,
            strips: vec![strip],
            buses: [bus(a), bus(b)],
        }
    }

    #[test]
    fn a_cable_strip_routed_into_its_own_cable_is_a_loop() {
        let v = view(strip(CABLE_OUT, false, [true, false]), Some(CABLE_IN), None);
        assert_eq!(v.feedback_loops(), vec![(1, 0)]);
    }

    #[test]
    fn a_loopback_tap_routed_into_the_device_it_taps_is_a_loop() {
        let v = view(
            strip("Speakers", true, [false, true]),
            None,
            Some("Speakers"),
        );
        assert_eq!(v.feedback_loops(), vec![(1, 1)]);
    }

    #[test]
    fn a_microphone_can_be_sent_anywhere() {
        let v = view(
            strip("Headset Microphone", false, [true, true]),
            Some(CABLE_IN),
            Some("Speakers"),
        );
        assert!(v.feedback_loops().is_empty());
        assert_eq!(v.capture_peer(0).as_deref(), Some(CABLE_OUT));
        assert_eq!(v.capture_peer(1), None);
    }

    #[test]
    fn an_unrouted_strip_never_loops() {
        let v = view(
            strip(CABLE_OUT, false, [false, false]),
            Some(CABLE_IN),
            None,
        );
        assert!(v.feedback_loops().is_empty());
    }
}
