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
use std::rc::Rc;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rustmeeter_core::*;
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
        }
    }

    fn view(&self, _input: &(), context: &mut ViewContext<Self>) -> View {
        let view = self.mixer.snapshot();

        context.window_title("RustMeeter");
        context.window_visuals(
            WindowVisuals::new()
                .theme(WindowTheme::Dark)
                .backdrop(WindowBackdrop::Acrylic)
                .client_size(940.0, 620.0)
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
                    action_button("Start", GREEN, INK, context.callback(|()| Message::ToggleRun))
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
        let cards: Vec<KeyedView> = view
            .strips
            .iter()
            .enumerate()
            .map(|(index, s)| keyed(u64::from(s.id), self.strip_card(index, s, context)))
            .collect();
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
            .columns([
                GridLength::Auto,
                GridLength::STAR,
                GridLength::Auto,
            ])
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
            .columns([
                GridLength::Auto,
                GridLength::STAR,
                GridLength::Auto,
            ])
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
                        label_text("×", 11.0, RED)
                            .margin(Thickness::new(-6.0, -6.0, -6.0, -6.0)),
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

        for band in 0..EQ_BAND_NAMES.len() {
            let eq = params.clone();
            body.push(
                Grid::new()
                    .columns([GridLength::Pixel(12.0), GridLength::STAR])
                    .column_spacing(4.0)
                    .children((
                        TextBlock::new()
                            .text(EQ_BAND_NAMES[band])
                            .font_size(9.0)
                            .foreground(Brush::Solid(DIM))
                            .vertical_alignment(VerticalAlignment::Center),
                        Slider::new()
                            .grid_column(1)
                            .height(22.0)
                            .min_height(22.0)
                            .minimum(-f64::from(EQ_RANGE_DB))
                            .maximum(f64::from(EQ_RANGE_DB))
                            .step_frequency(0.5)
                            .value(f64::from(s.eq_db[band]))
                            .on_value_changed(move |v: f64| eq.set_eq_db(band, v as f32)),
                    ))
                    .into(),
            );
        }

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

    fn bus_row(&self, bus: usize, view: &MixerView) -> Grid {
        let p = &view.buses[bus].params;
        let gain = p.clone();
        let muted = p.muted();
        let has_device = view.buses[bus].device.is_some();
        let device = view
            .buses[bus]
            .device
            .clone()
            .unwrap_or_else(|| "no device".to_string());
        let level = ((p.level_db() + 60.0) / 60.0).clamp(0.0, 1.0) as f64;

        Grid::new()
            .rows([
                GridLength::Auto,
                GridLength::Auto,
                GridLength::Auto,
            ])
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
                        .children((
                            self.bus_row(0, view),
                            self.bus_row(1, view).grid_row(1),
                        )),
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
    chip("M", if muted { RED } else { FILL_OFF }, if muted { INK } else { TEXT })
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
            .client_size(940.0, 620.0)
            .minimum_client_size(760.0, 460.0),
    )
    .unwrap();
}


