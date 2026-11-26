//! The frontend-facing service: mixer + captions + tool calling + settings in one place.
//!
//! A GUI, the CLI and the IPC server all want the same behaviour: restore the saved rack, keep the
//! settings file in sync with live changes, turn captions on and off, run Needle on a line. That
//! logic used to live in the GUI crate; it is here so every frontend behaves identically and the
//! GUI is only layout and message routing.
//!
//! `Service` owns the `Mixer`, which owns cpal streams, so it lives on the thread that created it
//! (the GUI/CLI main thread). The IPC server is polled from that same thread: see [`crate::ipc`].
//!
//! The shorter aliases (`mixer`, `captions`, `config`) are public because frontends reach for them
//! constantly; the settings bookkeeping stays private and is driven through `mark_dirty`,
//! `sync_settings` and `flush_settings`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::caption_controller::{CaptionCallback, CaptionController};
use crate::captions::Caption;
#[cfg(feature = "cactus")]
use crate::captions::Transcriber as _;
use crate::devices::{InputSource, default_input_name, default_output_name, list_inputs, list_outputs, same_device};
use crate::events::EventHandler;
use crate::mixer::{Mixer, MixerView, Strip, StripId};
use crate::params::NUM_BUSES;
use crate::settings::{BusSettings, Conf, StripSettings};
use crate::toolcall::{self, ToolCallHandle, ToolCallReport};

/// How long a settings edit must sit still before it is written. A drag moves in steps of a few
/// pixels, so this turns a few hundred writes into one, and the user never waits on the file.
pub const SETTINGS_SAVE_DELAY: Duration = Duration::from_millis(600);

/// Callbacks the frontend supplies. All of them may be called from arbitrary threads (events,
/// captions, the Needle worker), so they must be short and `Send + Sync`.
pub struct ServiceCallbacks {
    /// Stream lifecycle/errors. `None` keeps the core's stderr default.
    pub event: Option<EventHandler>,
    /// Every finished caption line, after it has also been offered to the tool worker.
    pub caption: Arc<dyn Fn(&Caption) + Send + Sync>,
    /// Result of the Needle worker for one caption line.
    pub toolcall: Arc<dyn Fn(ToolCallReport) + Send + Sync>,
    /// Language for the speech model (`None` = auto-detect).
    pub caption_lang: Option<String>,
    /// CPU thread limit for the speech model.
    pub caption_threads: Option<u32>,
}

impl Default for ServiceCallbacks {
    fn default() -> Self {
        Self {
            event: None,
            caption: Arc::new(|_| {}),
            toolcall: Arc::new(|_| {}),
            caption_lang: None,
            caption_threads: None,
        }
    }
}

/// The whole patch as the mixer has it right now: the rack in order, each strip's settings, and
/// both output buses. Compared against the last write to decide whether the file needs updating.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveSettings {
    pub strips: std::collections::BTreeMap<String, StripSettings>,
    pub order: Vec<InputSource>,
    pub buses: Option<[BusSettings; NUM_BUSES]>,
}

impl LiveSettings {
    pub fn capture(mixer: &Mixer) -> Self {
        Self {
            strips: mixer
                .strips()
                .iter()
                .map(|s| (s.source.label(), Service::strip_settings(s)))
                .collect(),
            order: mixer.strips().iter().map(|s| s.source.clone()).collect(),
            buses: Some(std::array::from_fn(|bus| {
                let live = mixer.bus(bus);
                BusSettings {
                    device: live.device.clone(),
                    gain_db: Some(live.params.gain_db()).filter(|db| zeroed(*db) != 0.0),
                    muted: live.params.muted(),
                }
            })),
        }
    }

    /// Whether this patch is already what `other` records. `other` may hold settings for strips
    /// that are no longer in the rack (kept so a removed device comes back configured), and both
    /// sides may hold all-default entries the serializer leaves out, so empty settings count as
    /// absent.
    pub fn same_as(&self, other: &Self) -> bool {
        self.order == other.order
            && self.buses == other.buses
            && self.strips.iter().all(|(label, settings)| {
                let mine = (!settings.is_empty()).then_some(settings);
                let theirs = other.strips.get(label).filter(|s| !s.is_empty());
                mine == theirs
            })
    }
}

