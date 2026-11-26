//! Local IPC for external scripts (Python, Node, a shell, another Rust program).
//!
//! Transport: TCP on `127.0.0.1`, one JSON object per line. This is intentionally boring: every
//! language has a socket and a JSON parser, and the loopback binding means nothing is exposed to
//! the network.
//!
//! Request:  `{"id": 1, "method": "list_devices", "params": {}}`
//! Response: `{"id": 1, "ok": true, "result": {...}}` or `{"id": 1, "ok": false, "error": "..."}`
//!
//! `Server` does the socket work on background threads; the frontend (the GUI or the CLI) calls
//! [`Server::poll`] from the thread that owns the [`Service`], because the `Mixer` owns cpal
//! streams and must not move between threads. `poll` executes the requests and writes the replies
//! back, so a client sees a normal synchronous call while the audio engine stays on its thread.
//!
//! Methods are listed in [`dispatch`]; the shape of a `status` reply matches `Mixer::snapshot()`.
//! `toolcall` and `transcribe` block the poller for as long as the model takes — call them from a
//! client that is happy to wait, and keep the GUI's IPC tick in mind when extending them.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::mixer::{MixerView, StripId};
use crate::params::{MAX_DB, MIN_DB, NUM_BUSES};
use crate::service::{Service, bus_name};

/// The default loopback port. Overridable by the frontend.
pub const DEFAULT_PORT: u16 = 7590;

struct Job {
    line: String,
    reply: mpsc::Sender<String>,
}

struct Inner {
    jobs: Mutex<Vec<Job>>,
    shutdown: AtomicBool,
}

/// A running IPC listener. Drop it to close the port and stop its threads.
pub struct Server {
    inner: Arc<Inner>,
    addr: SocketAddr,
}

impl Server {
    /// Bind `127.0.0.1:port` and start accepting clients in the background.
    pub fn bind(port: u16) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .with_context(|| format!("cannot bind 127.0.0.1:{port}"))?;
        let addr = listener.local_addr()?;
        let inner = Arc::new(Inner {
            jobs: Mutex::new(Vec::new()),
            shutdown: AtomicBool::new(false),
        });

        let accept_inner = inner.clone();
        thread::Builder::new()
            .name("rustmeeter-ipc-accept".to_string())
            .spawn(move || accept_loop(listener, accept_inner))
            .context("cannot start IPC accept thread")?;

        Ok(Self { inner, addr })
    }

    /// The address actually bound (useful when port 0 was requested in tests).
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Execute every request that arrived since the last call, writing each reply back to its
    /// client. Returns how many were handled. Call this from the thread that owns the `Service`.
    pub fn poll(&self, mut handler: impl FnMut(&str) -> String) -> usize {
        let jobs: Vec<Job> = {
            let mut queue = self.inner.jobs.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *queue)
        };
        let count = jobs.len();
        for job in jobs {
            let response = handler(&job.line);
            let _ = job.reply.send(response);
        }
        count
    }

    fn shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::Relaxed);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn accept_loop(listener: TcpListener, inner: Arc<Inner>) {
    // Non-blocking accept keeps the thread responsive to `shutdown`; a 25 ms poll is far below any
    // human-noticeable latency and costs nothing while idle.
    let _ = listener.set_nonblocking(true);
    while !inner.shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let connection = inner.clone();
                let _ = thread::Builder::new()
                    .name("rustmeeter-ipc-client".to_string())
                    .spawn(move || serve_connection(stream, connection));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(_) => thread::sleep(Duration::from_millis(25)),
        }
    }
}

fn serve_connection(stream: TcpStream, inner: Arc<Inner>) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    let reader_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = BufReader::new(reader_stream);
    let mut writer = stream;
    let mut line = String::new();

    loop {
        if inner.shutdown.load(Ordering::Relaxed) {
            break;
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // client closed
            Ok(_) => {
                let request = line.trim_end_matches(['\r', '\n']).to_string();
                if request.is_empty() {
                    continue;
                }
                let (tx, rx) = mpsc::channel();
                {
                    let mut queue = inner.jobs.lock().unwrap_or_else(|e| e.into_inner());
                    queue.push(Job {
                        line: request,
                        reply: tx,
                    });
                }
                // Wait for the owner thread to execute it. If the server is shutting down the
                // channel disappears and the connection ends.
                match rx.recv() {
                    Ok(response) => {
                        if writer.write_all(response.as_bytes()).is_err()
                            || writer.write_all(b"\n").is_err()
                            || writer.flush().is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => break,
        }
    }
}

/// Parse one request line and execute it against `service`, returning the JSON reply.
pub fn dispatch(service: &mut Service, line: &str) -> String {
    let request: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(e) => {
            return json!({"id": Value::Null, "ok": false, "error": format!("bad JSON: {e}")})
                .to_string();
        }
    };

    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = request
        .get("params")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));

    match handle(service, method, &params) {
        Ok(result) => json!({"id": id, "ok": true, "result": result}).to_string(),
        Err(e) => json!({"id": id, "ok": false, "error": format!("{e:#}")}).to_string(),
    }
}

