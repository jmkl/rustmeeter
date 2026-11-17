//! RustMeeter GUI (windows-reactor / WinUI 3).
//!
//! A thin frontend: no audio code here, only the public library API
//! (`Mixer`, `Params`, `Mixer::snapshot()`, `CaptionController`, `Mixer::set_event_handler()`).
//!
//! The `Mixer` owns cpal streams and is not `Send`, so it lives in the component state on the UI
//! thread. Widgets that map to `Params` atomics write them straight from their event callbacks;
//! structural changes (add/remove strip, change bus device) arrive as messages and are applied in
//! `update`, followed by `Mixer::apply()`.
//!
//! Audio-thread callbacks reach the UI through `ComponentCompletion::complete_with`, which wakes
//! the reactor queue from any thread, and a 33 ms timer pulls meters at ~30 fps.
//!
//!   cargo run --release --features gui --bin rustmeeter-gui

use std::cell::Cell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use rustmeeter_core::*;
use windows_canvas::{ColorF, DrawContext, Invalidator, PathBuilder, Vector2, canvas_invalidated};
use windows_reactor::*;

const MAX_LINES: usize = 500;
/// Meter refresh period (~30 fps).
const TICK: Duration = Duration::from_millis(33);

// Palette. The window is dark acrylic, so cards are translucent white on purpose: they pick up
// the backdrop instead of painting over it.
const CARD: Color = Color::argb(0x14, 0xFF, 0xFF, 0xFF);
const CARD_EDGE: Color = Color::argb(0x22, 0xFF, 0xFF, 0xFF);
const WELL: Color = Color::argb(0x33, 0x00, 0x00, 0x00);
const FILL_OFF: Color = Color::argb(0x2A, 0xFF, 0xFF, 0xFF);
const TRACK: Color = Color::argb(0x2E, 0xFF, 0xFF, 0xFF);
/// Very light scrim so text stays readable over a bright wallpaper.
const SCRIM: Color = Color::argb(0x40, 0x00, 0x00, 0x00);
const DIM: Color = Color::argb(0xB4, 0xFF, 0xFF, 0xFF);
const TEXT: Color = Color::argb(0xE6, 0xFF, 0xFF, 0xFF);
const INK: Color = Color::rgb(0x0A, 0x12, 0x1A);
const WHITE: Color = Color::rgb(0xFF, 0xFF, 0xFF);
const GREEN: Color = Color::rgb(0x34, 0xD3, 0x99);
const BLUE: Color = Color::rgb(0x60, 0xA5, 0xFA);
const AMBER: Color = Color::rgb(0xFB, 0xBF, 0x24);
const RED: Color = Color::rgb(0xF8, 0x71, 0x71);
const RED_STRONG: Color = Color::rgb(0xDC, 0x26, 0x26);

// Parametric EQ editor geometry. The plot spans 20 Hz..20 kHz (3 decades, log) and +-15 dB.
const EQ_W: f64 = 560.0;
const EQ_H: f64 = 150.0;
/// Response curve resolution: `EQ_COLS + 1` samples across `EQ_W` (lower = cheaper to redraw).
const EQ_COLS: usize = 112;
/// Sample rate used only to draw the curve.
const EQ_FS: f32 = 48_000.0;
const EQ_TABLE_H: f64 = 120.0;
const DOT: f64 = 16.0;
/// A press this close (px) to an existing dot never adds a new band.
const DOT_HIT: f64 = 14.0;
const CURVE: Color = Color::rgb(0x60, 0xA5, 0xFA);
const CURVE_FILL: Color = Color::argb(0x40, 0x60, 0xA5, 0xFA);
const GRID_STRONG: Color = Color::argb(0x40, 0xFF, 0xFF, 0xFF);
const GRID_WEAK: Color = Color::argb(0x18, 0xFF, 0xFF, 0xFF);
const BAND_COLORS: [Color; 10] = [
    Color::rgb(0xF8, 0x71, 0x71),
    Color::rgb(0xFB, 0x92, 0x3C),
    Color::rgb(0xFB, 0xBF, 0x24),
    Color::rgb(0xA3, 0xE6, 0x35),
    Color::rgb(0x34, 0xD3, 0x99),
    Color::rgb(0x22, 0xD3, 0xEE),
    Color::rgb(0x60, 0xA5, 0xFA),
    Color::rgb(0x81, 0x8C, 0xF8),
    Color::rgb(0xC0, 0x84, 0xFC),
    Color::rgb(0xF4, 0x72, 0xB6),
];

/// Messages sent to the UI thread: user actions, async events and the meter tick.
#[derive(Clone)]
enum Message {
    Tick,
    Event(Event),
    Caption(Caption),
    ToggleRun,
    Refresh,
    AddInput(Option<usize>),
    RemoveStrip(StripId),
    SetBus(usize, Option<usize>),
    CaptionStrip(Option<usize>),
    CaptionsOn,
    CaptionsOff,
    DismissError,
    // Parametric EQ editor.
    ToggleEq(StripId),
    EqPress {
        strip: StripId,
        band: usize,
        wx: f64,
        wy: f64,
        right: bool,
    },
    EqMove {
        wx: f64,
        wy: f64,
        left: bool,
    },
    EqRelease,
    EqAddAt {
        strip: StripId,
        x: f64,
        y: f64,
        left: bool,
    },
    EqAdd(StripId),
}

/// A dot being dragged: where the pointer was (window DIPs) and the band's values at that moment.
#[derive(Clone, Copy)]
struct EqDrag {
    strip: StripId,
    band: usize,
    wx: f64,
    wy: f64,
    freq: f32,
    gain: f32,
}

struct MixerApp {
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

    fn strip_params(&self, id: StripId) -> Option<Arc<Params>> {
        self.mixer.strip(id).map(|s| s.params.clone())
    }

