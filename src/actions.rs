use crate::settings::Tier;
use crate::state::AppState;
use crate::store::Action;
use crate::tools::{self, pty, shell, Risk};
use crate::util;
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

const CONCEAL_SETTLE: Duration = Duration::from_millis(350);

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ActionEvent {
	Changed { action: Box<Action> },
	Log { action_id: String, data: String, stream: String },
	Image { action_id: String, image_id: String },
}

pub type Concealer = Arc<dyn Fn(bool) -> bool + Send + Sync>;

pub struct Approver {
	pub events: broadcast::Sender<ActionEvent>,
	concealer: std::sync::RwLock<Option<Concealer>>,
}

impl Default for Approver {
	fn default() -> Self {
		Self::new()
	}
}

impl Approver {
	pub fn new() -> Approver {
		let (events, _) = broadcast::channel(512);
		Approver { events, concealer: std::sync::RwLock::new(None) }
	}

	pub fn set_concealer(&self, concealer: Concealer) {
		if let Ok(mut slot) = self.concealer.write() {
			*slot = Some(concealer);
		}
	}

	fn conceal(&self, hide: bool) -> bool {
		let concealer = self.concealer.read().ok().and_then(|slot| slot.clone());
		concealer.map(|concealer| concealer(hide)).unwrap_or(false)
	}

	pub fn emit(&self, event: ActionEvent) {
		let _ = self.events.send(event);
	}

	pub fn changed(&self, action: &Action) {
		self.emit(ActionEvent::Changed { action: Box::new(action.clone()) });
	}
}

pub struct Proposal {
	pub action_id: String,
	pub call_id: String,
	pub tool: String,
	pub title: String,
	pub explanation: String,
	pub detail: String,
	pub arguments: serde_json::Value,
	pub risk: Risk,
	pub root: bool,
}

pub fn propose(call_id: &str, name: &str, arguments: &serde_json::Value) -> Proposal {
	let (risk, root, title, packed) = tools::risk_for(name, arguments);
	let (detail, explanation) = packed.split_once('\u{1}').unwrap_or((packed.as_str(), ""));
	Proposal {
		action_id: util::new_id(),
		call_id: call_id.to_string(),
		tool: name.to_string(),
		title,
		explanation: explanation.to_string(),
		detail: detail.to_string(),
		arguments: arguments.clone(),
		risk,
		root,
	}
}

pub async fn needs_approval(state: &AppState, proposal: &Proposal, conversation_id: Option<&str>, tier: Tier) -> bool {
	let Some(spec) = tools::spec_for(&proposal.tool) else { return true };
	if !spec.mutating {
		return false;
	}
	if proposal.root || proposal.risk == Risk::Danger {
		return true;
	}
	if tier == Tier::Trusted {
		return false;
	}
	!state.remembered(conversation_id, &proposal.tool, proposal.risk).await
}

pub fn allowed(proposal: &Proposal, tier: Tier) -> bool {
	match tools::spec_for(&proposal.tool) {
		Some(spec) => tools::allowed(&spec, tier),
		None => false,
	}
}

fn record(state: &AppState, proposal: &Proposal, conversation_id: Option<&str>, message_id: Option<&str>, status: &str) -> Result<Action, String> {
	let action = Action {
		id: proposal.action_id.clone(),
		conversation_id: conversation_id.map(str::to_string),
		message_id: message_id.map(str::to_string),
		tool_call_id: (!proposal.call_id.is_empty()).then(|| proposal.call_id.clone()),
		tool: proposal.tool.clone(),
		title: proposal.title.clone(),
		explanation: proposal.explanation.clone(),
		detail: proposal.detail.clone(),
		args: proposal.arguments.clone(),
		risk: proposal.risk.label().to_string(),
		root: proposal.root,
		status: status.to_string(),
		output: String::new(),
		image_id: None,
		exit_code: None,
		created: util::now_ms(),
		finished: None,
	};
	state.store.insert_action(&action)?;
	Ok(action)
}

