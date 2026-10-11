use crate::actions::{self, Approver};
use crate::providers::{self, ChatRequest, Event, WireMessage, WireToolCall};
use crate::settings::Settings;
use crate::state::{AppState, Turn};
use crate::store::{Message, StoredToolCall};
use crate::util;
use futures_util::StreamExt;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

const MAX_STEPS: usize = 24;
const PERSIST_EVERY: Duration = Duration::from_millis(400);
const STREAM_IDLE: Duration = Duration::from_secs(600);
const RECENT_SCREENSHOTS: usize = 2;
const MODEL_IMAGE_EDGE: u32 = 1920;
const MODEL_IMAGE_BYTES: usize = 3 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ChatEvent {
	Delta { turn_id: String, message_id: String, content: String, thinking: String },
	Status { turn_id: String, text: String },
	ToolCalls { turn_id: String, calls: Vec<WireToolCall> },
	Usage { turn_id: String, usage: providers::Usage },
	Started { turn_id: String, conversation_id: String },
	Done { turn_id: String, conversation_id: String, message_id: String, finish_reason: String, stopped: bool },
	Error { turn_id: String, message: String, retry_after: Option<u64> },
	Title { conversation_id: String, title: String },
}

impl ChatEvent {
	pub fn channel(&self) -> &'static str {
		match self {
			ChatEvent::Delta { .. } => "chat://delta",
			ChatEvent::Status { .. } => "chat://status",
			ChatEvent::ToolCalls { .. } => "chat://tool_calls",
			ChatEvent::Usage { .. } => "chat://usage",
			ChatEvent::Started { .. } => "chat://started",
			ChatEvent::Done { .. } => "chat://done",
			ChatEvent::Error { .. } => "chat://error",
			ChatEvent::Title { .. } => "chat://title",
		}
	}
}

pub struct Chat {
	pub events: broadcast::Sender<ChatEvent>,
}

impl Default for Chat {
	fn default() -> Self {
		Self::new()
	}
}

impl Chat {
	pub fn new() -> Chat {
		let (events, _) = broadcast::channel(4096);
		Chat { events }
	}

	pub fn send(&self, event: ChatEvent) {
		let _ = self.events.send(event);
	}
}

pub enum Anchor {
	Tail,
	At(Option<String>),
}

pub struct Start {
	pub conversation_id: String,
	pub anchor: Anchor,
	pub provider: String,
	pub model: String,
	pub content: String,
	pub images: Vec<String>,
	pub think: Option<bool>,
}

pub struct Capabilities {
	pub tools: bool,
	pub vision: bool,
}

