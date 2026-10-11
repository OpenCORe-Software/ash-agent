use crate::paths;
use crate::util;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const TIMEOUT: Duration = Duration::from_secs(5);

fn wayland() -> bool {
	std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty())
}

fn x11() -> bool {
	std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty())
}

async fn klipper_read() -> Result<String, String> {
	let connection = zbus::Connection::session().await.map_err(|error| error.to_string())?;
	let reply = tokio::time::timeout(
		TIMEOUT,
		connection.call_method(Some("org.kde.klipper"), "/klipper", Some("org.kde.klipper.klipper"), "getClipboardContents", &()),
	)
	.await
	.map_err(|_| "Klipper did not answer.".to_string())?
	.map_err(|error| error.to_string())?;
	reply.body().deserialize::<String>().map_err(|error| error.to_string())
}

async fn klipper_write(text: &str) -> Result<(), String> {
	let connection = zbus::Connection::session().await.map_err(|error| error.to_string())?;
	tokio::time::timeout(
		TIMEOUT,
		connection.call_method(Some("org.kde.klipper"), "/klipper", Some("org.kde.klipper.klipper"), "setClipboardContents", &(text)),
	)
	.await
	.map_err(|_| "Klipper did not answer.".to_string())?
	.map_err(|error| error.to_string())?;
	Ok(())
}

async fn program_read(program: &str, args: &[&str]) -> Option<String> {
	if !paths::has_program(program) {
		return None;
	}
	let captured = util::capture(program, args, TIMEOUT).await;
	if captured.ok {
		return Some(captured.stdout);
	}
	let lowered = captured.stderr.to_lowercase();
	(lowered.contains("nothing is copied") || lowered.contains("no selection") || lowered.contains("no suitable type") || lowered.contains("target string not available")).then(String::new)
}

async fn program_write(program: &str, args: &[&str], text: &str) -> bool {
	if !paths::has_program(program) {
		return false;
	}
	let child = tokio::process::Command::new(program)
		.args(args)
		.stdin(std::process::Stdio::piped())
		.stdout(std::process::Stdio::null())
		.stderr(std::process::Stdio::null())
		.spawn();
	let Ok(mut child) = child else { return false };
	if let Some(mut stdin) = child.stdin.take() {
		if stdin.write_all(text.as_bytes()).await.is_err() {
			let _ = child.start_kill();
			return false;
		}
		let _ = stdin.shutdown().await;
	}
	matches!(tokio::time::timeout(TIMEOUT, child.wait()).await, Ok(Ok(status)) if status.success())
}

pub async fn read_text() -> Result<String, String> {
	if wayland() {
		if let Some(text) = program_read("wl-paste", &["--no-newline", "--type", "text/plain;charset=utf-8"]).await {
			return Ok(text);
		}
		if let Some(text) = program_read("wl-paste", &["--no-newline"]).await {
			return Ok(text);
		}
	}
	if let Ok(text) = klipper_read().await {
		return Ok(text);
	}
	if x11() {
		if let Some(text) = program_read("xclip", &["-selection", "clipboard", "-o"]).await {
			return Ok(text);
		}
		if let Some(text) = program_read("xsel", &["--clipboard", "--output"]).await {
			return Ok(text);
		}
	}
	Err("The clipboard could not be read. Install the wl-clipboard package, or make sure Klipper is running.".to_string())
}

async fn confirm(text: &str) -> bool {
	for _ in 0..6 {
		if read_text().await.map(|current| current == text).unwrap_or(false) {
			return true;
		}
		tokio::time::sleep(Duration::from_millis(80)).await;
	}
	false
}

pub async fn write_text(text: &str) -> Result<(), String> {
	if wayland() && program_write("wl-copy", &["--type", "text/plain;charset=utf-8"], text).await && confirm(text).await {
		return Ok(());
	}
	if klipper_write(text).await.is_ok() && confirm(text).await {
		return Ok(());
	}
	if x11() {
		if program_write("xclip", &["-selection", "clipboard", "-i"], text).await && confirm(text).await {
			return Ok(());
		}
		if program_write("xsel", &["--clipboard", "--input"], text).await && confirm(text).await {
			return Ok(());
		}
	}
	Err("The text could not be put on the clipboard. Install the wl-clipboard package, or make sure Klipper is running.".to_string())
}

pub async fn write_with_restore(text: &str) -> Result<String, String> {
	let previous = read_text().await.unwrap_or_default();
	write_text(text).await?;
	Ok(previous)
}

pub async fn restore(previous: &str) {
	if previous.is_empty() {
		return;
	}
	let _ = write_text(previous).await;
}

pub async fn has_wl_clipboard() -> bool {
	paths::has_program("wl-copy") && paths::has_program("wl-paste")
}

pub async fn available() -> bool {
	(wayland() && has_wl_clipboard().await) || klipper_read().await.is_ok() || (x11() && (paths::has_program("xclip") || paths::has_program("xsel")))
}
