//! Needle 3 tool calling that drives the OS: open a browser at a URL, toggle a "light".
//!
//! Run:
//!   cargo run --release --example needle3_automation --features cactus
//!
//! The model (needle3.cact) parses a natural-language request into a tool call; this
//! binary then executes it. Pass `--dry-run` to print the actions instead of performing them.

use cactus_core::Agent;
use serde_json::Value;
use std::env;
use std::process::Command;

const TOOLS: &str = r#"[
  {
    "name": "switch_light",
    "description": "Turn the light on or off, or set it to day or night mode.",
    "triggers": ["\\blight\\b"],
    "parameters": {
      "type": "object",
      "properties": {
        "command": {
          "type": "string",
          "description": "One of on, off, day, night.",
          "enum": ["on", "off", "day", "night"]
        }
      },
      "required": ["command"]
    }
  },
  {
    "name": "open_browser",
    "description": "Open the Helium browser at a given URL.",
    "triggers": ["\\b(browser|youtube|url|go to)\\b"],
    "parameters": {
      "type": "object",
      "properties": {
        "url": {"type": "string", "description": "Full URL, e.g. https://youtube.com"}
      },
      "required": ["url"]
    }
  }
]"#;

fn dry_run() -> bool {
    env::args().any(|a| a == "--dry-run")
}

fn open_browser(url: &str) -> String {
    let url = if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("https://{}", url.trim_start_matches('/'))
    };
    if dry_run() {
        return format!("[dry-run] would open Helium browser at: {url}");
    }
    let candidates = [
        "helium",
        r"C:\Program Files\Helium\helium.exe",
        r"C:\Program Files (x86)\Helium\helium.exe",
        r"C:\Users\jmkl\AppData\Local\Helium\helium.exe",
    ];
    let mut launched = false;
    for c in candidates {
        if Command::new(c).arg(&url).spawn().is_ok() {
            launched = true;
            break;
        }
    }
    if !launched {
        let _ = Command::new("cmd").args(["/c", "start", "", &url]).spawn();
        return format!("Helium not found; opened {url} with the default browser");
    }
    format!("opened Helium browser at: {url}")
}

fn switch_light(command: &str) -> String {
    match command {
        "day" => {
            if dry_run() { return "[dry-run] would switch the light to day".to_string(); }
            println!("switch to day");
            "light switched to day".to_string()
        }
        "night" => {
            if dry_run() { return "[dry-run] would switch the light to night".to_string(); }
            println!("switch to night");
            "light switched to night".to_string()
        }
        "on" => {
            if dry_run() { return "[dry-run] would turn on the light".to_string(); }
            println!("switch to on");
            "light turned on".to_string()
        }
        "off" => {
            if dry_run() { return "[dry-run] would turn off the light".to_string(); }
            println!("switch to off");
            "light turned off".to_string()
        }
        other => format!("unknown light command: {other}"),
    }
}

fn execute(name: &str, args: &Value) -> String {
    match name {
        "switch_light" => {
            let cmd = args["command"].as_str().unwrap_or("");
            switch_light(cmd)
        }
        "open_browser" => {
            let url = args["url"].as_str().unwrap_or("");
            open_browser(url)
        }
        other => format!("unknown tool: {other}"),
    }
}

fn main() -> anyhow::Result<()> {
    let model = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cactus").join("needle3.cact");
    if !model.exists() {
        anyhow::bail!("model not found at {}", model.display());
    }

    let mut agent = Agent::load(&model)?;
    agent.init(Some("date: 2026-07-21 Tue 14:30; locale: en-US"), Some(TOOLS), None)?;

    let requests = [
        "turn on the light",
        "turn off the light",
        "switch the light to day",
        "make it night",
        "open the browser and go to youtube.com",
    ];

    for req in requests {
        println!("\n> {req}");
        let raw = agent.complete(Some(req), None, 64)?;
        let v: Value = serde_json::from_str(&raw)?;
        let mut calls = v["function_calls"].as_array().cloned().unwrap_or_default();
        let suppressed = v["suppressed_calls"].as_array().cloned().unwrap_or_default();
        if calls.is_empty() && !suppressed.is_empty() {
            println!("  (suppressed, low confidence - executing anyway in demo)");
            calls = suppressed;
        }
        if calls.is_empty() {
            println!("  (no call - refused)");
            agent.reset();
            continue;
        }
        // Take only the first call: the model sometimes emits a duplicate.
        let c = &calls[0];
        let name = c["name"].as_str().unwrap_or("?");
        let args = &c["arguments"];
        println!("  -> {} {}  (confidence {})", name, args, v["confidence"]);
        let result = execute(name, args);
        println!("     result: {}", result);
        println!("  confidence: {}", v["confidence"]);
        agent.reset(); // keep each request independent
    }

    agent.reset();
    println!("\ndone");
    Ok(())
}