/// Display name for a bus index (A and B in the mixer model).
pub fn bus_name(bus: usize) -> &'static str {
    match bus {
        0 => "A",
        1 => "B",
        _ => "?",
    }
}

/// Rounding helper: a fader that reads 0.0 but is a hair off is still "untouched".
pub(crate) fn zeroed(db: f32) -> f32 {
    if db.abs() < 0.05 { 0.0 } else { db }
}

/// The patch as the settings file records it, for the first comparison after a load.
fn disk_live(config: &crate::settings::Settings) -> LiveSettings {
    LiveSettings {
        strips: config.strips.clone(),
        order: config.strip_order.clone().unwrap_or_default(),
        buses: config.buses.clone(),
    }
}

/// Where a new strip goes before the user touches anything: microphones to B (the app side),
/// loopback taps to A (the listening side).
pub fn default_routes(loopback: bool) -> [bool; NUM_BUSES] {
    if loopback { [true, false] } else { [false, true] }
}

pub struct Service {
    pub mixer: Mixer,
    pub captions: CaptionController,
    /// Needle worker and the title-bar on/off flag.
    pub toolcall: Arc<ToolCallHandle>,
    pub config: Conf,
    /// Which strip captions start on, 1-based; `None` = first mic.
    pub caption_strip: Option<usize>,
    pub latest_caption: Caption,
    /// The patch as of the last edit check, and as it was last written to disk.
    pending: LiveSettings,
    saved: LiveSettings,
    save_at: Option<Instant>,
}

impl Service {
    /// Load the settings, restore the saved rack and buses, and build the caption/tool workers.
    /// Audio is **not** started: the frontend calls [`Service::start`] when it is ready, so errors
    /// can be shown in a window instead of aborting at construction time.
    pub fn open(callbacks: ServiceCallbacks) -> Result<Self> {
        let mut mixer = Mixer::new();
        if let Some(event) = callbacks.event {
            mixer.set_event_handler(event);
        }

        let config = Conf::load()?;

        let whistle_cact = config
            .settings
            .model_root
            .as_ref()
            .map(|root| root.join("whistle.cact"))
            .unwrap_or_else(|| PathBuf::from("whistle.cact"));

        let needle_cact = config
            .settings
            .model_root
            .as_ref()
            .map(|root| root.join("needle3.cact"));

        // One worker for the whole session: loading Needle is expensive, so it is done on the
        // first enabled caption and kept for later lines.
        let toolcall = ToolCallHandle::start(
            needle_cact,
            config.settings.tools.clone(),
            Some(config.log.with_file_name("toolcall.log")),
            callbacks.toolcall,
        );
        toolcall.set_enabled(config.settings.toolcall_enabled);

        // A caption is offered to the tool worker first (cheap queue), then to the frontend.
        let caption_worker = toolcall.clone();
        let frontend_caption: CaptionCallback = callbacks.caption;
        let on_caption: CaptionCallback = Arc::new(move |c: &Caption| {
            caption_worker.submit(&c.text);
            frontend_caption(c);
        });

        let captions = CaptionController::new(
            whistle_cact,
            config.log.clone(),
            callbacks.caption_lang,
            callbacks.caption_threads,
            on_caption,
        );

        let mut service = Self {
            mixer,
            captions,
            toolcall,
            config,
            caption_strip: None,
            latest_caption: Caption::default(),
            pending: LiveSettings::default(),
            saved: LiveSettings::default(),
            save_at: None,
        };

        service.restore_rack();
        service.restore_buses();

        let live = service.live_settings();
        service.pending = live;
        // Compare against what the file already says. A setup that was never written (an older
        // file, or a first run) is saved now instead of being silently forgotten.
        service.saved = disk_live(&service.config.settings);
        if !service.pending.same_as(&service.saved)
            && let Err(e) = service.flush_settings()
        {
            service.config.warn(e);
        }

        Ok(service)
    }

