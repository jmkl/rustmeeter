//! Needle 3 tool calling: primitive parameter types (string, integer, number, boolean, enum).
//!
//! Run:
//!   cargo run --release --example needle3_primitives --features cactus

use cactus_core::Agent;
use serde_json::Value;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let model = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cactus").join("needle3.cact");
    if !model.exists() {
        anyhow::bail!("model not found at {}", model.display());
    }

    let tools = r#"[{
        "name": "create_user",
        "description": "Create a user with typed fields.",
        "parameters": {
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "User name"},
                "age": {"type": "integer", "description": "Age in years", "minimum": 0, "maximum": 120},
                "score": {"type": "number", "description": "Score value"},
                "active": {"type": "boolean", "description": "Whether the user is active"},
                "role": {"type": "string", "description": "Role: admin, editor, or viewer", "enum": ["admin", "editor", "viewer"]}
            },
            "required": ["name"]
        }
    }]"#;

    let mut agent = Agent::load(&model)?;
    agent.init(Some("date: 2026-07-21 Tue 14:30; locale: en-US"), Some(tools), None)?;

    let requests = [
        "create user Alice age 30 score 9.5 active true role admin",
        "create user Bob age 25 score 7.25 active false role viewer",
        "create user named Charlie with role editor",
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