    /// Dot pressed: right button removes the band, left button starts a drag.
    fn eq_press(&mut self, strip: StripId, band: usize, wx: f64, wy: f64, right: bool) {
        let Some(params) = self.strip_params(strip) else {
            return;
        };
        let Some(b) = params.eq().bands.get(band).copied() else {
            return;
        };
        if right {
            params.update_eq(|s| {
                if band < s.bands.len() {
                    s.bands.remove(band);
                }
            });
            self.eq_drag = None;
        } else {
            self.eq_drag = Some(EqDrag {
                strip,
                band,
                wx,
                wy,
                freq: b.freq_hz,
                gain: b.gain_db,
            });
        }
    }

    /// Pointer moved while a dot is captured. Uses window coordinates (the dot itself moves
    /// under the pointer, so element-local coordinates would drift).
    fn eq_move(&mut self, wx: f64, wy: f64, left: bool) {
        let Some(d) = self.eq_drag else {
            return;
        };
        if !left {
            self.eq_drag = None;
            return;
        }
        let Some(params) = self.strip_params(d.strip) else {
            return;
        };
        let freq = (f64::from(d.freq) * 10f64.powf((wx - d.wx) / EQ_W * 3.0)) as f32;
        let gain = d.gain - ((wy - d.wy) / (EQ_H / 2.0) * f64::from(EQ_GAIN_MAX)) as f32;
        params.update_eq(|s| {
            if let Some(b) = s.bands.get_mut(d.band) {
                b.freq_hz = freq;
                if b.kind.uses_gain() {
                    b.gain_db = gain;
                }
            }
        });
    }

    /// Left press on empty graph space adds a Bell band there. A press on a dot also bubbles up
    /// to the graph, so anything near an existing dot is ignored.
    fn eq_add_at(&mut self, strip: StripId, x: f64, y: f64, left: bool) {
        if !left || x < 0.0 || y < 0.0 || x > EQ_W || y > EQ_H {
            return;
        }
        let Some(params) = self.strip_params(strip) else {
            return;
        };
        let near_dot = params.eq().bands.iter().any(|b| {
            let g = if b.kind.uses_gain() { b.gain_db } else { 0.0 };
            let (dx, dy) = (freq_to_x(b.freq_hz) - x, gain_to_y(g) - y);
            dx * dx + dy * dy < DOT_HIT * DOT_HIT
        });
        if near_dot {
            return;
        }
        let band = EqBand::new(EqKind::Bell, x_to_freq(x), y_to_gain(y));
        params.update_eq(|s| {
            s.add(band);
        });
    }

    /// "+ Add band" button: a flat Bell at the first standard frequency that is still free.
    fn eq_add_default(&mut self, strip: StripId) {
        let Some(params) = self.strip_params(strip) else {
            return;
        };
        let used: Vec<f32> = params.eq().bands.iter().map(|b| b.freq_hz).collect();
        let freq = [
            1000.0_f32, 250.0, 4000.0, 100.0, 8000.0, 500.0, 2000.0, 60.0, 12000.0, 16000.0,
        ]
        .into_iter()
        .find(|f| used.iter().all(|u| (u / f).log10().abs() > 0.1))
        .unwrap_or(1000.0);
        params.update_eq(|s| {
            s.add(EqBand::new(EqKind::Bell, freq, 0.0));
        });
    }

    fn bus_selection(&self, view: &MixerView, bus: usize) -> Option<usize> {
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

        Grid::new()
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                TitleBar::new()
                    .title("RustMeeter")
                    .subtitle("mixer · live captions")
                    .right_header(run_state(view.running)),
                Border::new()
                    .grid_row(1)
                    .background(Brush::Solid(SCRIM))
                    .padding(Thickness::uniform(10.0))
                    .content(body),
            ))
            .into()
    }
}

