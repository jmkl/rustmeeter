//! RustMeeter: a small Voicemeeter-style mixer engine.
//! Frontends only need `Mixer`, `Params`, and the device helpers.

#[cfg(feature = "cactus")]
use cactus_core as cactus;

#[cfg(feature = "cactus")]
impl captions::Transcriber for cactus::Whistle {
    fn transcribe(&mut self, pcm16k: &[i16]) -> anyhow::Result<String> {
        cactus::Transcriber::transcribe(self, pcm16k)
    }
}

mod caption_controller;
mod captions;
mod devices;
mod engine;
mod eq;
mod events;
mod mixer;
mod params;
mod tap;
mod vad;

#[cfg(feature = "cactus")]
pub use cactus::{Agent, Cactus, Mirage, Models, Whistle};
pub use caption_controller::{CaptionCallback, CaptionController};
pub use captions::{
    Caption, CaptionLogger, CaptionState, CaptionStatus, SharedTranscriber, Transcriber,
    share_transcriber,
};
pub use devices::{
    InputSource, capture_peer, default_input_name, default_output_name, fed_by, list_inputs,
    list_outputs, same_device,
};

pub use eq::{
    EQ_FREQ_MAX, EQ_FREQ_MIN, EQ_GAIN_MAX, EQ_GAIN_MIN, EQ_MAX_BANDS, EQ_Q_MAX, EQ_Q_MIN, EqBand,
    EqKind, EqSettings,
};
pub use events::{Event, EventHandler};
pub use mixer::{Bus, BusView, Mixer, MixerView, Strip, StripId, StripView};
pub use params::{MAX_DB, MIN_DB, NUM_BUSES, Params};
pub use tap::{TAP_RATE, Tap};
pub use vad::{Segmenter, Utterance};