pub async fn spawn(state: Arc<AppState>, chat: Arc<Chat>, approver: Arc<Approver>, start: Start) -> Result<String, String> {
	if start.model.trim().is_empty() {
		return Err(if start.provider == "ml5" {
			"Choose a local model in the Models panel first.".to_string()
		} else {
			"Choose a model in Settings first.".to_string()
		});
	}
	let settings = state.settings_snapshot().await;
	let conversation_id = if start.conversation_id.trim().is_empty() {
		let mode = if settings.default_mode == "helper" { "helper" } else { "chat" };
		state.store.create_conversation(mode, &start.provider, &start.model, start.think.unwrap_or(settings.show_thinking))?.id
	} else {
		let mut conversation = state.store.get_conversation(&start.conversation_id)?;
		if conversation.provider != start.provider || conversation.model != start.model {
			conversation.provider = start.provider.clone();
			conversation.model = start.model.clone();
			state.store.update_conversation(&conversation)?;
		}
		conversation.id
	};
	if state.turns.lock().await.values().any(|turn| turn.conversation_id == conversation_id) {
		return Err("This conversation is still answering. Wait for it to finish or stop it first.".to_string());
	}
	let parent = match &start.anchor {
		Anchor::Tail => state.store.messages(&conversation_id)?.last().map(|message| message.id.clone()),
		Anchor::At(parent) => parent.clone(),
	};
	let tail = if !start.content.trim().is_empty() || !start.images.is_empty() {
		Some(state.store.add_message(&conversation_id, parent.as_deref(), "user", &start.content, &start.images, None, serde_json::json!({}))?.id)
	} else {
		parent
	};
	let Some(tail) = tail else {
		return Err("There is no message for Ash to answer yet.".to_string());
	};
	let turn_id = util::new_id();
	let cancel = CancellationToken::new();
	state
		.register_turn(&turn_id, Turn { cancel: cancel.clone(), conversation_id: conversation_id.clone(), message_id: String::new() })
		.await;
	chat.send(ChatEvent::Started { turn_id: turn_id.clone(), conversation_id: conversation_id.clone() });
	let handle = Arc::clone(&state);
	let task_turn = turn_id.clone();
	let start = Start { conversation_id: conversation_id.clone(), ..start };
	tokio::spawn(async move {
		let outcome = run_turn(Arc::clone(&handle), Arc::clone(&chat), Arc::clone(&approver), &task_turn, start, tail, cancel.clone(), settings).await;
		handle.finish_turn(&task_turn).await;
		let stopped = cancel.is_cancelled() && outcome.as_ref().map(|report| report.finish_reason == "stopped").unwrap_or(true);
		match outcome {
			Ok(report) => chat.send(ChatEvent::Done {
				turn_id: task_turn.clone(),
				conversation_id: conversation_id.clone(),
				message_id: report.message_id,
				finish_reason: report.finish_reason,
				stopped,
			}),
			Err(error) => {
				chat.send(ChatEvent::Error { turn_id: task_turn.clone(), message: error, retry_after: None });
				chat.send(ChatEvent::Done {
					turn_id: task_turn.clone(),
					conversation_id: conversation_id.clone(),
					message_id: String::new(),
					finish_reason: "error".to_string(),
					stopped,
				});
			}
		}
		let _ = handle.store.touch_conversation(&conversation_id);
	});
	Ok(turn_id)
}

pub async fn retry(state: Arc<AppState>, chat: Arc<Chat>, approver: Arc<Approver>, conversation_id: &str, provider: String, model: String, think: Option<bool>) -> Result<String, String> {
	let stored = state.store.messages(conversation_id)?;
	let Some(last_user) = stored.iter().rev().find(|message| message.role == "user") else {
		return Err("There is no message to send again.".to_string());
	};
	let anchor = Anchor::At(Some(last_user.id.clone()));
	spawn(state, chat, approver, Start { conversation_id: conversation_id.to_string(), anchor, provider, model, content: String::new(), images: Vec::new(), think }).await
}

#[allow(clippy::too_many_arguments)]
pub async fn regenerate(state: Arc<AppState>, chat: Arc<Chat>, approver: Arc<Approver>, conversation_id: &str, message_id: &str, provider: String, model: String, think: Option<bool>) -> Result<String, String> {
	let message = state.store.get_message(message_id)?;
	if message.conversation_id != conversation_id {
		return Err("That message belongs to a different conversation.".to_string());
	}
	let anchor = match message.role.as_str() {
		"user" => Anchor::At(Some(message.id.clone())),
		"assistant" => {
			let lineage = state.store.lineage(conversation_id, message_id)?;
			match lineage.iter().rev().find(|item| item.role == "user") {
				Some(user) => Anchor::At(Some(user.id.clone())),
				None => return Err("This reply does not answer one of your messages, so Ash cannot write it again.".to_string()),
			}
		}
		_ => return Err("Only your messages and Ash's replies can be written again.".to_string()),
	};
	spawn(state, chat, approver, Start { conversation_id: conversation_id.to_string(), anchor, provider, model, content: String::new(), images: Vec::new(), think }).await
}