impl MixerApp {
    fn toolbar(&self, view: &MixerView, context: &ViewContext<Self>) -> View {
        let transport = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(6.0)
            .children((
                if view.running {
                    action_button(
                        "Stop",
                        RED_STRONG,
                        WHITE,
                        context.callback(|()| Message::ToggleRun),
                    )
                } else {
                    action_button(
                        "Start",
                        GREEN,
                        INK,
                        context.callback(|()| Message::ToggleRun),
                    )
                },
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .height(30.0)
                    .content(label_text("Refresh", 11.0, DIM))
                    .on_click(context.callback(|()| Message::Refresh)),
            ));

        let mut captions: Vec<View> = vec![section_label("CAPTIONS").into()];
        match self.caps.active_strip() {
            Some(n) => captions.push(
                action_button(
                    &format!("Stop · strip {n}"),
                    RED,
                    INK,
                    context.callback(|()| Message::CaptionsOff),
                )
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
                        "Start captions",
                        BLUE,
                        INK,
                        context.callback(|()| Message::CaptionsOn),
                    )
                    .into(),
                );
            }
        }

        let top = Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .children((
                transport,
                StackPanel::new()
                    .grid_column(1)
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .children(captions),
            ));

        let devices = Grid::new()
            .columns([
                GridLength::Auto,
                GridLength::Pixel(196.0),
                GridLength::Auto,
                GridLength::Pixel(196.0),
                GridLength::Auto,
                GridLength::Pixel(176.0),
                GridLength::STAR,
            ])
            .column_spacing(6.0)
            .children((
                badge("A", GREEN),
                self.bus_combo(0, view, context).grid_column(1),
                badge("B", BLUE).grid_column(2),
                self.bus_combo(1, view, context).grid_column(3),
                section_label("ADD").grid_column(4),
                self.add_combo(context).grid_column(5),
                TextBlock::new()
                    .grid_column(6)
                    .text("A: what you hear · B: recorded by apps")
                    .font_size(9.0)
                    .foreground(Brush::Solid(DIM))
                    .text_alignment(TextAlignment::Right)
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .vertical_alignment(VerticalAlignment::Center),
            ));

        card(StackPanel::new().spacing(6.0).children((top, devices)))
            .grid_row(0)
            .grid_column_span(2)
            .into()
    }

    fn bus_combo(&self, bus: usize, view: &MixerView, context: &ViewContext<Self>) -> ComboBox {
        let items: Vec<String> = std::iter::once("None".to_string())
            .chain(self.outputs.iter().cloned())
            .collect();
        ComboBox::new()
            .width(196.0)
            .items_source(items)
            .selected_index(self.bus_selection(view, bus))
            .on_selection_changed(
                context.callback(move |index: Option<usize>| Message::SetBus(bus, index)),
            )
    }

    fn add_combo(&self, context: &ViewContext<Self>) -> ComboBox {
        ComboBox::new()
            .width(176.0)
            .placeholder_text("add input…")
            .items_source(self.inputs.iter().map(|s| s.label()))
            .selected_index(self.add_index)
            .on_selection_changed(context.callback(|index: Option<usize>| Message::AddInput(index)))
    }

    fn strips(&self, view: &MixerView, context: &ViewContext<Self>) -> View {
        let mut cards: Vec<KeyedView> = Vec::new();
        for (index, strip) in view.strips.iter().enumerate() {
            cards.push(keyed(
                u64::from(strip.id),
                self.strip_card(index, strip, context),
            ));
            // The EQ editor expands right next to the strip it belongs to.
            if self.eq_open == Some(strip.id) {
                cards.push(keyed(
                    (1u64 << 32) | u64::from(strip.id),
                    self.eq_card(index, strip, context),
                ));
            }
        }
        let content: View = if cards.is_empty() {
            card(
                TextBlock::new()
                    .text("No strips yet — pick a device under ADD.")
                    .font_size(10.5)
                    .foreground(Brush::Solid(DIM)),
            )
            .into()
        } else {
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .keyed_children(cards)
                .into()
        };
        let header = Grid::new()
            .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto])
            .column_spacing(8.0)
            .children((
                section_label(&format!("STRIPS  ·  {}", view.strips.len())),
                TextBlock::new()
                    .grid_column(2)
                    .text("mic → B · loopback → A · captions read the raw strip")
                    .font_size(9.0)
                    .foreground(Brush::Solid(DIM))
                    .vertical_alignment(VerticalAlignment::Center),
            ));

        Grid::new()
            .grid_row(2)
            .grid_column_span(2)
            .rows([GridLength::Auto, GridLength::Auto])
            .row_spacing(6.0)
            .children((
                header,
                rack(
                    ScrollViewer::new()
                        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto)
                        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                        .content(content),
                )
                .grid_row(1),
            ))
            .into()
    }

    fn strip_card(&self, index: usize, s: &StripView, context: &ViewContext<Self>) -> View {
        let id = s.id;
        let params = &s.params;

        let header = Grid::new()
            .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto])
            .column_spacing(6.0)
            .children((
                badge(&(index + 1).to_string(), GREEN),
                TextBlock::new()
                    .grid_column(1)
                    .text(&s.label)
                    .font_size(10.5)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .vertical_alignment(VerticalAlignment::Center),
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .grid_column(2)
                    .width(18.0)
                    .height(18.0)
                    .background(Brush::Solid(FILL_OFF))
                    .content(
                        label_text("×", 11.0, RED).margin(Thickness::new(-6.0, -6.0, -6.0, -6.0)),
                    )
                    .on_click(context.callback(move |()| Message::RemoveStrip(id))),
            ));

        let routes = Grid::new()
            .columns([
                GridLength::Auto,
                GridLength::Auto,
                GridLength::STAR,
                GridLength::Auto,
            ])
            .column_spacing(4.0)
            .children((
                route_chip(params, 0),
                route_chip(params, 1).grid_column(1),
                mute_chip(params).grid_column(3),
            ));

        let mut body: Vec<View> = vec![header.into(), routes.into()];

        body.push(
            eq_button(
                &s.eq,
                self.eq_open == Some(id),
                context.callback(move |()| Message::ToggleEq(id)),
            )
            .into(),
        );

        let gain = params.clone();
        body.push(
            Grid::new()
                .columns([
                    GridLength::STAR,
                    GridLength::Auto,
                    GridLength::Auto,
                    GridLength::STAR,
                ])
                .column_spacing(8.0)
                .children((
                    Slider::new()
                        .grid_column(1)
                        .orientation(Orientation::Vertical)
                        .height(76.0)
                        .minimum(f64::from(MIN_DB))
                        .maximum(f64::from(MAX_DB))
                        .step_frequency(0.5)
                        .value(f64::from(params.gain_db()))
                        .on_value_changed(move |v: f64| gain.set_gain_db(v as f32)),
                    vmeter(params.level_db(), 76.0).grid_column(2),
                ))
                .into(),
        );

        body.push(
            TextBlock::new()
                .text(format!("{:+.1} dB", zeroed(params.gain_db())))
                .font_size(10.0)
                .foreground(Brush::Solid(DIM))
                .horizontal_alignment(HorizontalAlignment::Center)
                .into(),
        );

        let muted = params.muted();
        Border::new()
            .width(168.0)
            .padding(Thickness::uniform(8.0))
            .corner_radius(10.0)
            .vertical_alignment(VerticalAlignment::Top)
            .background(Brush::Solid(CARD))
            .border_brush(Brush::Solid(if muted {
                Color::argb(0x7F, RED.r, RED.g, RED.b)
            } else {
                CARD_EDGE
            }))
            .border_thickness(Thickness::uniform(1.0))
            .content(StackPanel::new().spacing(5.0).children(body))
            .into()
    }

    /// Parametric EQ editor for one strip: response graph with draggable dots, plus a band table.
    fn eq_card(&self, index: usize, s: &StripView, context: &ViewContext<Self>) -> View {
        let id = s.id;
        let params = &s.params;
        let eq = &s.eq;
        let bypass = eq.bypass;

        // ---- header: title, bypass, close
        let header = {
            let live = params.clone();
            Grid::new()
                .columns([
                    GridLength::Auto,
                    GridLength::STAR,
                    GridLength::Auto,
                    GridLength::Auto,
                ])
                .column_spacing(6.0)
                .children((
                    badge(&(index + 1).to_string(), GREEN),
                    TextBlock::new()
                        .grid_column(1)
                        .text(format!("EQ · {}", s.label))
                        .font_size(10.5)
                        .font_weight(FontWeight::SEMI_BOLD)
                        .text_trimming(TextTrimming::CharacterEllipsis)
                        .vertical_alignment(VerticalAlignment::Center),
                    chip(
                        "Bypass",
                        if bypass { AMBER } else { FILL_OFF },
                        if bypass { INK } else { TEXT },
                    )
                    .grid_column(2)
                    .width(54.0)
                    .on_click({
                        let invalidator = self.eq_invalidator.clone();
                        move || {
                            live.update_eq(|e| e.bypass = !bypass);
                            invalidator.invalidate();
                        }
                    }),
                    x_button(context.callback(move |()| Message::ToggleEq(id))).grid_column(3),
                ))
        };

        // ---- plot: grid, response curve, dots (all positioned on a Canvas).
        // A Canvas reconciles its children positionally (unlike Grid/StackPanel, which are keyed),
        // so the plot is built in a stable order: grid, labels, then the curve canvas, then the
        // bands' dots last. Adding or removing a band only appends or drops at the tail, so a
        // pointer-captured dot is never recycled as a curve column.
        let mut plot: Vec<View> = Vec::new();
        for (db, strong) in [
            (-12.0_f32, false),
            (-6.0, false),
            (0.0, true),
            (6.0, false),
            (12.0, false),
        ] {
            plot.push(plot_line(true, gain_to_y(db), strong).into());
        }
        for (hz, strong) in [
            (50.0_f32, false),
            (100.0, true),
            (200.0, false),
            (500.0, false),
            (1000.0, true),
            (2000.0, false),
            (5000.0, false),
            (10000.0, true),
        ] {
            plot.push(plot_line(false, freq_to_x(hz), strong).into());
        }
        for (hz, text) in [(100.0_f32, "100"), (1000.0, "1k"), (10000.0, "10k")] {
            plot.push(plot_label(text, freq_to_x(hz) + 3.0, EQ_H - 13.0).into());
        }
        for (db, text) in [(12.0_f32, "+12"), (0.0, "0"), (-12.0, "-12")] {
            plot.push(plot_label(text, 3.0, gain_to_y(db) + 1.0).into());
        }

        // The response is drawn with Direct2D rather than a column of `Border`s: a polyline is
        // anti-aliased, where axis-aligned rectangles can only approximate a slope as a staircase.
        // It sits between the labels and the dots, so the dots stay on top and keep their handlers.
        {
            let curve = eq.clone();
            let invalidator = self.eq_invalidator.clone();
            plot.push(
                Border::new()
                    .width(EQ_W)
                    .height(EQ_H)
                    .canvas_left(0.0)
                    .canvas_top(0.0)
                    .content(canvas_invalidated(&invalidator, move |ctx| {
                        draw_eq_curve(ctx, &curve)
                    }))
                    .into(),
            );
        }

        for (i, b) in eq.bands.iter().enumerate() {
            let color = BAND_COLORS[i % BAND_COLORS.len()];
            let live = b.enabled && !bypass;
            let fill = if live {
                color
            } else {
                Color::argb(0x66, color.r, color.g, color.b)
            };
            let dragging = self.eq_drag.is_some_and(|d| d.strip == id && d.band == i);
            let x = freq_to_x(b.freq_hz);
            let y = gain_to_y(if b.kind.uses_gain() { b.gain_db } else { 0.0 });
            plot.push(
                Border::new()
                    .width(DOT)
                    .height(DOT)
                    .corner_radius(DOT / 2.0)
                    .background(Brush::Solid(fill))
                    .border_brush(Brush::Solid(if dragging {
                        WHITE
                    } else {
                        Color::argb(0x99, 0x0A, 0x12, 0x1A)
                    }))
                    .border_thickness(Thickness::uniform(if dragging { 2.0 } else { 1.0 }))
                    .canvas_left(x - DOT / 2.0)
                    .canvas_top(y - DOT / 2.0)
                    .capture_pointer_on_press(true)
                    .on_pointer_pressed(context.callback(move |p: PointerEventInfo| {
                        Message::EqPress {
                            strip: id,
                            band: i,
                            wx: p.window_x,
                            wy: p.window_y,
                            right: p.is_right_button_pressed,
                        }
                    }))
                    .on_pointer_moved(context.callback(|p: PointerEventInfo| Message::EqMove {
                        wx: p.window_x,
                        wy: p.window_y,
                        left: p.is_left_button_pressed,
                    }))
                    .on_pointer_released(context.callback(|_: PointerEventInfo| Message::EqRelease))
                    .on_pointer_capture_lost(context.callback(|()| Message::EqRelease))
                    .content(
                        TextBlock::new()
                            .text((i + 1).to_string())
                            .font_size(8.5)
                            .font_weight(FontWeight::BOLD)
                            .foreground(Brush::Solid(INK))
                            .horizontal_alignment(HorizontalAlignment::Center)
                            .vertical_alignment(VerticalAlignment::Center),
                    )
                    .into(),
            );
        }

        let graph = Border::new()
            .width(EQ_W)
            .height(EQ_H)
            .corner_radius(6.0)
            .background(Brush::Solid(WELL))
            .on_pointer_pressed(
                context.callback(move |p: PointerEventInfo| Message::EqAddAt {
                    strip: id,
                    x: p.x,
                    y: p.y,
                    left: p.is_left_button_pressed,
                }),
            )
            .content(Canvas::new().width(EQ_W).height(EQ_H).children(plot));

        // ---- table: one row per band
        let table_header = Grid::new()
            .columns(eq_columns())
            .column_spacing(4.0)
            .children((
                eq_head("#", 0),
                eq_head("Curve", 1),
                eq_head("Gain", 2),
                eq_head("Q", 3),
                eq_head("Frequency", 4),
            ));
        let rows: Vec<KeyedView> = eq
            .bands
            .iter()
            .enumerate()
            .map(|(i, b)| keyed(i as u64, eq_row(params, i, b)))
            .collect();
        let table_body: View = if rows.is_empty() {
            TextBlock::new()
                .text("No bands yet: click the graph or press + Add band.")
                .font_size(10.0)
                .foreground(Brush::Solid(DIM))
                .into()
        } else {
            StackPanel::new().spacing(3.0).keyed_children(rows).into()
        };
        let table = ScrollViewer::new()
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .max_height(EQ_TABLE_H)
            .content(table_body);

        // ---- footer
        let reset = params.clone();
        let footer = Grid::new()
            .columns([GridLength::Auto, GridLength::Auto, GridLength::STAR])
            .column_spacing(6.0)
            .children((
                action_button(
                    "+ Add band",
                    BLUE,
                    INK,
                    context.callback(move |()| Message::EqAdd(id)),
                ),
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .grid_column(1)
                    .height(30.0)
                    .content(label_text("Reset", 11.0, DIM))
                    .on_click(move || reset.set_eq(EqSettings::default())),
                TextBlock::new()
                    .grid_column(2)
                    .text("click graph: add · drag dot: freq / gain · right-click dot: remove")
                    .font_size(9.0)
                    .foreground(Brush::Solid(DIM))
                    .text_alignment(TextAlignment::Right)
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .vertical_alignment(VerticalAlignment::Center),
            ));

        Border::new()
            .width(EQ_W + 20.0)
            .padding(Thickness::uniform(8.0))
            .corner_radius(10.0)
            .vertical_alignment(VerticalAlignment::Top)
            .background(Brush::Solid(CARD))
            .border_brush(Brush::Solid(CARD_EDGE))
            .border_thickness(Thickness::uniform(1.0))
            .content(StackPanel::new().spacing(6.0).children((
                header,
                graph,
                table_header,
                table,
                footer,
            )))
            .into()
    }

    fn bus_row(&self, bus: usize, view: &MixerView) -> Grid {
        let p = &view.buses[bus].params;
        let gain = p.clone();
        let muted = p.muted();
        let has_device = view.buses[bus].device.is_some();
        let device = view.buses[bus]
            .device
            .clone()
            .unwrap_or_else(|| "no device".to_string());
        let level = ((p.level_db() + 60.0) / 60.0).clamp(0.0, 1.0) as f64;

        Grid::new()
            .rows([GridLength::Auto, GridLength::Auto, GridLength::Auto])
            .columns([
                GridLength::Auto,
                GridLength::STAR,
                GridLength::Auto,
                GridLength::Auto,
            ])
            .column_spacing(6.0)
            .row_spacing(3.0)
            .children((
                badge(bus_name(bus), if bus == 0 { GREEN } else { BLUE }),
                TextBlock::new()
                    .grid_column(1)
                    .text(&device)
                    .font_size(10.5)
                    .foreground(Brush::Solid(if has_device {
                        Color::argb(0xE6, 0xFF, 0xFF, 0xFF)
                    } else {
                        DIM
                    }))
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .vertical_alignment(VerticalAlignment::Center),
                mute_chip(p).grid_column(2).height(20.0),
                TextBlock::new()
                    .grid_column(3)
                    .text(trim_db(p.gain_db()))
                    .font_size(10.0)
                    .foreground(Brush::Solid(if muted { RED } else { DIM }))
                    .text_alignment(TextAlignment::Right)
                    .width(32.0)
                    .vertical_alignment(VerticalAlignment::Center),
                Slider::new()
                    .grid_row(1)
                    .grid_column_span(4)
                    .height(24.0)
                    .min_height(24.0)
                    .minimum(f64::from(MIN_DB))
                    .maximum(f64::from(MAX_DB))
                    .step_frequency(0.5)
                    .value(f64::from(p.gain_db()))
                    .on_value_changed(move |v: f64| gain.set_gain_db(v as f32)),
                ProgressBar::new()
                    .grid_row(2)
                    .grid_column_span(4)
                    .maximum(1.0)
                    .value(level)
                    .height(3.0),
            ))
            .into()
    }

    fn side(&self, view: &MixerView) -> View {
        Grid::new()
            .grid_row(3)
            .grid_column(1)
            .rows([GridLength::Auto, GridLength::STAR])
            .row_spacing(6.0)
            .children((
                section_label("BUSES"),
                card(
                    Grid::new()
                        .rows([GridLength::Auto, GridLength::Auto])
                        .row_spacing(8.0)
                        .children((self.bus_row(0, view), self.bus_row(1, view).grid_row(1))),
                )
                .grid_row(1),
            ))
            .into()
    }

    fn transcript(&self) -> View {
        let lines: Vec<KeyedView> = self
            .transcript
            .iter()
            .map(|(key, line)| {
                keyed(
                    *key,
                    TextBlock::new()
                        .text(line)
                        .font_size(11.5)
                        .text_wrapping(TextWrapping::NoWrap)
                        .is_text_selection_enabled(true),
                )
            })
            .collect();

        let content: View = if lines.is_empty() {
            TextBlock::new()
                .text("Captions appear here once you start them.")
                .font_size(10.0)
                .foreground(Brush::Solid(DIM))
                .into()
        } else {
            StackPanel::new().spacing(1.0).keyed_children(lines).into()
        };

        Grid::new()
            .grid_row(3)
            .rows([GridLength::Auto, GridLength::STAR])
            .row_spacing(6.0)
            .children((
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .children((
                        section_label("TRANSCRIPT"),
                        TextBlock::new()
                            .grid_column(1)
                            .text(self.caps.log_path().display().to_string())
                            .font_size(9.5)
                            .foreground(Brush::Solid(DIM)),
                    )),
                card(
                    ScrollViewer::new()
                        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                        .content(content),
                )
                .grid_row(1),
            ))
            .into()
    }
}

