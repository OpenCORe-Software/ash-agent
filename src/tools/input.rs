use crate::util;
use futures_util::{FutureExt, StreamExt};
use reis::event::{self, DeviceCapability, EiEvent};
use reis::ei;
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::FileExt;
use std::os::unix::net::UnixStream;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Mutex};
use xkbcommon::xkb;

const KWIN_SERVICE: &str = "org.kde.KWin";
const EIS_PATH: &str = "/org/kde/KWin/EIS/RemoteDesktop";
const EIS_INTERFACE: &str = "org.kde.KWin.EIS.RemoteDesktop";
const PORTAL_KEYBOARD: i32 = 1;
const PORTAL_POINTER: i32 = 2;
const CLIENT_NAME: &str = "Ash Agent";
const EVDEV_OFFSET: u32 = 8;
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);
const KEY_GAP: Duration = Duration::from_millis(6);
const CLICK_HOLD: Duration = Duration::from_millis(35);
const DOUBLE_GAP: Duration = Duration::from_millis(90);
const MOVE_SETTLE: Duration = Duration::from_millis(15);
const PASTE_SETTLE: Duration = Duration::from_millis(600);
const TERMINALS: &[&str] = &["konsole", "yakuake", "kitty", "alacritty", "foot", "wezterm", "xterm", "terminal", "terminator", "tilix", "ghostty", "cool-retro-term", "st-256color", "urxvt"];

static BRIDGE: OnceLock<Mutex<Option<mpsc::UnboundedSender<Job>>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Button {
	Left,
	Middle,
	Right,
}

impl Button {
	fn code(self) -> u32 {
		match self {
			Button::Left => BTN_LEFT,
			Button::Middle => BTN_MIDDLE,
			Button::Right => BTN_RIGHT,
		}
	}

	fn parse(value: Option<&str>) -> Result<Button, String> {
		match value.unwrap_or("left").trim().to_lowercase().as_str() {
			"" | "left" | "primary" => Ok(Button::Left),
			"middle" | "wheel" => Ok(Button::Middle),
			"right" | "secondary" | "context" => Ok(Button::Right),
			other => Err(format!("There is no mouse button called {other}. Use left, middle or right.")),
		}
	}
}