#[allow(clippy::too_many_arguments)]
pub async fn edit(
	state: Arc<AppState>,
	chat: Arc<Chat>,
	approver: Arc<Approver>,
	conversation_id: &str,
	message_id: &str,
	content: String,
	provider: String,
	model: String,
	think: Option<bool>,
) -> Result<String, String> {
	let message = state.store.get_message(message_id)?;
	if message.conversation_id != conversation_id {
		return Err("That message belongs to a different conversation.".to_string());
	}
	if message.role != "user" {
		return Err("Only your own messages can be edited and sent again.".to_string());
	}
	if content.trim().is_empty() && message.images.is_empty() {
		return Err("Type a message first.".to_string());
	}
	let anchor = Anchor::At(message.parent_id.clone());
	spawn(state, chat, approver, Start { conversation_id: conversation_id.to_string(), anchor, provider, model, content, images: message.images, think }).await
}

struct Report {
	message_id: String,
	finish_reason: String,
}

#[allow(clippy::too_many_arguments)]
async fn run_turn(
	state: Arc<AppState>,
	chat: Arc<Chat>,
	approver: Arc<Approver>,
	turn_id: &str,
	start: Start,
	mut tail: String,
	cancel: CancellationToken,
	settings: Settings,
) -> Result<Report, String> {
	let tier = settings.tier;
	let provider = start.provider.clone();
	let model = start.model.clone();
	let conversation_id = start.conversation_id.clone();
	let conversation = state.store.get_conversation(&conversation_id)?;
	let helper = conversation.mode == "helper";
	let base_url = base_url(&settings, &provider);
	let api_key = if provider == "ml5" { None } else { state.secrets.get(&provider).await };
	if provider == "opencore" && api_key.is_none() {
		return Err("Add your OpenCORe API key in Settings first.".to_string());
	}
	let caps = capabilities(&settings, &provider, &model).await;
	let use_tools = helper && caps.tools;
	let tools = use_tools.then(|| crate::tools::openai_tools(tier));
	let think = start.think.or(Some(conversation.think));
	let mut last_message_id = String::new();
	let finish_reason: String;
	let mut steps = 0usize;

	loop {
		if cancel.is_cancelled() {
			finish_reason = "stopped".to_string();
			break;
		}
		if steps >= MAX_STEPS {
			finish_reason = "step_limit".to_string();
			let note = format!("I stopped after {MAX_STEPS} steps so this does not run on forever. Tell me to continue if you want me to keep going.");
			let message = state.store.add_message(&conversation_id, Some(&tail), "assistant", &note, &[], None, serde_json::json!({ "notice": true }))?;
			chat.send(ChatEvent::Delta { turn_id: turn_id.to_string(), message_id: message.id.clone(), content: note, thinking: String::new() });
			last_message_id = message.id;
			break;
		}
		steps += 1;
		let history = messages(&state, &conversation_id, &tail, &settings, &provider, use_tools, helper, &model, &caps)?;
		let request = ChatRequest {
			provider: provider.clone(),
			base_url: base_url.clone(),
			api_key: api_key.clone(),
			model: model.clone(),
			messages: history,
			tools: tools.clone(),
			temperature: settings.temperature,
			max_tokens: None,
			think,
		};
		let mut stream = tokio::select! {
			_ = cancel.cancelled() => {
				finish_reason = "stopped".to_string();
				break;
			}
			opened = providers::stream(request) => opened?,
		};
		let message = state.store.add_message(&conversation_id, Some(&tail), "assistant", "", &[], None, serde_json::json!({ "provider": provider, "model": model }))?;
		if let Some(turn) = state.turns.lock().await.get_mut(turn_id) {
			turn.message_id = message.id.clone();
		}
		last_message_id = message.id.clone();
		let mut content = String::new();
		let mut thinking = String::new();
		let mut calls: Vec<WireToolCall> = Vec::new();
		let mut last_persist = std::time::Instant::now();
		let mut failed: Option<String> = None;
		let mut step_reason = "stop".to_string();
		loop {
			let next = tokio::select! {
				_ = cancel.cancelled() => break,
				item = tokio::time::timeout(STREAM_IDLE, stream.next()) => item,
			};
			let item = match next {
				Err(_) => {
					failed = Some("The model stopped responding. It may still be starting up; try again in a moment.".to_string());
					break;
				}
				Ok(None) => break,
				Ok(Some(item)) => item,
			};
			match item {
				Err(error) => {
					failed = Some(error);
					break;
				}
				Ok(Event::Delta { content: piece, thinking: thought }) => {
					if piece.is_empty() && thought.is_empty() {
						continue;
					}
					content.push_str(&piece);
					thinking.push_str(&thought);
					chat.send(ChatEvent::Delta { turn_id: turn_id.to_string(), message_id: message.id.clone(), content: piece, thinking: thought });
					if last_persist.elapsed() >= PERSIST_EVERY {
						last_persist = std::time::Instant::now();
						persist(&state, &message.id, &content, &thinking, &calls)?;
					}
				}
				Ok(Event::Status { text, .. }) => chat.send(ChatEvent::Status { turn_id: turn_id.to_string(), text }),
				Ok(Event::ToolCalls { calls: found }) => calls = found,
				Ok(Event::Usage { usage }) => chat.send(ChatEvent::Usage { turn_id: turn_id.to_string(), usage }),
				Ok(Event::RateLimited { retry_after, message }) => {
					chat.send(ChatEvent::Error { turn_id: turn_id.to_string(), message, retry_after: Some(retry_after) });
				}
				Ok(Event::Done { finish_reason: reason }) => {
					step_reason = reason;
					break;
				}
			}
		}
		if !use_tools {
			calls.clear();
		}
		persist(&state, &message.id, &content, &thinking, &calls)?;
		if let Some(error) = failed {
			if content.trim().is_empty() && thinking.trim().is_empty() && calls.is_empty() {
				let _ = state.store.delete_subtree(&conversation_id, &message.id);
				last_message_id.clear();
			}
			return Err(error);
		}
		tail = message.id.clone();
		if cancel.is_cancelled() {
			finish_reason = "stopped".to_string();
			break;
		}
		if calls.is_empty() {
			finish_reason = if step_reason == "empty" && content.trim().is_empty() {
				let _ = state.store.delete_subtree(&conversation_id, &message.id);
				return Err("The model returned an empty answer. Try sending the message again, or pick a different model.".to_string());
			} else {
				step_reason
			};
			break;
		}
		chat.send(ChatEvent::ToolCalls { turn_id: turn_id.to_string(), calls: calls.clone() });
		for call in &calls {
			if cancel.is_cancelled() {
				break;
			}
			let arguments = providers::parse_tool_arguments(&call.arguments);
			let proposal = actions::propose(&call.id, &call.name, &arguments);
			let title = proposal.title.clone();
			let outcome = actions::run(
				Arc::clone(&state),
				Arc::clone(&approver),
				proposal,
				Some(conversation_id.clone()),
				Some(message.id.clone()),
				cancel.clone(),
				tier,
			)
			.await;
			let (tool_output, image_id) = match outcome {
				Ok(executed) => (if executed.ok { executed.output } else { format!("The action did not succeed.\n{}", executed.output) }, executed.image_id),
				Err(error) => (format!("The action failed: {error}"), None),
			};
			let payload = match &image_id {
				Some(_) => format!("{tool_output}\nThe screenshot is attached in the next message."),
				None => tool_output,
			};
			tail = state
				.store
				.add_message(
					&conversation_id,
					Some(&tail),
					"tool",
					&util::truncate_middle(&payload, 24 * 1024, 8 * 1024),
					&[],
					Some(&call.id),
					serde_json::json!({ "tool": call.name, "imageId": image_id, "title": title }),
				)?
				.id;
		}
	}

	if finish_reason != "stopped" {
		let conversation = state.store.get_conversation(&conversation_id)?;
		if !conversation.titled {
			spawn_title(Arc::clone(&state), Arc::clone(&chat), conversation_id.clone(), provider.clone(), model.clone(), base_url.clone(), api_key.clone());
		}
	}
	Ok(Report { message_id: last_message_id, finish_reason })
}

