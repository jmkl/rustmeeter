//! The `MixerApp` component: state, lifecycle and the top-level layout.
//!
//! Each panel lives in its own submodule as an `impl MixerApp` block, so the state definition and
//! the message loop stay small. Submodules are descendants of `app`, which lets them read the
//! component's private fields without widening them to the whole crate.

mod buses;
mod eq;
mod status;
mod strips;
mod toolbar;
mod transcript;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustmeeter_core::*;
use windows_canvas::Invalidator;
use windows_reactor::*;

use self::eq::{EqDrag, MODAL_PAD};
use crate::config::{Conf, StripSettings};
use crate::messages::Message;
use crate::theme::*;
use crate::util::{bus_name, zeroed};

/// Meter refresh period (~30 fps).
const TICK: Duration = Duration::from_millis(33);
/// How long a settings edit must sit still before it is written. A drag moves in steps of a few
/// pixels, so this turns a few hundred writes into one, and the user never waits on the file.
const SETTINGS_SAVE_DELAY: Duration = Duration::from_millis(600);

/// Every strip's settings as the mixer has them right now, keyed by source label. Entries left at
/// their defaults are dropped by `set_strip` instead of saved: the file lists devices that were
/// configured, not every device that was ever plugged in.
type LiveStrips = BTreeMap<String, StripSettings>;

/// Where a new strip goes before the user touches anything: microphones to B (the app side),
/// loopback taps to A (the listening side).
fn default_routes(loopback: bool) -> [bool; NUM_BUSES] {
    if loopback {
        [true, false]
    } else {
        [false, true]
    }
}

pub(crate) struct MixerApp {
    mixer: Mixer,
    caps: CaptionController,
    inputs: Vec<InputSource>,
    outputs: Vec<String>,
    caption_strip: Option<usize>, // 1-based; None = first mic
    add_index: Option<usize>,     // selection in the "add input" combo
    error: Option<String>,
    timer: Option<ComponentTimer>,
    eq_open: Option<StripId>,
    eq_drag: Option<EqDrag>,
    /// Repaint requests for the open EQ graph's Direct2D canvas (see `draw_eq_curve`).
    eq_invalidator: Invalidator,
    /// Window client width in DIPs; sizes the EQ band table (see `eq_table_width`).
    window_w: f64,
    config: Conf,
    latest_caption: Caption,
    /// Settings as of the last edit check, and as they are on disk: the difference is what has to
    /// be written. Diffing the live `Params` catches every control (fader, mute, route chips, EQ
    /// drag, table slider, bypass, reset) without a "something changed" message per widget.
    pending: LiveStrips,
    saved: LiveStrips,
    /// When a pending change should be written.
    save_at: Option<Instant>,
}

impl MixerApp {
    fn start(&mut self) {
        if let Err(e) = self.mixer.start() {
            self.error = Some(format!("{e:#}"));
        }
    }

    /// Rebuild the streams after a structural change; a failed apply stays dirty in the `Mixer`.
    fn apply(&mut self) {
        if let Err(e) = self.mixer.apply() {
            self.error = Some(format!("{e:#}"));
        }
    }

    pub(crate) fn strip_params(&self, id: StripId) -> Option<Arc<Params>> {
        self.mixer.strip(id).map(|s| s.params.clone())
    }

    // ----- settings persistence -----

    /// Everything one strip remembers, with defaults as `None`/false so that `set_strip` can drop
    /// a line that says nothing.
    fn strip_settings(strip: &Strip) -> StripSettings {
        let eq = strip.params.eq();
        StripSettings {
            // Bypassed still counts as configured: un-bypassing must not lose the curve.
            eq: (eq.bypass || !eq.bands.is_empty()).then(|| (*eq).clone()),
            // The fader snaps to steps, but a hair below zero must not become a saved "-0.02".
            gain_db: Some(strip.params.gain_db()).filter(|db| zeroed(*db) != 0.0),
            muted: strip.params.muted(),
            routes: Some(strip.params.routes())
                .filter(|routes| *routes != default_routes(strip.source.loopback)),
        }
    }

    /// Every strip's settings, keyed by source label.
    fn live_strips(&self) -> LiveStrips {
        self.mixer
            .strips()
            .iter()
            .map(|s| (s.source.label(), Self::strip_settings(s)))
            .collect()
    }

    /// Put a freshly added strip back the way its device was left last time: gain, mute, routing
    /// and EQ. Called on every strip creation, so settings follow the device rather than the slot.
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

