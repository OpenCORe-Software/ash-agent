use crate::paths;
use crate::util;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::oneshot;

const KWIN_SERVICE: &str = "org.kde.KWin";
const SCRIPTING_PATH: &str = "/Scripting";
const SCRIPTING_INTERFACE: &str = "org.kde.kwin.Scripting";
const SCRIPT_INTERFACE: &str = "org.kde.kwin.Script";
const REPLY_PATH: &str = "/one/opencore/AshAgent/Script";
const REPLY_INTERFACE: &str = "one.opencore.AshAgent.ScriptReply";
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(8);

static HOST: tokio::sync::OnceCell<zbus::Connection> = tokio::sync::OnceCell::const_new();
static PENDING: OnceLock<std::sync::Mutex<HashMap<String, oneshot::Sender<String>>>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
	pub id: String,
	pub title: String,
	pub application: String,
	pub pid: i64,
	pub desktop: String,
	pub active: bool,
	pub minimized: bool,
	pub maximized: bool,
	pub full_screen: bool,
	pub x: i32,
	pub y: i32,
	pub width: i32,
	pub height: i32,
	pub client_x: i32,
	pub client_y: i32,
	pub client_width: i32,
	pub client_height: i32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Geometry {
	pub x: i32,
	pub y: i32,
	pub width: i32,
	pub height: i32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Screen {
	pub name: String,
	pub manufacturer: String,
	pub model: String,
	pub x: i32,
	pub y: i32,
	pub width: i32,
	pub height: i32,
	pub scale: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ScreenLayout {
	pub screens: Vec<Screen>,
	pub active: String,
	pub width: i32,
	pub height: i32,
}

struct Reporter;

#[zbus::interface(name = "one.opencore.AshAgent.ScriptReply")]
impl Reporter {
	async fn report(&self, token: String, payload: String) {
		let sender = pending().lock().ok().and_then(|mut waiting| waiting.remove(&token));
		if let Some(sender) = sender {
			let _ = sender.send(payload);
		}
	}
}

fn pending() -> &'static std::sync::Mutex<HashMap<String, oneshot::Sender<String>>> {
	PENDING.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

async fn host() -> Result<&'static zbus::Connection, String> {
	HOST.get_or_try_init(|| async {
		zbus::connection::Builder::session()
			.map_err(|_| "There is no desktop session to ask.".to_string())?
			.serve_at(REPLY_PATH, Reporter)
			.map_err(|error| format!("Could not prepare to hear from Plasma: {error}"))?
			.build()
			.await
			.map_err(|_| "There is no desktop session to ask.".to_string())
	})
	.await
}

fn json_string(value: &str) -> String {
	serde_json::Value::String(value.to_string()).to_string()
}

fn wrap(service: &str, token: &str, body: &str) -> String {
	format!(
		"(function () {{\n\tvar send = function (value) {{ callDBus({service}, {path}, {interface}, \"Report\", {token}, JSON.stringify(value)); }};\n\ttry {{\n\t\tvar result = (function () {{\n{body}\n\t\t}})();\n\t\tsend({{ ok: true, value: result === undefined ? null : result }});\n\t}} catch (error) {{\n\t\tsend({{ ok: false, error: String(error) }});\n\t}}\n}})();\n",
		service = json_string(service),
		path = json_string(REPLY_PATH),
		interface = json_string(REPLY_INTERFACE),
		token = json_string(token),
	)
}

fn friendly(error: &zbus::Error) -> String {
	let text = error.to_string();
	if text.contains("ServiceUnknown") || text.contains("NameHasNoOwner") || text.contains("UnknownObject") {
		"Plasma's window manager is not running, so windows cannot be inspected or managed.".to_string()
	} else {
		format!("Plasma would not run the window script: {text}")
	}
}

async fn run_script(body: &str) -> Result<serde_json::Value, String> {
	let connection = host().await?;
	let service = connection.unique_name().map(|name| name.to_string()).ok_or_else(|| "The desktop session connection has no name.".to_string())?;
	let token = util::new_id();
	let plugin = format!("ash-agent-{token}");
	let directory = paths::runtime_dir().join("kwin");
	paths::ensure_private_dir(&directory).map_err(|error| format!("Could not prepare the script directory: {error}"))?;
	let file = directory.join(format!("{plugin}.js"));
	std::fs::write(&file, wrap(&service, &token, body)).map_err(|error| format!("Could not write the window script: {error}"))?;
	let (sender, receiver) = oneshot::channel();
	if let Ok(mut waiting) = pending().lock() {
		waiting.insert(token.clone(), sender);
	}
	let outcome = execute(connection, &file, &plugin, receiver).await;
	if let Ok(mut waiting) = pending().lock() {
		waiting.remove(&token);
	}
	let _ = connection
		.call_method(Some(KWIN_SERVICE), SCRIPTING_PATH, Some(SCRIPTING_INTERFACE), "unloadScript", &(plugin.as_str()))
		.await;
	let _ = std::fs::remove_file(&file);
	let payload = outcome?;
	let value: serde_json::Value = serde_json::from_str(&payload).map_err(|error| format!("Plasma answered unexpectedly: {error}"))?;
	if value.get("ok").and_then(|ok| ok.as_bool()).unwrap_or(false) {
		Ok(value.get("value").cloned().unwrap_or(serde_json::Value::Null))
	} else {
		Err(format!(
			"The window script failed inside Plasma: {}",
			value.get("error").and_then(|error| error.as_str()).unwrap_or("unknown error")
		))
	}
}

async fn execute(connection: &zbus::Connection, file: &std::path::Path, plugin: &str, receiver: oneshot::Receiver<String>) -> Result<String, String> {
	let path = file.to_string_lossy().to_string();
	let reply = connection
		.call_method(Some(KWIN_SERVICE), SCRIPTING_PATH, Some(SCRIPTING_INTERFACE), "loadScript", &(path.as_str(), plugin))
		.await
		.map_err(|error| friendly(&error))?;
	let id: i32 = reply.body().deserialize().map_err(|error| format!("Plasma answered unexpectedly: {error}"))?;
	if id < 0 {
		return Err("Plasma refused to load the window script.".to_string());
	}
	let object = format!("{SCRIPTING_PATH}/Script{id}");
	connection
		.call_method(Some(KWIN_SERVICE), object.as_str(), Some(SCRIPT_INTERFACE), "run", &())
		.await
		.map_err(|error| friendly(&error))?;
	match tokio::time::timeout(SCRIPT_TIMEOUT, receiver).await {
		Ok(Ok(payload)) => Ok(payload),
		Ok(Err(_)) => Err("The window script ended without answering.".to_string()),
		Err(_) => Err("Plasma did not answer the window script in time.".to_string()),
	}
}

const LIST_BODY: &str = r#"
var out = [];
var list = workspace.windowList();
for (var i = 0; i < list.length; i++) {
	var w = list[i];
	if (w.deleted || !w.managed) continue;
	if (!w.normalWindow && !w.dialog) continue;
	var g = w.frameGeometry;
	var c = w.clientGeometry || g;
	var desktops = w.desktops || [];
	out.push({
		id: String(w.internalId),
		title: String(w.caption || ""),
		application: String(w.resourceClass || w.desktopFileName || ""),
		pid: Number(w.pid || 0),
		desktop: w.onAllDesktops ? "all" : (desktops.length ? String(desktops[0].name || "") : ""),
		active: w === workspace.activeWindow,
		minimized: !!w.minimized,
		maximized: Number(w.maximizeMode || 0) === 3,
		fullScreen: !!w.fullScreen,
		x: Math.round(g.x),
		y: Math.round(g.y),
		width: Math.round(g.width),
		height: Math.round(g.height),
		clientX: Math.round(c.x),
		clientY: Math.round(c.y),
		clientWidth: Math.round(c.width),
		clientHeight: Math.round(c.height)
	});
}
return out;
"#;

const GEOMETRY_BODY: &str = r#"
var g = workspace.virtualScreenGeometry;
return { x: Math.round(g.x), y: Math.round(g.y), width: Math.round(g.width), height: Math.round(g.height) };
"#;

const SCREENS_BODY: &str = r#"
var out = [];
var list = workspace.screens;
for (var i = 0; i < list.length; i++) {
	var o = list[i];
	var g = o.geometry;
	out.push({ name: o.name || "", manufacturer: o.manufacturer || "", model: o.model || "", x: Math.round(g.x), y: Math.round(g.y), width: Math.round(g.width), height: Math.round(g.height), scale: o.devicePixelRatio || 1 });
}
var v = workspace.virtualScreenGeometry;
return { screens: out, active: workspace.activeScreen ? (workspace.activeScreen.name || "") : "", width: Math.round(v.width), height: Math.round(v.height) };
"#;

fn action_body(action: &str, id: &str, title: &str, x: i32, y: i32, width: i32, height: i32) -> Result<String, String> {
	if action == "resize" && (width <= 0 || height <= 0) {
		return Err("Resizing a window needs a width and a height larger than zero.".to_string());
	}
	let operation = match action {
		"activate" | "focus" => "if (w.minimized) w.minimized = false; workspace.activeWindow = w;".to_string(),
		"minimize" => "if (!w.minimizable) return false; w.minimized = true;".to_string(),
		"maximize" => "if (!w.maximizable) return false; if (w.minimized) w.minimized = false; w.setMaximize(true, true);".to_string(),
		"restore" => "if (w.fullScreen) w.fullScreen = false; if (w.minimized) w.minimized = false; if (w.maximizable) w.setMaximize(false, false);".to_string(),
		"close" => "if (!w.closeable) return false; w.closeWindow();".to_string(),
		"move" => format!("if (!w.moveable) return false; var g = w.frameGeometry; w.frameGeometry = {{ x: {x}, y: {y}, width: g.width, height: g.height }};"),
		"resize" => format!("if (!w.resizeable) return false; var g = w.frameGeometry; w.frameGeometry = {{ x: g.x, y: g.y, width: {width}, height: {height} }};"),
		other => return Err(format!("There is no window action called {other}.")),
	};
	let single = matches!(action, "activate" | "focus" | "close" | "move" | "resize");
	Ok(format!(
		r#"
var wantId = {id};
var wantTitle = {title}.toLowerCase();
var applied = 0;
var matched = 0;
var list = workspace.windowList();
var apply = function (w) {{ {operation} return true; }};
for (var i = 0; i < list.length; i++) {{
	var w = list[i];
	if (w.deleted || !w.managed) continue;
	if (!w.normalWindow && !w.dialog) continue;
	var hit = wantId.length ? String(w.internalId) === wantId : String(w.caption || "").toLowerCase().indexOf(wantTitle) !== -1;
	if (!hit) continue;
	matched++;
	if (apply(w) !== false) applied++;
	if ({single}) break;
}}
return {{ applied: applied, matched: matched }};
"#,
		id = json_string(id),
		title = json_string(title),
	))
}

pub async fn list() -> Result<Vec<Window>, String> {
	let value = run_script(LIST_BODY).await?;
	serde_json::from_value::<Vec<Window>>(value).map_err(|error| format!("The window list came back in an unexpected form: {error}"))
}

pub async fn act(action: &str, id: &str, title: &str, x: i32, y: i32, width: i32, height: i32) -> Result<usize, String> {
	let action = action.trim().to_lowercase();
	let id = id.trim();
	let title = title.trim();
	if id.is_empty() && title.is_empty() {
		return Err("Which window? Give an id from list_windows or part of its title.".to_string());
	}
	let body = action_body(&action, id, title, x, y, width, height)?;
	let value = run_script(&body).await?;
	let matched = value.get("matched").and_then(|count| count.as_u64()).unwrap_or(0);
	let applied = value.get("applied").and_then(|count| count.as_u64()).unwrap_or(0) as usize;
	if matched > 0 && applied == 0 {
		return Err(format!("That window does not allow being told to {action}."));
	}
	Ok(applied)
}

pub async fn active() -> Option<Window> {
	list().await.ok().and_then(|windows| windows.into_iter().find(|window| window.active))
}

pub async fn screen_geometry() -> Option<Geometry> {
	run_script(GEOMETRY_BODY).await.ok().and_then(|value| serde_json::from_value::<Geometry>(value).ok())
}

pub async fn screen_layout() -> Option<ScreenLayout> {
	run_script(SCREENS_BODY).await.ok().and_then(|value| serde_json::from_value::<ScreenLayout>(value).ok())
}

pub async fn scripting_available() -> bool {
	match host().await {
		Ok(connection) => connection
			.call_method(Some(KWIN_SERVICE), SCRIPTING_PATH, Some("org.freedesktop.DBus.Introspectable"), "Introspect", &())
			.await
			.is_ok(),
		Err(_) => false,
	}
}
