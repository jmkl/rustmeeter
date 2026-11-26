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
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustmeeter_core::*;
use windows_canvas::Invalidator;
use windows_reactor::*;

use self::eq::{EqDrag, MODAL_PAD};
use crate::messages::Message;
use crate::theme::*;

/// Meter refresh period (~30 fps).
const TICK: Duration = Duration::from_millis(33);
/// How often the GUI pumps the IPC server. Slower than the meters: API calls are deliberate.
const IPC_TICK: Duration = Duration::from_millis(100);

/// Persistent state that must outlive the window. The core `Service` owns the mixer, the loaded
/// caption model and the Needle worker: all are expensive to recreate and must keep running while
/// the window is hidden or closed to tray. The GUI only adds the completion slot used to route
/// asynchronous callbacks back to whichever window is currently open.
///
/// Wrapped in `Rc<RefCell<>>` so the `Application` root and the per-window `Component` can share
/// it. Everything here runs on the single UI thread, so no `Arc` needed for the wrapper.
pub(crate) struct SharedMixer {
    pub(crate) service: Service,
    /// Completion handle for the current window component. Event and caption callbacks clone from
    /// this slot; a stale (closed) completion silently drops messages, which is fine while the
    /// window is closed.
    pub(crate) completion_slot: Arc<Mutex<Option<ComponentCompletion<Message>>>>,
}

impl Deref for SharedMixer {
    type Target = Service;

    fn deref(&self) -> &Self::Target {
        &self.service
    }
}

impl DerefMut for SharedMixer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.service
    }
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
    /// Build the core service and the completion slot that routes core callbacks to the UI thread.
    /// Audio is **not** started here: the first `MixerApp` creation calls `mixer.start()` so that
    /// errors can be shown in the window.
    pub(crate) fn new() -> SharedMixerRef {
        let completion_slot: Arc<Mutex<Option<ComponentCompletion<Message>>>> =
            Arc::new(Mutex::new(None));

        let event_slot = completion_slot.clone();
        let caption_slot = completion_slot.clone();
        let toolcall_slot = completion_slot.clone();

        let service = Service::open(ServiceCallbacks {
            event: Some(Arc::new(move |e: &Event| {
                let e = e.clone();
                if let Some(completion) = event_slot.lock().unwrap().clone() {
                    _ = completion.complete_with(move || Message::Event(e));
                }
            })),
            caption: Arc::new(move |c: &Caption| {
                let c = c.clone();
                if let Some(completion) = caption_slot.lock().unwrap().clone() {
                    _ = completion.complete_with(move || Message::Caption(c));
                }
            }),
            toolcall: Arc::new(move |report: ToolCallReport| {
                if let Some(completion) = toolcall_slot.lock().unwrap().clone() {
                    _ = completion.complete_with(move || Message::ToolCall(report));
                }
            }),
            caption_lang: Some("en".to_owned()),
            caption_threads: None,
        })
        .expect("Failed to load config");

        Rc::new(RefCell::new(Self {
            service,
            completion_slot,
        }))
    }
}

