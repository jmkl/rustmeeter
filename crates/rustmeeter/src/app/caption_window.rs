//! Standalone caption bar, opened from the tray.
//!
//! The mixer already shows a transcript panel and a status bar; this window is a compact, glanceable
//! version of the same thing ("what is the transcriber hearing, and what did it just say") that can
//! stay open with the mixer closed to the tray. It is also the caption on/off control.
//!
//! Everything is read from [`SharedMixer`], the same allocation both windows hold. This component
//! deliberately never touches `SharedMixer::completion_slot`: that slot is single and typed to the
//! mixer's `Message`, so claiming it here would silently steal every event and caption from the
//! mixer window. A 10 fps timer keeps the waveform and the state pill live, and works whether or
//! not the mixer window exists.
//!
//! Toggling goes through [`CaptionWindowInput::toggle`] - a callback into the application root -
//! rather than calling `CaptionController` directly, so the tray menu and the mic button share one
//! code path and cannot disagree about whether captions are on.

use std::rc::Rc;
use std::time::Duration;

use windows_reactor::*;

use super::SharedMixerRef;
use super::status::state_color;
use crate::theme::*;

const BAR: Color = Color::rgb(0x3B, 0x3B, 0x3B);
/// The state pill is darker than the bar so it reads as a badge, not a button.
const PILL: Color = Color::rgb(0x21, 0x21, 0x21);
const WAVE: Color = Color::rgb(0xE8, 0xE8, 0xE8);
const HINT: Color = Color::rgb(0x9A, 0x9A, 0x9A);
/// Red while captioning: the bar says "listening" in the same breath, so the colour is the state.
const MIC_ON: Color = Color::rgb(0xF4, 0x33, 0x3F);
const MIC_OFF: Color = Color::rgb(0x4A, 0x4A, 0x4A);

const TITLE_H: f64 = 32.0;
const TITLE_TEXT: Color = Color::rgb(0xB8, 0xB8, 0xB8);
const TITLE_FADE: Duration = Duration::from_millis(120);

const WAVE_H: f64 = 56.0;
const MIC_D: f64 = 58.0;
const BAR_W: f64 = 4.0;
const BAR_GAP: f64 = 4.0;

const HINT_IDLE: &str = "Incoming caption goes here\u{2026}";
const HINT_OFF: &str = "Press the mic to start captioning.";
const HINT_STOPPED: &str = "Audio is stopped \u{2014} start the mixer to hear anything.";

/// What the window needs from the application: the shared state, and the root's caption toggle.
#[derive(Clone)]
pub(crate) struct CaptionWindowInput {
    pub(crate) shared: SharedMixerRef,
    /// Sends `AppAction::Caption` to the application root.
    pub(crate) toggle: Callback<()>,
}

impl PartialEq for CaptionWindowInput {
    /// Identity of the shared state, like `SharedMixerInput`. The callback belongs to the root and
    /// does not distinguish two windows.
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.shared, &other.shared)
    }
}

pub(crate) enum Msg {
    Toggle,
    /// Index in the strip combo: 0 = "first mic", n = strip n.
    PickStrip(Option<usize>),
    /// Client width in DIPs, so the waveform fills the bar at any window size.
    Width(f64),
    /// Close button on the title bar.
    Close,
}

pub(crate) struct CaptionWindow {
    shared: SharedMixerRef,
    toggle: Callback<()>,
    width: f64,
    title_visible: bool,
}

/// Mirrored bar graph: the shape of what the tap is hearing, symmetric about the middle.
///
/// `CaptionStatus::history` is linear RMS, which puts normal speech within a few pixels of the
/// floor, so levels are mapped over -60..0 dB and each bar keeps the loudest sample in its bucket:
/// a single word stays visible instead of being averaged into the dots. A bar shorter than its
/// width rounds into a dot, so silence draws as a dotted line rather than an empty box.
fn waveform(values: &[f32], width: f64, height: f64) -> Border {
    let slots = ((width + BAR_GAP) / (BAR_W + BAR_GAP)).floor().max(1.0) as usize;
    let bars: Vec<View> = (0..slots)
        .map(|i| {
            let amp = if values.is_empty() {
                0.0
            } else {
                let start = i * values.len() / slots;
                let end = ((i + 1) * values.len() / slots)
                    .max(start + 1)
                    .min(values.len());
                values[start..end].iter().copied().fold(0.0, f32::max)
            };
            let db = 20.0 * amp.max(1e-6).log10();
            let norm = ((db + 60.0) / 60.0).clamp(0.0, 1.0) as f64;
            let h = (norm * height).clamp(BAR_W, height);
            Border::new()
                .width(BAR_W)
                .height(h)
                .corner_radius(BAR_W / 2.0)
                .background(Brush::Solid(WAVE))
                .canvas_left(i as f64 * (BAR_W + BAR_GAP))
                .canvas_top((height - h) / 2.0)
                .into()
        })
        .collect();

    Border::new()
        .width(width)
        .height(height)
        .content(Canvas::new().width(width).height(height).children(bars))
}

