use crate::paths;
use secret_service::{EncryptionType, SecretService};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::Mutex;

const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);

pub struct Secrets {
	cache: Mutex<HashMap<String, Option<String>>>,
	backend: std::sync::Mutex<String>,
}

fn file_path() -> PathBuf {
	paths::config_dir().join("secrets.json")
}

fn read_file() -> HashMap<String, String> {
	std::fs::read(file_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_file(map: &HashMap<String, String>) -> Result<(), String> {
	if map.is_empty() {
		let _ = std::fs::remove_file(file_path());
		return Ok(());
	}
	let bytes = serde_json::to_vec_pretty(map).map_err(|e| e.to_string())?;
	paths::write_private(&file_path(), &bytes).map_err(|e| format!("Could not write the key file: {e}"))
}

fn label_for(provider: &str) -> String {
	match provider {
		"opencore" => "Ash Agent API key (OpenCORe)".to_string(),
		other => format!("Ash Agent API key ({other})"),
	}
}

async fn ss_get(provider: &str) -> Result<Option<String>, String> {
	let service = SecretService::connect(EncryptionType::Dh).await.map_err(|e| e.to_string())?;
	let attrs = HashMap::from([("application", "ash-agent"), ("provider", provider)]);
	let found = service.search_items(attrs).await.map_err(|e| e.to_string())?;
	let item = match found.unlocked.into_iter().next() {
		Some(item) => item,
		None => match found.locked.into_iter().next() {
			Some(item) => {
				item.unlock().await.map_err(|e| e.to_string())?;
				item
			}
			None => return Ok(None),
		},
	};
	let secret = item.get_secret().await.map_err(|e| e.to_string())?;
	Ok(Some(String::from_utf8_lossy(&secret).trim().to_string()).filter(|s| !s.is_empty()))
}

async fn ss_set(provider: &str, value: &str) -> Result<(), String> {
	let service = SecretService::connect(EncryptionType::Dh).await.map_err(|e| e.to_string())?;
	let collection = match service.get_default_collection().await {
		Ok(c) => c,
		Err(_) => service.get_any_collection().await.map_err(|e| e.to_string())?,
	};
	collection.ensure_unlocked().await.map_err(|e| e.to_string())?;
	let attrs = HashMap::from([("application", "ash-agent"), ("provider", provider)]);
	collection
		.create_item(&label_for(provider), attrs, value.as_bytes(), true, "text/plain")
		.await
		.map_err(|e| e.to_string())?;
	Ok(())
}

async fn ss_delete(provider: &str) -> Result<(), String> {
	let service = SecretService::connect(EncryptionType::Dh).await.map_err(|e| e.to_string())?;
	let attrs = HashMap::from([("application", "ash-agent"), ("provider", provider)]);
	let found = service.search_items(attrs).await.map_err(|e| e.to_string())?;
	for item in found.unlocked.iter().chain(found.locked.iter()) {
		item.delete().await.map_err(|e| e.to_string())?;
	}
	Ok(())
}

impl Default for Secrets {
	fn default() -> Self {
		Self::new()
	}
}

impl Secrets {
	pub fn new() -> Self {
		Self { cache: Mutex::new(HashMap::new()), backend: std::sync::Mutex::new(String::new()) }
	}

	pub fn backend(&self) -> String {
		self.backend.lock().map(|b| b.clone()).unwrap_or_default()
	}

	fn set_backend(&self, name: &str) {
		if let Ok(mut b) = self.backend.lock() {
			*b = name.to_string();
		}
	}

	pub async fn get(&self, provider: &str) -> Option<String> {
		let mut cache = self.cache.lock().await;
		if let Some(value) = cache.get(provider) {
			return value.clone();
		}
		let from_file = read_file().get(provider).cloned().filter(|v| !v.is_empty());
		let value = if from_file.is_some() {
			self.set_backend("file");
			from_file
		} else {
			match tokio::time::timeout(PROMPT_TIMEOUT, ss_get(provider)).await {
				Ok(Ok(value)) => {
					self.set_backend("wallet");
					value
				}
				_ => None,
			}
		};
		cache.insert(provider.to_string(), value.clone());
		value
	}

	pub async fn has(&self, provider: &str) -> bool {
		self.get(provider).await.is_some()
	}

	pub async fn backend_of(&self, provider: &str) -> String {
		if self.get(provider).await.is_none() {
			return String::new();
		}
		if read_file().get(provider).is_some_and(|value| !value.is_empty()) {
			"file".to_string()
		} else {
			"wallet".to_string()
		}
	}

	pub async fn set(&self, provider: &str, value: &str) -> Result<String, String> {
		let value = value.trim();
		if value.is_empty() {
			return self.delete(provider).await.map(|_| String::new());
		}
		let mut cache = self.cache.lock().await;
		let stored_in_wallet = matches!(tokio::time::timeout(PROMPT_TIMEOUT, ss_set(provider, value)).await, Ok(Ok(())));
		let mut file = read_file();
		let backend = if stored_in_wallet {
			if file.remove(provider).is_some() {
				write_file(&file)?;
			}
			"wallet"
		} else {
			file.insert(provider.to_string(), value.to_string());
			write_file(&file)?;
			"file"
		};
		self.set_backend(backend);
		cache.insert(provider.to_string(), Some(value.to_string()));
		Ok(backend.to_string())
	}

	pub async fn delete(&self, provider: &str) -> Result<(), String> {
		let mut cache = self.cache.lock().await;
		let _ = tokio::time::timeout(PROMPT_TIMEOUT, ss_delete(provider)).await;
		let mut file = read_file();
		if file.remove(provider).is_some() {
			write_file(&file)?;
		}
		cache.insert(provider.to_string(), None);
		Ok(())
	}
}
