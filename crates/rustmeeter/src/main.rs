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
//!
//! Module map:
//! - [`app`]        component state, lifecycle and layout (one file per panel)
//! - [`messages`]   messages posted to the UI thread
//! - [`fader`]      custom channel fader (dB scale, groove, cap) plus its level meter
//! - [`hslider`]    compact horizontal slider used by bus faders and the EQ table
//! - [`theme`]      palette and reusable view builders
//! - [`util`]       small display/domain helpers

mod app;
mod config;
mod fader;
mod hslider;
mod messages;
mod theme;
mod util;
use windows_reactor::{App, WindowPolicy};

fn main() {
    App::run_component_with_policy::<app::MixerApp>(
        (),
        WindowPolicy::new()
            .title("RustMeeter")
            .client_size(400.0, 580.0)
            .minimum_client_size(760.0, 460.0),
    )
    .unwrap();
}
