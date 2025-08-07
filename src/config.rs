use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use dirs::config_dir;

#[derive(Serialize, Deserialize, Default)]
pub struct DocumentHistoryConfig {
    pub last_pages_opened: HashMap<String, usize>,
}

impl DocumentHistoryConfig {
    pub fn load() -> Self {
        if let Some(path) = Self::get_config_path() {
            if let Ok(data) = fs::read_to_string(path) {
                if let Ok(cfg) = serde_json::from_str(&data) {
                    return cfg;
                }
            }
        }
        Self::default()
    }

    pub fn save(&self) {
        if let Some(path) = Self::get_config_path() {
            if let Ok(data) = serde_json::to_string_pretty(self) {
                let _ = fs::write(path, data);
            }
        }
    }

    fn get_config_path() -> Option<std::path::PathBuf> {
        config_dir().map(|mut dir| {
            dir.push("tdf.config.json");
            dir
        })
    }
}