fn persist(state: &AppState, message_id: &str, content: &str, thinking: &str, calls: &[WireToolCall]) -> Result<(), String> {
	let mut message = state.store.get_message(message_id)?;
	message.content = content.to_string();
	message.thinking = thinking.to_string();
	message.tool_calls = calls.iter().map(|call| StoredToolCall { id: call.id.clone(), name: call.name.clone(), arguments: call.arguments.clone() }).collect();
	state.store.update_message(&message)
}

pub fn base_url(settings: &Settings, provider: &str) -> String {
	match provider {
		"ml5" => providers::ML5_BASE.to_string(),
		"opencore" => providers::OPENCORE_BASE.to_string(),
		_ => settings
			.custom_endpoints
			.iter()
			.find(|endpoint| endpoint.id == provider)
			.map(|endpoint| endpoint.base_url.clone())
			.unwrap_or_else(|| providers::OPENCORE_BASE.to_string()),
	}
}

pub async fn capabilities(settings: &Settings, provider: &str, model: &str) -> Capabilities {
	match provider {
		"ml5" => {
			let found = crate::ml5::models().await.ok().and_then(|models| models.into_iter().find(|candidate| candidate.id == model || candidate.name == model));
			match found {
				Some(info) => Capabilities { tools: true, vision: info.vision },
				None => Capabilities { tools: true, vision: false },
			}
		}
		"opencore" => {
			let lowered = model.to_lowercase();
			Capabilities { tools: true, vision: lowered.contains("xenon") || lowered.contains("core-pico") || lowered.contains("core-ex") }
		}
		other => match settings.custom_endpoints.iter().find(|endpoint| endpoint.id == other) {
			Some(endpoint) => Capabilities { tools: endpoint.tools, vision: endpoint.vision },
			None => Capabilities { tools: false, vision: false },
		},
	}
}