/// "listening" -> "Listening": the pill is a label, not a log line.
fn sentence_case(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

impl Component for CaptionWindow {
    type Input = CaptionWindowInput;
    type Message = Msg;

    fn create(input: &CaptionWindowInput, context: &ComponentContext<Self>) -> Self {
        Self {
            shared: input.shared.clone(),
            toggle: input.toggle.clone(),
            width: 620.0,
            title_visible: false,
        }
    }

    fn update(&mut self, message: Msg, context: &ComponentContext<Self>) {
        match message {
            // The re-render this triggers is the whole point: it repaints the waveform and the pill.
            Msg::Toggle => self.toggle.call(()),
            Msg::PickStrip(index) => {
                // 1-based; `None` means "the first mic".
                self.shared.borrow_mut().caption_strip = index.filter(|n| *n > 0);
            }
            Msg::Width(width) => self.width = width,
            Msg::Close => {
                let _ = context.close_window();
            }
        }
    }

    fn view(&self, _input: &CaptionWindowInput, context: &mut ViewContext<Self>) -> View {
        let (active, status, latest, _strips, _selected, icon, running) = {
            let shared = self.shared.borrow();
            (
                shared.caps.active_strip(),
                shared.caps.status(),
                shared.caps.latest(),
                shared
                    .mixer
                    .strips()
                    .iter()
                    .map(|s| s.source.label())
                    .collect::<Vec<_>>(),
                shared.caption_strip,
                shared.config.icon.clone(),
                shared.mixer.is_running(),
            )
        };

        context.window_title("RustMeeter Captions");
        context.on_window_size(context.callback(|size: WindowSize| Msg::Width(size.width)));
        context.window_visuals(
            WindowVisuals::new()
                .icon(icon)
                .always_on_top(true)
                .theme(WindowTheme::Dark)
                .backdrop(WindowBackdrop::Acrylic)
                .client_size(450.0, 150.0)
                .initial_position(ScreenPoint { x: 2090, y: 1230 })
                .constraints(WindowConstraints {
                    min_width: Some(450.0),
                    min_height: Some(150.0),
                    ..WindowConstraints::default()
                }),
        );

        let state = status.as_ref().map(|s| s.state());
        let (state_dot, word) = match state {
            Some(state) => (state_color(state), sentence_case(state.label())),
            None => (DIM, "Captions off".to_string()),
        };

        // Counters do not deserve a line of their own in a bar this small; the pill's tooltip has
        // them, so the answer is there when someone looks for it.
        let pill_hint = match &status {
            Some(s) => format!(
                "{} {} transcribed, last took {} ms. Raw input of the captioned strip.",
                s.utterances(),
                if s.utterances() == 1 { "line" } else { "lines" },
                s.last_ms()
            ),
            None => "Captions are not running.".to_string(),
        };
        let pill = Border::new()
            .background(Brush::Solid(PILL))
            .corner_radius(12.0)
            .padding(Thickness::new(9.0, 3.0, 11.0, 3.0))
            .content(
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .vertical_alignment(VerticalAlignment::Center)
                    .children((
                        dot(state_dot),
                        TextBlock::new()
                            .text(&word)
                            .font_size(11.0)
                            .foreground(Brush::Solid(WHITE))
                            .vertical_alignment(VerticalAlignment::Center),
                    )),
            )
            .tooltip(pill_hint);

        let header: Vec<View> = vec![pill.into()];

        let header = Grid::new()
            .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto])
            .children(header);

        let wave_w = (self.width - 2.0 * 6.0 - 2.0 * 18.0 - MIC_D - 14.0).max(120.0);
        let history = status.as_ref().map(|s| s.history()).unwrap_or_default();
        let wave = Border::new()
            .vertical_alignment(VerticalAlignment::Stretch)
            .content(waveform(&history, wave_w, WAVE_H));

        // One line, and the one thing worth reading. An error outranks the hint because nothing
        // else on this bar would explain the silence.
        let error = status.as_ref().and_then(|s| s.error());
        let line = latest
            .as_ref()
            .map(|c| c.text.trim().to_string())
            .unwrap_or_default();
        let (text, color) = match (&error, line.is_empty(), active.is_some(), running) {
            (Some(error), _, _, _) => (format!("Error: {error}"), RED),
            (None, false, _, _) => (line.clone(), TEXT),
            (None, true, false, _) => (HINT_OFF.to_string(), HINT),
            (None, true, true, false) => (HINT_STOPPED.to_string(), AMBER),
            (None, true, true, true) => (HINT_IDLE.to_string(), HINT),
        };
        let caption_line = TextBlock::new()
            .text(&text)
            .font_size(15.0)
            .foreground(Brush::Solid(color))
            .text_alignment(TextAlignment::Center)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .text_wrapping(TextWrapping::Wrap)
            .grid_row(2)
            .tooltip(&text);

        // A real circle. `Button` only has a template corner radius (`ControlCornerRadius`) and
        // overriding that resource trips a runtime error in the adapter, so the circle is a
        // `Border` with a pointer handler: same click, exact shape, no template involved.
        let mic = Border::new()
            .width(MIC_D)
            .height(MIC_D)
            .corner_radius(MIC_D / 2.0)
            .background(Brush::Solid(if active.is_some() {
                MIC_ON
            } else {
                MIC_OFF
            }))
            .on_pointer_pressed(context.callback(|_: PointerEventInfo| Msg::Toggle))
            .content(
                FontIcon::new()
                    .glyph(if active.is_some() {
                        "\u{E71A}" // stop
                    } else {
                        "\u{E720}" // microphone
                    })
                    .font_size(22.0)
                    .foreground(Brush::Solid(WHITE))
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .vertical_alignment(VerticalAlignment::Center),
            )
            .tooltip(match active {
                Some(_) => "Stop captioning.".to_string(),
                None => "Start captioning. The model loads on first use.".to_string(),
            });

        let body: View = Grid::new()
            .rows([GridLength::Auto, GridLength::STAR, GridLength::Auto])
            .vertical_alignment(VerticalAlignment::Stretch)
            .columns([GridLength::STAR, GridLength::Auto])
            .row_spacing(0.0)
            .column_spacing(5.0)
            .children((
                header.grid_row(0),
                wave.grid_row(1),
                caption_line,
                Border::new()
                    .grid_column(1)
                    .grid_row(0)
                    .grid_row_span(3)
                    .horizontal_alignment(HorizontalAlignment::Right)
                    .vertical_alignment(VerticalAlignment::Center)
                    .content(mic),
            ))
            .into();

        let card: View = Border::new()
            .corner_radius(4.0)
            .vertical_alignment(VerticalAlignment::Stretch)
            .padding(Thickness::uniform(0.0))
            .content(
                Border::new()
                    .padding(Thickness::new(18.0, 14.0, 18.0, 14.0))
                    .content(body),
            )
            .into();


        let drag = Border::new().background(Brush::Solid(Color::transparent()));
        let close = Button::new()
            .style(ButtonStyle::Subtle)
            .height(TITLE_H)
            .content(
                FontIcon::new()
                    .glyph("\u{E8BB}") // ChromeClose
                    .font_size(9.0)
                    .foreground(Brush::Solid(TITLE_TEXT)),
            )
            .on_click(context.callback(|()| Msg::Close))
            .tooltip("Close the caption window. Captioning keeps running; reopen from the tray.");
        let bar = Border::new()
            .corner_radius(CornerRadius::new(16.0, 16.0, 0.0, 0.0))
            .padding(Thickness::new(12.0, 0.0, 2.0, 0.0))
            .opacity_transition(TITLE_FADE)
            .content(
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .children((drag, in_column(close, 1))),
            );
        // The sensor is the rectangle, the bar is what you see; the bar must not take the pointer
        // events, or leaving the label for the close button would look like leaving the strip.
        let sensor = Border::new()
            .height(TITLE_H)
            .background(Brush::Solid(Color::transparent()))
            .content(bar)
            .vertical_alignment(VerticalAlignment::Top);

        // No system title bar: a `TitleBar` with `Collapsed` height switches the window into
        // "content extends into the title bar" mode and then removes the bar itself, so there is
        // no strip and no caption buttons. It still has to be declared, or the window keeps its
        // standard frame. It is pinned to zero height so the card starts at the very top of the
        // client area.
        let no_title = Border::new()
            .height(0.0)
            .content(TitleBar::new().preferred_height(WindowTitleBarHeight::Collapsed));

        Grid::new()
            .rows([ GridLength::STAR])
            .children((
                no_title,
                card,
                sensor

            ))
            .into()
    }
}