// ---- parametric EQ helpers ----

fn freq_to_x(freq: f32) -> f64 {
    (f64::from(freq.clamp(EQ_FREQ_MIN, EQ_FREQ_MAX)) / f64::from(EQ_FREQ_MIN)).log10() / 3.0 * EQ_W
}

fn x_to_freq(x: f64) -> f32 {
    (f64::from(EQ_FREQ_MIN) * 10f64.powf(x.clamp(0.0, EQ_W) / EQ_W * 3.0)) as f32
}

fn gain_to_y(gain: f32) -> f64 {
    EQ_H / 2.0
        - f64::from(gain.clamp(EQ_GAIN_MIN, EQ_GAIN_MAX)) / f64::from(EQ_GAIN_MAX) * EQ_H / 2.0
}

fn y_to_gain(y: f64) -> f32 {
    ((EQ_H / 2.0 - y) / (EQ_H / 2.0) * f64::from(EQ_GAIN_MAX)) as f32
}

fn fmt_hz(freq: f32) -> String {
    if freq >= 1000.0 {
        format!("{:.2} kHz", freq / 1000.0)
    } else {
        format!("{freq:.0} Hz")
    }
}

/// The strip card's EQ entry: one button that opens/closes the editor.
fn eq_button(eq: &EqSettings, open: bool, click: impl IntoUnitCallback) -> Button {
    let active = eq.active_bands();
    let label = if eq.bypass {
        "EQ · bypassed".to_string()
    } else if active == 0 {
        "EQ".to_string()
    } else {
        format!("EQ · {active} band{}", if active == 1 { "" } else { "s" })
    };
    let lit = !eq.bypass && active > 0;
    Button::new()
        .style(ButtonStyle::Subtle)
        .background(Brush::Solid(if open { BLUE } else { FILL_OFF }))
        .height(24.0)
        .width(152.0)
        .content(
            TextBlock::new()
                .text(format!("{} {label}", if open { "▾" } else { "▸" }))
                .font_size(10.5)
                .font_weight(FontWeight::SEMI_BOLD)
                .foreground(Brush::Solid(if open {
                    INK
                } else if lit {
                    BLUE
                } else {
                    TEXT
                })),
        )
        .on_click(click)
}

