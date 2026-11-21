//! The settings file: `%APPDATA%\rustmeeter\config.json`.
//!
//! Everything the GUI remembers between runs lives in one JSON document, so it can be read, diffed
//! and edited by hand. The shape is a flat map of sections. Per-strip data is keyed by the strip's
//! source label (`InputSource::label()`), which is the device name the user sees, prefixed with
//! `[Loopback] ` for output taps. Keying by device name rather than by strip index is deliberate:
//! strips are added and removed in any order, and settings belong to the *device*, not to slot 2.
//!
//! The rack itself is `strip_order`: the sources in the order their strips sit, so a session comes
//! back with the same strips in the same places. It is `null`/absent only in a file from before the
//! rack was recorded (or a first run); such a file is upgraded once by rebuilding the rack from the
//! devices named in `strips`, and a file with nothing there gets the first-run default of a single
//! microphone strip. `buses` records where each output bus plays and how loud it is, and is absent
//! for the same reason.
//!
//! Every field is optional and omitted when it holds its default, so a line only says what was
//! actually changed.
//!
//! ```json
//! {
//!   "model_root": "C:\\models\\cactus",
//!   "strip_order": [
//!     { "name": "Headset Microphone (MH752)", "loopback": false }
//!   ],
//!   "strips": {
//!     "Headset Microphone (MH752)": {
//!       "eq": {
//!         "bypass": false,
//!         "bands": [
//!           { "enabled": true, "kind": "low-shelf", "freq_hz": 120.0, "gain_db": -3.0, "q": 0.75 }
//!         ]
//!       },
//!       "gain_db": 2.5,
//!       "routes": [false, true]
//!     }
//!   },
//!   "buses": [
//!     { "device": "Speakers (Realtek(R) Audio)", "gain_db": -3.0 },
//!     { "device": "CABLE Input (VB-Audio Virtual Cable)", "muted": true }
//!   ],
//!   "toolcall_enabled": true,
//!   "tools": [
//!     {
//!       "name": "get_weather",
//!       "description": "Get the current weather for a city.",
//!       "parameters": {
//!         "type": "object",
//!         "properties": { "city": { "type": "string" } },
//!         "required": ["city"]
//!       }
//!     }
//!   ]
//! }
//! ```
//!
//! `tools` is passed to Needle 3 as its tool schema. A schema alone does not execute anything: the
//! matching handler must be registered under `src/toolcall/tools.rs`.
//!
//! Every caption is an independent turn: the worker resets Needle's conversation before each
//! completion. If any tool declares `triggers`, those regexes also gate the request, so a caption
//! that matches none of them is skipped without running the model. With no triggers declared, every
//! non-empty caption is sent, which is only appropriate for deliberate use.
//!
//! Older builds used a `key = value` file called `config`; it is read once to carry `model_root`
//! over, then left alone.

use anyhow::{Context, Result};
use rustmeeter_core::{EqSettings, InputSource, MAX_DB, MIN_DB, NUM_BUSES};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

const APP_ICON: &[u8] = include_bytes!("../assets/app.ico");
/// The settings document. JSON because it is meant to be read and edited by humans.
const SETTINGS_FILE: &str = "config.json";
/// The pre-JSON `key = value` file, read once for migration.
const LEGACY_FILE: &str = "config";

/// Per-source settings, kept even while no strip is using the device, so a device comes back the
/// way it was left when it is added again.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StripSettings {
    /// Parametric EQ, or `None` when the source has never been shaped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eq: Option<EqSettings>,
    /// Fader position in dB. Omitted at unity, which is also the default: a strip nobody touched
    /// has nothing to remember.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gain_db: Option<f32>,
    /// Mute switch. Only written when on.
    #[serde(skip_serializing_if = "is_false")]
    pub muted: bool,
    /// Which buses this strip feeds, or `None` while it uses the default for its kind
    /// (microphones -> B, loopback taps -> A).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routes: Option<[bool; NUM_BUSES]>,
}

impl StripSettings {
    /// Nothing worth a line in the file.
    pub fn is_empty(&self) -> bool {
        self.eq.is_none() && self.gain_db.is_none() && !self.muted && self.routes.is_none()
    }

    /// Clamp hand-edited values into their legal ranges.
    fn sanitize(&mut self) {
        if let Some(eq) = &mut self.eq {
            eq.sanitize();
        }
        if let Some(db) = &mut self.gain_db {
            *db = db.clamp(MIN_DB, MAX_DB);
        }
    }
}

