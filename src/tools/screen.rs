use crate::paths;
use crate::util;
use image::{DynamicImage, RgbaImage};
use serde::Serialize;
use std::collections::HashMap;
use std::io::Read;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::time::Duration;
use zbus::zvariant::{Fd, OwnedValue, Value};
use zbus::Connection;

const KWIN_SERVICE: &str = "org.kde.KWin";
const KWIN_PATH: &str = "/org/kde/KWin/ScreenShot2";
const KWIN_INTERFACE: &str = "org.kde.KWin.ScreenShot2";
const RESTRICTED_INTERFACES: &str = "org.kde.KWin.ScreenShot2";
const DESKTOP_NAME: &str = "ash-agent.desktop";
const SYSTEM_BINARY: &str = "/usr/bin/ash-agent";
const READ_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
	Kwin,
	Spectacle,
}

impl Source {
	pub fn label(self) -> &'static str {
		match self {
			Source::Kwin => "Plasma",
			Source::Spectacle => "Spectacle",
		}
	}
}

pub struct Shot {
	pub image: DynamicImage,
	pub source: Source,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenState {
	pub available: bool,
	pub source: String,
	pub message: String,
	pub width: u32,
	pub height: u32,
}

#[derive(Debug, Clone)]
pub enum Target {
	Workspace,
	Screen,
	Window { id: Option<String> },
	Region { x: i32, y: i32, width: i32, height: i32 },
}

type CaptureMap = HashMap<String, OwnedValue>;

fn options(include_cursor: bool) -> HashMap<String, OwnedValue> {
	let mut map: HashMap<String, OwnedValue> = HashMap::new();
	for (key, value) in [
		("include-cursor", include_cursor),
		("include-shadow", false),
		("include-decoration", true),
		("native-resolution", false),
		("hide-caller-windows", true),
	] {
		if let Ok(owned) = OwnedValue::try_from(Value::Bool(value)) {
			map.insert(key.to_string(), owned);
		}
	}
	map
}

fn pipe() -> Result<(OwnedFd, OwnedFd), String> {
	let mut ends = [0i32; 2];
	let result = unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) };
	if result != 0 {
		return Err(format!("Could not prepare to receive the picture: {}", std::io::Error::last_os_error()));
	}
	Ok(unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) })
}

fn refusal(error: &zbus::Error) -> String {
	let text = error.to_string();
	if text.contains("NoAuthorized") {
		return format!(
			"Plasma did not allow Ash Agent to take a screenshot. Its desktop entry must declare X-KDE-DBUS-Restricted-Interfaces={RESTRICTED_INTERFACES}{}",
			if desktop_file_installed() { ", and Plasma's application database must be refreshed." } else { ", and that entry is not installed." }
		);
	}
	if text.contains("ServiceUnknown") || text.contains("UnknownObject") || text.contains("UnknownMethod") || text.contains("NameHasNoOwner") {
		return "Plasma is not offering screenshots on this system.".to_string();
	}
	if text.contains("NoActiveWindow") {
		return "No window is active, so there is no window to capture.".to_string();
	}
	if text.contains("InvalidWindow") {
		return "That window no longer exists.".to_string();
	}
	if text.contains("InvalidArea") {
		return "That area of the screen is empty.".to_string();
	}
	if text.contains("Cancelled") {
		return "Plasma cancelled the screenshot.".to_string();
	}
	format!("Plasma could not take the screenshot: {text}")
}

