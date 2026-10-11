use serde::Serialize;
use std::collections::HashMap;
use std::time::Duration;
use zbus::proxy;
use zbus::Connection;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Resolved {
	Light,
	Dark,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeState {
	pub scheme: Resolved,
	pub accent: Option<String>,
	pub source: String,
}

#[proxy(
	interface = "org.freedesktop.portal.Settings",
	default_service = "org.freedesktop.portal.Desktop",
	default_path = "/org/freedesktop/portal/desktop"
)]
trait PortalSettings {
	fn read_one(&self, namespace: &str, key: &str) -> zbus::Result<zbus::zvariant::OwnedValue>;
	#[zbus(signal)]
	fn setting_changed(&self, namespace: String, key: String, value: zbus::zvariant::OwnedValue) -> zbus::Result<()>;
}

fn scheme_from_value(value: &zbus::zvariant::OwnedValue) -> Option<Resolved> {
	use zbus::zvariant::Value;
	match <Value as TryFrom<&zbus::zvariant::OwnedValue>>::try_from(value).ok()? {
		Value::U32(1) => Some(Resolved::Dark),
		Value::U32(2) => Some(Resolved::Light),
		Value::I32(1) => Some(Resolved::Dark),
		Value::I32(2) => Some(Resolved::Light),
		_ => None,
	}
}

pub async fn portal_state() -> Option<ThemeState> {
	let connection = Connection::session().await.ok()?;
	let proxy = PortalSettingsProxy::new(&connection).await.ok()?;
	let scheme = proxy
		.read_one("org.freedesktop.appearance", "color-scheme")
		.await
		.ok()
		.and_then(|v| scheme_from_value(&v));
	let scheme = scheme?;
	let accent = proxy.read_one("org.freedesktop.appearance", "accent-color").await.ok().and_then(|value| accent_to_hex(&value));
	Some(ThemeState { scheme, accent, source: "portal".into() })
}

fn accent_to_hex(value: &zbus::zvariant::OwnedValue) -> Option<String> {
	use zbus::zvariant::Value;
	match <Value as TryFrom<&zbus::zvariant::OwnedValue>>::try_from(value).ok()? {
		Value::Structure(structure) => {
			let fields = structure.fields();
			let numbers: Vec<f64> = fields
				.iter()
				.map(|field| match field {
					Value::F64(v) => *v,
					Value::U32(v) => *v as f64,
					_ => 0.0,
				})
				.collect();
			if numbers.len() < 3 {
				return None;
			}
			let channel = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
			Some(format!("#{:02x}{:02x}{:02x}", channel(numbers[0]), channel(numbers[1]), channel(numbers[2])))
		}
		_ => None,
	}
}

fn kdeglobals() -> HashMap<String, String> {
	let mut map = HashMap::new();
	let path = crate::paths::home().join(".config/kdeglobals");
	if let Ok(text) = std::fs::read_to_string(path) {
		for line in text.lines() {
			let line = line.trim();
			if line.starts_with('#') || line.starts_with('[') || line.is_empty() {
				continue;
			}
			if let Some((key, value)) = line.split_once('=') {
				map.insert(key.trim().to_string(), value.trim().to_string());
			}
		}
	}
	map
}

pub fn resolve(preference: &str) -> ThemeState {
	if preference == "light" || preference == "dark" {
		return ThemeState {
			scheme: if preference == "dark" { Resolved::Dark } else { Resolved::Light },
			accent: None,
			source: "manual".into(),
		};
	}
	let globals = kdeglobals();
	let scheme = globals
		.get("ColorScheme")
		.map(|name| name.to_lowercase().contains("dark"))
		.or_else(|| globals.get("LookAndFeelPackage").map(|name| name.to_lowercase().contains("dark")))
		.map(|dark| if dark { Resolved::Dark } else { Resolved::Light })
		.unwrap_or(Resolved::Dark);
	let accent = globals.get("DecorationFocus").or_else(|| globals.get("AccentColor")).and_then(|value| normalize_hex(value));
	ThemeState { scheme, accent, source: "kdeglobals".into() }
}

fn normalize_hex(value: &str) -> Option<String> {
	let digits: String = value.chars().filter(|c| c.is_ascii_hexdigit()).collect();
	if digits.len() != 6 {
		return None;
	}
	Some(format!("#{}", digits.to_lowercase()))
}

pub async fn watch(events: tokio::sync::broadcast::Sender<ThemeState>) {
	let Ok(connection) = Connection::session().await else { return };
	let Ok(proxy) = PortalSettingsProxy::new(&connection).await else { return };
	let Ok(mut stream) = proxy.receive_setting_changed().await else { return };
	use futures_util::StreamExt;
	while let Some(signal) = stream.next().await {
		let Ok(args) = signal.args() else { continue };
		if args.namespace != "org.freedesktop.appearance" {
			continue;
		}
		if args.key == "color-scheme" {
			if let Some(scheme) = scheme_from_value(&args.value) {
				let current = resolve("system");
				let _ = events.send(ThemeState { scheme, source: "portal".into(), accent: current.accent });
			}
		} else if args.key == "accent-color" {
			if let Some(accent) = accent_to_hex(&args.value) {
				let current = resolve("system");
				let _ = events.send(ThemeState { scheme: current.scheme, accent: Some(accent), source: "portal".into() });
			}
		}
	}
}

pub async fn current(preference: &str) -> ThemeState {
	if preference == "system" {
		if let Ok(Some(state)) = tokio::time::timeout(Duration::from_secs(3), portal_state()).await {
			let mut state = state;
			if state.accent.is_none() {
				state.accent = resolve("system").accent;
			}
			return state;
		}
	}
	resolve(preference)
}