fn x_button(click: impl IntoUnitCallback) -> Button {
    Button::new()
        .style(ButtonStyle::Subtle)
        .width(18.0)
        .height(18.0)
        .background(Brush::Solid(FILL_OFF))
        .content(label_text("×", 11.0, RED).margin(Thickness::new(-6.0, -6.0, -6.0, -6.0)))
        .on_click(click)
}

/// Converts a Reactor palette color to a Direct2D color.
fn color_f(color: Color) -> ColorF {
    ColorF::from_rgba8(color.r, color.g, color.b, color.a)
}

/// Draws the EQ response with Direct2D: the area between the response and the 0 dB line, plus the
/// response polyline itself. Direct2D anti-aliases the polyline, so the curve is smooth instead of
/// the 5 px staircase a column of `Border`s produces. Grid, labels and dots stay XAML on top.
fn draw_eq_curve(ctx: &DrawContext<'_>, eq: &EqSettings) -> windows_canvas::Result<()> {
    // Transparent: the surrounding Border paints the well, so the acrylic backdrop still shows.
    ctx.clear(ColorF::TRANSPARENT);
    if eq.bypass || eq.bands.is_empty() {
        return Ok(());
    }

    let baseline = (EQ_H / 2.0) as f32;
    let step = (EQ_W / EQ_COLS as f64) as f32;
    let response: Vec<Vector2> = (0..=EQ_COLS)
        .map(|i| {
            let x = i as f32 * step;
            let db = eq
                .response_db(x_to_freq(f64::from(x)), EQ_FS)
                .clamp(EQ_GAIN_MIN, EQ_GAIN_MAX);
            Vector2::new(x, gain_to_y(db) as f32)
        })
        .collect();

    // Filled area between the response and the 0 dB line (closed along the baseline).
    let area = std::iter::once(Vector2::new(0.0, baseline))
        .chain(response.iter().copied())
        .chain(std::iter::once(Vector2::new(EQ_W as f32, baseline)));
    let area = PathBuilder::new(ctx.device())?.polygon(area)?;
    ctx.fill_path(&area, &ctx.create_solid_brush(color_f(CURVE_FILL))?);

    // The response line itself.
    let mut points = response.into_iter();
    let first = points.next().expect("EQ_COLS + 1 samples");
    let mut figure = PathBuilder::new(ctx.device())?.begin_hollow(first);
    for point in points {
        figure = figure.line_to(point);
    }
    let line = figure.end_open().build()?;
    ctx.draw_path(&line, &ctx.create_solid_brush(color_f(CURVE))?, 2.0);
    Ok(())
}