#[derive(Debug, Clone)]
enum MouseStep {
	Move { x: i32, y: i32 },
	Click { button: Button, at: Option<(i32, i32)> },
	DoubleClick { button: Button, at: Option<(i32, i32)> },
	Press { button: Button, at: Option<(i32, i32)> },
	Release { button: Button, at: Option<(i32, i32)> },
	Scroll { amount: i32, at: Option<(i32, i32)> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyMotion {
	Press,
	Release,
	Tap,
}

#[derive(Debug, Clone)]
struct KeyStep {
	motion: KeyMotion,
	key: String,
	modifiers: Vec<String>,
}

enum Request {
	Mouse(Vec<MouseStep>),
	Keys(Vec<KeyStep>),
	Type(String),
	Paste(String),
}

struct Job {
	request: Request,
	reply: oneshot::Sender<Result<usize, String>>,
}

#[derive(Debug, Clone)]
struct Chord {
	key: u32,
	modifiers: Vec<u32>,
}

#[derive(Debug, Clone, Copy)]
struct ModifierKeys {
	shift: u32,
	control: u32,
	alt: u32,
	logo: u32,
	level3: Option<u32>,
}

struct Layout {
	chars: HashMap<char, Chord>,
	syms: HashMap<u32, Chord>,
	modifiers: ModifierKeys,
}

fn coordinate(item: &serde_json::Value, key: &str) -> Option<i32> {
	item.get(key).and_then(|value| value.as_f64()).map(|value| value.round() as i32)
}

fn position(item: &serde_json::Value) -> Result<Option<(i32, i32)>, String> {
	match (coordinate(item, "x"), coordinate(item, "y")) {
		(Some(x), Some(y)) => Ok(Some((x, y))),
		(None, None) => Ok(None),
		_ => Err("A position needs both x and y.".to_string()),
	}
}

fn parse_mouse(actions: &serde_json::Value) -> Result<Vec<MouseStep>, String> {
	let list = actions.as_array().ok_or_else(|| "The mouse actions must be a list.".to_string())?;
	if list.is_empty() {
		return Err("There were no mouse actions to perform.".to_string());
	}
	let mut steps = Vec::with_capacity(list.len());
	for item in list {
		let action = item.get("action").and_then(|value| value.as_str()).unwrap_or("").trim().to_lowercase();
		let button = Button::parse(item.get("button").and_then(|value| value.as_str()))?;
		let at = position(item)?;
		steps.push(match action.as_str() {
			"move" => {
				let (x, y) = at.ok_or_else(|| "Moving the pointer needs x and y.".to_string())?;
				MouseStep::Move { x, y }
			}
			"click" => MouseStep::Click { button, at },
			"double_click" | "doubleclick" | "double" => MouseStep::DoubleClick { button, at },
			"press" | "down" => MouseStep::Press { button, at },
			"release" | "up" => MouseStep::Release { button, at },
			"scroll" => {
				let amount = item.get("amount").and_then(|value| value.as_f64()).map(|value| value.round() as i32).unwrap_or(1);
				if amount == 0 {
					return Err("Scrolling needs a non-zero amount.".to_string());
				}
				MouseStep::Scroll { amount: amount.clamp(-50, 50), at }
			}
			"" => return Err("Each mouse action needs an action name.".to_string()),
			other => return Err(format!("There is no mouse action called {other}.")),
		});
	}
	Ok(steps)
}

fn parse_keys(actions: &serde_json::Value) -> Result<Vec<KeyStep>, String> {
	let list = actions.as_array().ok_or_else(|| "The key actions must be a list.".to_string())?;
	if list.is_empty() {
		return Err("There were no key actions to perform.".to_string());
	}
	let mut steps = Vec::with_capacity(list.len());
	for item in list {
		let motion = match item.get("action").and_then(|value| value.as_str()).unwrap_or("tap").trim().to_lowercase().as_str() {
			"tap" | "" | "type" => KeyMotion::Tap,
			"press" | "down" => KeyMotion::Press,
			"release" | "up" => KeyMotion::Release,
			other => return Err(format!("There is no key action called {other}. Use press, release or tap.")),
		};
		let raw = item.get("key").and_then(|value| value.as_str()).unwrap_or("").trim().to_string();
		if raw.is_empty() {
			return Err("Each key action needs a key.".to_string());
		}
		let mut modifiers: Vec<String> = item
			.get("modifiers")
			.and_then(|value| value.as_array())
			.map(|list| list.iter().filter_map(|value| value.as_str()).map(|value| value.trim().to_string()).filter(|value| !value.is_empty()).collect())
			.unwrap_or_default();
		let key = if raw.len() > 1 && raw.contains('+') && !raw.ends_with("++") {
			let parts: Vec<&str> = raw.split('+').map(str::trim).filter(|part| !part.is_empty()).collect();
			let (last, rest) = parts.split_last().ok_or_else(|| format!("The key {raw} could not be understood."))?;
			modifiers.extend(rest.iter().map(|part| part.to_string()));
			last.to_string()
		} else if raw.len() > 2 && raw.ends_with("++") {
			let base = raw.trim_end_matches('+');
			modifiers.extend(base.split('+').map(str::trim).filter(|part| !part.is_empty()).map(str::to_string));
			"+".to_string()
		} else {
			raw
		};
		for modifier in &modifiers {
			modifier_name(modifier).ok_or_else(|| format!("{modifier} is not a modifier key. Use ctrl, alt, shift, super or altgr."))?;
		}
		steps.push(KeyStep { motion, key, modifiers });
	}
	Ok(steps)
}

fn modifier_name(name: &str) -> Option<&'static str> {
	match name.trim().to_lowercase().as_str() {
		"ctrl" | "control" | "control_l" | "ctl" => Some("control"),
		"shift" | "shift_l" => Some("shift"),
		"alt" | "alt_l" | "option" => Some("alt"),
		"super" | "super_l" | "meta" | "win" | "windows" | "logo" | "cmd" | "command" => Some("logo"),
		"altgr" | "alt_gr" | "iso_level3_shift" | "level3" => Some("level3"),
		_ => None,
	}
}

fn keysym_alias(name: &str) -> Option<&'static str> {
	Some(match name.to_lowercase().as_str() {
		"enter" | "return" | "ret" => "Return",
		"esc" | "escape" => "Escape",
		"backspace" | "back" | "bksp" => "BackSpace",
		"del" | "delete" => "Delete",
		"ins" | "insert" => "Insert",
		"tab" => "Tab",
		"space" | "spacebar" => "space",
		"pageup" | "pgup" | "page_up" | "prior" => "Prior",
		"pagedown" | "pgdn" | "page_down" | "next" => "Next",
		"home" => "Home",
		"end" => "End",
		"up" | "arrowup" | "uparrow" => "Up",
		"down" | "arrowdown" | "downarrow" => "Down",
		"left" | "arrowleft" | "leftarrow" => "Left",
		"right" | "arrowright" | "rightarrow" => "Right",
		"capslock" | "caps" | "caps_lock" => "Caps_Lock",
		"numlock" | "num_lock" => "Num_Lock",
		"scrolllock" | "scroll_lock" => "Scroll_Lock",
		"menu" | "apps" | "contextmenu" => "Menu",
		"print" | "printscreen" | "prtsc" | "screenshot" => "Print",
		"pause" | "break" => "Pause",
		"ctrl" | "control" => "Control_L",
		"shift" => "Shift_L",
		"alt" => "Alt_L",
		"super" | "meta" | "win" | "windows" | "logo" | "cmd" => "Super_L",
		"altgr" | "alt_gr" => "ISO_Level3_Shift",
		"plus" => "plus",
		"minus" | "dash" | "hyphen" => "minus",
		"comma" => "comma",
		"period" | "dot" => "period",
		"slash" => "slash",
		"backslash" => "backslash",
		"semicolon" => "semicolon",
		"apostrophe" | "quote" => "apostrophe",
		"grave" | "backtick" => "grave",
		"equal" | "equals" => "equal",
		"volumeup" | "volume_up" => "XF86AudioRaiseVolume",
		"volumedown" | "volume_down" => "XF86AudioLowerVolume",
		"mute" | "volumemute" => "XF86AudioMute",
		"playpause" | "play" => "XF86AudioPlay",
		"nexttrack" => "XF86AudioNext",
		"prevtrack" | "previoustrack" => "XF86AudioPrev",
		_ => return None,
	})
}

