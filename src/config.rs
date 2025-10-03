use std::{collections::HashMap, fs};

use bitcode::{Decode, Encode};
use dirs::config_dir;

use crate::WrappedErr;

#[derive(Decode, Encode, Default)]
pub struct DocumentHistory {
	pub last_pages_opened: HashMap<String, usize>
}

impl DocumentHistory {
	pub fn load() -> Result<Self, WrappedErr> {
		Self::get_history_path()
			.ok_or_else(|| WrappedErr("Could not determine history directory".into()))
			.and_then(|path| {
				fs::read(path)
					.map_err(|e| WrappedErr(format!("Failed to read history file: {e}").into()))
			})
			.and_then(|data| {
				bitcode::decode(&data)
					.map_err(|e| WrappedErr(format!("Failed to decode history file: {e}").into()))
			})
	}

	pub fn save(&self) -> Result<(), WrappedErr> {
		let path = Self::get_history_path()
			.ok_or_else(|| WrappedErr("Could not determine history directory".into()))?;
		let data = bitcode::encode(self);
		fs::write(path, data)
			.map_err(|e| WrappedErr(format!("Failed to write history file: {e}").into()))?;
		Ok(())
	}

	fn get_history_path() -> Option<std::path::PathBuf> {
		config_dir().map(|mut dir| {
			dir.push("tdf.history.bin");
			dir
		})
	}
}

#[cfg(test)]
mod tests {
	use std::fs;

	use tempfile::tempdir;

	use super::*;

	#[test]
	fn test_default_history() {
		let history = DocumentHistory::default();
		assert!(history.last_pages_opened.is_empty());
	}

	#[test]
	fn test_history_serialization() {
		let mut history = DocumentHistory::default();
		history
			.last_pages_opened
			.insert("/path/to/file1.pdf".to_string(), 5);
		history
			.last_pages_opened
			.insert("/path/to/file2.pdf".to_string(), 10);

		let encoded = bitcode::encode(&history);
		let deserialized: DocumentHistory = bitcode::decode(&encoded).unwrap();

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
	fn test_history_with_temp_dir() {
		let temp_dir = tempdir().unwrap();
		let history_path = temp_dir.path().join("tdf.history.bin");

		let mut history = DocumentHistory::default();
		history
			.last_pages_opened
			.insert("/test/file.pdf".to_string(), 42);

		let encoded = bitcode::encode(&history);
		fs::write(&history_path, encoded).unwrap();

		let data = fs::read(&history_path).unwrap();
		let loaded_history: DocumentHistory = bitcode::decode(&data).unwrap();

		assert_eq!(
			loaded_history.last_pages_opened.get("/test/file.pdf"),
			Some(&42)
		);
	}

	#[test]
	fn test_load_with_invalid_binary() {
		let temp_dir = tempdir().unwrap();
		let history_path = temp_dir.path().join("tdf.history.bin");

		fs::write(&history_path, b"invalid binary data").unwrap();

		let data = fs::read(&history_path).unwrap();
		let result: Result<DocumentHistory, _> = bitcode::decode(&data);
		assert!(result.is_err());
	}

	#[test]
	fn test_history_with_empty_file() {
		let temp_dir = tempdir().unwrap();
		let history_path = temp_dir.path().join("tdf.history.bin");

		fs::write(&history_path, b"").unwrap();

		let data = fs::read(&history_path).unwrap();
		let result: Result<DocumentHistory, _> = bitcode::decode(&data);
		assert!(result.is_err());
	}

	#[test]
	fn test_history_save_and_load() {
		let temp_dir = tempdir().unwrap();
		let test_history_path = temp_dir.path().join("tdf.history.bin");

		let mut history = DocumentHistory::default();
		history
			.last_pages_opened
			.insert("/test/file.pdf".to_string(), 123);

		let encoded = bitcode::encode(&history);
		fs::write(&test_history_path, encoded).unwrap();

		let data = fs::read(&test_history_path).unwrap();
		let loaded_history: DocumentHistory = bitcode::decode(&data).unwrap();

		assert_eq!(
			loaded_history.last_pages_opened.get("/test/file.pdf"),
			Some(&123)
		);
	}
}
