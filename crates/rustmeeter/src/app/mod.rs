//! The `MixerApp` component: state, lifecycle and the top-level layout.
//!
//! Each panel lives in its own submodule as an `impl MixerApp` block, so the state definition and
//! the message loop stay small. Submodules are descendants of `app`, which lets them read the
//! component's private fields without widening them to the whole crate.

mod buses;
mod caption_window;
mod eq;
mod status;
mod strips;
mod toolbar;
mod transcript;

pub(crate) use caption_window::{CaptionWindow, CaptionWindowInput};

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustmeeter_core::*;
use windows_canvas::Invalidator;
use windows_reactor::*;

use self::eq::{EqDrag, MODAL_PAD};
use crate::config::{BusSettings, Conf, Settings, StripSettings};
use crate::messages::Message;
use crate::theme::*;
use crate::toolcall::{ToolCallHandle, ToolCallReport};
use crate::util::{bus_name, zeroed};

/// Meter refresh period (~30 fps).
const TICK: Duration = Duration::from_millis(33);
/// How long a settings edit must sit still before it is written. A drag moves in steps of a few
/// pixels, so this turns a few hundred writes into one, and the user never waits on the file.
const SETTINGS_SAVE_DELAY: Duration = Duration::from_millis(600);

/// The whole patch as the mixer has it right now: the rack in order, each strip's settings, and
/// both output buses. Compared against the last write to decide whether the file needs updating.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LiveSettings {
    strips: BTreeMap<String, StripSettings>,
    order: Vec<InputSource>,
    buses: Option<[BusSettings; NUM_BUSES]>,
}