    /// Debounced write of the settings.
    ///
    /// Runs from the meter tick: while the controls are still moving the deadline is pushed back,
    /// so a drag writes once when it ends. Writing is a few hundred bytes to one file, but doing it
    /// per pointer move would be 30 writes a second for no benefit.
    fn sync_settings(&mut self, now: Instant) {
        let live = self.live_strips();
        if live != self.pending {
            self.pending = live;
            self.save_at = Some(now + SETTINGS_SAVE_DELAY);
            return;
        }
        if !self.save_at.is_some_and(|due| now >= due) {
            return;
        }
        self.save_at = None;
        self.flush_settings();
    }

    /// Write the pending settings now (also used when the app is closing).
    ///
    /// Only strips that still exist are written; a device whose strip was removed keeps its line in
    /// the file, which is what makes it come back configured when it is added again.
    fn flush_settings(&mut self) {
        if self.pending == self.saved {
            return;
        }
        for (label, settings) in &self.pending {
            self.config.settings.set_strip(label, settings.clone());
        }
        match self.config.save() {
            Ok(()) => self.saved = self.pending.clone(),
            // Keep the change pending, and say why: the next edit retries.
            Err(e) => self.error = Some(format!("cannot save settings: {e:#}")),
        }
    }

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
                    .title("Audio error")
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
            .spacing(6.0)
            .children((
                if view.running {
                    action_button(
                        "\u{e769}",
                        RED_STRONG,
                        WHITE,
                        context.callback(|()| Message::ToggleRun),
                    )
                } else {
                    action_button(
                        "\u{e768}",
                        GREEN,
                        INK,
                        context.callback(|()| Message::ToggleRun),
                    )
                },
                action_button(
                    "\u{e72c}",
                    DIM,
                    INK,
                    context.callback(|()| Message::Refresh),
                ),
            ))
    }

    fn captions(&self, view: &MixerView, context: &ViewContext<Self>) -> StackPanel {
        let mut captions: Vec<View> = vec![];
        match self.caps.active_strip() {
            Some(_) => captions.push(
                action_button(
                    "\u{ead8}",
                    RED,
                    INK,
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
                        .width(104.0)
                        .items_source(items)
                        .selected_index(Some(self.caption_strip.unwrap_or(0)))
                        .on_selection_changed(
                            context.callback(|index: Option<usize>| Message::CaptionStrip(index)),
                        )
                        .into(),
                );
                captions.push(
                    action_button(
                        "\u{ead8}",
                        BLUE,
                        INK,
                        context.callback(|()| Message::CaptionsOn),
                    )
                    .tooltip("Caption")
                    .into(),
                );
            }
        }
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(6.0)
            .children(captions)
    }
}

impl Component for MixerApp {
    type Input = ();
    type Message = Message;

