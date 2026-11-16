//! CLI frontend. Uses only the public `rustmeeter` API (Mixer / Params / device helpers),
//! so a GUI can be built the same way.

use anyhow::{Result, anyhow, bail};
use rustmeeter_core::*;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

const USAGE: &str = "\
rustmeeter list
rustmeeter run [--bus-a DEVICE] [--bus-b DEVICE] [--mic DEVICE[:AB]]... [--loopback DEVICE[:AB]]...
              [--captions [FILE]] [--whistle whistle.cact] [--caption-strip N]
              [--caption-lang en] [--caption-threads N]

  DEVICE   case-insensitive name or substring, or 'default'. Quote names with spaces.
  :AB      routes, e.g. ':A', ':B', ':AB'. Defaults: --mic -> B, --loopback -> A.
  --bus-a  defaults to the default output device. --bus-b defaults to none.
  --captions [FILE]    start live captions right away (log defaults to captions.log).
                       You can also toggle them at any time with `captions on|off` at the prompt.
                       Captions are a passive copy of the raw strip: Discord/OBS are not affected.
  --whistle PATH       whistle.cact model (default: cactus/whistle.cact). Loaded on first use.
  --caption-strip N    which strip to caption (1-based; default: first --mic).
  --caption-lang CODE  force a language (en, de, fr, es, it, nl, pl); default: auto-detect.
  --caption-threads N  limit CPU threads used by the transcriber.

Example:
  rustmeeter run --bus-a Headphones --bus-b \"CABLE Input\" --loopback default --mic Microphone
";

const HELP: &str = "\
commands (strips are numbered 1.., buses are 'a' and 'b'):
  status | s                      show everything
  levels | l                      show meters once
  gain <strip|a|b> <db>           e.g. gain 2 -6
  mute <strip|a|b> [on|off]       toggles if on/off omitted
  eq <strip> <low|mid|high> <db>  -12..12
  route <strip> <a|b> <on|off>
  add <mic|loop> <device>[:AB]    add a strip (restarts audio)
  remove <strip>                  remove a strip (restarts audio)
  bus <a|b> <device|none>         change a bus output (restarts audio)
  captions on [strip]             start live captions (loads the model on first use)
  captions off                    stop captions;  `captions` alone shows status
  help | ?   quit | q
";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("list") => cmd_list(),
        Some("run") => cmd_run(&args[1..]),
        _ => {
            print!("{USAGE}");
            Ok(())
        }
    }
}

fn cmd_list() -> Result<()> {
    let def_out = default_output_name();
    let def_in = default_input_name();
    println!("Outputs (usable as --bus-a/--bus-b, or as --loopback sources):");
    for o in list_outputs() {
        println!(
            "  {}{}",
            o,
            if Some(&o) == def_out.as_ref() {
                "   (default)"
            } else {
                ""
            }
        );
    }
    println!("Inputs (usable as --mic):");
    for i in list_inputs().into_iter().filter(|i| !i.loopback) {
        println!(
            "  {}{}",
            i.name,
            if Some(&i.name) == def_in.as_ref() {
                "   (default)"
            } else {
                ""
            }
        );
    }
    Ok(())
}

// ---------- argument parsing / device matching ----------

struct SourceArg {
    query: String,
    loopback: bool,
    routes: [bool; NUM_BUSES],
}

fn parse_routes(s: &str) -> Option<[bool; NUM_BUSES]> {
    if s.is_empty() || s.len() > 2 {
        return None;
    }
    let mut r = [false; NUM_BUSES];
    for c in s.chars() {
        match c.to_ascii_uppercase() {
            'A' => r[0] = true,
            'B' => r[1] = true,
            _ => return None,
        }
    }
    Some(r)
}

fn parse_source(arg: &str, loopback: bool) -> SourceArg {
    let default_routes = if loopback {
        [true, false]
    } else {
        [false, true]
    };
    match arg.rsplit_once(':') {
        Some((name, r)) if parse_routes(r).is_some() => SourceArg {
            query: name.to_string(),
            loopback,
            routes: parse_routes(r).unwrap(),
        },
        _ => SourceArg {
            query: arg.to_string(),
            loopback,
            routes: default_routes,
        },
    }
}