fn handle(service: &mut Service, method: &str, params: &Value) -> Result<Value> {
    match method {
        "ping" => Ok(json!({
            "pong": true,
            "version": env!("CARGO_PKG_VERSION"),
        })),

        "list_devices" | "list_device" => {
            let inputs: Vec<Value> = crate::devices::list_inputs()
                .into_iter()
                .map(|s| json!({"name": s.name, "label": s.label(), "loopback": s.loopback}))
                .collect();
            Ok(json!({
                "inputs": inputs,
                "outputs": crate::devices::list_outputs(),
                "default_input": crate::devices::default_input_name(),
                "default_output": crate::devices::default_output_name(),
            }))
        }

        "status" | "snapshot" => Ok(snapshot_json(&service.snapshot())),

        "enable_mixer" | "enablemixer" | "start" => {
            service.start()?;
            Ok(json!({"running": true}))
        }
        "disable_mixer" | "disablemixer" | "stop" => {
            service.stop();
            Ok(json!({"running": false}))
        }

        "add_strip" => {
            let name = str_param(params, "name")?.to_string();
            let loopback = params
                .get("loopback")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let routes = routes_param(params, loopback)?;
            let source = crate::devices::InputSource { name, loopback };
            let id = service.add_strip(source, routes);
            service.apply()?;
            Ok(json!({"id": id}))
        }

        "remove_strip" => {
            let id = strip_id(service, params)?;
            let removed = service.remove_strip(id);
            service.apply()?;
            Ok(json!({"removed": removed}))
        }

        "set_bus" => {
            let bus = bus_param(params)?;
            let device = match params.get("device") {
                Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
                _ => None,
            };
            service.set_bus_device(bus, device.clone())?;
            Ok(json!({"bus": bus, "device": device}))
        }

        "set_gain" => {
            let db = number_param(params, "db")?.clamp(MIN_DB, MAX_DB);
            let params_handle = target_params(service, params)?;
            params_handle.set_gain_db(db);
            Ok(json!({"db": db}))
        }

        "set_mute" => {
            let muted = params
                .get("muted")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let params_handle = target_params(service, params)?;
            params_handle.set_muted(muted);
            Ok(json!({"muted": muted}))
        }

        "set_route" => {
            let id = strip_id(service, params)?;
            let bus = bus_param(params)?;
            let on = bool_param(params, "on")?;
            let strip = service
                .mixer
                .strip(id)
                .with_context(|| format!("no strip {id}"))?;
            strip.params.set_routed(bus, on);
            Ok(json!({"strip": id, "bus": bus, "on": on}))
        }

        "set_eq" => {
            let id = strip_id(service, params)?;
            let eq: crate::eq::EqSettings = serde_json::from_value(
                params.get("eq").cloned().context("missing \"eq\"")?,
            )
            .context("invalid EQ settings")?;
            let strip = service
                .mixer
                .strip(id)
                .with_context(|| format!("no strip {id}"))?;
            strip.params.set_eq(eq);
            Ok(json!({"strip": id}))
        }

        "captions_on" => {
            let strip = params.get("strip").and_then(Value::as_u64).map(|n| n as usize);
            let n = service.captions_on(strip)?;
            Ok(json!({"strip": n}))
        }
        "captions_off" => Ok(json!({"was_running": service.captions_off()})),

        "set_toolcall" => {
            let enabled = bool_param(params, "enabled")?;
            service.set_toolcall_enabled(enabled)?;
            Ok(json!({"enabled": enabled}))
        }

        "toolcall" => {
            let text = str_param(params, "text")?;
            let report = service.toolcall(text);
            Ok(json!({
                "text": report.text,
                "calls": report.calls.iter().map(|call| json!({
                    "name": call.name,
                    "arguments": call.arguments,
                    "result": match &call.result {
                        Ok(value) => json!({"ok": true, "value": value}),
                        Err(e) => json!({"ok": false, "error": e}),
                    },
                })).collect::<Vec<_>>(),
                "error": report.error,
            }))
        }

        "transcribe" => {
            let path = std::path::PathBuf::from(str_param(params, "path")?);
            let language = params.get("language").and_then(Value::as_str);
            let text = service.transcribe(&path, language)?;
            Ok(json!({"text": text}))
        }

        "save" => {
            service.flush_settings().map_err(anyhow::Error::msg)?;
            Ok(json!({"saved": true}))
        }

        // Stop audio before the host loop exits. The host decides what "shutdown" means for its
        // own lifetime; the CLI uses it to leave `serve`.
        "shutdown" | "quit" => {
            service.stop();
            Ok(json!({"shutting_down": true}))
        }

        other => {
            bail!(
                "unknown method \"{other}\" (try ping, list_devices, status, enable_mixer, \
                 disable_mixer, set_gain, set_mute, set_bus, captions_on, toolcall, transcribe)"
            )
        }
    }
}

