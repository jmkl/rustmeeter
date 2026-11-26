//! Tool implementations available to Needle 3.
//!
//! The names in `config.json`'s `tools` array are what the model sees. A tool only runs if a
//! matching handler is registered here, so adding a capability is two steps: describe it in the
//! config, then implement [`Tool`] and register it in [`super::tools::register`].

use anyhow::{Result, bail};
use serde_json::Value;
use std::collections::HashMap;

/// One executable tool. The schema lives in `config.json`; the code lives in an implementation of
/// this trait (see `toolcall/tools.rs`).
pub trait Tool: Send + Sync {
    /// Run the tool with the arguments Needle produced. Returning a JSON value keeps the result
    /// ready for a future follow-up turn or for logging.
    fn execute(&self, arguments: &Value) -> Result<Value>;
}

/// Name -> implementation. The registry is built once on the toolcall worker thread.
#[derive(Default)]
pub struct Registry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl Registry {
    pub fn new() -> Self {
        let mut registry = Self::default();
        super::tools::register(&mut registry);
        registry
    }

    /// Add a handler under the same name used in the `tools` schema.
    pub fn insert(&mut self, name: impl Into<String>, tool: impl Tool + 'static) {
        self.tools.insert(name.into(), Box::new(tool));
    }

    pub fn execute(&self, name: &str, arguments: &Value) -> Result<Value> {
        let Some(tool) = self.tools.get(name) else {
            bail!("no handler registered for tool \"{name}\"");
        };
        tool.execute(arguments)
    }
}
