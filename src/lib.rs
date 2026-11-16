//! RustMeeter: a small Voicemeeter-style mixer engine.
//! Frontends only need `Mixer`, `Params`, and the device helpers.

#[cfg(feature = "cactus")]
mod cactus;
mod caption_controller;
mod captions;
mod devices;
mod engine;
mod events;
mod mixer;
mod params;
mod tap;
mod vad;

#[cfg(feature = "cactus")]
pub use cactus::Whistle;
pub use caption_controller::{CaptionCallback, CaptionController};
pub use captions::{Caption, CaptionLogger, SharedTranscriber, Transcriber, share_transcriber};
pub use devices::{
    InputSource, default_input_name, default_output_name, list_inputs, list_outputs,
};
pub use events::{Event, EventHandler};
pub use mixer::{Bus, BusView, Mixer, MixerView, Strip, StripId, StripView};
pub use params::{EQ_BAND_NAMES, EQ_BANDS, EQ_RANGE_DB, MAX_DB, MIN_DB, NUM_BUSES, Params};
pub use tap::{TAP_RATE, Tap};
pub use vad::{Segmenter, Utterance};