/// One output bus as the user left it: which device it plays into, and how loud.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BusSettings {
    /// Playback device. Omitted while the bus points at nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// Fader position in dB. Omitted at unity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gain_db: Option<f32>,
    /// Mute switch. Only written when on.
    #[serde(skip_serializing_if = "is_false")]
    pub muted: bool,
}

impl BusSettings {
    /// Clamp hand-edited values into their legal ranges.
    fn sanitize(&mut self) {
        if let Some(db) = &mut self.gain_db {
            *db = db.clamp(MIN_DB, MAX_DB);
        }
    }
}

/// `skip_serializing_if` helper: `false` fields are not written.
fn is_false(flag: &bool) -> bool {
    !*flag
}

/// Everything the GUI remembers between runs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Directory holding `whistle.cact` (and, for tool calling, `needle3.cact`).
    pub model_root: Option<PathBuf>,
    /// The rack in order: which sources are strips and where each sits, so a session comes back the
    /// way it was left. `None` only while the file has no rack (a first run, or a file from an older
    /// build); `Some([])` is a rack the user deliberately emptied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strip_order: Option<Vec<InputSource>>,
    /// Per-source settings, keyed by `InputSource::label()`.
    pub strips: BTreeMap<String, StripSettings>,
    /// The output buses in bus order, or `None` while the file has none (a first run, or a file from
    /// an older build).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buses: Option<[BusSettings; NUM_BUSES]>,
    /// Whether the title bar checkbox is on. Tool calling still needs `needle3.cact` beside
    /// `whistle.cact` and at least one tool in `tools`.
    #[serde(skip_serializing_if = "is_false")]
    pub toolcall_enabled: bool,
    /// Needle tool schemas. Passed to the model verbatim; each `name` must also have a handler in
    /// `src/toolcall/tools.rs` before it can do anything. Declared `triggers` regexes also gate
    /// captions: text matching none of them is never sent to the model.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<serde_json::Value>,
}

impl Settings {
    /// The settings saved for this source, if it has any.
    pub fn strip(&self, label: &str) -> Option<&StripSettings> {
        self.strips.get(label)
    }

    /// Store (or clear) one source's settings. An entry with nothing left in it is dropped, so
    /// putting a device back to its defaults also removes its line from the file.
    pub fn set_strip(&mut self, label: &str, settings: StripSettings) {
        if settings.is_empty() {
            self.strips.remove(label);
        } else {
            self.strips.insert(label.to_string(), settings);
        }
    }

    /// Store one bus's settings. The array is created on first use, which is what makes the
    /// section appear in the file.
    pub fn set_bus(&mut self, bus: usize, settings: BusSettings) {
        self.buses
            .get_or_insert_with(|| std::array::from_fn(|_| BusSettings::default()))[bus] = settings;
    }
}