fn model_image(state: &AppState, image_id: &str) -> Option<(String, Option<String>)> {
	let record = state.store.image(image_id).ok().flatten()?;
	let oversized = record.width > MODEL_IMAGE_EDGE || record.height > MODEL_IMAGE_EDGE || record.data.len() > MODEL_IMAGE_BYTES;
	if !oversized {
		return Some((providers::data_url(&record.mime, &record.data), None));
	}
	let decoded = image::load_from_memory(&record.data).ok()?;
	let scaled = if decoded.width() > MODEL_IMAGE_EDGE || decoded.height() > MODEL_IMAGE_EDGE {
		decoded.resize(MODEL_IMAGE_EDGE, MODEL_IMAGE_EDGE, image::imageops::FilterType::Triangle)
	} else {
		decoded.clone()
	};
	let mut bytes: Vec<u8> = Vec::new();
	let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85);
	scaled.to_rgb8().write_with_encoder(encoder).ok()?;
	let note = (scaled.width() != decoded.width()).then(|| {
		format!(
			"This picture was shrunk from {}x{} to {}x{}. Multiply positions you read from it by {:.3} to get screen pixels.",
			decoded.width(),
			decoded.height(),
			scaled.width(),
			scaled.height(),
			decoded.width() as f64 / scaled.width() as f64
		)
	});
	Some((providers::data_url("image/jpeg", &bytes), note))
}

