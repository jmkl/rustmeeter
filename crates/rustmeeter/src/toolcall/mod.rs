//! Needle 3 tool calling for finished caption lines.
//!
//! A caption is handed to a worker thread as soon as the transcriber produces it. The worker owns
//! the `Agent`, initializes it lazily with the schemas from `config.json`'s `tools` array, asks it
//! for function calls, and executes each call through [`Registry`].
//!
//! Two rules keep an always-listening microphone from turning every stray word into an action:
//!
//! * **Triggers gate the request.** If the tools declare `triggers`, a caption is only sent to
//!   Needle when one of those regexes matches. A caption that matches nothing is skipped before
//!   the model runs. (The engine also uses triggers for routing; here they additionally act as the
//!   app-level "is this even addressed to a tool?" filter.)
//! * **Every caption is a fresh turn.** The worker calls `Agent::reset` before each completion, so
//!   "good night" spoken while chatting cannot become a follow-up to an earlier "turn on the
//!   light". Needle's conversation memory is for multi-turn chat, not for independent captions.
//!
//! The worker is deliberately not on the UI thread: loading Needle and running a completion can
//! take seconds. It is also serialized with the speech model through
//! `rustmeeter_core::cactus_call_lock`, because the Cactus runtime has one conversation shared by
//! every facade.
//!
//! Tool implementations live in `toolcall/tools.rs`; this module only knows how to run them.
//!
//! Set `ToolCallHandle::start`'s `log` argument to see every decision in `toolcall.log`: the
//! caption, whether it matched a trigger, Needle's raw JSON (including `reasoning`, `confidence`
//! and `suppressed_calls`), and each executed call's result.

mod registry;
mod tools;

use anyhow::{Context, Result, bail};
use regex::{Regex, RegexBuilder};
use rustmeeter_core::Agent;
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use self::registry::Registry;

/// What one caption line turned into. Reported back to the UI thread after the worker is done.
#[derive(Clone, Debug)]
pub(crate) struct ToolCallReport {
    /// The caption text that was sent to Needle.
    pub(crate) text: String,
    pub(crate) calls: Vec<ToolCallRecord>,
    /// Load, completion, or schema failure. A refusal (no function calls) is not an error.
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ToolCallRecord {
    pub(crate) name: String,
    pub(crate) arguments: Value,
    pub(crate) result: Result<Value, String>,
}

/// Handle shared with the caption callback. It only queues text; all model work happens on the
/// worker thread.
pub(crate) struct ToolCallHandle {
    sender: Mutex<Option<Sender<String>>>,
    enabled: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl ToolCallHandle {
    /// Start the worker. `model` is `needle3.cact`; `tools` is the raw schema array from settings,
    /// so the engine accepts either Needle's compact form or OpenAI-style functions.
    pub(crate) fn start(
        model: Option<PathBuf>,
        tools: Vec<Value>,
        log: Option<PathBuf>,
        notify: Arc<dyn Fn(ToolCallReport) + Send + Sync>,
    ) -> Arc<Self> {
        let triggers = Triggers::from_tools(&tools);
        let tools_json = if tools.is_empty() {
            None
        } else {
            serde_json::to_string(&tools).ok()
        };
        let (sender, receiver) = mpsc::channel();
        let enabled = Arc::new(AtomicBool::new(false));
        let worker_enabled = enabled.clone();
        let handle = Arc::new(Self {
            sender: Mutex::new(Some(sender)),
            enabled,
            worker: Mutex::new(None),
        });
        let spawned = thread::Builder::new()
            .name("needle3-toolcall".to_string())
            .spawn(move || {
                run(
                    receiver,
                    worker_enabled,
                    model,
                    tools_json,
                    triggers,
                    log,
                    notify,
                );
            });
        match spawned {
            Ok(worker) => {
                *handle.worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(worker);
            }
            Err(e) => {
                eprintln!("cannot start toolcall worker: {e}");
                *handle.sender.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        }
        handle
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled.load(Relaxed)
    }

    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Relaxed);
    }