fn pick<'a>(names: impl Iterator<Item = &'a String> + Clone, query: &str) -> Option<String> {
    let q = query.to_lowercase();
    names
        .clone()
        .find(|n| n.to_lowercase() == q)
        .or_else(|| names.clone().find(|n| n.to_lowercase().contains(&q)))
        .cloned()
}

fn resolve_output(query: &str) -> Result<String> {
    if query.eq_ignore_ascii_case("default") {
        return default_output_name().ok_or_else(|| anyhow!("no default output device"));
    }
    let outs = list_outputs();
    pick(outs.iter(), query)
        .ok_or_else(|| anyhow!("no output device matching '{query}' (try `rustmeeter list`)"))
}

fn resolve_source(a: &SourceArg) -> Result<InputSource> {
    if a.query.eq_ignore_ascii_case("default") {
        let name = if a.loopback {
            default_output_name()
        } else {
            default_input_name()
        }
        .ok_or_else(|| anyhow!("no default device"))?;
        return Ok(InputSource {
            name,
            loopback: a.loopback,
        });
    }
    let names: Vec<String> = list_inputs()
        .into_iter()
        .filter(|i| i.loopback == a.loopback)
        .map(|i| i.name)
        .collect();
    let name = pick(names.iter(), &a.query).ok_or_else(|| {
        anyhow!(
            "no {} device matching '{}' (try `rustmeeter list`)",
            if a.loopback { "loopback" } else { "input" },
            a.query
        )
    })?;
    Ok(InputSource {
        name,
        loopback: a.loopback,
    })
}

fn flag_value<'a>(it: &mut std::slice::Iter<'a, String>, flag: &str) -> Result<&'a String> {
    it.next().ok_or_else(|| anyhow!("{flag} needs a value"))
}

// ---------- run ----------

fn cmd_run(args: &[String]) -> Result<()> {
    let mut bus_q: [Option<String>; NUM_BUSES] = [None, None];
    let mut sources: Vec<SourceArg> = vec![];
    let mut captions_requested = false;
    let mut captions_path: Option<String> = None;
    let mut caption_strip: Option<usize> = None;
    let mut whistle_path: Option<String> = None;
    let mut caption_lang: Option<String> = None;
    let mut caption_threads: Option<u32> = None;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bus-a" => bus_q[0] = Some(flag_value(&mut it, a)?.clone()),
            "--bus-b" => bus_q[1] = Some(flag_value(&mut it, a)?.clone()),
            "--mic" => sources.push(parse_source(flag_value(&mut it, a)?, false)),
            "--loopback" => sources.push(parse_source(flag_value(&mut it, a)?, true)),
            "--captions" => {
                captions_requested = true;
                if let Some(v) = it.as_slice().first().filter(|v| !v.starts_with("--")) {
                    captions_path = Some((*v).clone());
                    it.next();
                }
            }
            "--caption-strip" => caption_strip = Some(flag_value(&mut it, a)?.parse()?),
            "--whistle" => whistle_path = Some(flag_value(&mut it, a)?.clone()),
            "--caption-lang" => caption_lang = Some(flag_value(&mut it, a)?.clone()),
            "--caption-threads" => caption_threads = Some(flag_value(&mut it, a)?.parse()?),
            other => bail!("unknown argument '{other}'\n\n{USAGE}"),
        }
    }
    if sources.is_empty() {
        bail!("need at least one --mic or --loopback\n\n{USAGE}");
    }

    let mut mixer = Mixer::new();
    mixer.set_bus_device(
        0,
        Some(resolve_output(bus_q[0].as_deref().unwrap_or("default"))?),
    );
    if let Some(q) = &bus_q[1] {
        mixer.set_bus_device(1, Some(resolve_output(q)?));
    }
    for s in &sources {
        mixer.add_strip(resolve_source(s)?, s.routes);
    }
    mixer.start()?;

    // Captions can be (re)started at any time from the prompt; --captions just starts them now.
    let mut caps = CaptionController::new(
        PathBuf::from(whistle_path.unwrap_or_else(|| "cactus/whistle.cact".into())),
        PathBuf::from(captions_path.unwrap_or_else(|| "captions.log".into())),
        caption_lang,
        caption_threads,
        Arc::new(|c: &Caption| {
            println!("\n[{}] {}: {}", c.time.format("%H:%M:%S"), c.label, c.text)
        }),
    );
    if captions_requested {
        captions_on(&mut caps, &mixer, caption_strip)?;
    }

    println!("Running. Type 'help' for commands.\n");
    print_status(&mixer);
    repl(&mut mixer, &mut caps)
}