pub struct Executed {
	pub output: String,
	pub code: Option<i32>,
	pub ok: bool,
	pub image_id: Option<String>,
	pub detail: String,
}

pub async fn run(
	state: Arc<AppState>,
	approver: Arc<Approver>,
	mut proposal: Proposal,
	conversation_id: Option<String>,
	message_id: Option<String>,
	cancel: tokio_util::sync::CancellationToken,
	tier: Tier,
) -> Result<Executed, String> {
	if proposal.tool == "shell_session" {
		let session_id = proposal.arguments.get("session").and_then(|value| value.as_str()).unwrap_or("").trim().to_string();
		let root_session = !session_id.is_empty() && state.sessions.lock().await.get(&session_id).map(|session| session.root).unwrap_or(false);
		if root_session && !proposal.root {
			proposal.root = true;
			let command = proposal.arguments.get("command").and_then(|value| value.as_str()).unwrap_or("");
			proposal.risk = shell::classify(command, true);
			proposal.explanation = shell::explain(command, true, proposal.risk);
			proposal.title = shell::title_for(command, true);
		}
	}
	if !allowed(&proposal, tier) {
		let mut action = record(&state, &proposal, conversation_id.as_deref(), message_id.as_deref(), "denied")?;
		action.output = "This action changes the system, which is not allowed while the permission tier is Observe.".to_string();
		action.finished = Some(util::now_ms());
		state.store.update_action(&action)?;
		approver.changed(&action);
		return Ok(Executed { output: action.output.clone(), code: None, ok: false, image_id: None, detail: action.detail.clone() });
	}
	let mut action = record(&state, &proposal, conversation_id.as_deref(), message_id.as_deref(), "pending")?;
	approver.changed(&action);
	let own = cancel.child_token();
	state.track_action(&action.id, own.clone()).await;
	let result = supervise(&state, &approver, &proposal, &mut action, conversation_id.as_deref(), &own, tier).await;
	state.untrack_action(&action.id).await;
	result
}

fn stopped_text(cancel: &tokio_util::sync::CancellationToken) -> String {
	if cancel.is_cancelled() {
		"The user stopped this action.".to_string()
	} else {
		"The action was interrupted.".to_string()
	}
}

async fn supervise(
	state: &Arc<AppState>,
	approver: &Arc<Approver>,
	proposal: &Proposal,
	action: &mut Action,
	conversation_id: Option<&str>,
	cancel: &tokio_util::sync::CancellationToken,
	tier: Tier,
) -> Result<Executed, String> {
	if needs_approval(state, proposal, conversation_id, tier).await {
		action.status = "awaiting".to_string();
		state.store.update_action(action)?;
		approver.changed(action);
		let receiver = state.register_approval(&action.id).await;
		let decision = tokio::select! {
			result = receiver => result.unwrap_or(false),
			_ = cancel.cancelled() => {
				state.cancel_approval(&action.id).await;
				action.status = "cancelled".to_string();
				action.finished = Some(util::now_ms());
				action.output = "The user stopped this action before it ran.".to_string();
				state.store.update_action(action)?;
				approver.changed(action);
				return Ok(Executed { output: action.output.clone(), code: None, ok: false, image_id: None, detail: action.detail.clone() });
			}
		};
		if !decision {
			action.status = "denied".to_string();
			action.finished = Some(util::now_ms());
			action.output = "The user did not allow this action.".to_string();
			state.store.update_action(action)?;
			approver.changed(action);
			return Ok(Executed { output: action.output.clone(), code: None, ok: false, image_id: None, detail: action.detail.clone() });
		}
	}
	action.status = "running".to_string();
	state.store.update_action(action)?;
	approver.changed(action);
	let snapshot = action.clone();
	let controls = matches!(proposal.tool.as_str(), "screenshot" | "mouse_actions" | "keyboard_actions" | "type_text") && state.settings_snapshot().await.hide_while_controlling;
	if controls && approver.conceal(true) {
		tokio::time::sleep(CONCEAL_SETTLE).await;
	}
	let result = tokio::select! {
		result = dispatch(state, approver, proposal, &snapshot, cancel) => result,
		_ = cancel.cancelled() => Err(stopped_text(cancel)),
	};
	if controls {
		approver.conceal(false);
	}
	match result {
		Ok(executed) => {
			action.status = if cancel.is_cancelled() {
				"cancelled".to_string()
			} else if executed.ok {
				"done".to_string()
			} else {
				"failed".to_string()
			};
			action.output = executed.output.clone();
			action.exit_code = executed.code.map(|code| code as i64);
			action.image_id = executed.image_id.clone();
			if !executed.detail.is_empty() {
				action.detail = executed.detail.clone();
			}
			action.finished = Some(util::now_ms());
			state.store.update_action(action)?;
			approver.changed(action);
			if let Some(image_id) = action.image_id.clone() {
				approver.emit(ActionEvent::Image { action_id: action.id.clone(), image_id });
			}
			Ok(executed)
		}
		Err(error) => {
			action.status = if cancel.is_cancelled() { "cancelled".to_string() } else { "failed".to_string() };
			action.output = error.clone();
			action.finished = Some(util::now_ms());
			state.store.update_action(action)?;
			approver.changed(action);
			Err(error)
		}
	}
}