/// Grid line on the plot: horizontal (at `pos` = y) or vertical (at `pos` = x).
fn plot_line(horizontal: bool, pos: f64, strong: bool) -> Border {
    let color = if strong { GRID_STRONG } else { GRID_WEAK };
    if horizontal {
        Border::new()
            .width(EQ_W)
            .height(1.0)
            .background(Brush::Solid(color))
            .canvas_left(0.0)
            .canvas_top(pos - 0.5)
    } else {
        Border::new()
            .width(1.0)
            .height(EQ_H)
            .background(Brush::Solid(color))
            .canvas_left(pos - 0.5)
            .canvas_top(0.0)
    }
}

fn plot_label(text: &str, x: f64, y: f64) -> TextBlock {
    TextBlock::new()
        .text(text)
        .font_size(8.5)
        .foreground(Brush::Solid(DIM))
        .canvas_left(x)
        .canvas_top(y)
}

/// Table layout: on, curve, gain, Q, frequency, remove (about 560 px, same as the plot).
fn eq_columns() -> [GridLength; 6] {
    [
        GridLength::Pixel(26.0),
        GridLength::Pixel(100.0),
        GridLength::Pixel(122.0),
        GridLength::Pixel(100.0),
        GridLength::Pixel(150.0),
        GridLength::Pixel(22.0),
    ]
}

