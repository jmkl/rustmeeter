//! Bottom status bar: the engine's state plus what the caption pipeline is doing right now.
//!
//! The graph is the point of the whole row. "Is the transcriber hearing anything?" cannot be read
//! off a dot: a dot says the thread is alive, not that audio is arriving. So the last ~2 s of level
//! at the caption tap is drawn as bars, and the word next to it changes when the VAD finds speech
//! or the model starts working. A flat graph next to "no sound" means silence is reaching the tap,
//! which is exactly the failure that used to be invisible.

use rustmeeter_core::*;
use windows_reactor::*;

use super::MixerApp;
use crate::theme::*;

/// Width of the level graph in DIPs: about 2 s wide, so a word forms and fades visibly.
const GRAPH_W: f64 = 190.0;
const GRAPH_H: f64 = 16.0;

const HINT_OFF: &str =
    "Click the caption button up top to start. Captions read one strip's raw input.";
const HINT_LISTENING: &str = "The tap is open and the transcriber is waiting: no speech detected. \
     The graph shows what it is actually receiving. \"No sound\" means the tap is getting silence — \
     check that the captioned strip is the device you meant, and that audio is running.";
const HINT_SPEECH: &str =
    "Voice detected: this utterance is being collected. Transcription starts when it ends.";
const HINT_TRANSCRIBING: &str =
    "The model is working on the last utterance. Its duration appears on the right.";
const HINT_GRAPH: &str = "Level at the caption tap: the raw audio of the captioned strip, before \
     gain, mute and routing. A flat line means the transcriber is receiving silence, whatever the \
     faders say.";
const HINT_STRIP: &str = "Captions read the raw input of this strip: they follow the source, not \
     the A/B routing, so a strip sent nowhere still transcribes.";
const HINT_COUNTERS: &str = "Utterances transcribed since captions were started, and how long the \
     model took on the last one.";
const HINT_MODEL: &str =
    "Starting captions loads whistle.cact once, then keeps it for later on/off cycles.";

/// Colour of the caption dot. Blue = open and quiet, green = voice detected, amber = the model is
/// busy (the "processing with Whisper" state to look for while a line is being turned into text).
fn state_color(state: CaptionState) -> Color {
    match state {
        CaptionState::Transcribing => AMBER,
        CaptionState::Speech => GREEN,
        CaptionState::Listening => BLUE,
        CaptionState::Idle => DIM,
    }
}

fn state_hint(state: Option<CaptionState>) -> &'static str {
    match state {
        None => HINT_OFF,
        Some(CaptionState::Listening) => HINT_LISTENING,
        Some(CaptionState::Speech) => HINT_SPEECH,
        Some(CaptionState::Transcribing) => HINT_TRANSCRIBING,
        Some(CaptionState::Idle) => HINT_OFF,
    }
}

/// Keep a status-bar string to one line. Device names and error chains are unbounded; the bar is
/// not. The text is still available in full as a tooltip.
fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().nth(max).is_some() {
        out.push('…');
    }
    out
}

impl MixerApp {
    pub(crate) fn status_bar(&self, view: &MixerView) -> View {
        let status = self.caps.status();
        let state = status.as_ref().map(|s| s.state());

        // Left: the audio engine. A stopped engine explains a silent graph without guessing.
        let engine = run_state(view.running);

        // Caption state. "listening" alone answers "is the thread alive?", not "is audio arriving?",
        // so while the VAD is quiet the word says which of the two it is.
        let quiet = status.as_ref().is_some_and(|s| s.level() < 0.0008);
        let (color, label) = match state {
            Some(CaptionState::Listening) if quiet => {
                (state_color(CaptionState::Listening), "no sound")
            }
            Some(state) => (state_color(state), state.label()),
            None => (DIM, "captions off"),
        };

        let mut caption_cell: Vec<View> = vec![
            dot(color).into(),
            TextBlock::new()
                .text(label)
                .font_size(10.5)
                .font_weight(FontWeight::SEMI_BOLD)
                .foreground(Brush::Solid(if state.is_some() { TEXT } else { DIM }))
                .vertical_alignment(VerticalAlignment::Center)
                .tooltip(state_hint(state))
                .into(),
        ];

        // The graph only exists while the tap is open: next to "captions off" an empty well reads
        // as a progress bar stuck at zero.
        if let Some(status) = &status {
            caption_cell.push(
                spark(&status.history(), GRAPH_W, GRAPH_H)
                    .margin(Thickness::new(8.0, 0.0, 0.0, 0.0))
                    .tooltip(HINT_GRAPH)
                    .into(),
            );
        }

        let caption_state = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(6.0)
            .vertical_alignment(VerticalAlignment::Center)
            .children(caption_cell);

        // Right: what the model produced, or why it failed.
        let mut tail: Vec<View> = vec![];
        if let Some(n) = self.caps.active_strip() {
            let strip = view
                .strips
                .get(n.saturating_sub(1))
                .map(|s| s.label.clone())
                .unwrap_or_else(|| format!("strip {n}"));
            tail.push(
                caption(&format!("strip {n} · {}", clip(&strip, 34)), DIM)
                    .tooltip(HINT_STRIP)
                    .into(),
            );
        }
        match (&status, status.as_ref().and_then(|s| s.error())) {
            (_, Some(error)) => tail.push(
                // One line, no wrapping: the bar must not grow. The tooltip has the whole error.
                caption(&format!("error: {}", clip(&error, 52)), RED)
                    .tooltip(&error)
                    .into(),
            ),
            (Some(s), None) => tail.push(
                caption(
                    &format!(
                        "{} {} · last {} ms",
                        s.utterances(),
                        if s.utterances() == 1 { "line" } else { "lines" },
                        s.last_ms()
                    ),
                    DIM,
                )
                .tooltip(HINT_COUNTERS)
                .into(),
            ),
            (None, None) => tail.push(
                caption("the model loads on first use", DIM)
                    .tooltip(HINT_MODEL)
                    .into(),
            ),
        }

        let tail = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(10.0)
            .horizontal_alignment(HorizontalAlignment::Right)
            .vertical_alignment(VerticalAlignment::Center)
            .children(tail);

        let row = Grid::new()
            .columns([
                GridLength::Auto, // engine
                GridLength::Auto, // caption state + graph
                GridLength::STAR, // slack
                GridLength::Auto, // counters, right-aligned
            ])
            .column_spacing(12.0)
            .children((
                engine,
                // Every child after the first needs its column named, or it lands back in column 0
                // and paints over the engine text.
                in_column(caption_state, 1),
                in_column(tail, 3),
            ));

        Border::new()
            .background(Brush::Solid(CARD))
            .border_brush(Brush::Solid(CARD_EDGE))
            .border_thickness(Thickness::uniform(1.0))
            .corner_radius(8.0)
            .padding(Thickness::new(10.0, 5.0, 10.0, 5.0))
            .content(row)
            .into()
    }
}