    fn create(_input: &(), context: &ComponentContext<Self>) -> Self {
        let mut mixer = Mixer::new();
        {
            // Called from audio/error threads: queue a message and let the reactor wake the UI.
            let completion = context.completion();
            mixer.set_event_handler(Arc::new(move |e: &Event| {
                let e = e.clone();
                let completion = completion.clone();
                _ = completion.complete_with(move || Message::Event(e));
            }));
        }

        let config = Conf::load().expect("Failed to load config");

        let whistle_cact = config
            .settings
            .model_root
            .as_ref()
            .map(|root| root.join("whistle.cact"))
            .unwrap_or_else(|| PathBuf::from("whistle.cact"));

        let caps = {
            // Caption thread: same hand-off as above.
            let completion = context.completion();

            CaptionController::new(
                PathBuf::from(whistle_cact),
                PathBuf::from(&config.log),
                None,
                None,
                Arc::new(move |c: &Caption| {
                    let c = c.clone();
                    let completion = completion.clone();
                    _ = completion.complete_with(move || Message::Caption(c));
                }),
            )
        };

        // Defaults: bus A = default output, default mic as strip 1 routed to bus B.
        mixer.set_bus_device(0, default_output_name());
        if let Some(name) = default_input_name() {
            mixer.add_strip(
                InputSource {
                    name,
                    loopback: false,
                },
                default_routes(false),
            );
        }

        let mut app = Self {
            mixer,
            caps,
            inputs: list_inputs(),
            outputs: list_outputs(),
            caption_strip: None,
            add_index: None,
            error: config.warning.clone(),
            timer: Some(context.set_timeout(TICK, Message::Tick)),
            eq_open: None,
            eq_drag: None,
            eq_invalidator: Invalidator::new(),
            window_w: 940.0,
            config,
            latest_caption: Caption::default(),
            pending: LiveStrips::new(),
            saved: LiveStrips::new(),
            save_at: None,
        };
        // The default mic keeps its settings across runs; treat that as the state already on disk.
        for id in app.mixer.strips().iter().map(|s| s.id) {
            app.restore_strip(id);
        }
        app.pending = app.live_strips();
        app.saved = app.pending.clone();
        app.start();
        app
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        match message {
            // No state change: the re-render triggered by this message is what moves the meters.
            // It is also the app's only heartbeat, so the debounced settings write rides along.
            Message::Tick => {
                self.sync_settings(Instant::now());
                self.timer = Some(context.set_timeout(TICK, Message::Tick));
            }
            Message::Event(Event::Error(e)) => self.error = Some(e),
            // The engine drops looped routes and says so once; the alert bar below is derived from
            // the live patch instead, so it also explains a loop before audio is started.
            Message::Event(Event::Warning(_)) => {}
            Message::Event(Event::Started) => {}
            Message::Caption(c) => {
                // Only the line being spoken right now is shown; the full transcript is written to
                // `captions.log` by the controller.
                self.latest_caption = c;
            }
            Message::ToggleRun => {
                if self.mixer.is_running() {
                    self.mixer.stop();
                } else {
                    self.start();
                }
            }
            Message::Refresh => {
                self.inputs = list_inputs();
                self.outputs = list_outputs();
            }
            Message::AddInput(index) => {
                if let Some(src) = index.and_then(|i| self.inputs.get(i)).cloned() {
                    let id = self
                        .mixer
                        .add_strip(src.clone(), default_routes(src.loopback));
                    // A device that was configured in an earlier session comes back configured.
                    self.restore_strip(id);
                    self.pending = self.live_strips();
                    self.apply();
                }
                self.add_index = None;
            }
            Message::RemoveStrip(id) => {
                // Snapshot before dropping it: a fader moved less than a debounce ago would
                // otherwise never reach the file, because the live map no longer has the strip.
                // The entry stays in the file on purpose -- that is what makes the device come back
                // configured when it is added again.
                if let Some(strip) = self.mixer.strip(id) {
                    let settings = Self::strip_settings(strip);
                    if !settings.is_empty() {
                        self.config
                            .settings
                            .set_strip(&strip.source.label(), settings);
                    }
                }
                self.mixer.remove_strip(id);
                if self.eq_open == Some(id) {
                    self.eq_open = None;
                    self.eq_drag = None;
                }
                self.eq_invalidator.invalidate();
                // Losing the label makes `pending` differ from `saved`, which schedules the write.
                self.pending = self.live_strips();
                self.apply();
            }
            Message::SetBus(bus, Some(index)) => {
                let device = index
                    .checked_sub(1)
                    .and_then(|i| self.outputs.get(i))
                    .cloned();
                self.mixer.set_bus_device(bus, device);
                self.apply();
            }
            Message::SetBus(_, None) => {}
            Message::CaptionStrip(Some(0)) => self.caption_strip = None,
            Message::CaptionStrip(Some(n)) => self.caption_strip = Some(n),
            Message::CaptionStrip(None) => {}
            Message::CaptionsOn => {
                if self.config.settings.model_root.is_none() {
                    self.error = Some(format!(
                        "no \"model_root\" in {}: add the folder that holds whistle.cact",
                        self.config.settings_path().display()
                    ));
                } else if let Err(e) = self.caps.on(&self.mixer, self.caption_strip) {
                    // First use loads the model, so this can block the UI for a moment.
                    self.error = Some(format!("{e:#}"));
                }
            }
            Message::CaptionsOff => {
                self.caps.off();
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
        }
    }

    fn view(&self, _input: &(), context: &mut ViewContext<Self>) -> View {
        let view = self.mixer.snapshot();

        context.window_title("RustMeeter");
        // The EQ table sizes its columns from the client width, so track resizes.
        context
            .on_window_size(context.callback(|size: WindowSize| Message::WindowWidth(size.width)));
        context.window_visuals(
            WindowVisuals::new()
                .icon(self.config.icon.clone())
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

        Grid::new()
            .rows([
                GridLength::Auto, // title bar
                GridLength::STAR, // mixer
                GridLength::Auto, // status bar
            ])
            .children((
                TitleBar::new()
                    .margin(Thickness::new(20., 0., 0., 0.))
                    .left_header(
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
                            )),
                    ),
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
        self.flush_settings();
    }
}