fn snapshot_json(view: &MixerView) -> Value {
    json!({
        "running": view.running,
        "strips": view.strips.iter().map(|s| json!({
            "id": s.id,
            "label": s.label,
            "name": s.name,
            "loopback": s.loopback,
            "fed_by": s.fed_by,
            "routes": s.routes,
            "gain_db": s.gain_db,
            "muted": s.muted,
            "eq": *s.eq,
            "level_db": s.params.level_db(),
        })).collect::<Vec<_>>(),
        "buses": view.buses.iter().enumerate().map(|(bus, b)| json!({
            "name": bus_name(bus),
            "device": b.device,
            "gain_db": b.gain_db,
            "muted": b.muted,
            "level_db": b.params.level_db(),
        })).collect::<Vec<_>>(),
    })
}

fn str_param<'a>(params: &'a Value, name: &str) -> Result<&'a str> {
    params
        .get(name)
        .and_then(Value::as_str)
        .with_context(|| format!("missing string \"{name}\""))
}

fn number_param(params: &Value, name: &str) -> Result<f32> {
    params
        .get(name)
        .and_then(Value::as_f64)
        .map(|n| n as f32)
        .with_context(|| format!("missing number \"{name}\""))
}

fn bool_param(params: &Value, name: &str) -> Result<bool> {
    params
        .get(name)
        .and_then(Value::as_bool)
        .with_context(|| format!("missing boolean \"{name}\""))
}

/// Bus given either as an index or as `"a"`/`"b"`.
fn bus_param(params: &Value) -> Result<usize> {
    let bus = match params.get("bus") {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(u64::MAX) as usize,
        Some(Value::String(s)) => match s.to_ascii_lowercase().as_str() {
            "a" => 0,
            "b" => 1,
            _ => usize::MAX,
        },
        _ => usize::MAX,
    };
    if bus < NUM_BUSES {
        Ok(bus)
    } else {
        bail!("bus must be 0, 1, \"a\" or \"b\"")
    }
}

/// Find a strip by `id` (stable) or by `strip` (1-based index, same numbering as the CLI).
fn strip_id(service: &Service, params: &Value) -> Result<StripId> {
    if let Some(id) = params.get("id").and_then(Value::as_u64) {
        return service
            .mixer
            .strip(id as StripId)
            .map(|s| s.id)
            .with_context(|| format!("no strip with id {id}"));
    }
    if let Some(index) = params.get("strip").and_then(Value::as_u64) {
        let index = index as usize;
        if index == 0 {
            bail!("strip numbers are 1-based");
        }
        return service
            .mixer
            .strips()
            .get(index - 1)
            .map(|s| s.id)
            .with_context(|| format!("no strip {index}"));
    }
    bail!("missing \"id\" or \"strip\"")
}

/// Controls for `set_gain`/`set_mute`: `{"bus": ...}` or `{"strip": ...}`.
fn target_params(service: &Service, params: &Value) -> Result<Arc<crate::params::Params>> {
    if params.get("bus").is_some() {
        let bus = bus_param(params)?;
        return Ok(service.mixer.bus(bus).params.clone());
    }
    let id = strip_id(service, params)?;
    service
        .mixer
        .strip(id)
        .map(|s| s.params.clone())
        .with_context(|| format!("no strip {id}"))
}

fn routes_param(params: &Value, loopback: bool) -> Result<[bool; NUM_BUSES]> {
    if let Some(values) = params.get("routes").and_then(Value::as_array) {
        if values.len() != NUM_BUSES {
            bail!("routes must have {NUM_BUSES} entries");
        }
        return Ok(std::array::from_fn(|bus| {
            values[bus].as_bool().unwrap_or(false)
        }));
    }
    Ok(if loopback { [true, false] } else { [false, true] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;
    use std::time::Instant;

    /// The transport is independent of `Service`: a client line is delivered to `poll` and the
    /// handler's reply comes back on the same connection.
    #[test]
    fn a_client_gets_the_polled_reply() {
        let server = Server::bind(0).unwrap();
        let addr = server.addr();
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream
                .write_all(b"{\"id\":1,\"method\":\"ping\"}\n")
                .unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).unwrap();
            line
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if server.poll(|line| format!("echo:{line}")) > 0 {
                break;
            }
            assert!(Instant::now() < deadline, "no request reached the poller");
            std::thread::sleep(Duration::from_millis(5));
        }

        let reply = client.join().unwrap();
        assert_eq!(reply.trim(), "echo:{\"id\":1,\"method\":\"ping\"}");
    }
}