fn eq_head(text: &str, column: i32) -> TextBlock {
    TextBlock::new()
        .grid_column(column)
        .text(text)
        .font_size(9.0)
        .foreground(Brush::Solid(DIM))
}

/// Slider + value readout in one table cell.
fn eq_cell(column: i32, slider: Slider, readout: String) -> View {
    Grid::new()
        .grid_column(column)
        .columns([GridLength::STAR, GridLength::Pixel(44.0)])
        .column_spacing(4.0)
        .children((
            slider,
            TextBlock::new()
                .grid_column(1)
                .text(readout)
                .font_size(9.5)
                .foreground(Brush::Solid(DIM))
                .vertical_alignment(VerticalAlignment::Center),
        ))
        .into()
}

/// One band in the table. Every widget writes straight to the strip's `Params` (live, no restart).
fn eq_row(params: &Arc<Params>, i: usize, b: &EqBand) -> Grid {
    let color = BAND_COLORS[i % BAND_COLORS.len()];
    let on = b.enabled;
    let uses_gain = b.kind.uses_gain();
    let (p_on, p_kind, p_gain, p_q, p_freq, p_del) = (
        params.clone(),
        params.clone(),
        params.clone(),
        params.clone(),
        params.clone(),
        params.clone(),
    );
    let kinds: Vec<String> = EqKind::ALL.iter().map(|k| k.label().to_string()).collect();
    let (lo_hz, hi_hz) = (
        f64::from(EQ_FREQ_MIN).log10(),
        f64::from(EQ_FREQ_MAX).log10(),
    );

    Grid::new()
        .columns(eq_columns())
        .column_spacing(4.0)
        .children((
            chip(
                &(i + 1).to_string(),
                if on { color } else { FILL_OFF },
                if on { INK } else { TEXT },
            )
            .width(26.0)
            .on_click(move || {
                p_on.update_eq(|s| {
                    if let Some(band) = s.bands.get_mut(i) {
                        band.enabled = !on;
                    }
                })
            }),
            ComboBox::new()
                .grid_column(1)
                .width(100.0)
                .items_source(kinds)
                .selected_index(Some(b.kind.index()))
                .on_selection_changed(move |index: Option<usize>| {
                    if let Some(kind) = index.and_then(|n| EqKind::ALL.get(n)).copied() {
                        p_kind.update_eq(|s| {
                            if let Some(band) = s.bands.get_mut(i) {
                                band.kind = kind;
                            }
                        });
                    }
                }),
            eq_cell(
                2,
                Slider::new()
                    .height(22.0)
                    .min_height(22.0)
                    .minimum(f64::from(EQ_GAIN_MIN))
                    .maximum(f64::from(EQ_GAIN_MAX))
                    .step_frequency(0.1)
                    .value(f64::from(b.gain_db))
                    .is_enabled(uses_gain)
                    .on_value_changed(move |v: f64| {
                        p_gain.update_eq(|s| {
                            if let Some(band) = s.bands.get_mut(i) {
                                band.gain_db = v as f32;
                            }
                        })
                    }),
                if uses_gain {
                    format!("{:+.1} dB", zeroed(b.gain_db))
                } else {
                    "-".to_string()
                },
            ),
            // Q and frequency sliders are logarithmic: the slider value is log10 of the parameter.
            eq_cell(
                3,
                Slider::new()
                    .height(22.0)
                    .min_height(22.0)
                    .minimum(f64::from(EQ_Q_MIN).log10())
                    .maximum(f64::from(EQ_Q_MAX).log10())
                    .step_frequency(0.01)
                    .value(f64::from(b.q).log10())
                    .on_value_changed(move |v: f64| {
                        p_q.update_eq(|s| {
                            if let Some(band) = s.bands.get_mut(i) {
                                band.q = 10f64.powf(v) as f32;
                            }
                        })
                    }),
                format!("{:.2}", b.q),
            ),
            eq_cell(
                4,
                Slider::new()
                    .height(22.0)
                    .min_height(22.0)
                    .minimum(lo_hz)
                    .maximum(hi_hz)
                    .step_frequency(0.002)
                    .value(f64::from(b.freq_hz).log10())
                    .on_value_changed(move |v: f64| {
                        p_freq.update_eq(|s| {
                            if let Some(band) = s.bands.get_mut(i) {
                                band.freq_hz = 10f64.powf(v) as f32;
                            }
                        })
                    }),
                fmt_hz(b.freq_hz),
            ),
            x_button(move || {
                p_del.update_eq(|s| {
                    if i < s.bands.len() {
                        s.bands.remove(i);
                    }
                })
            })
            .grid_column(5),
        ))
}