fn build_layout(keymap: &xkb::Keymap, locked: u32, group: u32) -> Layout {
	let min = keymap.min_keycode().raw();
	let max = keymap.max_keycode().raw();
	let find_modifier = |target: xkb::Keysym| -> Option<(u32, u32)> {
		for raw in min..=max {
			let code = xkb::Keycode::new(raw);
			if keymap.num_layouts_for_key(code) == 0 {
				continue;
			}
			if keymap.key_get_syms_by_level(code, group, 0).contains(&target) {
				let mut state = xkb::State::new(keymap);
				state.update_key(code, xkb::KeyDirection::Down);
				return Some((raw, state.serialize_mods(xkb::STATE_MODS_DEPRESSED)));
			}
		}
		None
	};
	let shift = find_modifier(xkb::Keysym::Shift_L).or_else(|| find_modifier(xkb::Keysym::Shift_R));
	let control = find_modifier(xkb::Keysym::Control_L).or_else(|| find_modifier(xkb::Keysym::Control_R));
	let alt = find_modifier(xkb::Keysym::Alt_L).or_else(|| find_modifier(xkb::Keysym::Meta_L));
	let logo = find_modifier(xkb::Keysym::Super_L).or_else(|| find_modifier(xkb::Keysym::Super_R));
	let level3 = find_modifier(xkb::Keysym::ISO_Level3_Shift);
	let evdev = |found: Option<(u32, u32)>, fallback: u32| found.map(|(raw, _)| raw - EVDEV_OFFSET).unwrap_or(fallback);
	let modifiers = ModifierKeys {
		shift: evdev(shift, 42),
		control: evdev(control, 29),
		alt: evdev(alt, 56),
		logo: evdev(logo, 125),
		level3: level3.map(|(raw, _)| raw - EVDEV_OFFSET),
	};
	let mut combos: Vec<(u32, Vec<u32>)> = vec![(0, Vec::new())];
	if let Some((raw, mask)) = shift {
		combos.push((mask, vec![raw - EVDEV_OFFSET]));
	}
	if let Some((raw, mask)) = level3 {
		combos.push((mask, vec![raw - EVDEV_OFFSET]));
		if let Some((shift_raw, shift_mask)) = shift {
			combos.push((mask | shift_mask, vec![shift_raw - EVDEV_OFFSET, raw - EVDEV_OFFSET]));
		}
	}
	let mut chars: HashMap<char, Chord> = HashMap::new();
	let mut syms: HashMap<u32, Chord> = HashMap::new();
	let mut state = xkb::State::new(keymap);
	for (mask, held) in &combos {
		state.update_mask(*mask, 0, locked, 0, 0, group);
		for raw in min..=max {
			let code = xkb::Keycode::new(raw);
			if keymap.num_layouts_for_key(code) == 0 {
				continue;
			}
			let sym = state.key_get_one_sym(code);
			if sym == xkb::Keysym::NoSymbol {
				continue;
			}
			let chord = Chord { key: raw - EVDEV_OFFSET, modifiers: held.clone() };
			syms.entry(sym.raw()).or_insert_with(|| chord.clone());
			let text = state.key_get_utf8(code);
			let mut letters = text.chars();
			if let (Some(letter), None) = (letters.next(), letters.next()) {
				if !letter.is_control() {
					chars.entry(letter).or_insert(chord);
				}
			}
		}
	}
	Layout { chars, syms, modifiers }
}

fn compile_keymap(text: Option<&str>) -> Result<xkb::Keymap, String> {
	let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
	if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
		if let Some(keymap) = xkb::Keymap::new_from_string(&context, text.to_string(), xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS) {
			return Ok(keymap);
		}
	}
	xkb::Keymap::new_from_names(&context, "", "", "", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
		.ok_or_else(|| "The keyboard layout could not be read, so keys cannot be typed.".to_string())
}

impl Layout {
	fn modifier(&self, name: &str) -> Result<u32, String> {
		match modifier_name(name) {
			Some("control") => Ok(self.modifiers.control),
			Some("shift") => Ok(self.modifiers.shift),
			Some("alt") => Ok(self.modifiers.alt),
			Some("logo") => Ok(self.modifiers.logo),
			Some("level3") => self.modifiers.level3.ok_or_else(|| "This keyboard layout has no AltGr key.".to_string()),
			_ => Err(format!("{name} is not a modifier key.")),
		}
	}

	fn resolve(&self, name: &str) -> Result<Chord, String> {
		let mut letters = name.chars();
		if let (Some(letter), None) = (letters.next(), letters.next()) {
			if let Some(chord) = self.chars.get(&letter) {
				return Ok(chord.clone());
			}
			let lowered: Vec<char> = letter.to_lowercase().collect();
			if lowered.len() == 1 {
				if let Some(chord) = self.chars.get(&lowered[0]) {
					return Ok(chord.clone());
				}
			}
		}
		let candidate = keysym_alias(name).unwrap_or(name);
		let mut sym = xkb::keysym_from_name(candidate, xkb::KEYSYM_NO_FLAGS);
		if sym == xkb::Keysym::NoSymbol {
			sym = xkb::keysym_from_name(candidate, xkb::KEYSYM_CASE_INSENSITIVE);
		}
		if sym == xkb::Keysym::NoSymbol {
			return Err(format!("There is no key called {name}."));
		}
		self.syms
			.get(&sym.raw())
			.cloned()
			.ok_or_else(|| format!("The key {name} is not on the current keyboard layout."))
	}