#[allow(clippy::too_many_arguments)]
fn messages(
	state: &AppState,
	conversation_id: &str,
	tail: &str,
	settings: &Settings,
	provider: &str,
	use_tools: bool,
	helper: bool,
	model: &str,
	caps: &Capabilities,
) -> Result<Vec<WireMessage>, String> {
	let tier = settings.tier;
	let local = provider == "ml5";
	let base = crate::prompts::chat_system(tier, local.then_some(model), &settings.system_prompt);
	let system = if helper { format!("{base}\n\n{}", crate::prompts::helper_extra(tier, use_tools)) } else { base };
	let mut history: Vec<WireMessage> = vec![WireMessage::text("system", system)];
	let stored = state.store.lineage(conversation_id, tail)?;
	let screenshot_positions: Vec<usize> = stored
		.iter()
		.enumerate()
		.filter(|(_, message)| message.role == "tool" && message.meta.get("imageId").and_then(|value| value.as_str()).is_some())
		.map(|(index, _)| index)
		.collect();
	let recent: HashSet<usize> = screenshot_positions.iter().rev().take(RECENT_SCREENSHOTS).copied().collect();
	let mut open_calls: Vec<String> = Vec::new();
	let mut pending_images: Vec<(String, Option<String>)> = Vec::new();
	let close_calls = |history: &mut Vec<WireMessage>, open_calls: &mut Vec<String>, pending_images: &mut Vec<(String, Option<String>)>| {
		for id in open_calls.drain(..) {
			history.push(WireMessage { role: "tool".to_string(), content: "This action was not run.".to_string(), tool_call_id: Some(id), ..Default::default() });
		}
		if !pending_images.is_empty() {
			let notes: Vec<String> = pending_images.iter().filter_map(|(_, note)| note.clone()).collect();
			let mut text = "Here is the screenshot the tool just took.".to_string();
			if !notes.is_empty() {
				text.push(' ');
				text.push_str(&notes.join(" "));
			}
			let mut wire = WireMessage::text("user", text);
			wire.images = pending_images.drain(..).map(|(url, _)| url).collect();
			history.push(wire);
		}
	};
	for (index, message) in stored.iter().enumerate() {
		if message.role != "tool" {
			close_calls(&mut history, &mut open_calls, &mut pending_images);
		}
		match message.role.as_str() {
			"user" => {
				let mut wire = WireMessage::text("user", message.content.clone());
				if caps.vision {
					let mut notes: Vec<String> = Vec::new();
					for image in &message.images {
						if let Some((url, note)) = model_image(state, image) {
							wire.images.push(url);
							notes.extend(note);
						}
					}
					if !notes.is_empty() {
						wire.content = format!("{}\n\n{}", wire.content, notes.join(" "));
					}
				} else if !message.images.is_empty() {
					wire.content = format!("{}\n\n[The user attached {} picture(s), but this model cannot see pictures.]", wire.content, message.images.len());
				}
				history.push(wire);
			}
			"assistant" => {
				if message.content.trim().is_empty() && message.tool_calls.is_empty() {
					continue;
				}
				let mut wire = WireMessage::text("assistant", message.content.clone());
				if use_tools && !message.tool_calls.is_empty() {
					wire.tool_calls = message.tool_calls.iter().map(|call| WireToolCall { id: call.id.clone(), name: call.name.clone(), arguments: call.arguments.clone() }).collect();
					open_calls = message.tool_calls.iter().map(|call| call.id.clone()).collect();
				} else if !message.tool_calls.is_empty() && wire.content.trim().is_empty() {
					wire.content = format!("(used: {})", message.tool_calls.iter().map(|call| call.name.as_str()).collect::<Vec<_>>().join(", "));
				}
				history.push(wire);
			}
			"tool" => push_tool(state, message, index, use_tools, caps, &recent, &mut history, &mut open_calls, &mut pending_images),
			_ => {}
		}
	}
	close_calls(&mut history, &mut open_calls, &mut pending_images);
	Ok(history)
}