    // ----- lifecycle -----

    pub fn start(&mut self) -> Result<()> {
        self.mixer.start()
    }

    pub fn stop(&mut self) {
        self.mixer.stop();
        // The tick may never fire again: write any pending settings now.
        let _ = self.flush_settings();
    }

    pub fn is_running(&self) -> bool {
        self.mixer.is_running()
    }

    pub fn snapshot(&self) -> MixerView {
        self.mixer.snapshot()
    }

    /// Rebuild the streams after a structural change; a failed apply stays dirty in the `Mixer`.
    pub fn apply(&mut self) -> Result<()> {
        self.mixer.apply()
    }

    // ----- rack -----

    /// Rebuild the rack from `strip_order`, or from the first-run default when the file has no
    /// rack yet. A saved source whose device is gone is skipped and said so, and its settings stay
    /// in the file for the next time the device appears.
    pub fn restore_rack(&mut self) {
        let order = match self.config.settings.strip_order.clone() {
            Some(order) => order,
            None => {
                // A file from before the rack was recorded. The per-device settings are the only
                // record of which strips existed, so rebuild the rack from them (a one-time
                // upgrade: the written `strip_order` takes over from the next launch). An empty
                // file is a first run and gets the default microphone.
                let migrated: Vec<InputSource> = self
                    .config
                    .settings
                    .strips
                    .keys()
                    .map(|label| {
                        let (name, loopback) = label
                            .strip_prefix("[Loopback] ")
                            .map_or((label.as_str(), false), |name| (name, true));
                        InputSource {
                            name: name.to_string(),
                            loopback,
                        }
                    })
                    .collect();
                if migrated.is_empty() {
                    if let Some(name) = default_input_name() {
                        let id = self.mixer.add_strip(
                            InputSource {
                                name,
                                loopback: false,
                            },
                            default_routes(false),
                        );
                        self.restore_strip(id);
                    }
                    return;
                }
                migrated
            }
        };
        let inputs = list_inputs();
        for source in order {
            if !inputs.iter().any(|i| i == &source) {
                self.config.warn(format!(
                    "saved strip \"{}\" is not available; it was not added",
                    source.label()
                ));
                continue;
            }
            let routes = default_routes(source.loopback);
            let id = self.mixer.add_strip(source, routes);
            self.restore_strip(id);
        }
    }

    /// Put each bus back on its saved device and fader, or on the first-run default when the file
    /// has no bus settings yet. A saved device that is gone is left unplugged and said so.
    pub fn restore_buses(&mut self) {
        let Some(saved) = self.config.settings.buses.clone() else {
            self.mixer.set_bus_device(0, default_output_name());
            return;
        };
        let outputs = list_outputs();
        for (bus, bus_settings) in saved.iter().enumerate() {
            let device = bus_settings.device.clone().filter(|name| {
                if outputs.iter().any(|o| same_device(o, name)) {
                    true
                } else {
                    self.config.warn(format!(
                        "bus {} device \"{name}\" is not available; it was left unplugged",
                        bus_name(bus)
                    ));
                    false
                }
            });
            self.mixer.set_bus_device(bus, device);
            let params = &self.mixer.bus(bus).params;
            if let Some(db) = bus_settings.gain_db {
                params.set_gain_db(db);
            }
            params.set_muted(bus_settings.muted);
        }
    }

    /// Add a strip and put back whatever the file remembers for that device.
    pub fn add_strip(&mut self, source: InputSource, routes: [bool; NUM_BUSES]) -> StripId {
        let id = self.mixer.add_strip(source, routes);
        self.restore_strip(id);
        self.mark_dirty();
        id
    }

    /// Remove a strip, keeping its settings on disk so the device comes back configured.
    pub fn remove_strip(&mut self, id: StripId) -> bool {
        if let Some(strip) = self.mixer.strip(id) {
            let settings = Self::strip_settings(strip);
            let label = strip.source.label();
            if !settings.is_empty() {
                self.config.settings.set_strip(&label, settings);
            }
        }
        let removed = self.mixer.remove_strip(id);
        if removed {
            self.mark_dirty();
        }
        removed
    }

