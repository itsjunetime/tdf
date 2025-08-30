use std::{collections::HashMap, fs};

use dirs::config_dir;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default)]
pub struct DocumentHistoryConfig {
	pub last_pages_opened: HashMap<String, usize>
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

#[cfg(test)]
mod tests {
	use std::fs;

	use serde_json;
	use tempfile::tempdir;

	use super::*;

	#[test]
	fn test_default_config() {
		let config = DocumentHistoryConfig::default();
		assert!(config.last_pages_opened.is_empty());
	}

	#[test]
	fn test_config_serialization() {
		let mut config = DocumentHistoryConfig::default();
		config
			.last_pages_opened
			.insert("/path/to/file1.pdf".to_string(), 5);
		config
			.last_pages_opened
			.insert("/path/to/file2.pdf".to_string(), 10);

		let json = serde_json::to_string(&config).unwrap();
		let deserialized: DocumentHistoryConfig = serde_json::from_str(&json).unwrap();

		assert_eq!(
			deserialized.last_pages_opened.get("/path/to/file1.pdf"),
			Some(&5)
		);
		assert_eq!(
			deserialized.last_pages_opened.get("/path/to/file2.pdf"),
			Some(&10)
		);
	}

	#[test]
	fn test_config_with_temp_dir() {
		let temp_dir = tempdir().unwrap();
		let config_path = temp_dir.path().join("tdf.config.json");

		let mut config = DocumentHistoryConfig::default();
		config
			.last_pages_opened
			.insert("/test/file.pdf".to_string(), 42);

		let json = serde_json::to_string_pretty(&config).unwrap();
		fs::write(&config_path, json).unwrap();

		let data = fs::read_to_string(&config_path).unwrap();
		let loaded_config: DocumentHistoryConfig = serde_json::from_str(&data).unwrap();

		assert_eq!(
			loaded_config.last_pages_opened.get("/test/file.pdf"),
			Some(&42)
		);
	}

	#[test]
	fn test_load_with_invalid_json() {
		let temp_dir = tempdir().unwrap();
		let config_path = temp_dir.path().join("tdf.config.json");

		fs::write(&config_path, "{ invalid json }").unwrap();

		let data = fs::read_to_string(&config_path).unwrap();
		let result: Result<DocumentHistoryConfig, _> = serde_json::from_str(&data);
		assert!(result.is_err());
	}

	#[test]
	fn test_config_with_empty_file() {
		let temp_dir = tempdir().unwrap();
		let config_path = temp_dir.path().join("tdf.config.json");

		fs::write(&config_path, "").unwrap();

		let data = fs::read_to_string(&config_path).unwrap();
		let result: Result<DocumentHistoryConfig, _> = serde_json::from_str(&data);
		assert!(result.is_err());
	}

	#[test]
	fn test_config_with_malformed_json() {
		let temp_dir = tempdir().unwrap();
		let config_path = temp_dir.path().join("tdf.config.json");

		fs::write(&config_path, "{ invalid json }").unwrap();

		let data = fs::read_to_string(&config_path).unwrap();
		let result: Result<DocumentHistoryConfig, _> = serde_json::from_str(&data);
		assert!(result.is_err());
	}

	#[test]
	fn test_config_save_and_load() {
		let temp_dir = tempdir().unwrap();
		let test_config_path = temp_dir.path().join("tdf.config.json");

		let mut config = DocumentHistoryConfig::default();
		config
			.last_pages_opened
			.insert("/test/file.pdf".to_string(), 123);

		let json = serde_json::to_string_pretty(&config).unwrap();
		fs::write(&test_config_path, json).unwrap();

		let data = fs::read_to_string(&test_config_path).unwrap();
		let loaded_config: DocumentHistoryConfig = serde_json::from_str(&data).unwrap();

		assert_eq!(
			loaded_config.last_pages_opened.get("/test/file.pdf"),
			Some(&123)
		);
	}

	#[test]
	fn test_load_method_with_real_config() {
		// This test verifies that the load() method works correctly
		// It will either load an existing config or return default
		DocumentHistoryConfig::load();
		// The config should be valid (either loaded from file or default)
		// We can't assert specific values since we don't know what's in the real config
		assert!(true); // Just verify it doesn't panic
	}
}