async fn capture_kwin(target: &Target, cursor: bool) -> Result<DynamicImage, String> {
	if let Target::Region { width, height, .. } = target {
		if *width <= 0 || *height <= 0 {
			return Err("A region needs a width and a height larger than zero.".to_string());
		}
	}
	let connection = Connection::session().await.map_err(|_| "There is no desktop session, so nothing can be captured.".to_string())?;
	let opts = options(cursor);
	let (reader, writer) = pipe()?;
	let reading = tokio::task::spawn_blocking(move || {
		let mut file = std::fs::File::from(reader);
		let mut bytes = Vec::new();
		file.read_to_end(&mut bytes).map(|_| bytes)
	});
	let reply = {
		let fd = Fd::from(writer.as_fd());
		match target {
			Target::Workspace => connection.call_method(Some(KWIN_SERVICE), KWIN_PATH, Some(KWIN_INTERFACE), "CaptureWorkspace", &(&opts, fd)).await,
			Target::Screen => connection.call_method(Some(KWIN_SERVICE), KWIN_PATH, Some(KWIN_INTERFACE), "CaptureActiveScreen", &(&opts, fd)).await,
			Target::Window { id: None } => connection.call_method(Some(KWIN_SERVICE), KWIN_PATH, Some(KWIN_INTERFACE), "CaptureActiveWindow", &(&opts, fd)).await,
			Target::Window { id: Some(id) } => {
				connection
					.call_method(Some(KWIN_SERVICE), KWIN_PATH, Some(KWIN_INTERFACE), "CaptureWindow", &(id.as_str(), &opts, fd))
					.await
			}
			Target::Region { x, y, width, height } => {
				connection
					.call_method(Some(KWIN_SERVICE), KWIN_PATH, Some(KWIN_INTERFACE), "CaptureArea", &(*x, *y, *width as u32, *height as u32, &opts, fd))
					.await
			}
		}
	};
	drop(writer);
	let reply = match reply {
		Ok(reply) => reply,
		Err(error) => {
			let _ = tokio::time::timeout(Duration::from_secs(2), reading).await;
			return Err(refusal(&error));
		}
	};
	let map: CaptureMap = reply.body().deserialize().map_err(|error| format!("Plasma described the screenshot unexpectedly: {error}"))?;
	let bytes = match tokio::time::timeout(READ_TIMEOUT, reading).await {
		Ok(Ok(Ok(bytes))) => bytes,
		Ok(Ok(Err(error))) => return Err(format!("The screenshot data could not be read: {error}")),
		Ok(Err(_)) => return Err("Reading the screenshot data failed.".to_string()),
		Err(_) => return Err("Plasma took too long to send the screenshot.".to_string()),
	};
	decode(&map, &bytes)
}

fn number(map: &CaptureMap, key: &str) -> Result<u32, String> {
	let value = map.get(key).ok_or_else(|| format!("The screenshot did not report its {key}."))?;
	u32::try_from(value)
		.or_else(|_| i32::try_from(value).map(|number| number.max(0) as u32))
		.or_else(|_| u64::try_from(value).map(|number| number.min(u32::MAX as u64) as u32))
		.map_err(|_| format!("The screenshot reported an unusable {key}."))
}

fn decode(map: &CaptureMap, bytes: &[u8]) -> Result<DynamicImage, String> {
	if let Some(kind) = map.get("type").and_then(|value| String::try_from(value.clone()).ok()) {
		if kind != "raw" {
			return Err(format!("Plasma sent the screenshot as {kind}, which is not supported."));
		}
	}
	let width = number(map, "width")?;
	let height = number(map, "height")?;
	let stride = number(map, "stride")?;
	let format = number(map, "format")?;
	if width == 0 || height == 0 {
		return Err("Plasma returned an empty screenshot.".to_string());
	}
	let needed = stride as usize * (height as usize - 1) + width as usize * bytes_per_pixel(format)?;
	if bytes.len() < needed {
		return Err(format!("The screenshot arrived incomplete ({} of {needed} bytes).", bytes.len()));
	}
	to_image(bytes, width, height, stride as usize, format)
}

fn bytes_per_pixel(format: u32) -> Result<usize, String> {
	Ok(match format {
		4 | 5 | 6 | 16 | 17 | 18 | 19 | 20 | 21 | 22 => 4,
		13 | 29 => 3,
		24 => 1,
		28 => 2,
		25 | 26 | 27 | 30 | 31 | 32 => 8,
		33..=35 => 16,
		other => return Err(format!("Plasma sent the screenshot in a pixel format this helper cannot read ({other}).")),
	})
}