pub fn app_config_dir() -> Result<PathBuf> {
    let appdata = std::env::var("APPDATA").context("APPDATA environment variable not found")?;
    let dir = PathBuf::from(appdata).join("rustmeeter");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub struct Conf {
    pub settings: Settings,
    pub root: PathBuf,
    pub log: PathBuf,
    pub icon: String,
    /// Something the user should know about the file itself: unreadable settings were moved
    /// aside, an unparsable `model_root`, ... `None` on a clean load.
    pub warning: Option<String>,
}

impl Conf {
    /// Read the settings file, creating it on first run. An unreadable file never stops the app
    /// from starting: it is moved aside and the defaults are used.
    pub fn load() -> Result<Self> {
        let root = app_config_dir()?;
        let icon = root.join("app.ico");
        if !icon.exists() {
            std::fs::write(&icon, APP_ICON)?;
        }
        let caption_log = root.join("captions.log");
        _ = OpenOptions::new()
            .write(true)
            .create(true)
            .open(&caption_log)?;

        let path = root.join(SETTINGS_FILE);
        let mut warning = None;
        // Only a file that does not exist yet (or that had to be moved aside) is written here.
        // A file that loaded fine is left byte-for-byte alone until something actually changes, so
        // hand edits keep their formatting and `config.json` is not touched on every launch.
        let mut must_write = false;
        let settings = if path.exists() {
            match read_settings(&path) {
                Ok(settings) => settings,
                Err(e) => {
                    // Keep the bad file for inspection instead of overwriting it, and say so.
                    let broken = root.join("config.broken.json");
                    let _ = std::fs::rename(&path, &broken);
                    warning = Some(format!(
                        "settings were unreadable ({e:#}); moved to {}, starting from defaults",
                        broken.display()
                    ));
                    must_write = true;
                    Settings::default()
                }
            }
        } else {
            must_write = true;
            let model_root = legacy_model_root(&root.join(LEGACY_FILE));
            Settings {
                model_root,
                ..Settings::default()
            }
        };

        let mut conf = Self {
            settings,
            root,
            log: caption_log,
            icon: icon.to_string_lossy().to_string(),
            warning,
        };
        // The document people are meant to edit has to exist.
        if must_write {
            if let Err(e) = conf.save() {
                conf.warn(format!("cannot write settings: {e:#}"));
            }
        }
        Ok(conf)
    }

    /// Add a note for the user without losing an earlier one.
    pub(crate) fn warn(&mut self, message: String) {
        self.warning = Some(match self.warning.take() {
            Some(previous) => format!("{previous}; {message}"),
            None => message,
        });
    }

    /// Where the settings live, for messages.
    pub fn settings_path(&self) -> PathBuf {
        self.root.join(SETTINGS_FILE)
    }

    /// Write the settings atomically: a crash mid-write leaves the old file intact.
    pub fn save(&self) -> Result<()> {
        let path = self.settings_path();
        let text = serde_json::to_string_pretty(&self.settings)
            .context("cannot encode settings as JSON")?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("cannot write {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("cannot replace {}", path.display()))?;
        Ok(())
    }
}

fn read_settings(path: &Path) -> Result<Settings> {
    let text = std::fs::read_to_string(path)?;
    let mut settings: Settings = serde_json::from_str(&text)?;
    // Hand edits and older files are not trusted: clamp every value into its legal range, then drop
    // entries that hold nothing but defaults.
    for strip in settings.strips.values_mut() {
        strip.sanitize();
    }
    if let Some(buses) = &mut settings.buses {
        for bus in buses.iter_mut() {
            bus.sanitize();
        }
    }
    settings.strips.retain(|_, strip| !strip.is_empty());
    Ok(settings)
}

/// `model_root` from the pre-JSON `key = value` file. The file itself is left untouched.
fn legacy_model_root(path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(path).ok()?;
    let value = text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "model_root")
            .then(|| value.trim().trim_matches('"').to_string())
            .filter(|v| !v.is_empty())
    })?;
    Some(PathBuf::from(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustmeeter_core::{EQ_FREQ_MAX, EQ_GAIN_MAX, EQ_Q_MIN, EqBand, EqKind};

    fn shaped() -> EqSettings {
        let mut eq = EqSettings::default();
        eq.add(EqBand::new(EqKind::LowShelf, 120.0, -3.0));
        eq.add(EqBand::new(EqKind::Bell, 3200.0, 4.5));
        eq.bands[1].q = 2.0;
        eq
    }

    fn mic() -> InputSource {
        InputSource {
            name: "Headset Microphone (MH752)".to_string(),
            loopback: false,
        }
    }

    #[test]
    fn the_file_round_trips_and_stays_readable() {
        let mut settings = Settings {
            model_root: Some(PathBuf::from(r"C:\models\cactus")),
            strip_order: Some(vec![mic()]),
            ..Settings::default()
        };
        settings.set_strip(
            "Headset Microphone (MH752)",
            StripSettings {
                eq: Some(shaped()),
                gain_db: Some(2.5),
                muted: true,
                routes: Some([false, true]),
            },
        );
        settings.set_bus(
            0,
            BusSettings {
                device: Some("Speakers (Realtek(R) Audio)".to_string()),
                gain_db: Some(-3.0),
                ..BusSettings::default()
            },
        );
        settings.set_bus(
            1,
            BusSettings {
                device: Some("CABLE Input (VB-Audio Virtual Cable)".to_string()),
                muted: true,
                ..BusSettings::default()
            },
        );

        let json = serde_json::to_string_pretty(&settings).unwrap();
        // The curve names in the file are the ones the UI and the CLI use.
        assert!(json.contains("\"low-shelf\""), "{json}");
        assert!(json.contains("\"gain_db\": 2.5"), "{json}");
        assert!(json.contains("\"muted\": true"), "{json}");
        assert!(json.contains("\"routes\""), "{json}");
        assert!(json.contains("\"strip_order\""), "{json}");
        assert!(json.contains("\"loopback\""), "{json}");
        assert!(json.contains("\"buses\""), "{json}");
        assert!(json.contains("\"device\""), "{json}");

        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, settings);
        assert_eq!(
            back.strip("Headset Microphone (MH752)"),
            settings.strip("Headset Microphone (MH752)")
        );
        assert_eq!(back.buses, settings.buses);
    }

    #[test]
    fn needle_tools_and_the_checkbox_round_trip() {
        let mut settings = Settings {
            toolcall_enabled: true,
            ..Settings::default()
        };
        settings.tools = vec![serde_json::json!({
            "name": "get_weather",
            "description": "Get the current weather for a city.",
            "parameters": {
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"]
            }
        })];

        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"toolcall_enabled\":true"), "{json}");
        assert!(json.contains("\"get_weather\""), "{json}");

        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, settings);
    }

    #[test]
    fn defaults_do_not_keep_a_line_in_the_file() {
        let mut settings = Settings::default();
        settings.set_strip(
            "Speakers",
            StripSettings {
                eq: Some(shaped()),
                ..StripSettings::default()
            },
        );
        assert!(settings.strip("Speakers").is_some());

        // Putting every field back to its default removes the key instead of writing `{}`.
        settings.set_strip("Speakers", StripSettings::default());
        assert!(settings.strip("Speakers").is_none());
        assert!(settings.strips.is_empty());
        assert_eq!(
            serde_json::to_string(&settings).unwrap(),
            r#"{"model_root":null,"strips":{}}"#
        );

        // One non-default flag is enough for a line, and unset fields stay out of it.
        settings.set_strip(
            "Speakers",
            StripSettings {
                muted: true,
                ..StripSettings::default()
            },
        );
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"muted\":true"), "{json}");
        assert!(
            !json.contains("gain_db"),
            "unset fields are not written: {json}"
        );
        assert!(
            !json.contains("routes"),
            "unset fields are not written: {json}"
        );
    }

    #[test]
    fn an_empty_rack_is_remembered_as_empty() {
        // `null`/absent means "no rack recorded yet" and gets the first-run default; `[]` means the
        // user removed every strip and must not get one back on the next launch.
        let no_rack: Settings = serde_json::from_str(r#"{"strips":{}}"#).unwrap();
        assert_eq!(no_rack.strip_order, None);
        assert_eq!(no_rack.buses, None);

        let mut settings = Settings::default();
        settings.strip_order = Some(vec![]);
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"strip_order\":[]"), "{json}");
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.strip_order, Some(vec![]));
    }

    #[test]
    fn a_hand_edited_file_is_clamped_on_load() {
        let dir = std::env::temp_dir().join(format!("rustmeeter-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            r#"{
                "strip_order": [
                    { "name": "mic", "loopback": false }
                ],
                "strips": {
                    "mic": {
                        "gain_db": 99.0,
                        "eq": { "bands": [
                            { "kind": "bell", "freq_hz": 999999.0, "gain_db": 99.0, "q": 0.0 }
                        ] }
                    },
                    "untouched": {}
                },
                "buses": [
                    { "gain_db": 99.0 },
                    { "device": "Speakers" }
                ]
            }"#,
        )
        .unwrap();

        let settings = read_settings(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        let strip = settings.strip("mic").unwrap();
        assert_eq!(strip.gain_db, Some(MAX_DB));
        assert!(!strip.muted, "an omitted flag takes its default");
        assert_eq!(strip.routes, None, "an omitted route map is not a choice");
        let band = strip.eq.as_ref().unwrap().bands[0];
        assert_eq!(band.freq_hz, EQ_FREQ_MAX);
        assert_eq!(band.gain_db, EQ_GAIN_MAX);
        assert_eq!(band.q, EQ_Q_MIN);
        assert!(band.enabled, "an omitted field takes its default");
        // A line that says nothing is not a setting.
        assert!(settings.strip("untouched").is_none());

        // The rack and buses come back too, clamped.
        assert_eq!(settings.strip_order, Some(vec![mic_with_name("mic")]));
        let buses = settings.buses.unwrap();
        assert_eq!(buses[0].gain_db, Some(MAX_DB));
        assert_eq!(buses[1].device.as_deref(), Some("Speakers"));
    }

    fn mic_with_name(name: &str) -> InputSource {
        InputSource {
            name: name.to_string(),
            loopback: false,
        }
    }
}
