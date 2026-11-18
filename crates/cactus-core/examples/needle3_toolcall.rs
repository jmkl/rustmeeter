//! Example: Needle 3 (tool calling) through the Rust wrapper in `src/cactus.rs`.
//!
//! Run with:
//!   cargo run --release --example needle3_toolcall --features cactus
//!
//! Requires `cactus/needle3.cact` (download from huggingface.co/Cactus-Compute/needle3).
//! The weather function below is a toy; swap in real logic as needed.

use cactus_core::Agent;
use serde_json::Value;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let model = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("cactus")
        .join("needle3.cact");
    if !model.exists() {
        anyhow::bail!(
            "model not found at {}\n\
             download it with: cactus download needle3 --out cactus  (or fetch from Hugging Face)",
            model.display()
        );
    }

    // Needle's compact tool schema.
    let tools = r#"[{
        "name": "get_weather",
        "description": "Get the current weather for a city.",
        "parameters": {
            "type": "object",
            "properties": {
                "city": {"type": "string", "description": "City name"}
            },
            "required": ["city"]
        }
    },{
        "name": "set_lights",
        "description": "Turn a room's lights on or off and set brightness.",
        "parameters": {
            "type": "object",
            "properties": {
                "room": {"type": "string"},
                "on": {"type": "boolean"},
                "brightness": {"type": "integer", "minimum": 0, "maximum": 100}
            },
            "required": ["room", "on"]
        }
    }]"#;

    let mut agent = Agent::load(&model)?;
    agent.init(
        Some("date: 2026-07-21 Tue 14:30; locale: en-US"),
        Some(tools),
        None,
    )?;

    let queries = [
        "What's the weather in Lagos?",
        "Dim the kitchen lights to 30 percent",
        "What is the capital of France?", // off-topic -> empty call (refusal)
    ];

    for q in queries {
        println!("\n> {q}");
        let raw = agent.complete(Some(q), None, 256)?;
        let v: Value = serde_json::from_str(&raw)?;
        let calls = v["function_calls"].as_array().cloned().unwrap_or_default();
        if calls.is_empty() {
            println!("  (no call - refusal / off-topic)");
            continue;
        }
        for c in calls {
            let name = c["name"].as_str().unwrap_or("?");
            let args = &c["arguments"];
            println!("  call: {} {}", name, args);
            // Execute the toy tool and feed the result back for a follow-up turn.
            let result = match name {
                "get_weather" => {
                    let city = args["city"].as_str().unwrap_or("?");
                    serde_json::json!({"city": city, "temp_c": 27, "sky": "clear"})
                }
                "set_lights" => {
                    let room = args["room"].as_str().unwrap_or("?");
                    let on = args["on"].as_bool().unwrap_or(false);
                    let brightness = args["brightness"].as_i64().unwrap_or(100);
                    serde_json::json!({"room": room, "on": on, "brightness": brightness})
                }
                _ => serde_json::json!({"error": "unknown tool"}),
            };
            println!("  tool result: {}", result);
            let followup = agent.complete(Some(&result.to_string()), None, 256)?;
            println!("  followup: {}", followup);
        }
        println!("  confidence: {}", v["confidence"]);
    }

    // Embedding demo.
    let v = agent.embed(Some("hello world"), None)?;
    println!("\nembedding dim = {}", v.len());

    agent.reset();
    println!("\ndone (conversation reset)");
    Ok(())
}
