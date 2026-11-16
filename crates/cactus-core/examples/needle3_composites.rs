//! Needle 3 tool calling: composite parameter types (array, object).
//!
//! Run:
//!   cargo run --release --example needle3_composites --features cactus

use cactus_core::Agent;
use serde_json::Value;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let model = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cactus").join("needle3.cact");
    if !model.exists() {
        anyhow::bail!("model not found at {}", model.display());
    }

    let tools = r#"[
      {
        "name": "add_tags",
        "description": "Add tags to a project.",
        "triggers": ["\\btag\\b"],
        "parameters": {
          "type": "object",
          "properties": {
            "project": {"type": "string", "description": "Project name"},
            "tags": {
              "type": "array",
              "description": "List of tag strings",
              "items": {"type": "string"}
            }
          },
          "required": ["project", "tags"]
        }
      },
      {
        "name": "set_location",
        "description": "Set a location object.",
        "triggers": ["\\blocation\\b"],
        "parameters": {
          "type": "object",
          "properties": {
            "location": {
              "type": "object",
              "description": "Location with city and country",
              "properties": {
                "city": {"type": "string", "description": "City name"},
                "country": {"type": "string", "description": "Country name"}
              },
              "required": ["city", "country"]
            }
          },
          "required": ["location"]
        }
      }
    ]"#;

    let mut agent = Agent::load(&model)?;
    agent.init(Some("date: 2026-07-21 Tue 14:30; locale: en-US"), Some(tools), None)?;

    let requests = [
        "add tags rust, audio, ai to project Atlas",
        "tag project Beacon with web and ui",
        "set location city Berlin country Germany",
        "location: Paris, France",
    ];

    for req in requests {
        println!("\n> {req}");
        let raw = agent.complete(Some(req), None, 128)?;
        let v: Value = serde_json::from_str(&raw)?;
        let calls = v["function_calls"].as_array().cloned().unwrap_or_default();
        if let Some(c) = calls.first() {
            println!("  call: {} {}", c["name"], c["arguments"]);
        } else {
            println!("  (no call)");
        }
        println!("  confidence: {}", v["confidence"]);
        agent.reset();
    }
    Ok(())
}