fn captions_on(caps: &mut CaptionController, mixer: &Mixer, strip: Option<usize>) -> Result<()> {
    if !caps.model_loaded() {
        println!("Loading Whistle from {} ...", caps.model_path().display());
    }
    let n = caps.on(mixer, strip)?;
    println!("Captions ON for strip {n} -> {}", caps.log_path().display());
    Ok(())
}

fn captions_off(caps: &mut CaptionController) {
    if caps.off() {
        println!("Captions OFF");
    } else {
        println!("captions are not running");
    }
}

fn captions_status(caps: &CaptionController) {
    match caps.active_strip() {
        Some(n) => println!("Captions: ON for strip {n} -> {}", caps.log_path().display()),
        None => println!(
            "Captions: OFF (model {}, {})",
            caps.model_path().display(),
            if caps.model_loaded() { "loaded" } else { "not loaded yet" }
        ),
    }
}

fn repl(mixer: &mut Mixer, caps: &mut CaptionController) -> Result<()> {
    let stdin = io::stdin();
    loop {
        print!("> ");
        io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break; // EOF
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        match handle(mixer, caps, &parts) {
            Ok(true) => break,
            Ok(false) => {}
            Err(e) => println!("error: {e}"),
        }
    }
    Ok(())
}

// ---------- command handling ----------

fn bus_index(t: &str) -> Option<usize> {
    match t.to_ascii_lowercase().as_str() {
        "a" => Some(0),
        "b" => Some(1),
        _ => None,
    }
}

fn strip_params(m: &Mixer, t: &str) -> Result<Arc<Params>> {
    let n: usize = t
        .parse()
        .map_err(|_| anyhow!("'{t}' is not a strip number"))?;
    m.strips()
        .get(n.wrapping_sub(1))
        .map(|s| s.params.clone())
        .ok_or_else(|| anyhow!("no strip {n}"))
}

/// A strip number or a bus letter.
fn any_params(m: &Mixer, t: &str) -> Result<Arc<Params>> {
    match bus_index(t) {
        Some(b) => Ok(m.bus(b).params.clone()),
        None => strip_params(m, t),
    }
}

fn parse_on_off(s: &str) -> Result<bool> {
    match s {
        "on" | "1" | "true" => Ok(true),
        "off" | "0" | "false" => Ok(false),
        _ => bail!("expected on or off"),
    }
}