impl LiveSettings {
    fn capture(mixer: &Mixer) -> Self {
        Self {
            strips: mixer
                .strips()
                .iter()
                .map(|s| (s.source.label(), SharedMixer::strip_settings(s)))
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
    fn same_as(&self, other: &Self) -> bool {
        self.order == other.order
            && self.buses == other.buses
            && self.strips.iter().all(|(label, settings)| {
                let mine = (!settings.is_empty()).then_some(settings);
                let theirs = other.strips.get(label).filter(|s| !s.is_empty());
                mine == theirs
            })
    }
}

/// The patch as the settings file records it, for the first comparison after a load.
fn disk_live(config: &Settings) -> LiveSettings {
    LiveSettings {
        strips: config.strips.clone(),
        order: config.strip_order.clone().unwrap_or_default(),
        buses: config.buses.clone(),
    }
}

/// Where a new strip goes before the user touches anything: microphones to B (the app side),
/// loopback taps to A (the listening side).
fn default_routes(loopback: bool) -> [bool; NUM_BUSES] {
    if loopback {
        [true, false]
    } else {
        [false, true]
    }
}

/// Persistent state that must outlive the window. `Mixer` owns cpal audio streams and
/// `CaptionController` owns the loaded whisper model: both are expensive to recreate and must keep
/// running while the window is hidden or closed to tray.
///
/// Wrapped in `Rc<RefCell<>>` so the `Application` root and the per-window `Component` can share
/// it. Everything here runs on the single UI thread, so no `Arc` needed for the wrapper.
pub(crate) struct SharedMixer {
    pub(crate) mixer: Mixer,
    pub(crate) caps: CaptionController,
    /// Needle worker and the title-bar on/off flag.
    pub(crate) toolcall: Arc<ToolCallHandle>,
    pub(crate) config: Conf,
    pub(crate) caption_strip: Option<usize>, // 1-based; None = first mic
    pub(crate) latest_caption: Caption,
    /// The patch as of the last edit check, and as it was last written to disk.
    pub(crate) pending: LiveSettings,
    pub(crate) saved: LiveSettings,
    pub(crate) save_at: Option<Instant>,
    /// Completion handle for the current window component. Event and caption callbacks clone from
    /// this slot; a stale (closed) completion silently drops messages, which is fine while the
    /// window is closed.
    pub(crate) completion_slot: Arc<Mutex<Option<ComponentCompletion<Message>>>>,
}

pub(crate) type SharedMixerRef = Rc<RefCell<SharedMixer>>;

/// Newtype for `SharedMixerRef` that satisfies `Component::Input: Clone + PartialEq + 'static`.
/// Two handles are equal iff they point to the same allocation (pointer identity).
#[derive(Clone)]
pub(crate) struct SharedMixerInput(pub SharedMixerRef);

impl PartialEq for SharedMixerInput {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl SharedMixer {
    /// Build the mixer, caption controller and config. Audio is **not** started here: the first
    /// `MixerApp` creation calls `mixer.start()` so that errors can be shown in the window.
    pub(crate) fn new() -> SharedMixerRef {
        let completion_slot: Arc<Mutex<Option<ComponentCompletion<Message>>>> =
            Arc::new(Mutex::new(None));

        let mut mixer = Mixer::new();
        {
            let slot = completion_slot.clone();
            mixer.set_event_handler(Arc::new(move |e: &Event| {
                let e = e.clone();
                if let Some(completion) = slot.lock().unwrap().clone() {
                    _ = completion.complete_with(move || Message::Event(e));
                }
            }));
        }

        let config = Conf::load().expect("Failed to load config");

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
        let toolcall = {
            let slot = completion_slot.clone();
            let notify: Arc<dyn Fn(ToolCallReport) + Send + Sync> = Arc::new(move |report| {
                if let Some(completion) = slot.lock().unwrap().clone() {
                    _ = completion.complete_with(move || Message::ToolCall(report));
                }
            });
            let handle = ToolCallHandle::start(
                needle_cact,
                config.settings.tools.clone(),
                Some(config.log.with_file_name("toolcall.log")),
                notify,
            );
            handle.set_enabled(config.settings.toolcall_enabled);
            handle
        };

        let caps = {
            let slot = completion_slot.clone();
            let toolcall = toolcall.clone();
            CaptionController::new(
                PathBuf::from(whistle_cact),
                PathBuf::from(&config.log),
                Some("en".to_owned()),
                None,
                Arc::new(move |c: &Caption| {
                    let c = c.clone();
                    toolcall.submit(&c.text);
                    if let Some(completion) = slot.lock().unwrap().clone() {
                        _ = completion.complete_with(move || Message::Caption(c));
                    }
                }),
            )
        };

        let mut shared = Self {
            mixer,
            caps,
            toolcall,
            caption_strip: None,
            latest_caption: Caption::default(),
            pending: LiveSettings::default(),
            saved: LiveSettings::default(),
            save_at: None,
            completion_slot,
            config,
        };

        shared.restore_rack();
        shared.restore_buses();

        let live = shared.live_settings();
        shared.pending = live;
        // Compare against what the file already says. A setup that was never written (an older file,
        // or a first run) is saved now instead of being silently forgotten.
        shared.saved = disk_live(&shared.config.settings);
        if !shared.pending.same_as(&shared.saved)
            && let Err(e) = shared.flush_settings()
        {
            shared.config.warn(e);
        }

        Rc::new(RefCell::new(shared))
    }

    /// Rebuild the rack from `strip_order`, or from the first-run default when the file has no
    /// rack yet. A saved source whose device is gone is skipped and said so, and its settings stay
    /// in the file for the next time the device appears.
    fn restore_rack(&mut self) {
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
    fn restore_buses(&mut self) {
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

    // ----- settings persistence (moved from MixerApp) -----

    fn strip_settings(strip: &Strip) -> StripSettings {
        let eq = strip.params.eq();
        StripSettings {
            eq: (eq.bypass || !eq.bands.is_empty()).then(|| (*eq).clone()),
            gain_db: Some(strip.params.gain_db()).filter(|db| zeroed(*db) != 0.0),
            muted: strip.params.muted(),
            routes: Some(strip.params.routes())
                .filter(|routes| *routes != default_routes(strip.source.loopback)),
        }
    }

    fn live_settings(&self) -> LiveSettings {
        LiveSettings::capture(&self.mixer)
    }

    fn restore_strip(&self, id: StripId) {
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

    /// Turn Needle tool calling on or off and remember the choice. The model itself loads lazily
    /// on the worker's first caption.
    pub(crate) fn set_toolcall_enabled(&mut self, enabled: bool) -> Result<(), String> {
        let previous = self.config.settings.toolcall_enabled;
        self.config.settings.toolcall_enabled = enabled;
        if let Err(e) = self.config.save() {
            self.config.settings.toolcall_enabled = previous;
            return Err(format!("cannot save settings: {e:#}"));
        }
        self.toolcall.set_enabled(enabled);
        Ok(())
    }

    /// Debounced write of the settings. Returns an error message on I/O failure.
    fn sync_settings(&mut self, now: Instant) -> Result<(), String> {
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
    fn flush_settings(&mut self) -> Result<(), String> {
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

pub(crate) struct MixerApp {
    shared: SharedMixerRef,
    inputs: Vec<InputSource>,
    outputs: Vec<String>,
    add_index: Option<usize>, // selection in the "add input" combo
    error: Option<String>,
    timer: Option<ComponentTimer>,
    eq_open: Option<StripId>,
    eq_drag: Option<EqDrag>,
    /// Repaint requests for the open EQ graph's Direct2D canvas (see `draw_eq_curve`).
    eq_invalidator: Invalidator,
    /// Window client width in DIPs; sizes the EQ band table (see `eq_table_width`).
    window_w: f64,
    titlebar_content_show: bool,
}

impl MixerApp {
    fn start(&mut self) {
        if let Err(e) = self.shared.borrow_mut().mixer.start() {
            self.error = Some(format!("{e:#}"));
        }
    }

    /// Rebuild the streams after a structural change; a failed apply stays dirty in the `Mixer`.
    fn apply(&mut self) {
        if let Err(e) = self.shared.borrow_mut().mixer.apply() {
            self.error = Some(format!("{e:#}"));
        }
    }

    pub(crate) fn strip_params(&self, id: StripId) -> Option<Arc<Params>> {
        self.shared
            .borrow()
            .mixer
            .strip(id)
            .map(|s| s.params.clone())
    }

    // ----- settings persistence: forwarded to SharedMixer -----

    pub(crate) fn bus_selection(&self, view: &MixerView, bus: usize) -> Option<usize> {
        match &view.buses[bus].device {
            None => Some(0),
            Some(device) => self.outputs.iter().position(|o| o == device).map(|i| i + 1),
        }
    }

    /// The capture endpoint apps pick as a microphone, when this bus is the render side of a
    /// virtual cable *and* that capture endpoint really exists. `None` for a real speaker: a bus is
    /// a destination, and no speaker is something you can record.
    pub(crate) fn bus_capture_peer(&self, view: &MixerView, bus: usize) -> Option<String> {
        view.capture_peer(bus).filter(|peer| {
            self.inputs
                .iter()
                .any(|i| !i.loopback && same_device(&i.name, peer))
        })
    }

    /// One line answering "who ends up hearing this bus?", plus the long form for a tooltip.
    pub(crate) fn bus_role(&self, view: &MixerView, bus: usize) -> (&'static str, String) {
        let name = bus_name(bus);
        if view.buses[bus].device.is_none() {
            return (
                "NO DEVICE",
                "Pick an output device: everything routed here is played into it.".to_string(),
            );
        }
        match self.bus_capture_peer(view, bus) {
            Some(peer) => (
                "MIC FOR APPS",
                format!(
                    "This is not a speaker: it is the render side of a virtual cable. The endpoint \
                     named below is what an app hears when it records \"{peer}\" as its microphone."
                ),
            ),
            None => (
                "WHAT YOU HEAR",
                format!(
                    "Bus {name} plays into this output device. To hand audio to another app as a \
                     microphone, pick a virtual cable's *Input* device here instead."
                ),
            ),
        }
    }

    /// Every routed round trip in the current patch, phrased as what to do about it.
    fn loop_notes(&self, view: &MixerView) -> Vec<String> {
        view.feedback_loops()
            .into_iter()
            .map(|(id, bus)| {
                let strip = view.strips.iter().find(|s| s.id == id);
                let label = strip.map_or("strip", |s| s.label.as_str());
                let device = view.buses[bus].device.as_deref().unwrap_or("?");
                let name = bus_name(bus);
                format!(
                    "{label} listens to \"{device}\", which is the device bus {name} plays into: a \
                     round trip, so the route is dropped. Send bus {name} somewhere else, or remove \
                     the strip."
                )
            })
            .collect()
    }

    /// Everything the mixer wants to tell you, above the rack: one bar per problem, so each can be
    /// read on its own. Empty (and zero height) when there is nothing to say.
    fn alerts(&self, view: &MixerView, context: &ViewContext<Self>) -> StackPanel {
        let mut bars: Vec<View> = vec![];
        if let Some(error) = &self.error {
            bars.push(
                InfoBar::new()
                    .is_open(true)
                    .is_closable(true)
                    .severity(InfoBarSeverity::Error)
                    .title("Error")
                    .message(error)
                    .on_closed(context.callback(|()| Message::DismissError))
                    .into(),
            );
        }
        for note in self.loop_notes(view) {
            bars.push(
                InfoBar::new()
                    .is_open(true)
                    .severity(InfoBarSeverity::Warning)
                    .title("Routing loop")
                    .message(&note)
                    .into(),
            );
        }
        StackPanel::new().spacing(6.0).children(bars)
    }

    fn transport(&self, view: &MixerView, context: &ViewContext<Self>) -> StackPanel {
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(0.0)
            .children((
                if view.running {
                    action_button(
                        "\u{e769}",
                        RED_STRONG,
                        WHITE,
                        crate::component::button::Side::Left,
                        context.callback(|()| Message::ToggleRun),
                    )
                } else {
                    action_button(
                        "\u{e768}",
                        GREEN,
                        INK,
                        crate::component::button::Side::Left,
                        context.callback(|()| Message::ToggleRun),
                    )
                },
                action_button(
                    "\u{e72c}",
                    DIM,
                    INK,
                    crate::component::button::Side::Right,
                    context.callback(|()| Message::Refresh),
                ),
            ))
    }

    fn captions(&self, view: &MixerView, context: &ViewContext<Self>) -> StackPanel {
        let (active_strip, caption_strip, toolcall) = {
            let shared = self.shared.borrow();
            (
                shared.caps.active_strip(),
                shared.caption_strip,
                shared.toolcall.enabled(),
            )
        };
        let mut captions: Vec<View> = vec![];

        match active_strip {
            Some(_) => captions.push(
                action_button(
                    "\u{E720}",
                    RED,
                    INK,
                    crate::component::button::Side::None,
                    context.callback(|()| Message::CaptionsOff),
                )
                .tooltip("Caption")
                .into(),
            ),
            None => {
                let mut items: Vec<String> = vec!["first mic".to_string()];
                for i in 1..=view.strips.len() {
                    items.push(format!("strip {i}"));
                }
                captions.push(
                    ComboBox::new()
                        .exit_transition(ExitTransition::fade(Duration::from_millis(500)))
                        .width(104.0)
                        .vertical_alignment(VerticalAlignment::Center)
                        .items_source(items)
                        .selected_index(Some(caption_strip.unwrap_or(0)))
                        .on_selection_changed(
                            context.callback(|index: Option<usize>| Message::CaptionStrip(index)),
                        )
                        .into(),
                );
                captions.push(
                    action_button(
                        "\u{E720}",
                        BLUE,
                        INK,
                        crate::component::button::Side::None,
                        context.callback(|()| Message::CaptionsOn),
                    )
                    .tooltip("Caption")
                    .into(),
                );
            }
        }
        captions.push(
            CheckBox::new()
                .is_checked(toolcall)
                .content("TOOLS")
                .on_is_checked_changed(context.callback(|checked: Option<bool>| {
                    Message::SetToolCall(checked.unwrap_or(false))
                }))
                .tooltip(
                    "Run each finished caption through Needle 3 and execute the tool calls \
                     described in config.json's \"tools\" array.",
                )
                .into(),
        );
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(6.0)
            .children(captions)
    }
}

impl Component for MixerApp {
    // `SharedMixerInput` wraps `Rc<RefCell<SharedMixer>>` with pointer-equality for `PartialEq`.
    type Input = SharedMixerInput;
    type Message = Message;

    fn create(input: &SharedMixerInput, context: &ComponentContext<Self>) -> Self {
        // Wire up the completion slot so event/caption callbacks forward to this component.
        {
            let shared = input.0.borrow();
            *shared.completion_slot.lock().unwrap() = Some(context.completion());
        }

        // Start audio if this is the first window open (not already running).
        let error = {
            let mut shared = input.0.borrow_mut();
            if !shared.mixer.is_running() {
                match shared.mixer.start() {
                    Ok(()) => shared.config.warning.clone(),
                    Err(e) => Some(format!("{e:#}")),
                }
            } else {
                None
            }
        };

        let app = Self {
            shared: input.0.clone(),
            inputs: list_inputs(),
            outputs: list_outputs(),
            add_index: None,
            error,
            timer: Some(context.set_timeout(TICK, Message::Tick)),
            eq_open: None,
            eq_drag: None,
            titlebar_content_show: false,
            eq_invalidator: Invalidator::new(),
            window_w: 940.0,
        };
        app
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        match message {
            // No state change: the re-render triggered by this message is what moves the meters.
            // It is also the app's only heartbeat, so the debounced settings write rides along.
            Message::Tick => {
                if let Err(e) = self.shared.borrow_mut().sync_settings(Instant::now()) {
                    self.error = Some(e);
                }
                // Only keep ticking while audio is running: meters need ~30 fps updates,
                // but an idle app has nothing to repaint.
                if self.shared.borrow().mixer.is_running() {
                    self.timer = Some(context.set_timeout(TICK, Message::Tick));
                } else {
                    self.timer = None;
                }
            }
            Message::Event(Event::Error(e)) => self.error = Some(e),
            // The engine drops looped routes and says so once; the alert bar below is derived from
            // the live patch instead, so it also explains a loop before audio is started.
            Message::Event(Event::Warning(_)) => {}
            Message::Event(Event::Started) => {}
            Message::Caption(c) => {
                self.shared.borrow_mut().latest_caption = c;
            }
            Message::ToolCall(report) => {
                let ToolCallReport { text, calls, error } = report;
                let mut problems: Vec<String> = error.into_iter().collect();
                problems.extend(calls.into_iter().filter_map(|call| match call.result {
                    Ok(_) => None,
                    Err(e) => Some(format!("{} {} failed: {e}", call.name, call.arguments)),
                }));
                if !problems.is_empty() {
                    self.error = Some(format!(
                        "toolcall \"{}\": {}",
                        text.trim(),
                        problems.join("; ")
                    ));
                }
            }
            Message::SetToolCall(enabled) => {
                if let Err(e) = self.shared.borrow_mut().set_toolcall_enabled(enabled) {
                    self.error = Some(e);
                }
            }
            Message::ToggleRun => {
                let is_running = self.shared.borrow().mixer.is_running();
                if is_running {
                    self.shared.borrow_mut().mixer.stop();
                    // The tick may never fire again: write any pending settings now.
                    if let Err(e) = self.shared.borrow_mut().flush_settings() {
                        self.error = Some(e);
                    }
                } else {
                    self.start();
                    // Restart the meter tick if it was stopped while idle.
                    if self.timer.is_none() {
                        self.timer = Some(context.set_timeout(TICK, Message::Tick));
                    }
                }
            }
            Message::ToggleTitleBar => self.titlebar_content_show = !self.titlebar_content_show,
            Message::Refresh => {
                self.inputs = list_inputs();
                self.outputs = list_outputs();
            }
            Message::AddInput(index) => {
                if let Some(src) = index.and_then(|i| self.inputs.get(i)).cloned() {
                    let mut shared = self.shared.borrow_mut();
                    let id = shared
                        .mixer
                        .add_strip(src.clone(), default_routes(src.loopback));
                    // A device that was configured in an earlier session comes back configured.
                    shared.restore_strip(id);
                    shared.pending = shared.live_settings();
                    // `pending` was updated by hand, so the tick's change check will not see this:
                    // put the write on the debounce timer explicitly.
                    shared.save_at = Some(Instant::now() + SETTINGS_SAVE_DELAY);
                    if let Err(e) = shared.mixer.apply() {
                        drop(shared);
                        self.error = Some(format!("{e:#}"));
                    }
                }
                self.add_index = None;
            }
            Message::RemoveStrip(id) => {
                {
                    let mut shared = self.shared.borrow_mut();
                    // Snapshot before dropping it: a fader moved less than a debounce ago would
                    // otherwise never reach the file, because the live map no longer has the strip.
                    // The entry stays in the file on purpose -- that is what makes the device come
                    // back configured when it is added again.
                    if let Some(strip) = shared.mixer.strip(id) {
                        let settings = SharedMixer::strip_settings(strip);
                        let label = strip.source.label();
                        if !settings.is_empty() {
                            shared.config.settings.set_strip(&label, settings);
                        }
                    }
                    shared.mixer.remove_strip(id);
                    shared.pending = shared.live_settings();
                    // As in `AddInput`: the rack changed, so schedule the write by hand.
                    shared.save_at = Some(Instant::now() + SETTINGS_SAVE_DELAY);
                    if let Err(e) = shared.mixer.apply() {
                        drop(shared);
                        self.error = Some(format!("{e:#}"));
                    }
                }
                if self.eq_open == Some(id) {
                    self.eq_open = None;
                    self.eq_drag = None;
                }
                self.eq_invalidator.invalidate();
                // Losing the label makes `pending` differ from `saved`, which schedules the write.
            }
            Message::SetBus(bus, Some(index)) => {
                let device = index
                    .checked_sub(1)
                    .and_then(|i| self.outputs.get(i))
                    .cloned();
                self.shared.borrow_mut().mixer.set_bus_device(bus, device);
                self.apply();
            }
            Message::SetBus(_, None) => {}
            Message::CaptionStrip(Some(0)) => self.shared.borrow_mut().caption_strip = None,
            Message::CaptionStrip(Some(n)) => self.shared.borrow_mut().caption_strip = Some(n),
            Message::CaptionStrip(None) => {}
            Message::CaptionsOn => {
                let result = {
                    let mut shared = self.shared.borrow_mut();
                    if shared.config.settings.model_root.is_none() {
                        Err(format!(
                            "no \"model_root\" in {}: add the folder that holds whistle.cact",
                            shared.config.settings_path().display()
                        ))
                    } else {
                        let s = &mut *shared;
                        s.caps
                            .on(&s.mixer, s.caption_strip)
                            .map_err(|e| format!("{e:#}"))
                    }
                };
                if let Err(e) = result {
                    self.error = Some(e);
                }
            }
            Message::CaptionsOff => {
                self.shared.borrow_mut().caps.off();
            }
            Message::DismissError => self.error = None,
            Message::WindowWidth(width) => self.window_w = width,
            Message::ToggleEq(id) => {
                self.eq_open = if self.eq_open == Some(id) {
                    None
                } else {
                    Some(id)
                };
                self.eq_drag = None;
                self.eq_invalidator.invalidate();
            }
            Message::EqPress {
                strip,
                band,
                wx,
                wy,
                right,
            } => {
                self.eq_press(strip, band, wx, wy, right);
                self.eq_invalidator.invalidate();
            }
            Message::EqMove { wx, wy, left } => {
                self.eq_move(wx, wy, left);
                self.eq_invalidator.invalidate();
            }
            Message::EqRelease => self.eq_drag = None,
            Message::EqAddAt { strip, x, y, left } => {
                self.eq_add_at(strip, x, y, left);
                self.eq_invalidator.invalidate();
            }
            Message::EqAdd(strip) => {
                self.eq_add_default(strip);
                self.eq_invalidator.invalidate();
            }
            Message::MinimizeToTray => {
                let _ = context.close_window();
            }
        }
    }

    fn view(&self, _input: &SharedMixerInput, context: &mut ViewContext<Self>) -> View {
        let (view, icon) = {
            let shared = self.shared.borrow();
            (shared.mixer.snapshot(), shared.config.icon.clone())
        };

        context.window_title("RustMeeter");
        // The EQ table sizes its columns from the client width, so track resizes.
        context
            .on_window_size(context.callback(|size: WindowSize| Message::WindowWidth(size.width)));
        context.window_visuals(
            WindowVisuals::new()
                .icon(icon)
                .theme(WindowTheme::Dark)
                .backdrop(WindowBackdrop::Acrylic)
                .client_size(800.0, 660.0)
                .constraints(WindowConstraints {
                    min_width: Some(760.0),
                    min_height: Some(460.0),
                    ..WindowConstraints::default()
                }),
        );

        let rows: Vec<KeyedView> = vec![
            keyed("toolbar", self.toolbar(&view, context)),
            keyed("strips", self.strips(&view, context)),
            keyed("side", self.side(&view)),
            keyed("transcript", self.transcript()),
            keyed(
                "alerts",
                self.alerts(&view, context).grid_row(1).grid_column_span(2),
            ),
        ];

        let body = Grid::new()
            .rows([
                GridLength::Auto, // toolbar
                GridLength::Auto, // alerts
                GridLength::Auto, // strips rack
                GridLength::STAR, // transcript + buses
            ])
            .columns([GridLength::STAR, GridLength::Pixel(300.0)])
            .row_spacing(8.0)
            .column_spacing(8.0)
            .keyed_children(rows);

        let content: View = Border::new()
            .background(Brush::Solid(SCRIM))
            .padding(Thickness::uniform(10.0))
            .content(body)
            .into();

        // The EQ editor is a modal: it covers the whole content area (everything below the title
        // bar) instead of expanding next to the strip it belongs to. Both children share the same
        // cell of a one-cell `Grid`, so the overlay paints on top of the mixer.
        let stage: View = match self
            .eq_open
            .and_then(|id| view.strips.iter().position(|s| s.id == id))
        {
            Some(index) => Grid::new()
                .rows([GridLength::STAR])
                .columns([GridLength::STAR])
                .children((
                    content,
                    Border::new()
                        .background(Brush::Solid(MODAL))
                        .padding(Thickness::uniform(MODAL_PAD))
                        .content(self.eq_card(index, &view.strips[index], context)),
                ))
                .into(),
            None => content,
        };
        let titlebar_content = if self.titlebar_content_show {
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(12.0)
                .children((
                    TextBlock::new()
                        .text("RustMeeter")
                        .font_weight(FontWeight::BOLD)
                        .font_size(20.)
                        .vertical_alignment(VerticalAlignment::Center)
                        .padding(Thickness::new(0., 0., 5.0, 0.)),
                    self.transport(&view, context),
                    self.captions(&view, context),
                ))
        } else {
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(12.0)
                .children((TextBlock::new()
                    .text("RustMeeter")
                    .font_weight(FontWeight::BOLD)
                    .font_size(16.)
                    .vertical_alignment(VerticalAlignment::Center)
                    .padding(Thickness::new(0., 0., 5.0, 0.)),))
        };
        Grid::new()
            .rows([
                GridLength::Auto, // title bar
                GridLength::STAR, // mixer
                GridLength::Auto, // status bar
            ])

            .children((
                TitleBar::new()
                    .preferred_height(WindowTitleBarHeight::Tall)
                    .is_pane_toggle_button_visible(true)
                    .on_pane_toggle_requested(context.message(Message::ToggleTitleBar))
                    .left_header(titlebar_content),
                Border::new().grid_row(1).content(stage),
                // The run state moved down here: one place for "what is happening", instead of a
                // dot in the title bar and another story at the bottom.
                Border::new()
                    .grid_row(2)
                    .padding(Thickness::new(10.0, 6.0, 10.0, 6.0))
                    .content(self.status_bar(&view)),
            ))
            .into()
    }
}

impl Drop for MixerApp {
    /// Closing the window a moment after a change must not lose it: the debounce timer will never
    /// fire again.
    fn drop(&mut self) {
        let _ = self.shared.borrow_mut().flush_settings();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
