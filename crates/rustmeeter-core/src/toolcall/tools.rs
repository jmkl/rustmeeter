//! Handlers for the tools described in `config.json`.
//!
//! These live in the core crate so the GUI, the CLI and the IPC server all execute the same
//! implementations. A frontend that wants its own tools can build a [`Registry`] and add them
//! there; the worker's own registry is built by [`register`].
//!
//! Nothing is registered yet. To add a tool, put its implementation in this folder and register it
//! in [`register`]:
//!
//! ```ignore
//! struct GetWeather;
//!
//! impl super::registry::Tool for GetWeather {
//!     fn execute(&self, arguments: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
//!         let city = arguments["city"].as_str().unwrap_or("?");
//!         Ok(serde_json::json!({ "city": city, "temp_c": 27, "sky": "clear" }))
//!     }
//! }
//!
//! pub(super) fn register(registry: &mut super::registry::Registry) {
//!     registry.insert("get_weather", GetWeather);
//! }
//! ```

use super::registry::{Registry, Tool};

struct WizLight;
impl Tool for WizLight {
    fn execute(&self, arguments: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
        if let Some(command) = arguments["command"].as_str() {
            println!("Command {command}");
            std::process::Command::new("tool").args(["wiz", command]).output().expect("Error");
        }
        Ok(serde_json::json!({ "ok":true}))
    }
}

pub(super) fn register(registry: &mut Registry) {
    registry.insert("switch_light", WizLight);
}
