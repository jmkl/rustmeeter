//! Things the core reports asynchronously (from audio/error threads) to whichever frontend is
//! attached. Callback-based, like `CaptionController`: a CLI prints, a GUI forwards the event into
//! its own channel and requests a repaint. The handler may run on any thread: keep it short.

use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum Event {
    /// Streams were (re)built and audio is running.
    Started,
    /// A stream reported an error (device unplugged, format change, ...).
    Error(String),
    /// The engine worked around a routing problem the user should know about (a route that would
    /// feed a device back into itself, ...). Audio keeps running; the route is dropped.
    Warning(String),
}

pub type EventHandler = Arc<dyn Fn(&Event) + Send + Sync>;

/// Default: print errors and warnings to stderr.
pub(crate) fn default_handler() -> EventHandler {
    Arc::new(|e| match e {
        Event::Error(m) => eprintln!("error: {m}"),
        Event::Warning(m) => eprintln!("warning: {m}"),
        Event::Started => {}
    })
}
