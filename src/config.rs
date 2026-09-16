use std::{fs, path::PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    pub show_reasoning: bool,
    pub mouse: bool,
    pub default_mode_request_user_input: bool,
    pub notifications: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            show_reasoning: true,
            mouse: true,
            default_mode_request_user_input: true,
            notifications: true,
        }
    }
}

impl ClientConfig {
    pub fn load() -> Self {
        let path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("magdex/config.toml");
        fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }
}
