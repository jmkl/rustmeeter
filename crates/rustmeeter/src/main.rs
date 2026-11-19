//! RustMeeter GUI (windows-reactor / WinUI 3).
//!
//! A thin frontend: no audio code here, only the public library API
//! (`Mixer`, `Params`, `Mixer::snapshot()`, `CaptionController`, `Mixer::set_event_handler()`).
//!
//! The `Mixer` and `CaptionController` live in a `SharedMixer` that outlives window open/close
//! cycles. Closing the window drops only the UI state; audio streams keep playing in the
//! background. Clicking the tray icon reopens the window, which reconnects to the same mixer.
//!
//! The tray menu also declares a second, independent window (`app::CaptionWindow`): same
//! `SharedMixer`, its own component, opened on demand. Add further windows the same way — one
//! `.window::<C>(key, input)` per window, one `context.show_window(key)` to open it.
//!
//! Audio-thread callbacks reach the UI through a `ComponentCompletion` stored in the shared state;
//! the slot is updated each time a window component is created.
//!
//!   cargo run --release --features gui --bin rustmeeter-gui
//!
//! Module map:
//! - [`app`]        component state, lifecycle and layout (one file per panel)
//! - [`messages`]   messages posted to the UI thread//! - [`fader`]      custom channel fader (dB scale, groove, cap) plus its level meter
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

use std::path::PathBuf;
use std::rc::Rc;
use windows_reactor::{
    App, Application, ApplicationContext, ApplicationView, ApplicationViewContext, Key, Menu,
    MenuItem, NotifyIcon,
};

use app::{SharedMixerInput, SharedMixerRef};

struct RustMeeterApp {
    shared: SharedMixerRef,
    icon_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
enum AppAction {
    Show,
    Quit,
    Caption,
    /// Open the standalone caption window, without touching the mixer window.
    CaptionWindow,
}

impl Application for RustMeeterApp {
    type Input = ();
    type Message = AppAction;

    fn create(_input: &(), context: &ApplicationContext<Self>) -> Self {
        let shared = app::SharedMixer::new();
        let icon_path = PathBuf::from(shared.borrow().config.icon.clone());
        context.show_window("main");
        context.show_window("caption"); // TEMP debug
        Self { shared, icon_path }
    }

    fn update(&mut self, message: Self::Message, context: &ApplicationContext<Self>) {
        match message {
            AppAction::Show => {
                context.show_window("main");
            }
            AppAction::Quit => {
                context.exit();
            }
            AppAction::Caption => {
                let mut shared = self.shared.borrow_mut();
                if shared.caps.active_strip().is_some() {
                    shared.caps.off();
                } else if shared.config.settings.model_root.is_some() {
                    let s = &mut *shared;
                    let _ = s.caps.on(&s.mixer, s.caption_strip);
                }
            }
            AppAction::CaptionWindow => {
                context.show_window("caption");
            }
        }
    }

    fn view(&self, _input: &(), context: &ApplicationViewContext<Self>) -> ApplicationView {
        let show_cb = context.callback(|()| AppAction::Show);
        let menu_cb = context.callback(|key: Key| {
            if key == Key::from("quit") {
                AppAction::Quit
            } else if key == Key::from("caption") {
                AppAction::Caption
            } else if key == Key::from("caption_window") {
                AppAction::CaptionWindow
            } else {
                AppAction::Show
            }
        });
        // Read the live state rather than tracking it: any of the windows can toggle captions.
        let caption = if self.shared.borrow().caps.active_strip().is_some() {
            "Stop Caption"
        } else {
            "Start Caption"
        };

        let menu = Menu::new(
            [
                MenuItem::item("caption", Rc::from(caption)),
                MenuItem::separator("sep2"),
                MenuItem::item("caption_window", Rc::from("Caption Window")),
                MenuItem::item("show", Rc::from("Show RustMeeter")),
                MenuItem::separator("sep"),
                MenuItem::item("quit", Rc::from("Quit")),
            ],
            menu_cb,
        );

        let tray = NotifyIcon::new(self.icon_path.clone())
            .tooltip("RustMeeter")
            .on_activate(move |_point| show_cb.call(()))
            .menu(menu);

        let caption_window = app::CaptionWindowInput {
            shared: self.shared.clone(),
            toggle: context.callback(|()| AppAction::Caption),
        };

        ApplicationView::new()
            .window::<app::MixerApp>("main", SharedMixerInput(self.shared.clone()))
            // Declared, not opened: only the tray menu's "Caption Window" shows it.
            .window::<app::CaptionWindow>("caption", caption_window)
            .notify_icon("tray", tray)
    }
}

fn main() {
    App::run_application::<RustMeeterApp>(()).unwrap();
}
