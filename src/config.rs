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
    #[allow(dead_code)] // audio config is consumed in M5
    #[serde(default)]
    pub audio: AudioConfig,
    #[serde(default)]
    pub ui: UiConfig,
    /// Device model catalog. Selected by name via `[ui].model`. Config-defined
    /// profiles override built-ins of the same name (see `builtin_devices`).
    #[serde(default)]
    pub device: Vec<DeviceProfile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChannelConfig {
    #[serde(default = "default_host")]
    pub host: String,
    pub port: u16,
    #[allow(dead_code)] // TLS wiring lands in a later milestone
    #[serde(default)]
    pub use_tls: bool,
    #[allow(dead_code)]
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

#[allow(dead_code)] // fields consumed by the audio path in M5
#[derive(Debug, Clone, Deserialize)]
pub struct AudioConfig {
    /// Enable the two-way voice path (decode downlink, encode uplink).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Directory holding the prebuilt ACELP codec libraries
    /// (`tetra_acelp*.dll` / `.so`). ETSI-copyrighted, never committed here.
    #[serde(default = "default_codec_dir")]
    pub codec_dir: String,
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

/// How the operator interacts with the device. Drives which layout and input
/// handling the UI uses: touch-first tap targets vs a keypad/softkey driven,
/// focus-based interface. Extend this as new device classes appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputKind {
    #[serde(alias = "touchscreen")]
    Touch,
    #[serde(alias = "keys", alias = "keyboard")]
    Keypad,
}

/// A device model: a named window geometry plus a UI scale factor and input
/// kind. Lets the same binary target different panels (a landscape Pi
/// touchscreen, a keypad handheld, etc.) by selecting a model in config instead
/// of editing code.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceProfile {
    pub name: String,
    /// Native panel width in device pixels.
    pub width: u32,
    /// Native panel height in device pixels.
    pub height: u32,
    /// UI scale factor (device pixel ratio). Content authored in logical pixels
    /// is multiplied by this. 1.0 = crisp 1:1 on the panel.
    #[serde(default = "default_scale")]
    pub scale: f32,
    /// Interaction model for this device (touch or keypad).
    #[serde(default = "default_input")]
    pub input: InputKind,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UiConfig {
    /// Name of a device model to use from the catalog (built-ins + `[[device]]`).
    #[serde(default)]
    pub model: Option<String>,
    /// Explicit width override (device pixels). Takes precedence over the model.
    #[serde(default)]
    pub width: Option<u32>,
    /// Explicit height override (device pixels). Takes precedence over the model.
    #[serde(default)]
    pub height: Option<u32>,
    /// Explicit scale override. Takes precedence over the model. Also used in dev
    /// to override the host display scaling (e.g. Windows 150%) via SLINT_SCALE_FACTOR.
    #[serde(default)]
    pub scale: Option<f32>,
    /// Explicit interaction-model override. Takes precedence over the model.
    #[serde(default)]
    pub input: Option<InputKind>,
    /// Whether the Event Log entry appears in the Menu (default true).
    #[serde(default)]
    pub show_event_log: Option<bool>,
    #[serde(default = "default_theme")]
    pub theme: String,
}

/// A fully resolved UI target: the model catalog, `[ui]` overrides, and built-in
/// defaults collapsed into concrete values ready to drive the window.
#[derive(Debug, Clone)]
pub struct ResolvedUi {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub input: InputKind,
    pub show_event_log: bool,
    pub theme: String,
    pub model: Option<String>,
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

    /// Resolve the target UI geometry by layering, in order of precedence:
    /// explicit `[ui]` overrides, then the selected `[ui].model` from the device
    /// catalog, then built-in defaults (portrait 720x1280 at scale 1.0). The
    /// catalog is the built-in models merged with `[[device]]` profiles, where a
    /// config-defined profile overrides a built-in of the same name.
    pub fn resolve_ui(&self) -> ResolvedUi {
        let mut catalog = builtin_devices();
        for profile in &self.device {
            if let Some(existing) = catalog.iter_mut().find(|c| c.name == profile.name) {
                *existing = profile.clone();
            } else {
                catalog.push(profile.clone());
            }
        }

        let base = self.ui.model.as_ref().and_then(|name| {
            let found = catalog.iter().find(|d| &d.name == name).cloned();
            if found.is_none() {
                tracing::warn!(
                    model = %name,
                    "ui.model not found in device catalog; using explicit values or defaults"
                );
            }
            found
        });

        let width = self
            .ui
            .width
            .or(base.as_ref().map(|d| d.width))
            .unwrap_or_else(default_width);
        let height = self
            .ui
            .height
            .or(base.as_ref().map(|d| d.height))
            .unwrap_or_else(default_height);
        let scale = self
            .ui
            .scale
            .or(base.as_ref().map(|d| d.scale))
            .unwrap_or_else(default_scale);
        let input = self
            .ui
            .input
            .or(base.as_ref().map(|d| d.input))
            .unwrap_or_else(default_input);

        ResolvedUi {
            width,
            height,
            scale,
            input,
            show_event_log: self.ui.show_event_log.unwrap_or(true),
            theme: self.ui.theme.clone(),
            model: self.ui.model.clone(),
        }
    }
}

/// Built-in device model catalog. These are editable examples; adjust the values
/// or add your own `[[device]]` profiles in config.toml. Real panel resolutions
/// should be confirmed against the hardware.
pub fn builtin_devices() -> Vec<DeviceProfile> {
    vec![
        DeviceProfile {
            name: "pi-1280x720".to_string(),
            width: 1280,
            height: 720,
            scale: 1.0,
            input: InputKind::Touch,
        },
        DeviceProfile {
            name: "pi-720x1280".to_string(),
            width: 720,
            height: 1280,
            scale: 1.0,
            input: InputKind::Touch,
        },
        DeviceProfile {
            name: "linht".to_string(),
            width: 480,
            height: 800,
            scale: 1.0,
            input: InputKind::Keypad,
        },
    ]
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

fn default_true() -> bool {
    true
}

fn default_codec_dir() -> String {
    // The reference UI keeps the prebuilt (ETSI-copyrighted) codec libraries here.
    r"C:\Users\mihaj\PycharmProjects\tnmm_ui\native".to_string()
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

fn default_scale() -> f32 {
    1.0
}

fn default_input() -> InputKind {
    InputKind::Touch
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
            device: Vec::new(),
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
            enabled: true,
            codec_dir: default_codec_dir(),
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
            model: None,
            width: None,
            height: None,
            scale: None,
            input: None,
            show_event_log: None,
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
        let ui = cfg.resolve_ui();
        assert_eq!((ui.width, ui.height), (720, 1280));
        assert_eq!(ui.scale, 1.0);
    }

    #[test]
    fn parses_repo_config() {
        let cfg = Config::load("config.toml").expect("repo config parses");
        assert_eq!(cfg.command.port, 9102);
        assert_eq!(cfg.telemetry.port, 9101);
        assert_eq!(cfg.audio.sample_rate, 8000);
        // The repo config should resolve to a concrete, sane geometry.
        let ui = cfg.resolve_ui();
        assert!(ui.width > 0 && ui.height > 0);
        assert!(ui.scale > 0.0);
    }

    #[test]
    fn resolves_model_from_builtin_catalog() {
        let cfg: Config = toml::from_str("[ui]\nmodel = \"pi-1280x720\"\n").unwrap();
        let ui = cfg.resolve_ui();
        assert_eq!((ui.width, ui.height), (1280, 720));
        assert_eq!(ui.scale, 1.0);
        assert_eq!(ui.input, InputKind::Touch);
        assert_eq!(ui.model.as_deref(), Some("pi-1280x720"));
    }

    #[test]
    fn keypad_model_resolves_input_kind() {
        let cfg: Config = toml::from_str("[ui]\nmodel = \"linht\"\n").unwrap();
        let ui = cfg.resolve_ui();
        assert_eq!(ui.input, InputKind::Keypad);
    }

    #[test]
    fn explicit_input_overrides_model() {
        let cfg: Config =
            toml::from_str("[ui]\nmodel = \"pi-1280x720\"\ninput = \"keypad\"\n").unwrap();
        assert_eq!(cfg.resolve_ui().input, InputKind::Keypad);
    }

    #[test]
    fn input_kind_accepts_aliases() {
        let cfg: Config = toml::from_str("[ui]\ninput = \"touchscreen\"\n").unwrap();
        assert_eq!(cfg.resolve_ui().input, InputKind::Touch);
    }

    #[test]
    fn explicit_values_override_model() {
        let cfg: Config =
            toml::from_str("[ui]\nmodel = \"pi-1280x720\"\nheight = 900\nscale = 2.0\n").unwrap();
        let ui = cfg.resolve_ui();
        assert_eq!((ui.width, ui.height), (1280, 900));
        assert_eq!(ui.scale, 2.0);
    }

    #[test]
    fn config_device_profile_is_selectable_and_overrides_builtin() {
        let toml = "\
[[device]]
name = \"myrig\"
width = 640
height = 480
scale = 1.5

[[device]]
name = \"linht\"
width = 320
height = 240

[ui]
model = \"myrig\"
";
        let cfg: Config = toml::from_str(toml).unwrap();
        let ui = cfg.resolve_ui();
        assert_eq!((ui.width, ui.height, ui.scale), (640, 480, 1.5));

        // The config-defined "linht" overrides the built-in of the same name.
        let cfg2: Config = toml::from_str(&format!("{toml}\n")).unwrap();
        let linht = cfg2
            .device
            .iter()
            .find(|d| d.name == "linht")
            .expect("linht present");
        assert_eq!((linht.width, linht.height), (320, 240));
    }

    #[test]
    fn unknown_model_falls_back_to_defaults() {
        let cfg: Config = toml::from_str("[ui]\nmodel = \"nope\"\n").unwrap();
        let ui = cfg.resolve_ui();
        assert_eq!((ui.width, ui.height), (720, 1280));
    }
}
