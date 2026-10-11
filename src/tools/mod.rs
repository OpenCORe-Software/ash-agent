use crate::paths;
use crate::settings::Tier;
use crate::util;
use serde::Serialize;
use std::time::Duration;

pub mod access;
pub mod clipboard;
pub mod input;
pub mod pty;
pub mod root;
pub mod screen;
pub mod shell;
pub mod status;
pub mod window;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
	Low,
	Medium,
	High,
	Danger,
}

impl Risk {
	pub fn label(self) -> &'static str {
		match self {
			Risk::Low => "low",
			Risk::Medium => "medium",
			Risk::High => "high",
			Risk::Danger => "danger",
		}
	}

	pub fn from_label(label: &str) -> Risk {
		match label {
			"medium" => Risk::Medium,
			"high" => Risk::High,
			"danger" => Risk::Danger,
			_ => Risk::Low,
		}
	}
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSpec {
	pub name: String,
	pub description: String,
	pub group: String,
	pub parameters: serde_json::Value,
	pub mutating: bool,
	pub risk: Risk,
	pub root: bool,
}

#[derive(Debug, Clone)]
pub struct ToolCall {
	pub name: String,
	pub arguments: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct Outcome {
	pub title: String,
	pub explanation: String,
	pub detail: String,
	pub output: String,
}

pub fn requires_approval(spec: &ToolSpec, tier: Tier) -> bool {
	if !spec.mutating {
		return false;
	}
	match tier {
		Tier::Observe => true,
		Tier::Ask => true,
		Tier::Trusted => spec.root || spec.risk == Risk::Danger,
	}
}

pub fn allowed(spec: &ToolSpec, tier: Tier) -> bool {
	!spec.mutating || tier != Tier::Observe
}

fn schema(properties: serde_json::Value, required: &[&str]) -> serde_json::Value {
	serde_json::json!({ "type": "object", "properties": properties, "required": required })
}

pub fn catalog() -> Vec<ToolSpec> {
	vec![
		ToolSpec {
			name: "computer_status".into(),
			description: "Report the state of this computer: distribution, kernel, uptime, desktop and its version, each display with its resolution, refresh rate and scaling, processor, graphics card, memory, swap, every mounted disk, network, audio output, installed package counts and whether the helper has the permissions it needs. Call this first when the user describes a problem, so advice matches their actual machine.".into(),
			group: "observe".into(),
			parameters: schema(serde_json::json!({}), &[]),
			mutating: false,
			risk: Risk::Low,
			root: false,
		},
		ToolSpec {
			name: "shell_execute".into(),
			description: "Run one shell command and return its output. Use it for inspection and for simple changes. The command runs as the user; set root only when the task genuinely needs administrator rights.".into(),
			group: "command".into(),
			parameters: schema(
				serde_json::json!({
					"command": { "type": "string", "description": "The command line to run." },
					"root": { "type": "boolean", "description": "Run with administrator rights. Defaults to false." },
					"timeout": { "type": "integer", "description": "Seconds to wait before giving up. Defaults to 60." },
					"cwd": { "type": "string", "description": "Directory to run in. Defaults to the home directory." }
				}),
				&["command"],
			),
			mutating: true,
			risk: Risk::Medium,
			root: false,
		},
		ToolSpec {
			name: "list_windows".into(),
			description: "List the open windows with their titles, applications and geometry. Use it to find out what is on screen before acting on it.".into(),
			group: "observe".into(),
			parameters: schema(serde_json::json!({}), &[]),
			mutating: false,
			risk: Risk::Low,
			root: false,
		},
		ToolSpec {
			name: "window_action".into(),
			description: "Act on a window: activate it, minimize it, maximize it, restore it, move it, resize it or close it. Identify the window by the id or title you saw in list_windows.".into(),
			group: "desktop".into(),
			parameters: schema(
				serde_json::json!({
					"action": { "type": "string", "enum": ["activate", "minimize", "maximize", "restore", "move", "resize", "close"] },
					"id": { "type": "string", "description": "Window id from list_windows." },
					"title": { "type": "string", "description": "Window title or part of it, when the id is unknown." },
					"x": { "type": "integer" },
					"y": { "type": "integer" },
					"width": { "type": "integer" },
					"height": { "type": "integer" }
				}),
				&["action"],
			),
			mutating: true,
			risk: Risk::Medium,
			root: false,
		},
		ToolSpec {
			name: "screenshot".into(),
			description: "Take a picture of the screen and return it, so you can see what the user sees. Prefer a specific window or region when that is enough, because it is smaller and clearer.".into(),
			group: "observe".into(),
			parameters: schema(
				serde_json::json!({
					"target": { "type": "string", "enum": ["screen", "window", "region"], "description": "What to capture. Defaults to the whole screen." },
					"id": { "type": "string", "description": "Window id when target is window." },
					"x": { "type": "integer" },
					"y": { "type": "integer" },
					"width": { "type": "integer" },
					"height": { "type": "integer" },
					"cursor": { "type": "boolean", "description": "Include the mouse pointer. Defaults to false." }
				}),
				&[],
			),
			mutating: false,
			risk: Risk::Low,
			root: false,
		},
		ToolSpec {
			name: "mouse_actions".into(),
			description: "Move the pointer and click. Coordinates are pixels from the top-left of the screen. Send several actions at once when they belong together.".into(),
			group: "input".into(),
			parameters: schema(
				serde_json::json!({
					"actions": {
						"type": "array",
						"items": {
							"type": "object",
							"properties": {
								"action": { "type": "string", "enum": ["move", "click", "double_click", "press", "release", "scroll"] },
								"x": { "type": "integer" },
								"y": { "type": "integer" },
								"button": { "type": "string", "enum": ["left", "middle", "right"] },
								"amount": { "type": "integer", "description": "Scroll steps; negative scrolls up." }
							},
							"required": ["action"]
						}
					}
				}),
				&["actions"],
			),
			mutating: true,
			risk: Risk::Medium,
			root: false,
		},
		ToolSpec {
			name: "keyboard_actions".into(),
			description: "Press keys and key combinations, for example ctrl+c or alt+Tab. Use type_text instead when you want to enter words.".into(),
			group: "input".into(),
			parameters: schema(
				serde_json::json!({
					"actions": {
						"type": "array",
						"items": {
							"type": "object",
							"properties": {
								"action": { "type": "string", "enum": ["press", "release", "tap"] },
								"key": { "type": "string", "description": "A key name such as a, Return, Escape, Tab or F5." },
								"modifiers": { "type": "array", "items": { "type": "string" }, "description": "ctrl, alt, shift, super." }
							},
							"required": ["action", "key"]
						}
					}
				}),
				&["actions"],
			),
			mutating: true,
			risk: Risk::Medium,
			root: false,
		},
		ToolSpec {
			name: "type_text".into(),
			description: "Type a piece of text as if the user typed it. Use it to fill in a field; the text goes to whatever window has focus.".into(),
			group: "input".into(),
			parameters: schema(
				serde_json::json!({
					"text": { "type": "string" },
					"method": { "type": "string", "enum": ["keys", "clipboard"], "description": "Typing key by key is safer for short text; the clipboard is faster for long text and is restored afterwards." }
				}),
				&["text"],
			),
			mutating: true,
			risk: Risk::Medium,
			root: false,
		},
		ToolSpec {
			name: "accessibility_tree".into(),
			description: "Read the structure of the focused application: its controls, their names, roles and values. Use it to find a button or field precisely when a screenshot is not enough.".into(),
			group: "observe".into(),
			parameters: schema(
				serde_json::json!({
					"depth": { "type": "integer", "description": "How deep to walk. Defaults to 6." },
					"window": { "type": "string", "description": "Limit to a window title." }
				}),
				&[],
			),
			mutating: false,
			risk: Risk::Low,
			root: false,
		},
		ToolSpec {
			name: "clipboard_read".into(),
			description: "Read the current clipboard text.".into(),
			group: "observe".into(),
			parameters: schema(serde_json::json!({}), &[]),
			mutating: false,
			risk: Risk::Low,
			root: false,
		},
		ToolSpec {
			name: "clipboard_write".into(),
			description: "Put text on the clipboard, ready for the user to paste.".into(),
			group: "desktop".into(),
			parameters: schema(serde_json::json!({ "text": { "type": "string" } }), &["text"]),
			mutating: true,
			risk: Risk::Low,
			root: false,
		},
		ToolSpec {
			name: "shell_session".into(),
			description: "Run commands in a terminal session that stays open, so state such as the current directory or an interactive prompt is kept between calls. Returns the session id, the output, and the exit code.".into(),
			group: "command".into(),
			parameters: schema(
				serde_json::json!({
					"session": { "type": "string", "description": "An existing session id. Leave empty to start a new one." },
					"command": { "type": "string" },
					"root": { "type": "boolean", "description": "Start the session with administrator rights. Only used for a new session." },
					"timeout": { "type": "integer", "description": "Seconds to wait. Defaults to 60." }
				}),
				&["command"],
			),
			mutating: true,
			risk: Risk::Medium,
			root: false,
		},
		ToolSpec {
			name: "shell_session_stop".into(),
			description: "Close a terminal session you opened earlier.".into(),
			group: "command".into(),
			parameters: schema(serde_json::json!({ "session": { "type": "string" } }), &["session"]),
			mutating: true,
			risk: Risk::Low,
			root: false,
		},
		ToolSpec {
			name: "read_file".into(),
			description: "Read a text file so you can look at a configuration file or a log before changing it.".into(),
			group: "observe".into(),
			parameters: schema(
				serde_json::json!({
					"path": { "type": "string" },
					"head": { "type": "integer", "description": "Only read the first N lines." }
				}),
				&["path"],
			),
			mutating: false,
			risk: Risk::Low,
			root: false,
		},
		ToolSpec {
			name: "list_directory".into(),
			description: "List the contents of a directory.".into(),
			group: "observe".into(),
			parameters: schema(serde_json::json!({ "path": { "type": "string" } }), &["path"]),
			mutating: false,
			risk: Risk::Low,
			root: false,
		},
	]
}

pub fn spec_for(name: &str) -> Option<ToolSpec> {
	catalog().into_iter().find(|spec| spec.name == name)
}

pub fn openai_tools(tier: Tier) -> serde_json::Value {
	let tools: Vec<serde_json::Value> = catalog()
		.into_iter()
		.filter(|spec| allowed(spec, tier))
		.map(|spec| {
			serde_json::json!({
				"type": "function",
				"function": {
					"name": spec.name,
					"description": spec.description,
					"parameters": spec.parameters
				}
			})
		})
		.collect();
	serde_json::Value::Array(tools)
}

pub fn observe_only(tier: Tier) -> bool {
	tier == Tier::Observe
}

pub fn risk_for(name: &str, arguments: &serde_json::Value) -> (Risk, bool, String, String) {
	let spec = spec_for(name);
	let base_risk = spec.as_ref().map(|spec| spec.risk).unwrap_or(Risk::Medium);
	let root = arguments.get("root").and_then(|v| v.as_bool()).unwrap_or(false);
	match name {
		"shell_execute" | "shell_session" => {
			let command = arguments.get("command").and_then(|v| v.as_str()).unwrap_or("");
			let risk = shell::classify(command, root);
			let detail = if root { format!("sudo {command}") } else { command.to_string() };
			let title = shell::title_for(command, root);
			let explanation = shell::explain(command, root, risk);
			(risk, root, title, format!("{detail}\u{1}{explanation}"))
		}
		"window_action" => {
			let action = arguments.get("action").and_then(|v| v.as_str()).unwrap_or("");
			let title = format!("{} a window", capitalize(action));
			(base_risk, false, title, describe_arguments(arguments))
		}
		"mouse_actions" | "keyboard_actions" => (base_risk, false, format!("{} input", capitalize(name.trim_end_matches("_actions"))), describe_arguments(arguments)),
		"type_text" => (base_risk, false, "Type text".to_string(), describe_arguments(arguments)),
		_ => {
			let title = spec.as_ref().map(|spec| humanize(&spec.name)).unwrap_or_else(|| humanize(name));
			(base_risk, root, title, describe_arguments(arguments))
		}
	}
}

fn capitalize(value: &str) -> String {
	let mut chars = value.chars();
	match chars.next() {
		Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
		None => String::new(),
	}
}

fn humanize(name: &str) -> String {
	let mut out = String::new();
	for (index, word) in name.split('_').enumerate() {
		if index > 0 {
			out.push(' ');
		}
		out.push_str(&capitalize(word));
	}
	out
}

fn describe_arguments(arguments: &serde_json::Value) -> String {
	match arguments {
		serde_json::Value::Object(fields) => fields
			.iter()
			.filter(|(_, value)| !value.is_null())
			.map(|(key, value)| format!("{key}: {}", util::truncate_end(&render_value(value), 400)))
			.collect::<Vec<_>>()
			.join("\n"),
		other => render_value(other),
	}
}

fn render_value(value: &serde_json::Value) -> String {
	match value {
		serde_json::Value::String(text) => text.clone(),
		other => other.to_string(),
	}
}

pub fn now() -> i64 {
	util::now_ms()
}

pub fn home() -> std::path::PathBuf {
	paths::home()
}

pub const EXEC_TIMEOUT: Duration = Duration::from_secs(600);