async fn dispatch(
	state: &Arc<AppState>,
	approver: &Arc<Approver>,
	proposal: &Proposal,
	action: &Action,
	cancel: &tokio_util::sync::CancellationToken,
) -> Result<Executed, String> {
	let arguments = &proposal.arguments;
	match proposal.tool.as_str() {
		"computer_status" => {
			let text = crate::tools::status::report().await;
			Ok(Executed { output: text, code: Some(0), ok: true, image_id: None, detail: String::new() })
		}
		"shell_execute" => {
			let command = text_argument(arguments, "command")?;
			let root = arguments.get("root").and_then(|value| value.as_bool()).unwrap_or(false);
			let timeout = Duration::from_secs(arguments.get("timeout").and_then(|value| value.as_u64()).unwrap_or(60).clamp(1, 3600));
			let cwd = shell::resolve_cwd(arguments.get("cwd").and_then(|value| value.as_str()));
			let executed = shell::execute(&command, &cwd, root, timeout, Some(log_sink(approver, &action.id))).await;
			Ok(Executed { ok: executed.ok, code: executed.code, output: executed.output, image_id: None, detail: action.detail.clone() })
		}
		"list_windows" => {
			let windows = crate::tools::window::list().await?;
			let text = if windows.is_empty() {
				"No windows are open.".to_string()
			} else {
				windows
					.iter()
					.map(|window| {
						let mut flags: Vec<&str> = Vec::new();
						window.active.then(|| flags.push("active"));
						window.minimized.then(|| flags.push("minimized"));
						window.maximized.then(|| flags.push("maximized"));
						window.full_screen.then(|| flags.push("full screen"));
						format!(
							"- id {}: \"{}\" from {} at {},{} size {}x{}{}",
							window.id,
							window.title,
							if window.application.is_empty() { "an unknown application" } else { window.application.as_str() },
							window.x,
							window.y,
							window.width,
							window.height,
							if flags.is_empty() { String::new() } else { format!(" [{}]", flags.join(", ")) }
						)
					})
					.collect::<Vec<_>>()
					.join("\n")
			};
			Ok(Executed { output: text, code: Some(0), ok: true, image_id: None, detail: String::new() })
		}
		"window_action" => {
			let action_name = text_argument(arguments, "action")?;
			let id = arguments.get("id").and_then(|value| value.as_str()).unwrap_or("");
			let title = arguments.get("title").and_then(|value| value.as_str()).unwrap_or("");
			let x = arguments.get("x").and_then(|value| value.as_i64()).unwrap_or(0) as i32;
			let y = arguments.get("y").and_then(|value| value.as_i64()).unwrap_or(0) as i32;
			let width = arguments.get("width").and_then(|value| value.as_i64()).unwrap_or(0) as i32;
			let height = arguments.get("height").and_then(|value| value.as_i64()).unwrap_or(0) as i32;
			if id.is_empty() && title.is_empty() {
				return Err("Which window? Give an id from list_windows or part of its title.".to_string());
			}
			let applied = crate::tools::window::act(&action_name, id, title, x, y, width, height).await?;
			let output = if applied == 0 {
				"No window matched, so nothing changed.".to_string()
			} else {
				format!("Applied to {applied} window{}.", if applied == 1 { "" } else { "s" })
			};
			Ok(Executed { output, code: Some(0), ok: applied > 0, image_id: None, detail: String::new() })
		}
		"screenshot" => {
			let target = match arguments.get("target").and_then(|value| value.as_str()).unwrap_or("screen") {
				"window" => crate::tools::screen::Target::Window {
					id: arguments.get("id").and_then(|value| value.as_str()).map(str::trim).filter(|id| !id.is_empty()).map(str::to_string),
				},
				"active_screen" | "monitor" => crate::tools::screen::Target::Screen,
				"region" => crate::tools::screen::Target::Region {
					x: arguments.get("x").and_then(|value| value.as_i64()).unwrap_or(0) as i32,
					y: arguments.get("y").and_then(|value| value.as_i64()).unwrap_or(0) as i32,
					width: arguments.get("width").and_then(|value| value.as_i64()).unwrap_or(0) as i32,
					height: arguments.get("height").and_then(|value| value.as_i64()).unwrap_or(0) as i32,
				},
				_ => crate::tools::screen::Target::Workspace,
			};
			let cursor = arguments.get("cursor").and_then(|value| value.as_bool()).unwrap_or(false);
			let shot = crate::tools::screen::capture(target, cursor).await?;
			let image = crate::tools::screen::encode_png(&shot.image)?;
			let image_id = state.store.add_image(action.conversation_id.as_deref(), "image/png", &image, shot.image.width(), shot.image.height())?;
			let output = format!("Captured the screen with {} ({}x{}).", shot.source.label(), shot.image.width(), shot.image.height());
			Ok(Executed { output, code: Some(0), ok: true, image_id: Some(image_id), detail: String::new() })
		}
		"mouse_actions" => {
			let actions = arguments.get("actions").cloned().unwrap_or(serde_json::Value::Array(Vec::new()));
			let count = crate::tools::input::mouse(actions).await?;
			Ok(Executed { output: format!("Sent {count} pointer action{}.", plural(count)), code: Some(0), ok: true, image_id: None, detail: String::new() })
		}
		"keyboard_actions" => {
			let actions = arguments.get("actions").cloned().unwrap_or(serde_json::Value::Array(Vec::new()));
			let count = crate::tools::input::keyboard(actions).await?;
			Ok(Executed { output: format!("Sent {count} key action{}.", plural(count)), code: Some(0), ok: true, image_id: None, detail: String::new() })
		}
		"type_text" => {
			let text = text_argument(arguments, "text")?;
			let method = arguments.get("method").and_then(|value| value.as_str()).unwrap_or("keys");
			if method == "clipboard" || text.len() > 400 {
				crate::tools::input::type_clipboard(&text).await?;
				Ok(Executed { output: format!("Typed {} characters using the clipboard.", text.chars().count()), code: Some(0), ok: true, image_id: None, detail: "The text itself is not repeated here.".to_string() })
			} else {
				crate::tools::input::type_keys(&text).await?;
				Ok(Executed { output: format!("Typed {} characters.", text.chars().count()), code: Some(0), ok: true, image_id: None, detail: "The text itself is not repeated here.".to_string() })
			}
		}
		"accessibility_tree" => {
			let depth = arguments.get("depth").and_then(|value| value.as_u64()).unwrap_or(crate::tools::access::DEFAULT_DEPTH as u64).clamp(1, crate::tools::access::MAX_DEPTH as u64) as u32;
			let window = arguments.get("window").and_then(|value| value.as_str());
			let text = crate::tools::access::tree(depth, window).await?;
			Ok(Executed { output: util::truncate_end(&text, 24 * 1024), code: Some(0), ok: true, image_id: None, detail: String::new() })
		}
		"clipboard_read" => {
			let text = crate::tools::clipboard::read_text().await?;
			Ok(Executed { output: util::truncate_end(&text, 8192), code: Some(0), ok: true, image_id: None, detail: String::new() })
		}
		"clipboard_write" => {
			let text = text_argument(arguments, "text")?;
			crate::tools::clipboard::write_text(&text).await?;
			Ok(Executed { output: "Put the text on the clipboard.".to_string(), code: Some(0), ok: true, image_id: None, detail: "The text itself is not repeated here.".to_string() })
		}
		"shell_session" => {
			let session_id = arguments.get("session").and_then(|value| value.as_str()).unwrap_or("").trim().to_string();
			let command = arguments.get("command").and_then(|value| value.as_str()).unwrap_or("").to_string();
			let timeout = Duration::from_secs(arguments.get("timeout").and_then(|value| value.as_u64()).unwrap_or(60).clamp(1, 1800));
			let session = if session_id.is_empty() {
				if command.trim().is_empty() {
					return Err("The command is missing.".to_string());
				}
				let id = util::new_id();
				let root = arguments.get("root").and_then(|value| value.as_bool()).unwrap_or(false);
				let starter = cancel.clone();
				let created = tokio::task::spawn_blocking(move || pty::Session::start_agent(&id, root, &starter))
					.await
					.map_err(|error| format!("The terminal could not be started: {error}"))??;
				state.sessions.lock().await.insert(created.id.clone(), Arc::clone(&created));
				created
			} else {
				let found = state.sessions.lock().await.get(&session_id).cloned();
				match found {
					Some(session) if session.agent && session.alive() => session,
					Some(session) if session.agent => {
						state.sessions.lock().await.remove(&session_id);
						drop(session);
						return Err("That terminal session has ended. Start a new one by leaving the session empty.".to_string());
					}
					Some(_) => return Err("That terminal belongs to the user and cannot be driven by the helper. Start a new session by leaving the session empty.".to_string()),
					None => return Err("That terminal session has ended. Start a new one by leaving the session empty.".to_string()),
				}
			};
			let id = session.id.clone();
			let (stop_sender, mut stop_receiver) = tokio::sync::oneshot::channel::<()>();
			let mut feed = session.events.subscribe();
			let live = Arc::clone(approver);
			let action_id = action.id.clone();
			let forwarder = tokio::spawn(async move {
				loop {
					tokio::select! {
						_ = &mut stop_receiver => break,
						received = feed.recv() => match received {
							Ok(output) if output.stream == "out" => {
								let cleaned = util::clean_terminal(&output.data);
								if !cleaned.is_empty() {
									live.emit(ActionEvent::Log { action_id: action_id.clone(), data: cleaned, stream: "stdout".to_string() });
								}
							}
							Ok(_) => break,
							Err(broadcast::error::RecvError::Lagged(_)) => continue,
							Err(broadcast::error::RecvError::Closed) => break,
						},
					}
				}
			});
			let runner = Arc::clone(&session);
			let watcher = cancel.clone();
			let ran = tokio::task::spawn_blocking(move || runner.run_once(&command, timeout, &watcher)).await;
			let _ = stop_sender.send(());
			let _ = forwarder.await;
			let result = ran.map_err(|error| format!("The terminal session failed: {error}"))?;
			let body = util::truncate_middle(result.output.trim_end(), 24 * 1024, 8 * 1024);
			let body = if body.trim().is_empty() { "(no output)".to_string() } else { body };
			let (output, ok, code) = match result.state {
				pty::RunState::Finished => {
					let code = result.code.unwrap_or(0);
					(format!("{body}\n\nSession {id}: the command finished with exit code {code}."), code == 0, Some(code))
				}
				pty::RunState::Running => (
					format!("{body}\n\nSession {id}: the command is still running after {} seconds. Call shell_session again with this session to type a reply into it, or with an empty command to keep waiting.", timeout.as_secs()),
					true,
					None,
				),
				pty::RunState::Ended => {
					state.sessions.lock().await.remove(&id);
					(format!("{body}\n\nSession {id} has ended{}.", result.code.map(|code| format!(" with exit code {code}")).unwrap_or_default()), false, result.code)
				}
				pty::RunState::Cancelled => (format!("{body}\n\nSession {id}: the command was interrupted."), false, None),
			};
			Ok(Executed { output, code, ok, image_id: None, detail: format!("{}\nsession: {id}", action.detail) })
		}
		"shell_session_stop" => {
			let session_id = text_argument(arguments, "session")?;
			let removed = state.sessions.lock().await.remove(&session_id);
			match removed {
				Some(session) => {
					session.stop();
					Ok(Executed { output: "Closed the terminal session.".to_string(), code: Some(0), ok: true, image_id: None, detail: String::new() })
				}
				None => Ok(Executed { output: "That session was already closed.".to_string(), code: Some(0), ok: true, image_id: None, detail: String::new() }),
			}
		}
		"read_file" => {
			let path = shell::resolve_cwd(Some(&text_argument(arguments, "path")?));
			let text = std::fs::read_to_string(&path).map_err(|error| format!("Could not read {}: {error}", path.display()))?;
			let head = arguments.get("head").and_then(|value| value.as_u64()).unwrap_or(0) as usize;
			let limited = if head > 0 {
				text.lines().take(head).collect::<Vec<_>>().join("\n")
			} else {
				text
			};
			Ok(Executed { output: util::truncate_end(&limited, 64 * 1024), code: Some(0), ok: true, image_id: None, detail: String::new() })
		}
		"list_directory" => {
			let path = shell::resolve_cwd(Some(&text_argument(arguments, "path")?));
			let mut entries: Vec<String> = Vec::new();
			let reader = std::fs::read_dir(&path).map_err(|error| format!("Could not read {}: {error}", path.display()))?;
			for entry in reader.flatten() {
				let name = entry.file_name().to_string_lossy().to_string();
				let suffix = match entry.file_type() {
					Ok(kind) if kind.is_dir() => "/",
					Ok(kind) if kind.is_symlink() => "@",
					_ => "",
				};
				entries.push(format!("{name}{suffix}"));
			}
			entries.sort();
			Ok(Executed { output: entries.join("\n"), code: Some(0), ok: true, image_id: None, detail: String::new() })
		}
		other => Err(format!("The tool {other} is not available.")),
	}
}

fn log_sink(approver: &Arc<Approver>, action_id: &str) -> tools::root::LogSink {
	let approver = Arc::clone(approver);
	let action_id = action_id.to_string();
	Arc::new(move |stream: &str, data: &str| {
		approver.emit(ActionEvent::Log { action_id: action_id.clone(), data: util::clean_terminal(data), stream: stream.to_string() });
	})
}

fn plural(count: usize) -> &'static str {
	if count == 1 {
		""
	} else {
		"s"
	}
}

fn text_argument(arguments: &serde_json::Value, key: &str) -> Result<String, String> {
	arguments
		.get(key)
		.and_then(|value| value.as_str())
		.map(str::to_string)
		.filter(|text| !text.trim().is_empty())
		.ok_or_else(|| format!("The {key} is missing."))
}

pub async fn cancel_running(state: &AppState, action_id: &str) -> bool {
	let answered = state.resolve_approval(action_id, false).await;
	state.cancel_action(action_id).await || answered
}