fn half_to_f32(bits: u16) -> f32 {
	let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
	let exponent = ((bits >> 10) & 0x1f) as i32;
	let mantissa = (bits & 0x3ff) as f32;
	match exponent {
		0 => sign * mantissa * 2f32.powi(-24),
		31 => {
			if mantissa == 0.0 {
				sign * f32::INFINITY
			} else {
				0.0
			}
		}
		_ => sign * (1.0 + mantissa / 1024.0) * 2f32.powi(exponent - 15),
	}
}

fn unit_to_byte(value: f32) -> u8 {
	let clamped = if value.is_finite() { value.clamp(0.0, 1.0) } else { 0.0 };
	let encoded = if clamped <= 0.003_130_8 { clamped * 12.92 } else { 1.055 * clamped.powf(1.0 / 2.4) - 0.055 };
	(encoded * 255.0).round() as u8
}

fn unpremultiply(pixel: [u8; 4]) -> [u8; 4] {
	let alpha = pixel[3] as u32;
	if alpha == 0 || alpha == 255 {
		return pixel;
	}
	let scale = |channel: u8| ((channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
	[scale(pixel[0]), scale(pixel[1]), scale(pixel[2]), pixel[3]]
}

fn to_image(bytes: &[u8], width: u32, height: u32, stride: usize, format: u32) -> Result<DynamicImage, String> {
	let size = bytes_per_pixel(format)?;
	let mut image = RgbaImage::new(width, height);
	let premultiplied = matches!(format, 6 | 18 | 20 | 22 | 27 | 32 | 35);
	let opaque = matches!(format, 4 | 16 | 19 | 21 | 25 | 30 | 33 | 13 | 29 | 24 | 28);
	for y in 0..height as usize {
		let row = &bytes[y * stride..];
		for x in 0..width as usize {
			let at = x * size;
			let px = &row[at..at + size];
			let u16_at = |index: usize| u16::from_le_bytes([px[index * 2], px[index * 2 + 1]]);
			let f32_at = |index: usize| f32::from_le_bytes([px[index * 4], px[index * 4 + 1], px[index * 4 + 2], px[index * 4 + 3]]);
			let mut pixel = match format {
				4..=6 => [px[2], px[1], px[0], px[3]],
				16..=18 => [px[0], px[1], px[2], px[3]],
				19 | 20 => {
					let word = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
					let ten = |shift: u32| (((word >> shift) & 0x3ff) >> 2) as u8;
					[ten(0), ten(10), ten(20), (((word >> 30) & 0x3) * 85) as u8]
				}
				21 | 22 => {
					let word = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
					let ten = |shift: u32| (((word >> shift) & 0x3ff) >> 2) as u8;
					[ten(20), ten(10), ten(0), (((word >> 30) & 0x3) * 85) as u8]
				}
				13 => [px[0], px[1], px[2], 255],
				29 => [px[2], px[1], px[0], 255],
				24 => [px[0], px[0], px[0], 255],
				28 => {
					let level = (u16_at(0) >> 8) as u8;
					[level, level, level, 255]
				}
				25..=27 => [(u16_at(0) >> 8) as u8, (u16_at(1) >> 8) as u8, (u16_at(2) >> 8) as u8, (u16_at(3) >> 8) as u8],
				30..=32 => [
					unit_to_byte(half_to_f32(u16_at(0))),
					unit_to_byte(half_to_f32(u16_at(1))),
					unit_to_byte(half_to_f32(u16_at(2))),
					(half_to_f32(u16_at(3)).clamp(0.0, 1.0) * 255.0).round() as u8,
				],
				_ => [unit_to_byte(f32_at(0)), unit_to_byte(f32_at(1)), unit_to_byte(f32_at(2)), (f32_at(3).clamp(0.0, 1.0) * 255.0).round() as u8],
			};
			if opaque {
				pixel[3] = 255;
			} else if premultiplied {
				pixel = unpremultiply(pixel);
			}
			image.put_pixel(x as u32, y as u32, image::Rgba(pixel));
		}
	}
	Ok(DynamicImage::ImageRgba8(image))
}

async fn capture_kwin_authorized(target: &Target, cursor: bool) -> Result<DynamicImage, String> {
	let first = capture_kwin(target, cursor).await;
	match first {
		Err(error) if error.starts_with("Plasma did not allow") => {
			if !ensure_authorized().await {
				refresh_desktop_database().await;
			}
			for _ in 0..3 {
				tokio::time::sleep(Duration::from_millis(700)).await;
				match capture_kwin(target, cursor).await {
					Err(again) if again.starts_with("Plasma did not allow") => continue,
					other => return other,
				}
			}
			Err(error)
		}
		other => other,
	}
}

pub async fn capture(target: Target, cursor: bool) -> Result<Shot, String> {
	match capture_kwin_authorized(&target, cursor).await {
		Ok(image) => Ok(Shot { image, source: Source::Kwin }),
		Err(kwin_error) => match capture_spectacle(&target, cursor).await {
			Ok(image) => Ok(Shot { image, source: Source::Spectacle }),
			Err(spectacle_error) => Err(format!("{kwin_error} {spectacle_error}")),
		},
	}
}

async fn capture_spectacle(target: &Target, cursor: bool) -> Result<DynamicImage, String> {
	if !paths::has_program("spectacle") {
		return Err("Spectacle is not installed either, so no screenshot could be taken.".to_string());
	}
	if let Target::Window { id: Some(_) } = target {
		return Err("Spectacle can only capture the active window, not a chosen one.".to_string());
	}
	let directory = paths::runtime_dir().join("shots");
	paths::ensure_private_dir(&directory).map_err(|error| format!("Could not prepare a place for the picture: {error}"))?;
	let file = directory.join(format!("shot-{}.png", util::new_id()));
	let path = file.to_string_lossy().to_string();
	let mut args: Vec<&str> = vec!["--background", "--nonotify", "--output", path.as_str()];
	args.push(match target {
		Target::Workspace | Target::Region { .. } => "--fullscreen",
		Target::Screen => "--current",
		Target::Window { .. } => "--activewindow",
	});
	if cursor {
		args.push("--pointer");
	}
	let captured = util::capture("spectacle", &args, Duration::from_secs(45)).await;
	let bytes = std::fs::read(&file);
	let _ = std::fs::remove_file(&file);
	if !captured.ok {
		return Err(format!("Spectacle could not take the picture either. {}", util::truncate_end(captured.text().trim(), 300)));
	}
	let bytes = bytes.map_err(|_| "Spectacle reported success but wrote no picture.".to_string())?;
	let image = image::load_from_memory(&bytes).map_err(|error| format!("The picture could not be decoded: {error}"))?;
	match target {
		Target::Region { x, y, width, height } => crop(image, *x, *y, *width, *height),
		_ => Ok(image),
	}
}

fn crop(image: DynamicImage, x: i32, y: i32, width: i32, height: i32) -> Result<DynamicImage, String> {
	let (image_width, image_height) = (image.width() as i64, image.height() as i64);
	let left = (x as i64).clamp(0, image_width);
	let top = (y as i64).clamp(0, image_height);
	let right = (x as i64 + width as i64).clamp(0, image_width);
	let bottom = (y as i64 + height as i64).clamp(0, image_height);
	if right <= left || bottom <= top {
		return Err("That area is outside the screen.".to_string());
	}
	Ok(image.crop_imm(left as u32, top as u32, (right - left) as u32, (bottom - top) as u32))
}

pub fn encode_png(image: &DynamicImage) -> Result<Vec<u8>, String> {
	let mut bytes: Vec<u8> = Vec::new();
	image
		.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
		.map_err(|error| format!("The picture could not be encoded: {error}"))?;
	Ok(bytes)
}

pub async fn state() -> ScreenState {
	if Connection::session().await.is_err() {
		return ScreenState { available: false, source: "none".into(), message: "There is no desktop session, so screenshots are unavailable.".into(), width: 0, height: 0 };
	}
	let (width, height) = workspace_size().await.unwrap_or((0, 0));
	match capture_kwin(&Target::Region { x: 0, y: 0, width: 1, height: 1 }, false).await {
		Ok(_) => ScreenState { available: true, source: "plasma".into(), message: "working through Plasma".into(), width, height },
		Err(kwin_error) => {
			if paths::has_program("spectacle") {
				ScreenState { available: true, source: "spectacle".into(), message: format!("working through Spectacle ({kwin_error})"), width, height }
			} else {
				ScreenState { available: false, source: "none".into(), message: kwin_error, width, height }
			}
		}
	}
}

pub async fn workspace_size() -> Option<(u32, u32)> {
	crate::tools::window::screen_geometry()
		.await
		.filter(|geometry| geometry.width > 0 && geometry.height > 0)
		.map(|geometry| (geometry.width as u32, geometry.height as u32))
}

pub fn restricted_interfaces() -> &'static str {
	RESTRICTED_INTERFACES
}

pub fn desktop_entry(exec: &str) -> String {
	format!(
		"[Desktop Entry]\nType=Application\nName=Ash Agent\nGenericName=AI Assistant\nComment=Chat with AI models and get help using this computer\nExec={exec}\nIcon=ash-agent\nTerminal=false\nCategories=Utility;\nKeywords=AI;assistant;chat;help;\nStartupWMClass=ash-agent\nX-KDE-DBUS-Restricted-Interfaces={RESTRICTED_INTERFACES}\n"
	)
}

fn system_entry() -> PathBuf {
	PathBuf::from("/usr/share/applications").join(DESKTOP_NAME)
}

fn user_entry() -> Option<PathBuf> {
	dirs::data_dir().map(|data| data.join("applications").join(DESKTOP_NAME))
}

fn entry_declares(path: &std::path::Path, exec: &str) -> bool {
	std::fs::read_to_string(path)
		.map(|text| {
			let declares = text.lines().any(|line| line.trim().starts_with("X-KDE-DBUS-Restricted-Interfaces=") && line.contains(RESTRICTED_INTERFACES));
			let runs = text.lines().any(|line| {
				line.trim()
					.strip_prefix("Exec=")
					.and_then(|command| command.split_whitespace().next())
					.map(|program| program == exec || paths::find_program(program).map(|found| found.to_string_lossy() == exec).unwrap_or(false))
					.unwrap_or(false)
			});
			let hidden = text.lines().any(|line| line.trim().eq_ignore_ascii_case("NoDisplay=true"));
			declares && runs && !hidden
		})
		.unwrap_or(false)
}

pub fn desktop_file_installed() -> bool {
	let exec = current_exe();
	entry_declares(&system_entry(), &exec) || user_entry().map(|path| entry_declares(&path, &exec)).unwrap_or(false)
}

fn current_exe() -> String {
	std::env::current_exe()
		.ok()
		.and_then(|path| path.canonicalize().ok())
		.map(|path| path.to_string_lossy().to_string())
		.unwrap_or_else(|| SYSTEM_BINARY.to_string())
}

pub async fn ensure_authorized() -> bool {
	let exec = current_exe();
	if entry_declares(&system_entry(), &exec) {
		return false;
	}
	let Some(path) = user_entry() else { return false };
	if entry_declares(&path, &exec) {
		return false;
	}
	if let Some(parent) = path.parent() {
		if std::fs::create_dir_all(parent).is_err() {
			return false;
		}
	}
	if std::fs::write(&path, desktop_entry(&exec)).is_err() {
		return false;
	}
	refresh_desktop_database().await;
	true
}

pub async fn refresh_desktop_database() {
	for program in ["kbuildsycoca6", "kbuildsycoca5"] {
		if paths::has_program(program) {
			let _ = util::capture(program, &[], Duration::from_secs(60)).await;
			return;
		}
	}
}
