//! Messages sent to the UI thread: user actions, async events and the meter tick.

use rustmeeter_core::*;

/// Messages sent to the UI thread: user actions, async events and the meter tick.
#[derive(Clone)]
pub(crate) enum Message {
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
    /// Client width in DIPs, so the EQ table can size its columns to the window.
    WindowWidth(f64),
    /// Minimize/hide window to system tray.
    MinimizeToTray,
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