#[allow(clippy::too_many_arguments)]
fn push_tool(
	state: &AppState,
	message: &Message,
	index: usize,
	use_tools: bool,
	caps: &Capabilities,
	recent: &HashSet<usize>,
	history: &mut Vec<WireMessage>,
	open_calls: &mut Vec<String>,
	pending_images: &mut Vec<(String, Option<String>)>,
) {
	let Some(call_id) = message.tool_call_id.clone() else { return };
	if !use_tools {
		return;
	}
	let Some(position) = open_calls.iter().position(|id| *id == call_id) else { return };
	open_calls.remove(position);
	let image_id = message.meta.get("imageId").and_then(|value| value.as_str());
	let mut content = message.content.clone();
	match image_id {
		Some(image_id) if caps.vision && recent.contains(&index) => match model_image(state, image_id) {
			Some(image) => pending_images.push(image),
			None => content = content.replace("The screenshot is attached in the next message.", "The screenshot could not be attached."),
		},
		Some(_) if caps.vision => content = content.replace("The screenshot is attached in the next message.", "This older screenshot is no longer attached."),
		Some(_) => content = content.replace("The screenshot is attached in the next message.", "This model cannot see pictures, so the screenshot was not attached; use accessibility_tree or list_windows instead."),
		None => {}
	}
	history.push(WireMessage {
		role: "tool".to_string(),
		content,
		tool_calls: Vec::new(),
		tool_call_id: Some(call_id),
		name: message.meta.get("tool").and_then(|value| value.as_str()).map(str::to_string),
		images: Vec::new(),
	});
}

fn spawn_title(state: Arc<AppState>, chat: Arc<Chat>, conversation_id: String, provider: String, model: String, base_url: String, api_key: Option<String>) {
	tokio::spawn(async move {
		let _ = generate_title(&state, &chat, &conversation_id, provider, model, base_url, api_key).await;
	});
}

pub async fn generate_title(
	state: &AppState,
	chat: &Chat,
	conversation_id: &str,
	provider: String,
	model: String,
	base_url: String,
	api_key: Option<String>,
) -> Result<String, String> {
	let stored = state.store.messages(conversation_id)?;
	let first_user = stored
		.iter()
		.find(|message| message.role == "user" && !message.content.trim().is_empty())
		.map(|message| message.content.clone())
		.ok_or_else(|| "There is nothing to name yet.".to_string())?;
	let request = ChatRequest {
		provider,
		base_url,
		api_key,
		model,
		messages: vec![WireMessage::text("system", crate::prompts::title_prompt()), WireMessage::text("user", util::truncate_end(&first_user, 2000))],
		tools: None,
		temperature: Some(0.2),
		max_tokens: Some(48),
		think: Some(false),
	};
	let mut stream = providers::stream(request).await?;
	let mut title = String::new();
	let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
	loop {
		match tokio::time::timeout_at(deadline, stream.next()).await {
			Ok(Some(Ok(Event::Delta { content, .. }))) => title.push_str(&content),
			Ok(Some(Ok(Event::Done { .. }))) | Ok(None) => break,
			Ok(Some(Ok(_))) => {}
			Ok(Some(Err(error))) => return Err(error),
			Err(_) => break,
		}
	}
	let cleaned = clean_title(&title);
	if cleaned.is_empty() {
		return Err("The model did not suggest a title.".to_string());
	}
	let mut conversation = state.store.get_conversation(conversation_id)?;
	conversation.title = cleaned.clone();
	conversation.titled = true;
	state.store.update_conversation(&conversation)?;
	chat.send(ChatEvent::Title { conversation_id: conversation_id.to_string(), title: cleaned.clone() });
	Ok(cleaned)
}

fn clean_title(raw: &str) -> String {
	let without_thinking = match (raw.find("<think>"), raw.find("</think>")) {
		(Some(_), Some(end)) => &raw[end + "</think>".len()..],
		_ => raw,
	};
	let line = without_thinking.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
	let line = line.strip_prefix("Title:").or_else(|| line.strip_prefix("title:")).unwrap_or(line);
	let trimmed = line.trim().trim_matches(|character: char| matches!(character, '"' | '\'' | '#' | '*' | '`' | '“' | '”' | '«' | '»')).trim().trim_end_matches(['.', '!', ':', ';']).trim();
	util::truncate_end(trimmed, 80)
}

pub async fn stop(state: &AppState, turn_id: &str) -> bool {
	state.cancel_turn(turn_id).await
}
