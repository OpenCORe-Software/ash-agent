use crate::paths;
use std::path::PathBuf;

const SYSTEM_ENTRY: &str = "/etc/xdg/autostart/ash-agent.desktop";

fn override_path() -> PathBuf {
	dirs::config_dir().unwrap_or_else(|| paths::home().join(".config")).join("autostart/ash-agent.desktop")
}

fn executable() -> String {
	std::env::current_exe()
		.ok()
		.and_then(|path| path.canonicalize().ok())
		.map(|path| path.to_string_lossy().to_string())
		.unwrap_or_else(|| "/usr/bin/ash-agent".to_string())
}

pub fn system_entry_text() -> String {
	entry_text(false)
}

fn entry_text(hidden: bool) -> String {
	let exec = executable();
	let mut text = format!(
		"[Desktop Entry]\nType=Application\nName=Ash Agent\nComment=Start Ash Agent in the system tray\nExec={exec} --hidden\nIcon=ash-agent\nTerminal=false\nX-KDE-autostart-phase=2\n"
	);
	if hidden {
		text.push_str("Hidden=true\nX-GNOME-Autostart-enabled=false\n");
	}
	text
}

fn entry_disabled(text: &str) -> bool {
	text.lines().any(|line| {
		let line = line.trim();
		line.eq_ignore_ascii_case("Hidden=true") || line.eq_ignore_ascii_case("X-GNOME-Autostart-enabled=false")
	})
}

pub fn available() -> bool {
	true
}

pub fn enabled() -> bool {
	match std::fs::read_to_string(override_path()) {
		Ok(text) => !entry_disabled(&text),
		Err(_) => PathBuf::from(SYSTEM_ENTRY).is_file(),
	}
}

pub fn set(enabled: bool) -> Result<(), String> {
	let path = override_path();
	let system = PathBuf::from(SYSTEM_ENTRY).is_file();
	if enabled && system {
		if path.exists() {
			std::fs::remove_file(&path).map_err(|error| format!("Could not turn on starting with the desktop: {error}"))?;
		}
		return Ok(());
	}
	if !enabled && !system {
		if path.exists() {
			std::fs::remove_file(&path).map_err(|error| format!("Could not turn off starting with the desktop: {error}"))?;
		}
		return Ok(());
	}
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent).map_err(|error| format!("Could not write the autostart entry: {error}"))?;
	}
	std::fs::write(&path, entry_text(!enabled)).map_err(|error| format!("Could not write the autostart entry: {error}"))
}