/// Returns Ok(true) to quit.
fn handle(m: &mut Mixer, caps: &mut CaptionController, p: &[&str]) -> Result<bool> {
    match p[0] {
        "quit" | "q" | "exit" => return Ok(true),
        "help" | "?" => print!("{HELP}"),
        "status" | "s" => print_status(m),
        "levels" | "l" => print_levels(m),
        "captions" | "cap" => match p.get(1).copied() {
            Some("on") => {
                let strip = p.get(2).map(|v| v.parse::<usize>()).transpose()?;
                captions_on(caps, m, strip)?;
            }
            Some("off") => captions_off(caps),
            None | Some("status") => captions_status(caps),
            _ => bail!("usage: captions on [strip] | captions off | captions"),
        },
        "gain" => {
            if p.len() != 3 {
                bail!("usage: gain <strip|a|b> <db>");
            }
            any_params(m, p[1])?.set_gain_db(p[2].parse()?);
        }
        "mute" => {
            if p.len() < 2 {
                bail!("usage: mute <strip|a|b> [on|off]");
            }
            let t = any_params(m, p[1])?;
            let new = match p.get(2) {
                Some(v) => parse_on_off(v)?,
                None => !t.muted(),
            };
            t.set_muted(new);
        }
        "eq" => {
            if p.len() != 4 {
                bail!("usage: eq <strip> <low|mid|high> <db>");
            }
            let t = strip_params(m, p[1])?;
            let band = EQ_BAND_NAMES
                .iter()
                .position(|b| *b == p[2])
                .ok_or_else(|| anyhow!("band must be low, mid or high"))?;
            t.set_eq_db(band, p[3].parse()?);
        }
        "route" => {
            if p.len() != 4 {
                bail!("usage: route <strip> <a|b> <on|off>");
            }
            let t = strip_params(m, p[1])?;
            let b = bus_index(p[2]).ok_or_else(|| anyhow!("bus must be a or b"))?;
            t.set_routed(b, parse_on_off(p[3])?);
        }
        "add" => {
            if p.len() < 3 {
                bail!("usage: add <mic|loop> <device>[:AB]");
            }
            let loopback = match p[1] {
                "mic" => false,
                "loop" | "loopback" => true,
                _ => bail!("expected mic or loop"),
            };
            let arg = parse_source(&p[2..].join(" "), loopback);
            m.add_strip(resolve_source(&arg)?, arg.routes);
            m.apply()?;
            print_status(m);
        }
        "remove" => {
            if p.len() != 2 {
                bail!("usage: remove <strip>");
            }
            let n: usize = p[1].parse()?;
            let id = m
                .strips()
                .get(n.wrapping_sub(1))
                .map(|s| s.id)
                .ok_or_else(|| anyhow!("no strip {n}"))?;
            m.remove_strip(id);
            m.apply()?;
            print_status(m);
        }
        "bus" => {
            if p.len() < 3 {
                bail!("usage: bus <a|b> <device|none>");
            }
            let b = bus_index(p[1]).ok_or_else(|| anyhow!("bus must be a or b"))?;
            let dev = p[2..].join(" ");
            if dev.eq_ignore_ascii_case("none") {
                m.set_bus_device(b, None);
            } else {
                m.set_bus_device(b, Some(resolve_output(&dev)?));
            }
            m.apply()?;
            print_status(m);
        }
        other => bail!("unknown command '{other}' (try 'help')"),
    }
    Ok(false)
}

// ---------- output ----------

fn db_str(db: f32) -> String {
    if db <= -89.0 {
        "  -inf".to_string()
    } else {
        format!("{db:+6.1}")
    }
}

fn print_status(m: &Mixer) {
    for b in 0..NUM_BUSES {
        let bus = m.bus(b);
        println!(
            "Bus {}: {:<40} gain {} dB{}  level {} dB",
            if b == 0 { "A" } else { "B" },
            bus.device.as_deref().unwrap_or("(none)"),
            db_str(bus.params.gain_db()),
            if bus.params.muted() { " [MUTED]" } else { "" },
            db_str(bus.params.level_db()),
        );
    }
    for (i, s) in m.strips().iter().enumerate() {
        let p = &s.params;
        println!(
            "[{}] {:<44} ->{}{}  gain {} dB{}  eq L{:+.0} M{:+.0} H{:+.0}  level {} dB",
            i + 1,
            s.source.label(),
            if p.routed(0) { "A" } else { "-" },
            if p.routed(1) { "B" } else { "-" },
            db_str(p.gain_db()),
            if p.muted() { " [MUTED]" } else { "" },
            p.eq_db(0),
            p.eq_db(1),
            p.eq_db(2),
            db_str(p.level_db()),
        );
    }
}

fn print_levels(m: &Mixer) {
    let bar = |db: f32| {
        let n = (((db + 60.0) / 60.0).clamp(0.0, 1.0) * 30.0) as usize;
        format!("{:<30}", "#".repeat(n))
    };
    for (i, s) in m.strips().iter().enumerate() {
        println!(
            "[{}] |{}| {} dB",
            i + 1,
            bar(s.params.level_db()),
            db_str(s.params.level_db())
        );
    }
    for b in 0..NUM_BUSES {
        let db = m.bus(b).params.level_db();
        println!(
            " {} |{}| {} dB",
            if b == 0 { "A" } else { "B" },
            bar(db),
            db_str(db)
        );
    }
}