    /// Queue one finished caption. Cheap: the caption thread never waits for the model.
    pub(crate) fn submit(&self, text: &str) {
        if !self.enabled() || text.trim().is_empty() {
            return;
        }
        if let Ok(slot) = self.sender.lock()
            && let Some(sender) = slot.as_ref()
        {
            let _ = sender.send(text.to_string());
        }
    }
}

impl Drop for ToolCallHandle {
    fn drop(&mut self) {
        // Stop queued work, then close the channel: that ends the worker's loop, so joining cannot
        // hang behind captions that were queued just before shutdown.
        self.enabled.store(false, Relaxed);
        let sender = match self.sender.get_mut() {
            Ok(slot) => slot.take(),
            Err(e) => e.into_inner().take(),
        };
        drop(sender);
        let worker = match self.worker.get_mut() {
            Ok(slot) => slot.take(),
            Err(e) => e.into_inner().take(),
        };
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

/// Trigger regexes collected from every tool schema.
///
/// `All` means no tool declared `triggers`, so every caption is attempted, which is only sensible
/// when the app is used deliberately. When at least one trigger exists, a caption must match one
/// of them before Needle is asked anything.
enum Triggers {
    All,
    Some(Vec<Regex>),
}

impl Triggers {
    fn from_tools(tools: &[Value]) -> Self {
        let mut patterns: Vec<Regex> = vec![];
        let mut declared = false;
        for tool in tools {
            let Some(triggers) = tool.get("triggers").and_then(Value::as_array) else {
                continue;
            };
            for pattern in triggers {
                let Some(pattern) = pattern.as_str() else {
                    continue;
                };
                declared = true;
                match RegexBuilder::new(pattern).case_insensitive(true).build() {
                    Ok(regex) => patterns.push(regex),
                    Err(e) => eprintln!("ignoring bad trigger regex {pattern:?}: {e}"),
                }
            }
        }
        if declared {
            Self::Some(patterns)
        } else {
            Self::All
        }
    }

    fn matches(&self, text: &str) -> bool {
        match self {
            Self::All => true,
            Self::Some(patterns) => patterns.iter().any(|p| p.is_match(text)),
        }
    }
}

/// Append-only debug log. Missing or unopenable file just silences logging.
struct Log {
    file: Option<File>,
}

impl Log {
    fn new(path: Option<PathBuf>) -> Self {
        let file = path.and_then(|path| match OpenOptions::new().create(true).append(true).open(&path)
        {
            Ok(file) => Some(file),
            Err(e) => {
                eprintln!("cannot open {}: {e}", path.display());
                None
            }
        });
        Self { file }
    }

    fn write(&mut self, tag: &str, message: &str) {
        let Some(file) = &mut self.file else {
            return;
        };
        let _ = writeln!(
            file,
            "[{}] [{tag}] {message}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        let _ = file.flush();
    }
}

fn run(
    receiver: Receiver<String>,
    enabled: Arc<AtomicBool>,
    model: Option<PathBuf>,
    tools_json: Option<String>,
    triggers: Triggers,
    log: Option<PathBuf>,
    notify: Arc<dyn Fn(ToolCallReport) + Send + Sync>,
) {
    let registry = Registry::new();
    let mut log = Log::new(log);
    let mut agent: Option<Agent> = None;
    for text in receiver {
        if !enabled.load(Relaxed) {
            continue;
        }
        log.write("heard", &text);
        if !triggers.matches(&text) {
            log.write("skip", "no trigger matched");
            continue;
        }
        let report = execute(
            &mut agent,
            model.as_deref(),
            tools_json.as_deref(),
            &registry,
            &text,
            &mut log,
        );
        notify(report);
    }
}

fn execute(
    agent: &mut Option<Agent>,
    model: Option<&Path>,
    tools_json: Option<&str>,
    registry: &Registry,
    text: &str,
    log: &mut Log,
) -> ToolCallReport {
    match execute_inner(agent, model, tools_json, registry, text, log) {
        Ok(calls) => ToolCallReport {
            text: text.to_string(),
            calls,
            error: None,
        },
        Err(error) => {
            let error = format!("{error:#}");
            log.write("error", &error);
            ToolCallReport {
                text: text.to_string(),
                calls: Vec::new(),
                error: Some(error),
            }
        }
    }
}

fn execute_inner(
    agent: &mut Option<Agent>,
    model: Option<&Path>,
    tools_json: Option<&str>,
    registry: &Registry,
    text: &str,
    log: &mut Log,
) -> Result<Vec<ToolCallRecord>> {
    let Some(model) = model else {
        bail!("no \"model_root\" in config.json, so needle3.cact cannot be found");
    };
    if !model.exists() {
        bail!(
            "Needle model not found at {} (put needle3.cact beside whistle.cact)",
            model.display()
        );
    }

    // The Cactus runtime has one conversation shared with the speech model. Hold this only for the
    // model turn, so a caption transcription cannot interleave with Needle's but a slow tool body
    // does not stall the captions.
    let raw = {
        let _guard = rustmeeter_core::cactus_call_lock();
        if agent.is_none() {
            log.write("load", &format!("{}", model.display()));
            let loaded =
                Agent::load(model).with_context(|| format!("cannot load {}", model.display()))?;
            loaded
                .init(None, tools_json, None)
                .context("cannot initialize Needle tools")?;
            *agent = Some(loaded);
        }
        let agent = agent.as_mut().expect("agent loaded above");
        // Captions are independent utterances, not a conversation: drop the previous turn so
        // Needle does not read this line as "follow-up, changes to ..." of whatever was said
        // before.
        agent.reset();
        agent
            .complete(Some(text), None, 256)
            .context("Needle completion failed")?
    };
    log.write("needle", &raw);

    let envelope: Value =
        serde_json::from_str(&raw).with_context(|| format!("Needle returned non-JSON: {raw}"))?;

    if let Some(suppressed) = envelope.get("suppressed_calls").and_then(Value::as_array)
        && !suppressed.is_empty()
    {
        log.write(
            "suppressed",
            &serde_json::to_string(suppressed).unwrap_or_default(),
        );
    }

    let calls = envelope
        .get("function_calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut records = Vec::with_capacity(calls.len());
    for call in calls {
        let name = call
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
        let result = registry
            .execute(&name, &arguments)
            .map_err(|e| format!("{e:#}"));
        match &result {
            Ok(value) => log.write(
                "call",
                &format!("{name} {arguments} -> {value}"),
            ),
            Err(e) => log.write("call", &format!("{name} {arguments} failed: {e}")),
        }
        records.push(ToolCallRecord {
            name,
            arguments,
            result,
        });
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::Triggers;
    use serde_json::{Value, json};

    fn switch_light() -> Value {
        json!({
            "name": "switch_light",
            "description": "Turn the light on or off, or set it to day or night mode.",
            "triggers": [
                "\\b(light|lamp)\\b.*\\b(on|off|day|night)\\b",
                "\\b(on|off|day|night)\\b.*\\b(light|lamp)\\b"
            ]
        })
    }

    #[test]
    fn triggers_gate_and_are_case_insensitive() {
        let triggers = Triggers::from_tools(&[switch_light()]);
        assert!(triggers.matches("turn on the light"));
        assert!(triggers.matches("Night Light."));
        assert!(triggers.matches("lamp off"));
        // Commands with no light word, and mentions of light with no command, are ignored.
        assert!(!triggers.matches("The light."));
        assert!(!triggers.matches("Good night, so..."));
        assert!(!triggers.matches("What are you doing?"));
        assert!(!triggers.matches("Yes."));
    }

    #[test]
    fn tools_without_triggers_accept_every_caption() {
        let triggers = Triggers::from_tools(&[json!({"name": "open_browser"})]);
        assert!(triggers.matches("anything at all"));
    }

    #[test]
    fn a_bad_regex_is_skipped_instead_of_rejected() {
        let triggers = Triggers::from_tools(&[json!({
            "name": "broken",
            "triggers": ["(", "\\blight\\b"]
        })]);
        assert!(triggers.matches("light"));
        assert!(!triggers.matches("dark"));
    }
}