pub(crate) struct MixerApp {
    shared: SharedMixerRef,
    inputs: Vec<InputSource>,
    outputs: Vec<String>,
    add_index: Option<usize>, // selection in the "add input" combo
    error: Option<String>,
    timer: Option<ComponentTimer>,
    /// Local API listener, if it could be bound. Polled on the UI thread: the `Mixer` must stay
    /// here, and `poll` is what executes requests and replies to clients.
    ipc: Option<rustmeeter_core::ipc::Server>,
    ipc_timer: Option<ComponentTimer>,
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
        let (icon, col1, col2) = if view.running {
            ("\u{e769}", RED_STRONG, WHITE)
        } else {
            ("\u{e768}", GREEN, INK)
        };
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(0.0)
            .children((
                action_button(
                    icon,
                    col1,
                    col2,
                    crate::component::button::Side::Left,
                    (28., 28.),
                    14.,
                    context.callback(|()| Message::ToggleRun),
                ),
                action_button(
                    "\u{e72c}",
                    DIM,
                    INK,
                    crate::component::button::Side::Center,
                    (28., 28.),
                    14.,
                    context.callback(|()| Message::Refresh),
                ),
                action_button(
                    "\u{e713}",
                    SCRIM,
                    WHITE,
                    crate::component::button::Side::Right,
                    (28., 28.),
                    14.,
                    context.callback(|()| Message::OpenSetting),
                ),
            ))
    }

    fn captions(&self, view: &MixerView, context: &ViewContext<Self>) -> StackPanel {
        let (active_strip, caption_strip, toolcall) = {
            let shared = self.shared.borrow();
            (
                shared.captions.active_strip(),
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
                    (28., 28.),
                    14.,
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
                        (28., 28.),
                        14.,
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

        // The local API is on by default. `RUSTMEETER_IPC_PORT` selects another port, and `0`
        // disables it for a session where an open listener is not wanted.
        let port = std::env::var("RUSTMEETER_IPC_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(rustmeeter_core::ipc::DEFAULT_PORT);
        let (ipc, ipc_error) = if port == 0 {
            (None, None)
        } else {
            match rustmeeter_core::ipc::Server::bind(port) {
                Ok(server) => (Some(server), None),
                Err(e) => (None, Some(format!("IPC disabled: {e:#}"))),
            }
        };

        let ipc_timer = ipc
            .is_some()
            .then(|| context.set_timeout(IPC_TICK, Message::IpcTick));

        Self {
            shared: input.0.clone(),
            inputs: list_inputs(),
            outputs: list_outputs(),
            add_index: None,
            error: error.or(ipc_error),
            timer: Some(context.set_timeout(TICK, Message::Tick)),
            ipc,
            ipc_timer,
            eq_open: None,
            eq_drag: None,
            titlebar_content_show: false,
            eq_invalidator: Invalidator::new(),
            window_w: 940.0,
        }
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        match message {
            // Open setting file with default json file opener
            Message::OpenSetting => {
                let confpath = &self.shared.borrow().config;
                let config_file = confpath.root.join(rustmeeter_core::settings::SETTINGS_FILE);
                if config_file.exists() {
                    if let Some(file) = config_file.to_str() {
                        use std::os::windows::process::CommandExt;
                        if let Err(err) = std::process::Command::new("powershell.exe")
                            .arg("-NoProfile")
                            .arg("-Command")
                            .arg(file)
                            .creation_flags(0x08000000)
                            .output()
                        {
                            eprintln!("Error opening config.json : {err}");
                        }
                    }
                }
            }
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
            // The IPC listener runs on its own threads; this is the only place requests touch the
            // mixer, keeping cpal streams on the UI thread.
            Message::IpcTick => {
                if let Some(server) = &self.ipc {
                    let mut shared = self.shared.borrow_mut();
                    server.poll(|line| rustmeeter_core::ipc::dispatch(&mut shared, line));
                }
                if self.ipc.is_some() {
                    self.ipc_timer = Some(context.set_timeout(IPC_TICK, Message::IpcTick));
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
                    self.error = Some(format!("{e:#}"));
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
                    // `Service::add_strip` restores the device's saved settings and schedules a
                    // settings write, then we rebuild the streams for the new rack.
                    shared.add_strip(src.clone(), default_routes(src.loopback));
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
                    // `Service::remove_strip` snapshots the strip's settings before dropping it
                    // (a fader moved less than a debounce ago would otherwise never reach the
                    // file) and schedules the write. The entry stays on disk on purpose: that is
                    // what makes the device come back configured when it is added again.
                    shared.remove_strip(id);
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
                        let strip = shared.caption_strip;
                        shared
                            .captions_on(strip)
                            .map(|_| ())
                            .map_err(|e| format!("{e:#}"))
                    }
                };
                if let Err(e) = result {
                    self.error = Some(e);
                }
            }
            Message::CaptionsOff => {
                self.shared.borrow_mut().captions.off();
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
        let anim = |duration: u64| {
            (
                (
                    if self.titlebar_content_show {
                        0.
                    } else {
                        -600.
                    },
                    0.,
                ),
                Duration::from_millis(duration),
            )
        };
        let ((t_x, t_y), dur) = anim(100);

        let vuew: Vec<View> = vec![
            swatch(
                TextBlock::new()
                    .text("RustMeeter")
                    .font_weight(FontWeight::BOLD)
                    .font_size(20.)
                    .vertical_alignment(VerticalAlignment::Center)
                    .padding(Thickness::new(0., 0., 5.0, 0.))
                    .into(),
            )
            .into(),
            swatch(self.transport(&view, context).into())
                .translation(t_x, t_y)
                .translation_transition(dur)
                .into(),
            swatch(self.captions(&view, context).into())
                .translation(t_x, t_y)
                .translation_transition(dur * 2)
                .into(),
        ];
        let titlebar_content = if self.titlebar_content_show {
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(12.0)
                .children_transitions([ThemeTransition::Content])
                .children(vuew)
        } else {
            StackPanel::new().children_transitions([ThemeTransition::Content])
        };
        Grid::new()
            .rows([
                GridLength::Auto, // title bar
                GridLength::STAR, // mixer
                GridLength::Auto, // status bar
            ])
            .children_transitions([ThemeTransition::Content])
            .children((
                TitleBar::new()
                    .preferred_height(WindowTitleBarHeight::Standard)
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
    use std::collections::BTreeMap;

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

fn swatch(content: View) -> Border {
    Border::new()
        .horizontal_alignment(HorizontalAlignment::Left)
        .content(content)
}
