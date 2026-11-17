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