fn bus_name(bus: usize) -> &'static str {
    if bus == 0 { "A" } else { "B" }
}

/// Translucent, rounded panel used for every group.
fn card(child: impl Into<View>) -> Border {
    Border::new()
        .background(Brush::Solid(CARD))
        .border_brush(Brush::Solid(CARD_EDGE))
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(10.0)
        .padding(Thickness::uniform(8.0))
        .content(child)
}

/// Recessed area that holds the strip cards, so leftover space reads as part of the mixer.
fn rack(child: impl Into<View>) -> Border {
    Border::new()
        .background(Brush::Solid(WELL))
        .border_brush(Brush::Solid(CARD_EDGE))
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(10.0)
        .padding(Thickness::uniform(6.0))
        .content(child)
}

fn badge(text: &str, color: Color) -> Border {
    Border::new()
        .width(18.0)
        .height(18.0)
        .corner_radius(9.0)
        .background(Brush::Solid(color))
        .content(
            TextBlock::new()
                .text(text)
                .font_size(9.5)
                .font_weight(FontWeight::BOLD)
                .foreground(Brush::Solid(INK))
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Center),
        )
}

fn label_text(text: &str, size: f64, color: Color) -> TextBlock {
    TextBlock::new()
        .text(text)
        .font_size(size)
        .foreground(Brush::Solid(color))
}

/// Keeps a hair below zero from rendering as `-0`.
fn zeroed(db: f32) -> f32 {
    if db.abs() < 0.05 { 0.0 } else { db }
}

fn trim_db(db: f32) -> String {
    format!("{:+.0}", zeroed(db))
}

fn section_label(text: &str) -> TextBlock {
    TextBlock::new()
        .text(text)
        .font_size(9.5)
        .character_spacing(120)
        .font_weight(FontWeight::SEMI_BOLD)
        .foreground(Brush::Solid(DIM))
        .vertical_alignment(VerticalAlignment::Center)
}

/// Filled button: transport and caption actions. Local background wins over the style, so the
/// color is stable (Subtle style keeps the flat look and rounded corners).
fn action_button(text: &str, bg: Color, fg: Color, click: impl IntoUnitCallback) -> Button {
    Button::new()
        .style(ButtonStyle::Subtle)
        .background(Brush::Solid(bg))
        .height(30.0)
        .min_width(84.0)
        .content(
            TextBlock::new()
                .text(text)
                .font_size(11.5)
                .font_weight(FontWeight::SEMI_BOLD)
                .foreground(Brush::Solid(fg)),
        )
        .on_click(click)
}

/// Colored A/B routing toggle, read from the live atomics and written straight back.
fn route_chip(params: &Arc<Params>, bus: usize) -> Button {
    let on = params.routed(bus);
    let (bg, fg) = if on {
        (if bus == 0 { GREEN } else { BLUE }, INK)
    } else {
        (FILL_OFF, TEXT)
    };
    let live = params.clone();
    chip(bus_name(bus), bg, fg)
        .width(32.0)
        .on_click(move || live.set_routed(bus, !on))
}

fn mute_chip(params: &Arc<Params>) -> Button {
    let muted = params.muted();
    let live = params.clone();
    chip(
        "M",
        if muted { RED } else { FILL_OFF },
        if muted { INK } else { TEXT },
    )
    .width(24.0)
    .on_click(move || live.set_muted(!muted))
}

/// Small square toggle. The button's own padding would clip a single glyph, so the label is
/// pulled outward with a negative margin (WinUI buttons do not clip their content).
fn chip(text: &str, bg: Color, fg: Color) -> Button {
    Button::new()
        .style(ButtonStyle::Subtle)
        .background(Brush::Solid(bg))
        .height(22.0)
        .content(
            TextBlock::new()
                .text(text)
                .font_size(10.0)
                .font_weight(FontWeight::SEMI_BOLD)
                .foreground(Brush::Solid(fg))
                .text_alignment(TextAlignment::Center)
                .margin(Thickness::new(-10.0, -5.0, -10.0, -5.0)),
        )
}

fn run_state(running: bool) -> View {
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(6.0)
        .vertical_alignment(VerticalAlignment::Center)
        .children((
            Border::new()
                .width(8.0)
                .height(8.0)
                .corner_radius(4.0)
                .background(Brush::Solid(if running { GREEN } else { RED })),
            TextBlock::new()
                .text(if running { "Running" } else { "Stopped" })
                .font_size(10.5)
                .foreground(Brush::Solid(DIM)),
        ))
        .into()
}

fn level_color(db: f32) -> Color {
    if db > -6.0 {
        RED
    } else if db > -20.0 {
        AMBER
    } else {
        GREEN
    }
}

/// Vertical peak meter next to a strip fader: a track with a bottom-aligned fill.
fn vmeter(db: f32, height: f64) -> Border {
    let level = ((db + 60.0) / 60.0).clamp(0.0, 1.0) as f64;
    let fill = if level <= 0.0 {
        0.0
    } else {
        (height * level).max(2.0)
    };
    Border::new()
        .width(12.0)
        .height(height)
        .corner_radius(6.0)
        .background(Brush::Solid(TRACK))
        .content(
            Border::new()
                .width(12.0)
                .height(fill)
                .corner_radius(6.0)
                .vertical_alignment(VerticalAlignment::Bottom)
                .background(Brush::Solid(level_color(db))),
        )
}

fn main() {
    App::run_component_with_policy::<MixerApp>(
        (),
        WindowPolicy::new()
            .title("RustMeeter")
            .client_size(940.0, 740.0)
            .minimum_client_size(760.0, 460.0),
    )
    .unwrap();
}