	fn for_char(&self, letter: char) -> Option<Chord> {
		match letter {
			'\n' => self.syms.get(&xkb::Keysym::Return.raw()).cloned(),
			'\t' => self.syms.get(&xkb::Keysym::Tab.raw()).cloned(),
			_ => self.chars.get(&letter).cloned(),
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Need {
	Pointer,
	Keyboard,
}

impl Need {
	fn capability(self) -> DeviceCapability {
		match self {
			Need::Pointer => DeviceCapability::PointerAbsolute,
			Need::Keyboard => DeviceCapability::Keyboard,
		}
	}
}

struct Tracked {
	device: event::Device,
	resumed: bool,
	emulating: bool,
}

struct Session {
	bus: zbus::Connection,
	connection: event::Connection,
	events: reis::tokio::EiConvertEventStream,
	cookie: i32,
	devices: Vec<Tracked>,
	sequence: u32,
	keymap_text: Option<String>,
	locked: u32,
	group: u32,
	layout: Option<Layout>,
	pressed_keys: HashSet<u32>,
	pressed_buttons: HashSet<u32>,
}

fn friendly_bus_error(error: &zbus::Error) -> String {
	let text = error.to_string();
	if std::env::var("XDG_SESSION_TYPE").map(|kind| kind == "x11").unwrap_or(false) {
		return "Controlling the mouse and keyboard needs the Plasma Wayland session. This computer is running the X11 session.".to_string();
	}
	if text.contains("UnknownObject") || text.contains("UnknownMethod") || text.contains("UnknownInterface") || text.contains("ServiceUnknown") || text.contains("NameHasNoOwner") {
		return "Plasma on this computer does not offer input control, so the mouse and keyboard cannot be operated.".to_string();
	}
	if text.contains("AccessDenied") || text.contains("NotAuthorized") || text.contains("NoAuthorized") {
		return "Plasma did not allow Ash Agent to control the mouse and keyboard.".to_string();
	}
	format!("Plasma could not start input control: {text}")
}

fn flush_result(result: Result<(), impl Into<std::io::Error>>) -> Result<(), String> {
	match result {
		Ok(()) => Ok(()),
		Err(error) => {
			let error: std::io::Error = error.into();
			if error.kind() == std::io::ErrorKind::WouldBlock {
				Ok(())
			} else {
				Err(format!("The connection to Plasma's input control broke: {error}"))
			}
		}
	}
}

impl Session {
	async fn open() -> Result<Session, String> {
		let bus = zbus::Connection::session().await.map_err(|_| "There is no desktop session to control.".to_string())?;
		let reply = bus
			.call_method(Some(KWIN_SERVICE), EIS_PATH, Some(EIS_INTERFACE), "connectToEIS", &(PORTAL_KEYBOARD | PORTAL_POINTER))
			.await
			.map_err(|error| friendly_bus_error(&error))?;
		let body = reply.body();
		let (descriptor, cookie): (zbus::zvariant::OwnedFd, i32) =
			body.deserialize().map_err(|error| format!("Plasma answered the input request unexpectedly: {error}"))?;
		let descriptor: std::os::fd::OwnedFd = descriptor.into();
		let socket = UnixStream::from(descriptor);
		let context = ei::Context::new(socket).map_err(|error| format!("The input connection could not be prepared: {error}"))?;
		let handshake = tokio::time::timeout(SETUP_TIMEOUT, context.handshake_tokio(CLIENT_NAME, ei::handshake::ContextType::Sender)).await;
		let (connection, events) = match handshake {
			Ok(Ok(pair)) => pair,
			Ok(Err(error)) => {
				let _ = bus.call_method(Some(KWIN_SERVICE), EIS_PATH, Some(EIS_INTERFACE), "disconnect", &(cookie)).await;
				return Err(format!("Plasma refused the input connection: {error}"));
			}
			Err(_) => {
				let _ = bus.call_method(Some(KWIN_SERVICE), EIS_PATH, Some(EIS_INTERFACE), "disconnect", &(cookie)).await;
				return Err("Plasma did not answer the input connection in time.".to_string());
			}
		};
		Ok(Session {
			bus,
			connection,
			events,
			cookie,
			devices: Vec::new(),
			sequence: 1,
			keymap_text: None,
			locked: 0,
			group: 0,
			layout: None,
			pressed_keys: HashSet::new(),
			pressed_buttons: HashSet::new(),
		})
	}

	async fn close(mut self) {
		self.release_all();
		self.connection.connection().disconnect();
		let _ = self.connection.flush();
		let _ = self.bus.call_method(Some(KWIN_SERVICE), EIS_PATH, Some(EIS_INTERFACE), "disconnect", &(self.cookie)).await;
	}

	fn flush(&self) -> Result<(), String> {
		flush_result(self.connection.flush())
	}

	fn handle(&mut self, item: EiEvent) -> Result<(), String> {
		match item {
			EiEvent::SeatAdded(added) => {
				added.seat.bind_capabilities(DeviceCapability::PointerAbsolute | DeviceCapability::Button | DeviceCapability::Scroll | DeviceCapability::Keyboard);
			}
			EiEvent::SeatRemoved(_) => {
				self.devices.clear();
				self.pressed_keys.clear();
				self.pressed_buttons.clear();
			}
			EiEvent::DeviceAdded(added) => {
				if added.device.device().version() >= 3 {
					added.device.device().ready();
				}
				if let Some(keymap) = added.device.keymap() {
					if let Some(text) = read_keymap(keymap) {
						self.keymap_text = Some(text);
						self.layout = None;
					}
				}
				self.devices.retain(|tracked| tracked.device != added.device);
				self.devices.push(Tracked { device: added.device, resumed: false, emulating: false });
			}
			EiEvent::DeviceRemoved(removed) => {
				if removed.device.has_capability(DeviceCapability::Keyboard) {
					self.pressed_keys.clear();
				}
				if removed.device.has_capability(DeviceCapability::Button) {
					self.pressed_buttons.clear();
				}
				self.devices.retain(|tracked| tracked.device != removed.device);
			}
			EiEvent::DeviceResumed(resumed) => {
				let serial = self.connection.serial();
				let sequence = self.sequence;
				let mut started = false;
				if let Some(tracked) = self.devices.iter_mut().find(|tracked| tracked.device == resumed.device) {
					tracked.resumed = true;
					if !tracked.emulating {
						tracked.device.device().start_emulating(serial, sequence);
						tracked.emulating = true;
						started = true;
					}
				}
				if started {
					self.sequence = self.sequence.wrapping_add(1).max(1);
				}
			}
			EiEvent::DevicePaused(paused) => {
				if let Some(tracked) = self.devices.iter_mut().find(|tracked| tracked.device == paused.device) {
					tracked.resumed = false;
					tracked.emulating = false;
				}
				if paused.device.has_capability(DeviceCapability::Keyboard) {
					self.pressed_keys.clear();
				}
				if paused.device.has_capability(DeviceCapability::Button) {
					self.pressed_buttons.clear();
				}
			}
			EiEvent::KeyboardModifiers(modifiers) => {
				if modifiers.locked != self.locked || modifiers.group != self.group {
					self.locked = modifiers.locked;
					self.group = modifiers.group;
					self.layout = None;
				}
			}
			EiEvent::Disconnected(disconnected) => {
				return Err(match disconnected.explanation {
					Some(reason) if !reason.trim().is_empty() => format!("Plasma ended input control: {reason}"),
					_ => "Plasma ended input control.".to_string(),
				});
			}
			_ => {}
		}
		self.flush()
	}

	fn drain(&mut self) -> Result<(), String> {
		while let Some(item) = self.events.next().now_or_never() {
			match item {
				Some(Ok(event)) => self.handle(event)?,
				Some(Err(error)) => return Err(format!("The input connection failed: {error}")),
				None => return Err("Plasma closed the input connection.".to_string()),
			}
		}
		Ok(())
	}

	fn ready_device(&self, need: Need) -> Option<event::Device> {
		self.devices
			.iter()
			.find(|tracked| tracked.resumed && tracked.emulating && tracked.device.has_capability(need.capability()))
			.map(|tracked| tracked.device.clone())
	}

	async fn wait_for(&mut self, need: Need) -> Result<event::Device, String> {
		self.drain()?;
		if let Some(device) = self.ready_device(need) {
			return Ok(device);
		}
		let deadline = tokio::time::Instant::now() + SETUP_TIMEOUT;
		loop {
			let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
			if remaining.is_zero() {
				return Err(match need {
					Need::Pointer => "Plasma did not provide a pointer to control.".to_string(),
					Need::Keyboard => "Plasma did not provide a keyboard to control.".to_string(),
				});
			}
			match tokio::time::timeout(remaining, self.events.next()).await {
				Ok(Some(Ok(event))) => self.handle(event)?,
				Ok(Some(Err(error))) => return Err(format!("The input connection failed: {error}")),
				Ok(None) => return Err("Plasma closed the input connection.".to_string()),
				Err(_) => {}
			}
			if let Some(device) = self.ready_device(need) {
				return Ok(device);
			}
		}
	}

	fn frame(&self, device: &event::Device) -> Result<(), String> {
		device.device().frame(self.connection.serial(), util::monotonic_us());
		self.flush()
	}

	fn layout(&mut self) -> Result<&Layout, String> {
		if self.layout.is_none() {
			let keymap = compile_keymap(self.keymap_text.as_deref())?;
			self.layout = Some(build_layout(&keymap, self.locked, self.group));
		}
		self.layout.as_ref().ok_or_else(|| "The keyboard layout could not be read.".to_string())
	}

	async fn move_to(&mut self, x: i32, y: i32) -> Result<(), String> {
		let device = self.wait_for(Need::Pointer).await?;
		let regions = device.regions();
		if !regions.is_empty() {
			let inside = regions.iter().any(|region| {
				x >= region.x as i32 && y >= region.y as i32 && x < (region.x + region.width) as i32 && y < (region.y + region.height) as i32
			});
			if !inside {
				let right = regions.iter().map(|region| region.x + region.width).max().unwrap_or(0);
				let bottom = regions.iter().map(|region| region.y + region.height).max().unwrap_or(0);
				return Err(format!("The point {x},{y} is outside the screen, which spans 0,0 to {},{}.", right.saturating_sub(1), bottom.saturating_sub(1)));
			}
		}
		let pointer = device.interface::<ei::PointerAbsolute>().ok_or_else(|| "Plasma's pointer cannot be positioned.".to_string())?;
		pointer.motion_absolute(x as f32, y as f32);
		self.frame(&device)?;
		tokio::time::sleep(MOVE_SETTLE).await;
		Ok(())
	}

	async fn button(&mut self, button: Button, press: bool) -> Result<(), String> {
		let device = self.wait_for(Need::Pointer).await?;
		let buttons = device.interface::<ei::Button>().ok_or_else(|| "Plasma's pointer has no buttons to press.".to_string())?;
		let code = button.code();
		if press && self.pressed_buttons.contains(&code) {
			return Ok(());
		}
		if !press && !self.pressed_buttons.contains(&code) {
			return Ok(());
		}
		buttons.button(code, if press { ei::button::ButtonState::Press } else { ei::button::ButtonState::Released });
		self.frame(&device)?;
		if press {
			self.pressed_buttons.insert(code);
		} else {
			self.pressed_buttons.remove(&code);
		}
		Ok(())
	}

	async fn click(&mut self, button: Button) -> Result<(), String> {
		self.button(button, true).await?;
		tokio::time::sleep(CLICK_HOLD).await;
		self.button(button, false).await
	}

	async fn scroll(&mut self, amount: i32) -> Result<(), String> {
		let device = self.wait_for(Need::Pointer).await?;
		let wheel = device.interface::<ei::Scroll>().ok_or_else(|| "Plasma's pointer cannot scroll.".to_string())?;
		let direction = amount.signum();
		for _ in 0..amount.abs() {
			wheel.scroll_discrete(0, direction * 120);
			self.frame(&device)?;
			tokio::time::sleep(Duration::from_millis(25)).await;
		}
		Ok(())
	}

	async fn key(&mut self, code: u32, press: bool) -> Result<(), String> {
		let device = self.wait_for(Need::Keyboard).await?;
		let keyboard = device.interface::<ei::Keyboard>().ok_or_else(|| "Plasma's keyboard cannot type.".to_string())?;
		if press == self.pressed_keys.contains(&code) {
			return Ok(());
		}
		keyboard.key(code, if press { ei::keyboard::KeyState::Press } else { ei::keyboard::KeyState::Released });
		self.frame(&device)?;
		if press {
			self.pressed_keys.insert(code);
		} else {
			self.pressed_keys.remove(&code);
		}
		tokio::time::sleep(KEY_GAP).await;
		Ok(())
	}

	async fn chord_down(&mut self, chord: &Chord) -> Result<(), String> {
		for modifier in &chord.modifiers {
			self.key(*modifier, true).await?;
		}
		self.key(chord.key, true).await
	}

	async fn chord_up(&mut self, chord: &Chord) -> Result<(), String> {
		self.key(chord.key, false).await?;
		for modifier in chord.modifiers.iter().rev() {
			self.key(*modifier, false).await?;
		}
		Ok(())
	}

	async fn tap(&mut self, chord: &Chord) -> Result<(), String> {
		let outcome = self.chord_down(chord).await;
		let released = self.chord_up(chord).await;
		outcome.and(released)
	}

	fn release_all(&mut self) {
		let keyboard = self.ready_device(Need::Keyboard);
		let pointer = self.ready_device(Need::Pointer);
		if let Some(device) = keyboard {
			if let Some(keys) = device.interface::<ei::Keyboard>() {
				for code in self.pressed_keys.drain() {
					keys.key(code, ei::keyboard::KeyState::Released);
				}
				device.device().frame(self.connection.serial(), util::monotonic_us());
			}
		}
		if let Some(device) = pointer {
			if let Some(buttons) = device.interface::<ei::Button>() {
				for code in self.pressed_buttons.drain() {
					buttons.button(code, ei::button::ButtonState::Released);
				}
				device.device().frame(self.connection.serial(), util::monotonic_us());
			}
		}
		self.pressed_keys.clear();
		self.pressed_buttons.clear();
		let _ = self.flush();
	}

	async fn paste_chord(&mut self) -> Result<Chord, String> {
		let terminal = crate::tools::window::active()
			.await
			.map(|window| {
				let haystack = format!("{} {}", window.application, window.title).to_lowercase();
				TERMINALS.iter().any(|name| haystack.contains(name))
			})
			.unwrap_or(false);
		let layout = self.layout()?;
		let base = layout.resolve("v")?;
		let mut modifiers = vec![layout.modifiers.control];
		if terminal {
			modifiers.push(layout.modifiers.shift);
		}
		for code in base.modifiers {
			if !modifiers_contains(&modifiers, code) {
				modifiers.push(code);
			}
		}
		Ok(Chord { key: base.key, modifiers })
	}

	async fn paste(&mut self, text: &str, cancelled: &dyn Fn() -> bool) -> Result<(), String> {
		let previous = crate::tools::clipboard::write_with_restore(text).await?;
		if cancelled() {
			crate::tools::clipboard::restore(&previous).await;
			return Err("The typing was stopped.".to_string());
		}
		let chord = self.paste_chord().await;
		let outcome = match chord {
			Ok(chord) => self.tap(&chord).await,
			Err(error) => Err(error),
		};
		tokio::time::sleep(PASTE_SETTLE).await;
		crate::tools::clipboard::restore(&previous).await;
		outcome
	}

	async fn run_mouse(&mut self, steps: Vec<MouseStep>, cancelled: &dyn Fn() -> bool) -> Result<usize, String> {
		let mut done = 0usize;
		for step in steps {
			if cancelled() {
				return Err("The pointer actions were stopped.".to_string());
			}
			match step {
				MouseStep::Move { x, y } => self.move_to(x, y).await?,
				MouseStep::Click { button, at } => {
					if let Some((x, y)) = at {
						self.move_to(x, y).await?;
					}
					self.click(button).await?;
				}
				MouseStep::DoubleClick { button, at } => {
					if let Some((x, y)) = at {
						self.move_to(x, y).await?;
					}
					self.click(button).await?;
					tokio::time::sleep(DOUBLE_GAP).await;
					self.click(button).await?;
				}
				MouseStep::Press { button, at } => {
					if let Some((x, y)) = at {
						self.move_to(x, y).await?;
					}
					self.button(button, true).await?;
				}
				MouseStep::Release { button, at } => {
					if let Some((x, y)) = at {
						self.move_to(x, y).await?;
					}
					self.button(button, false).await?;
				}
				MouseStep::Scroll { amount, at } => {
					if let Some((x, y)) = at {
						self.move_to(x, y).await?;
					}
					self.scroll(amount).await?;
				}
			}
			done += 1;
		}
		Ok(done)
	}

	async fn run_keys(&mut self, steps: Vec<KeyStep>, cancelled: &dyn Fn() -> bool) -> Result<usize, String> {
		let mut resolved: Vec<(KeyMotion, Chord)> = Vec::with_capacity(steps.len());
		{
			let layout = self.layout()?;
			for step in &steps {
				let base = layout.resolve(&step.key)?;
				let mut modifiers: Vec<u32> = Vec::new();
				for name in &step.modifiers {
					let code = layout.modifier(name)?;
					if !modifiers_contains(&modifiers, code) {
						modifiers.push(code);
					}
				}
				for code in base.modifiers {
					if !modifiers_contains(&modifiers, code) {
						modifiers.push(code);
					}
				}
				resolved.push((step.motion, Chord { key: base.key, modifiers }));
			}
		}
		let mut done = 0usize;
		for (motion, chord) in resolved {
			if cancelled() {
				return Err("The key presses were stopped.".to_string());
			}
			match motion {
				KeyMotion::Tap => self.tap(&chord).await?,
				KeyMotion::Press => self.chord_down(&chord).await?,
				KeyMotion::Release => self.chord_up(&chord).await?,
			}
			done += 1;
		}
		Ok(done)
	}

	async fn run_type(&mut self, text: &str, cancelled: &dyn Fn() -> bool) -> Result<usize, String> {
		let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
		enum Piece {
			Keys(Vec<Chord>),
			Paste(String),
		}
		let mut pieces: Vec<Piece> = Vec::new();
		{
			let layout = self.layout()?;
			for letter in normalized.chars() {
				match layout.for_char(letter) {
					Some(chord) => match pieces.last_mut() {
						Some(Piece::Keys(chords)) => chords.push(chord),
						_ => pieces.push(Piece::Keys(vec![chord])),
					},
					None => match pieces.last_mut() {
						Some(Piece::Paste(buffer)) => buffer.push(letter),
						_ => pieces.push(Piece::Paste(letter.to_string())),
					},
				}
			}
		}
		let mut typed = 0usize;
		for piece in pieces {
			match piece {
				Piece::Keys(chords) => {
					for chord in chords {
						if cancelled() {
							return Err("The typing was stopped.".to_string());
						}
						self.tap(&chord).await?;
						typed += 1;
					}
				}
				Piece::Paste(buffer) => {
					self.wait_for(Need::Keyboard).await?;
					self.paste(&buffer, cancelled).await?;
					typed += buffer.chars().count();
				}
			}
		}
		Ok(typed)
	}

	async fn run(&mut self, request: Request, cancelled: &dyn Fn() -> bool) -> Result<usize, String> {
		let outcome = match request {
			Request::Mouse(steps) => self.run_mouse(steps, cancelled).await,
			Request::Keys(steps) => self.run_keys(steps, cancelled).await,
			Request::Type(text) => self.run_type(&text, cancelled).await,
			Request::Paste(text) => {
				self.wait_for(Need::Keyboard).await?;
				self.paste(&text, cancelled).await.map(|_| text.chars().count())
			}
		};
		if outcome.is_err() {
			self.release_all();
		}
		outcome
	}
}

fn modifiers_contains(list: &[u32], code: u32) -> bool {
	list.contains(&code)
}

fn read_keymap(keymap: &event::Keymap) -> Option<String> {
	if keymap.size == 0 {
		return None;
	}
	let file = std::fs::File::from(keymap.fd.try_clone().ok()?);
	let mut buffer = vec![0u8; keymap.size as usize];
	file.read_exact_at(&mut buffer, 0).ok()?;
	while buffer.last() == Some(&0) {
		buffer.pop();
	}
	String::from_utf8(buffer).ok()
}

enum Wake {
	Job(Option<Job>),
	Event(Result<EiEvent, String>),
}

async fn next_event(session: &mut Option<Session>) -> Result<EiEvent, String> {
	match session {
		Some(active) => match active.events.next().await {
			Some(Ok(event)) => Ok(event),
			Some(Err(error)) => Err(format!("The input connection failed: {error}")),
			None => Err("Plasma closed the input connection.".to_string()),
		},
		None => std::future::pending().await,
	}
}

async fn bridge(mut jobs: mpsc::UnboundedReceiver<Job>) {
	let mut session: Option<Session> = None;
	loop {
		let wake = tokio::select! {
			job = jobs.recv() => Wake::Job(job),
			event = next_event(&mut session) => Wake::Event(event),
		};
		match wake {
			Wake::Job(None) => break,
			Wake::Job(Some(job)) => {
				if session.is_none() {
					match Session::open().await {
						Ok(opened) => session = Some(opened),
						Err(error) => {
							let _ = job.reply.send(Err(error));
							continue;
						}
					}
				}
				let Some(active) = session.as_mut() else { continue };
				let reply = job.reply;
				let outcome = {
					let watcher = &reply;
					let cancelled = || watcher.is_closed();
					active.run(job.request, &cancelled).await
				};
				let broken = outcome.as_ref().err().is_some_and(|error| error.starts_with("Plasma closed") || error.starts_with("Plasma ended") || error.starts_with("The input connection failed") || error.starts_with("The connection to Plasma"));
				if broken {
					if let Some(stale) = session.take() {
						stale.close().await;
					}
				}
				let _ = reply.send(outcome);
			}
			Wake::Event(Ok(event)) => {
				let failed = session.as_mut().map(|active| active.handle(event).is_err()).unwrap_or(false);
				if failed {
					if let Some(stale) = session.take() {
						stale.close().await;
					}
				}
			}
			Wake::Event(Err(_)) => {
				if let Some(stale) = session.take() {
					stale.close().await;
				}
			}
		}
	}
	if let Some(active) = session.take() {
		active.close().await;
	}
}

fn start_bridge(receiver: mpsc::UnboundedReceiver<Job>) -> Result<(), String> {
	std::thread::Builder::new()
		.name("ash-input".to_string())
		.spawn(move || {
			let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
			if let Ok(runtime) = runtime {
				runtime.block_on(bridge(receiver));
			}
		})
		.map(|_| ())
		.map_err(|error| format!("Input control could not start: {error}"))
}

async fn submit(request: Request, budget: Duration) -> Result<usize, String> {
	let slot = BRIDGE.get_or_init(|| Mutex::new(None));
	let (reply, answer) = oneshot::channel();
	let mut job = Job { request, reply };
	{
		let mut guard = slot.lock().await;
		for _ in 0..2 {
			let sender = match guard.as_ref() {
				Some(sender) if !sender.is_closed() => sender.clone(),
				_ => {
					let (sender, receiver) = mpsc::unbounded_channel();
					start_bridge(receiver)?;
					*guard = Some(sender.clone());
					sender
				}
			};
			match sender.send(job) {
				Ok(()) => break,
				Err(mpsc::error::SendError(returned)) => {
					*guard = None;
					job = returned;
				}
			}
		}
	}
	match tokio::time::timeout(budget, answer).await {
		Ok(Ok(outcome)) => outcome,
		Ok(Err(_)) => Err("Input control stopped unexpectedly.".to_string()),
		Err(_) => Err("Input control took too long and was stopped.".to_string()),
	}
}

fn budget_for(units: usize) -> Duration {
	Duration::from_secs(30) + Duration::from_millis(80 * units as u64)
}

pub async fn mouse(actions: serde_json::Value) -> Result<usize, String> {
	let steps = parse_mouse(&actions)?;
	let units = steps
		.iter()
		.map(|step| match step {
			MouseStep::Scroll { amount, .. } => amount.unsigned_abs() as usize + 1,
			_ => 2,
		})
		.sum();
	submit(Request::Mouse(steps), budget_for(units)).await
}

pub async fn keyboard(actions: serde_json::Value) -> Result<usize, String> {
	let steps = parse_keys(&actions)?;
	let units = steps.iter().map(|step| step.modifiers.len() * 2 + 2).sum();
	submit(Request::Keys(steps), budget_for(units)).await
}

pub async fn type_keys(text: &str) -> Result<(), String> {
	if text.is_empty() {
		return Err("There is no text to type.".to_string());
	}
	let units = text.chars().count() * 4;
	submit(Request::Type(text.to_string()), budget_for(units)).await.map(|_| ())
}

pub async fn type_clipboard(text: &str) -> Result<(), String> {
	if text.is_empty() {
		return Err("There is no text to type.".to_string());
	}
	submit(Request::Paste(text.to_string()), budget_for(16)).await.map(|_| ())
}

pub async fn available() -> Result<(), String> {
	if std::env::var("XDG_SESSION_TYPE").map(|kind| kind == "x11").unwrap_or(false) {
		return Err("controlling the mouse and keyboard needs the Plasma Wayland session".to_string());
	}
	let bus = zbus::Connection::session().await.map_err(|_| "there is no desktop session".to_string())?;
	let reply = tokio::time::timeout(Duration::from_secs(4), bus.call_method(Some(KWIN_SERVICE), EIS_PATH, Some("org.freedesktop.DBus.Introspectable"), "Introspect", &()))
		.await
		.map_err(|_| "Plasma did not answer".to_string())?
		.map_err(|error| friendly_bus_error(&error))?;
	let xml: String = reply.body().deserialize().map_err(|_| "Plasma answered unexpectedly".to_string())?;
	if xml.contains(EIS_INTERFACE) {
		Ok(())
	} else {
		Err("Plasma on this computer does not offer input control".to_string())
	}
}