    /// Change a bus device and rebuild the streams if audio is running.
    pub fn set_bus_device(&mut self, bus: usize, device: Option<String>) -> Result<()> {
        self.mixer.set_bus_device(bus, device);
        self.apply()
    }

    /// Put a strip's saved EQ/gain/mute/routes back.
    pub fn restore_strip(&self, id: StripId) {
        let Some(strip) = self.mixer.strip(id) else {
            return;
        };
        let Some(saved) = self.config.settings.strip(&strip.source.label()) else {
            return;
        };
        if let Some(eq) = &saved.eq {
            strip.params.set_eq(eq.clone());
        }
        if let Some(db) = saved.gain_db {
            strip.params.set_gain_db(db);
        }
        strip.params.set_muted(saved.muted);
        if let Some(routes) = saved.routes {
            for bus in 0..NUM_BUSES {
                strip.params.set_routed(bus, routes[bus]);
            }
        }
    }

    pub fn strip_settings(strip: &Strip) -> StripSettings {
        let eq = strip.params.eq();
        StripSettings {
            eq: (eq.bypass || !eq.bands.is_empty()).then(|| (*eq).clone()),
            gain_db: Some(strip.params.gain_db()).filter(|db| zeroed(*db) != 0.0),
            muted: strip.params.muted(),
            routes: Some(strip.params.routes())
                .filter(|routes| *routes != default_routes(strip.source.loopback)),
        }
    }

    pub fn live_settings(&self) -> LiveSettings {
        LiveSettings::capture(&self.mixer)
    }

    // ----- captions / tools -----

    /// Start captioning a strip (1-based). `None` = the first mic strip.
    pub fn captions_on(&mut self, strip: Option<usize>) -> Result<usize> {
        let n = self.captions.on(&self.mixer, strip)?;
        self.caption_strip = Some(n);
        Ok(n)
    }

    /// Stop captioning. Returns false if it was not running.
    pub fn captions_off(&mut self) -> bool {
        self.captions.off()
    }

    /// Turn Needle tool calling on or off and remember the choice. The model itself loads lazily
    /// on the worker's first caption.
    pub fn set_toolcall_enabled(&mut self, enabled: bool) -> Result<()> {
        let previous = self.config.settings.toolcall_enabled;
        self.config.settings.toolcall_enabled = enabled;
        if let Err(e) = self.config.save() {
            self.config.settings.toolcall_enabled = previous;
            return Err(e.context("cannot save settings"));
        }
        self.toolcall.set_enabled(enabled);
        Ok(())
    }

    /// Run one line through Needle now, outside the caption worker. Used by IPC and by a future
    /// "ask" box. Blocks while the model works.
    pub fn toolcall(&self, text: &str) -> ToolCallReport {
        let model = self
            .config
            .settings
            .model_root
            .as_ref()
            .map(|root| root.join("needle3.cact"));
        toolcall::run_once(
            model.as_deref(),
            &self.config.settings.tools,
            text,
            Some(&self.config.log.with_file_name("toolcall.log")),
        )
    }

    /// Transcribe a WAV file with the configured Whistle model. Blocks while the model works.
    #[cfg(feature = "cactus")]
    pub fn transcribe(&self, path: &std::path::Path, language: Option<&str>) -> Result<String> {
        let model = self
            .config
            .settings
            .model_root
            .as_ref()
            .map(|root| root.join("whistle.cact"))
            .unwrap_or_else(|| PathBuf::from("whistle.cact"));
        let pcm = crate::wav::read_pcm16k(path)?;
        let mut whistle = crate::cactus::Whistle::load(&model, language)?;
        whistle.transcribe(&pcm)
    }

    /// Transcribe is only meaningful in builds that link the Cactus engine.
    #[cfg(not(feature = "cactus"))]
    pub fn transcribe(&self, _path: &std::path::Path, _language: Option<&str>) -> Result<String> {
        anyhow::bail!("this build has no Cactus engine: rebuild with default features (see README)")
    }

    // ----- settings persistence -----

