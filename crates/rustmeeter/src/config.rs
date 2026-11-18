use anyhow::{Context, Result};
use std::{collections::HashMap, fs::OpenOptions, path::PathBuf};

const APP_ICON: &[u8] = include_bytes!("../assets/app.ico");
const DEFAULT_CONF: &str = r#"model_root = "E:\_CODE\rust\automatools\crates\cactus-core\cactus""#;

fn parse_config(input: &str) -> HashMap<String, String> {
    input
        .lines()
        .filter_map(|line| {
            let line = line.trim();

            if line.is_empty() || line.starts_with('#') {
                return None;
            }

            let (key, value) = line.split_once('=')?;

            Some((
                key.trim().to_string(),
                value.trim().trim_matches('"').to_string(),
            ))
        })
        .collect()
}

pub fn app_config_dir() -> Result<PathBuf> {
    let appdata = std::env::var("APPDATA").context("APPDATA environment variable not found")?;
    let dir = PathBuf::from(appdata).join("rustmeeter");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub struct Config {
    pub model_root: PathBuf,
}
impl TryFrom<HashMap<String, String>> for Config {
    type Error = anyhow::Error;

    fn try_from(map: HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            model_root: map.get("model_root").context("Missing model_root")?.into(),
        })
    }
}
pub struct Conf {
    pub config: Config,
    pub root: PathBuf,
    pub log: PathBuf,
    pub icon: String,
}

impl Conf {
    pub fn load() -> Result<Self> {
        let config_dir = app_config_dir()?;
        let icon = config_dir.join("app.ico");
        if !icon.exists() {
            std::fs::write(&icon, APP_ICON)?;
        }
        let config_file = config_dir.join("config");
        if !config_file.exists() {
            std::fs::write(&config_file, DEFAULT_CONF)?;
        }
        let content = std::fs::read_to_string(&config_file).context("Failed to read config")?;
        let map = parse_config(&content);
        let caption_log = config_dir.join("captions.log");
        _ = OpenOptions::new()
            .write(true)
            .create(true)
            .open(&caption_log)?;

        let config = Config::try_from(map).context("Cannot parse configuration")?;
        Ok(Self {
            config,
            root: config_dir,
            log: caption_log,
            icon: icon.to_string_lossy().to_string(),
        })
    }
}
