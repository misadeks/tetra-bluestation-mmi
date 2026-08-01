// M1 config parsing stub.
//
// This app is the SERVER side of the BlueStation MS external interface: the
// stack (or fake_stack.py) is the WebSocket client and dials OUT to the host
// and ports configured here. Networking is wired up in M2; for M1 we only
// parse the file and log it.

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub command: ChannelConfig,
    #[serde(default)]
    pub telemetry: ChannelConfig,
    #[serde(default)]
    pub registration: RegistrationConfig,
    #[serde(default)]
    pub audio: AudioConfig,
    #[serde(default)]
    pub ui: UiConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChannelConfig {
    #[serde(default = "default_host")]
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub use_tls: bool,
    #[serde(default)]
    pub ca_cert: Option<String>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistrationConfig {
    #[serde(default = "default_registration_type")]
    pub registration_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AudioConfig {
    #[serde(default = "default_device")]
    pub output_device: String,
    #[serde(default = "default_device")]
    pub input_device: String,
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    #[serde(default = "default_frame_ms")]
    pub frame_ms: u32,
    #[serde(default = "default_jitter_ms")]
    pub jitter_ms: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UiConfig {
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default = "default_theme")]
    pub theme: String,
}

impl Config {
    /// Load configuration from a TOML file. Returns defaults when the file is
    /// absent so the M1 spike runs with zero setup.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(ConfigError::Parse),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(ConfigError::Io(err)),
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(err) => write!(f, "failed to read config file: {err}"),
            ConfigError::Parse(err) => write!(f, "failed to parse config file: {err}"),
        }
    }
}

impl std::error::Error for ConfigError {}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_registration_type() -> String {
    "RegistrationToIndicatedCell".to_string()
}

fn default_device() -> String {
    "default".to_string()
}

fn default_sample_rate() -> u32 {
    8000
}

fn default_frame_ms() -> u32 {
    60
}

fn default_jitter_ms() -> u32 {
    120
}

fn default_width() -> u32 {
    720
}

fn default_height() -> u32 {
    1280
}

fn default_theme() -> String {
    "classic".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            command: ChannelConfig::with_port(9102),
            telemetry: ChannelConfig::with_port(9101),
            registration: RegistrationConfig::default(),
            audio: AudioConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

impl ChannelConfig {
    fn with_port(port: u16) -> Self {
        ChannelConfig {
            host: default_host(),
            port,
            use_tls: false,
            ca_cert: None,
            username: String::new(),
            password: String::new(),
        }
    }
}

impl Default for ChannelConfig {
    fn default() -> Self {
        ChannelConfig::with_port(0)
    }
}

impl Default for RegistrationConfig {
    fn default() -> Self {
        RegistrationConfig {
            registration_type: default_registration_type(),
        }
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        AudioConfig {
            output_device: default_device(),
            input_device: default_device(),
            sample_rate: default_sample_rate(),
            frame_ms: default_frame_ms(),
            jitter_ms: default_jitter_ms(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            width: default_width(),
            height: default_height(),
            theme: default_theme(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let cfg = Config::load("does-not-exist.toml").expect("defaults on missing file");
        assert_eq!(cfg.command.port, 9102);
        assert_eq!(cfg.telemetry.port, 9101);
        assert_eq!(cfg.ui.width, 720);
        assert_eq!(cfg.ui.height, 1280);
    }

    #[test]
    fn parses_repo_config() {
        let cfg = Config::load("config.toml").expect("repo config parses");
        assert_eq!(cfg.command.port, 9102);
        assert_eq!(cfg.telemetry.port, 9101);
        assert_eq!(cfg.audio.sample_rate, 8000);
    }
}
