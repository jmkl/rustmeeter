//! The `MixerApp` component: state, lifecycle and the top-level layout.
//!
//! Each panel lives in its own submodule as an `impl MixerApp` block, so the state definition and
//! the message loop stay small. Submodules are descendants of `app`, which lets them read the
//! component's private fields without widening them to the whole crate.

mod buses;
mod eq;
mod strips;
mod toolbar;
mod transcript;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rustmeeter_core::*;
use windows_canvas::Invalidator;
use windows_reactor::*;

use self::eq::{EqDrag, MODAL_PAD};
use crate::messages::Message;
use crate::theme::*;

const MAX_LINES: usize = 500;
/// Meter refresh period (~30 fps).
const TICK: Duration = Duration::from_millis(33);

pub(crate) struct MixerApp {
    mixer: Mixer,
    caps: CaptionController,
    inputs: Vec<InputSource>,
    outputs: Vec<String>,
    caption_strip: Option<usize>,        // 1-based; None = first mic
    add_index: Option<usize>,            // selection in the "add input" combo
    transcript: VecDeque<(u64, String)>, // (stable key, line)
    next_line: u64,
    error: Option<String>,
    timer: Option<ComponentTimer>,
    eq_open: Option<StripId>,
    eq_drag: Option<EqDrag>,
    /// Repaint requests for the open EQ graph's Direct2D canvas (see `draw_eq_curve`).
    eq_invalidator: Invalidator,
    /// Window client width in DIPs; sizes the EQ band table (see `eq_table_width`).
    window_w: f64,
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

    pub(crate) fn bus_selection(&self, view: &MixerView, bus: usize) -> Option<usize> {
        match &view.buses[bus].device {
            None => Some(0),
            Some(device) => self.outputs.iter().position(|o| o == device).map(|i| i + 1),
        }
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

        let caps = {
            // Caption thread: same hand-off as above.
            let completion = context.completion();
            CaptionController::new(
                PathBuf::from("cactus/whistle.cact"),
                PathBuf::from("captions.log"),
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
                [false, true],
            );
        }

        let mut app = Self {
            mixer,
            caps,
            inputs: list_inputs(),
            outputs: list_outputs(),
            caption_strip: None,
            add_index: None,
            transcript: VecDeque::new(),
            next_line: 0,
            error: None,
            timer: Some(context.set_timeout(TICK, Message::Tick)),
            eq_open: None,
            eq_drag: None,
            eq_invalidator: Invalidator::new(),
            window_w: 940.0,
        };
        app.start();
        app
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        match message {
            // No state change: the re-render triggered by this message is what moves the meters.
            Message::Tick => {
                self.timer = Some(context.set_timeout(TICK, Message::Tick));
            }
            Message::Event(Event::Error(e)) => self.error = Some(e),
            Message::Event(Event::Started) => {}
            Message::Caption(c) => {
                self.transcript.push_back((
                    self.next_line,
                    format!("[{}] {}: {}", c.time.format("%H:%M:%S"), c.label, c.text),
                ));
                self.next_line += 1;
                while self.transcript.len() > MAX_LINES {
                    self.transcript.pop_front();
                }
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
                    let routes = if src.loopback {
                        [true, false]
                    } else {
                        [false, true]
                    };
                    self.mixer.add_strip(src, routes);
                    self.apply();
                }
                self.add_index = None;
            }
            Message::RemoveStrip(id) => {
                self.mixer.remove_strip(id);
                if self.eq_open == Some(id) {
                    self.eq_open = None;
                    self.eq_drag = None;
                }
                self.eq_invalidator.invalidate();
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
                // First use loads the model, so this can block the UI for a moment.
                if let Err(e) = self.caps.on(&self.mixer, self.caption_strip) {
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
        context.on_window_size(
            context.callback(|size: WindowSize| Message::WindowWidth(size.width)),
        );
        context.window_visuals(
            WindowVisuals::new()
                .theme(WindowTheme::Dark)
                .backdrop(WindowBackdrop::Acrylic)
                .client_size(940.0, 740.0)
                .constraints(WindowConstraints {
                    min_width: Some(760.0),
                    min_height: Some(460.0),
                    ..WindowConstraints::default()
                }),
        );

        let mut rows: Vec<KeyedView> = vec![
            keyed("toolbar", self.toolbar(&view, context)),
            keyed("strips", self.strips(&view, context)),
            keyed("side", self.side(&view)),
            keyed("transcript", self.transcript()),
        ];
        if let Some(error) = &self.error {
            rows.push(keyed(
                "error",
                InfoBar::new()
                    .grid_row(1)
                    .grid_column_span(2)
                    .is_open(true)
                    .is_closable(true)
                    .severity(InfoBarSeverity::Error)
                    .title("Audio error")
                    .message(error)
                    .on_closed(context.callback(|()| Message::DismissError)),
            ));
        }

        let body = Grid::new()
            .rows([
                GridLength::Auto, // toolbar
                GridLength::Auto, // error
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
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                TitleBar::new()
                    .title("RustMeeter")
                    .subtitle("mixer · live captions")
                    .right_header(run_state(view.running)),
                Border::new().grid_row(1).content(stage),
            ))
            .into()
    }
}