    /// The rack changed from outside the normal message flow (add/remove, IPC, ...): schedule a
    /// write instead of waiting for the tick's change detection.
    pub fn mark_dirty(&mut self) {
        self.pending = self.live_settings();
        self.save_at = Some(Instant::now() + SETTINGS_SAVE_DELAY);
    }

    /// Debounced write of the settings. Returns an error message on I/O failure.
    pub fn sync_settings(&mut self, now: Instant) -> std::result::Result<(), String> {
        let live = self.live_settings();
        if live != self.pending {
            self.pending = live;
            self.save_at = Some(now + SETTINGS_SAVE_DELAY);
            return Ok(());
        }
        if !self.save_at.is_some_and(|due| now >= due) {
            return Ok(());
        }
        self.save_at = None;
        self.flush_settings()
    }

    /// Write the pending settings now. Returns an error message on I/O failure.
    pub fn flush_settings(&mut self) -> std::result::Result<(), String> {
        if self.pending.same_as(&self.saved) {
            return Ok(());
        }
        self.config.settings.strip_order = Some(self.pending.order.clone());
        for (label, settings) in &self.pending.strips {
            self.config.settings.set_strip(label, settings.clone());
        }
        if let Some(buses) = &self.pending.buses {
            for (bus, settings) in buses.iter().enumerate() {
                self.config.settings.set_bus(bus, settings.clone());
            }
        }
        match self.config.save() {
            Ok(()) => {
                self.saved = self.pending.clone();
                Ok(())
            }
            Err(e) => Err(format!("cannot save settings: {e:#}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::InputSource;
    use std::collections::BTreeMap;

    fn strip(gain_db: f32) -> StripSettings {
        StripSettings {
            gain_db: Some(gain_db),
            ..StripSettings::default()
        }
    }

    fn patch() -> LiveSettings {
        LiveSettings {
            strips: BTreeMap::from([
                ("mic".to_string(), strip(2.0)),
                ("untouched".to_string(), StripSettings::default()),
            ]),
            order: vec![
                InputSource {
                    name: "mic".to_string(),
                    loopback: false,
                },
                InputSource {
                    name: "Speakers".to_string(),
                    loopback: true,
                },
            ],
            buses: Some([
                BusSettings {
                    device: Some("Speakers".to_string()),
                    ..BusSettings::default()
                },
                BusSettings::default(),
            ]),
        }
    }

    #[test]
    fn a_patch_is_not_rewritten_when_only_ignored_details_differ() {
        let live = patch();
        // The file keeps settings for a strip that is no longer in the rack, and may spell a
        // default strip as `{}` or leave it out: neither is a change worth a write.
        let mut disk = live.clone();
        disk.strips.insert("removed".to_string(), strip(-3.0));
        assert!(live.same_as(&disk));

        let mut disk = live.clone();
        disk.strips.remove("untouched");
        assert!(live.same_as(&disk));
    }

    #[test]
    fn a_changed_rack_or_bus_is_a_write() {
        let live = patch();

        let mut disk = live.clone();
        disk.order.swap(0, 1);
        assert!(!live.same_as(&disk));

        let mut disk = live.clone();
        disk.order.clear();
        assert!(!live.same_as(&disk));

        let mut disk = live.clone();
        disk.buses = None;
        assert!(!live.same_as(&disk));

        let mut disk = live.clone();
        disk.buses.as_mut().unwrap()[0].device = None;
        assert!(!live.same_as(&disk));
    }

    #[test]
    fn a_changed_strip_is_a_write() {
        let live = patch();

        let mut disk = live.clone();
        disk.strips.insert("mic".to_string(), strip(3.0));
        assert!(!live.same_as(&disk));

        // A strip put back to its defaults must clear the line the file still holds.
        let mut disk = live.clone();
        disk.strips
            .insert("mic".to_string(), StripSettings::default());
        assert!(!live.same_as(&disk));

        // A strip the file has never heard of is a write, even at its defaults.
        let mut disk = live.clone();
        disk.strips.remove("mic");
        assert!(!live.same_as(&disk));
    }
}
