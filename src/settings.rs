use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
	Observe,
	#[default]
	Ask,
	Trusted,
}

impl Tier {
	pub fn label(self) -> &'static str {
		match self {
			Tier::Observe => "Observe",
			Tier::Ask => "Ask",
			Tier::Trusted => "Trusted",
		}
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CustomEndpoint {
	pub id: String,
	pub name: String,
	pub base_url: String,
	pub tools: bool,
	pub vision: bool,
}

impl Default for CustomEndpoint {
	fn default() -> Self {
		Self { id: String::new(), name: String::new(), base_url: String::new(), tools: true, vision: false }
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WindowState {
	pub width: f64,
	pub height: f64,
	pub maximized: bool,
}

impl Default for WindowState {
	fn default() -> Self {
		Self { width: 1180.0, height: 780.0, maximized: false }
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
	pub provider: String,
	pub models: HashMap<String, String>,
	pub custom_endpoints: Vec<CustomEndpoint>,
	pub tier: Tier,
	pub theme: String,
	pub send_with_enter: bool,
	pub system_prompt: String,
	pub temperature: Option<f32>,
	pub show_thinking: bool,
	pub default_mode: String,
	pub hide_while_controlling: bool,
	pub close_to_tray: bool,
	pub notifications: bool,
	pub font_scale: f32,
	pub sidebar_collapsed: bool,
	pub window: WindowState,
	pub portal_restore_token: Option<String>,
	pub onboarded: bool,
	pub legacy_handled: bool,
}

impl Default for Settings {
	fn default() -> Self {
		Self {
			provider: "ml5".into(),
			models: HashMap::new(),
			custom_endpoints: Vec::new(),
			tier: Tier::Ask,
			theme: "system".into(),
			send_with_enter: true,
			system_prompt: String::new(),
			temperature: None,
			show_thinking: true,
			default_mode: "chat".into(),
			hide_while_controlling: true,
			close_to_tray: true,
			notifications: true,
			font_scale: 1.0,
			sidebar_collapsed: false,
			window: WindowState::default(),
			portal_restore_token: None,
			onboarded: false,
			legacy_handled: false,
		}
	}
}

pub struct LegacyConfig {
	pub provider: Option<String>,
	pub api_key: Option<String>,
	pub path: PathBuf,
}

pub fn settings_path() -> PathBuf {
	paths::config_dir().join("settings.json")
}

impl Settings {
	pub fn load() -> Settings {
		let mut settings = std::fs::read(settings_path())
			.ok()
			.and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
			.unwrap_or_default();
		settings.normalize();
		settings
	}

	pub fn normalize(&mut self) {
		if !matches!(self.theme.as_str(), "system" | "light" | "dark") {
			self.theme = "system".into();
		}
		if !matches!(self.default_mode.as_str(), "chat" | "helper") {
			self.default_mode = "chat".into();
		}
		if !(0.8..=1.4).contains(&self.font_scale) {
			self.font_scale = 1.0;
		}
		self.temperature = self.temperature.filter(|t| (0.0..=2.0).contains(t));
		self.custom_endpoints.retain(|e| !e.id.is_empty() && !e.base_url.trim().is_empty());
		for endpoint in &mut self.custom_endpoints {
			endpoint.base_url = endpoint.base_url.trim().trim_end_matches('/').to_string();
			if endpoint.name.trim().is_empty() {
				endpoint.name = endpoint.base_url.clone();
			}
		}
		let valid = matches!(self.provider.as_str(), "ml5" | "opencore") || self.custom_endpoints.iter().any(|e| e.id == self.provider);
		if !valid {
			self.provider = "ml5".into();
		}
		self.window.width = self.window.width.clamp(720.0, 7680.0);
		self.window.height = self.window.height.clamp(520.0, 4320.0);
	}

	pub fn save(&self) -> Result<(), String> {
		let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
		paths::write_private(&settings_path(), &bytes).map_err(|e| format!("Could not save settings: {e}"))
	}

	pub fn model_for(&self, provider: &str) -> Option<String> {
		self.models.get(provider).filter(|m| !m.is_empty()).cloned()
	}
}

pub fn read_legacy() -> Option<LegacyConfig> {
	let path = paths::config_dir().join("config.json");
	let bytes = std::fs::read(&path).ok()?;
	let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
	let provider = match value["backend"].as_str() {
		Some("opencore") => Some("opencore".to_string()),
		Some("ml5") => Some("ml5".to_string()),
		_ => None,
	};
	let api_key = value["api_key"].as_str().map(str::trim).filter(|k| !k.is_empty()).map(str::to_string);
	Some(LegacyConfig { provider, api_key, path })
}
