use crate::actions::{self, ActionEvent, Approver};
use crate::chat::{self, Chat};
use crate::ml5::{self, PullEvent};
use crate::providers::{self, ModelInfo};
use crate::settings::{self, Settings, Tier};
use crate::state::AppState;
use crate::store::{Action, Conversation, DeleteScope, Message, StoredToolCall};
use crate::theme::{self, ThemeState};
use crate::tools::{self, clipboard, pty, status};
use crate::{autostart, markdown, paths, util};
use futures_util::StreamExt;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

pub const MAIN_WINDOW: &str = "main";
const MAX_IMAGES: usize = 8;
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const MAX_USER_SESSIONS: usize = 8;
const MAX_SESSION_WRITE: usize = 64 * 1024;
const MAX_TITLE: usize = 200;
const EXPORT_OUTPUT: usize = 8 * 1024;

pub struct Launch {
	pub hidden: AtomicBool,
	pub ready: AtomicBool,
	pub tray: Arc<AtomicBool>,
}

impl Launch {
	pub fn new(hidden: bool, tray: Arc<AtomicBool>) -> Launch {
		Launch { hidden: AtomicBool::new(hidden), ready: AtomicBool::new(false), tray }
	}
}

pub fn show_main(app: &AppHandle) {
	if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
		let _ = window.unminimize();
		let _ = window.show();
		let _ = window.set_focus();
	}
}

pub async fn update_settings(state: &AppState, change: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
	let mut guard = state.settings.write().await;
	let mut next = guard.clone();
	change(&mut next);
	next.normalize();
	next.save()?;
	*guard = next.clone();
	Ok(next)
}

fn known_provider(settings: &Settings, provider: &str) -> bool {
	matches!(provider, "ml5" | "opencore") || settings.custom_endpoints.iter().any(|endpoint| endpoint.id == provider)
}

fn provider_label(settings: &Settings, provider: &str) -> String {
	match provider {
		"ml5" => "the local engine".to_string(),
		"opencore" => "OpenCORe".to_string(),
		other => settings.custom_endpoints.iter().find(|endpoint| endpoint.id == other).map(|endpoint| endpoint.name.clone()).unwrap_or_else(|| other.to_string()),
	}
}

async fn conversation_busy(state: &AppState, conversation_id: &str) -> bool {
	state.turns.lock().await.values().any(|turn| turn.conversation_id == conversation_id)
}

async fn stop_conversation(state: &AppState, conversation_id: &str) {
	for turn in state.turns.lock().await.values().filter(|turn| turn.conversation_id == conversation_id) {
		turn.cancel.cancel();
	}
	if let Ok(found) = state.store.actions_for_conversation(conversation_id) {
		for action in found.iter().filter(|action| matches!(action.status.as_str(), "pending" | "awaiting" | "running")) {
			actions::cancel_running(state, &action.id).await;
		}
	}
	for _ in 0..30 {
		if !conversation_busy(state, conversation_id).await {
			break;
		}
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
	state.forget_conversation(conversation_id).await;
}

fn emit_status(app: &AppHandle) {
	let app = app.clone();
	tauri::async_runtime::spawn(async move {
		let state = ml5::state(true).await;
		let _ = app.emit("ml5://status", &state);
	});
}

#[tauri::command]
pub async fn ui_ready(app: AppHandle, launch: State<'_, Launch>) -> Result<(), String> {
	let first = !launch.ready.swap(true, Ordering::SeqCst);
	let hidden = launch.hidden.swap(false, Ordering::SeqCst);
	if first && hidden {
		for _ in 0..50 {
			if launch.tray.load(Ordering::SeqCst) {
				return Ok(());
			}
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}
	show_main(&app);
	Ok(())
}

#[tauri::command]
pub async fn settings_get(state: State<'_, Arc<AppState>>) -> Result<Settings, String> {
	Ok(state.settings_snapshot().await)
}

#[tauri::command]
pub async fn settings_set(state: State<'_, Arc<AppState>>, settings: Settings) -> Result<Settings, String> {
	let saved = update_settings(&state, |current| {
		let window = current.window.clone();
		let token = current.portal_restore_token.clone();
		let legacy = current.legacy_handled;
		*current = settings;
		current.window = window;
		current.portal_restore_token = token;
		current.legacy_handled = current.legacy_handled || legacy;
	})
	.await?;
	let resolved = theme::current(&saved.theme).await;
	*state.theme.write().await = resolved;
	Ok(saved)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretStatus {
	pub has_key: bool,
	pub backend: String,
}

fn check_secret_provider(settings: &Settings, provider: &str) -> Result<(), String> {
	if provider == "ml5" {
		return Err("The local engine does not use an API key.".to_string());
	}
	if !known_provider(settings, provider) {
		return Err("That provider is not set up in Ash, so no key can be stored for it.".to_string());
	}
	Ok(())
}

async fn secret_state(state: &AppState, provider: &str) -> SecretStatus {
	let backend = state.secrets.backend_of(provider).await;
	SecretStatus { has_key: !backend.is_empty(), backend }
}

#[tauri::command]
pub async fn secret_status(state: State<'_, Arc<AppState>>, provider: String) -> Result<SecretStatus, String> {
	check_secret_provider(&state.settings_snapshot().await, &provider)?;
	Ok(secret_state(&state, &provider).await)
}

#[tauri::command]
pub async fn secret_set(state: State<'_, Arc<AppState>>, provider: String, value: String) -> Result<SecretStatus, String> {
	check_secret_provider(&state.settings_snapshot().await, &provider)?;
	let value = value.trim();
	if value.chars().any(|character| character.is_control() || character.is_whitespace()) {
		return Err("API keys cannot contain spaces or line breaks. Paste the key again.".to_string());
	}
	if value.len() > 4096 {
		return Err("That is far too long to be an API key.".to_string());
	}
	state.secrets.set(&provider, value).await?;
	Ok(secret_state(&state, &provider).await)
}

#[tauri::command]
pub async fn secret_delete(state: State<'_, Arc<AppState>>, provider: String) -> Result<SecretStatus, String> {
	let provider = provider.trim().to_string();
	if provider.is_empty() || provider == "ml5" {
		return Err("There is no key to remove for that provider.".to_string());
	}
	state.secrets.delete(&provider).await?;
	Ok(secret_state(&state, &provider).await)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyImport {
	pub provider: Option<String>,
	pub has_key: bool,
}

#[tauri::command]
pub async fn legacy_import() -> Result<Option<LegacyImport>, String> {
	Ok(settings::read_legacy().map(|legacy| LegacyImport { provider: legacy.provider, has_key: legacy.api_key.is_some() }))
}

#[tauri::command]
pub async fn legacy_apply(state: State<'_, Arc<AppState>>) -> Result<Settings, String> {
	let legacy = settings::read_legacy().ok_or_else(|| "No settings from the old agent were found on this computer.".to_string())?;
	if let Some(key) = legacy.api_key.as_deref() {
		state.secrets.set("opencore", key).await.map_err(|error| format!("The old API key could not be saved: {error}"))?;
	}
	update_settings(&state, |current| {
		if let Some(provider) = legacy.provider.clone() {
			current.provider = provider;
		}
		current.legacy_handled = true;
	})
	.await
}

#[tauri::command]
pub async fn legacy_dismiss(state: State<'_, Arc<AppState>>) -> Result<Settings, String> {
	update_settings(&state, |current| current.legacy_handled = true).await
}

#[tauri::command]
pub async fn theme_get(state: State<'_, Arc<AppState>>) -> Result<ThemeState, String> {
	let preference = state.settings_snapshot().await.theme;
	let resolved = theme::current(&preference).await;
	*state.theme.write().await = resolved.clone();
	Ok(resolved)
}

static THEME_WATCH: std::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>> = std::sync::Mutex::new(None);

fn theme_key(theme: &ThemeState) -> String {
	serde_json::to_string(theme).unwrap_or_default()
}

#[tauri::command]
pub async fn theme_watch(state: State<'_, Arc<AppState>>, enable: bool) -> Result<(), String> {
	let mut slot = THEME_WATCH.lock().map_err(|_| "The theme watcher is unavailable.".to_string())?;
	if !enable {
		if let Some(handle) = slot.take() {
			handle.abort();
		}
		return Ok(());
	}
	if slot.is_some() {
		return Ok(());
	}
	let sender = state.theme_events.clone();
	*slot = Some(tauri::async_runtime::spawn(async move {
		let mut last = theme_key(&theme::resolve("system"));
		loop {
			theme::watch(sender.clone()).await;
			for _ in 0..10 {
				tokio::time::sleep(Duration::from_secs(3)).await;
				let now = theme::resolve("system");
				let key = theme_key(&now);
				if key != last {
					last = key;
					let _ = sender.send(now);
				}
			}
		}
	}));
	Ok(())
}

#[tauri::command]
pub async fn markdown_render(text: String) -> Result<String, String> {
	tokio::task::spawn_blocking(move || markdown::render(&text)).await.map_err(|error| format!("The message could not be shown: {error}"))
}

fn local_model_info(model: ml5::Model) -> ModelInfo {
	let description = [model.parameter_size.clone(), model.quantization.clone(), model.architecture.clone()]
		.into_iter()
		.flatten()
		.filter(|part| !part.trim().is_empty())
		.collect::<Vec<_>>()
		.join(" · ");
	ModelInfo {
		id: model.id,
		name: model.name,
		provider: "ml5".to_string(),
		capabilities: model.capabilities,
		vision: model.vision,
		tools: model.tools,
		thinking: model.thinking,
		size_bytes: (model.size_bytes > 0).then_some(model.size_bytes),
		loaded: model.loaded,
		context_length: model.context_length.map(|length| length.min(u32::MAX as u64) as u32),
		format: (!model.format.is_empty()).then_some(model.format),
		description: (!description.is_empty()).then_some(description),
	}
}

#[tauri::command]
pub async fn models_available(state: State<'_, Arc<AppState>>, provider: String) -> Result<Vec<ModelInfo>, String> {
	let settings = state.settings_snapshot().await;
	if !known_provider(&settings, &provider) {
		return Err("That provider is not set up in Ash.".to_string());
	}
	if provider == "ml5" {
		let mut models: Vec<ModelInfo> = ml5::models().await?.into_iter().map(local_model_info).collect();
		models.sort_by(|left, right| right.loaded.cmp(&left.loaded).then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase())));
		return Ok(models);
	}
	let key = state.secrets.get(&provider).await;
	if provider == "opencore" && key.is_none() {
		return Err("Add your OpenCORe API key in Settings to see its models.".to_string());
	}
	let base = chat::base_url(&settings, &provider);
	let ids = ml5::remote_models(&base, key.as_deref()).await?;
	let mut models = Vec::with_capacity(ids.len());
	for id in ids {
		let caps = chat::capabilities(&settings, &provider, &id).await;
		let mut capabilities = vec!["completion".to_string()];
		if caps.tools {
			capabilities.push("tools".to_string());
		}
		if caps.vision {
			capabilities.push("vision".to_string());
		}
		models.push(ModelInfo {
			name: id.clone(),
			id,
			provider: provider.clone(),
			capabilities,
			vision: caps.vision,
			tools: caps.tools,
			thinking: false,
			size_bytes: None,
			loaded: false,
			context_length: None,
			format: None,
			description: None,
		});
	}
	Ok(models)
}

#[tauri::command]
pub async fn model_health(state: State<'_, Arc<AppState>>, provider: String) -> Result<ml5::BackendHealth, String> {
	let settings = state.settings_snapshot().await;
	if !known_provider(&settings, &provider) {
		return Ok(ml5::BackendHealth { online: false, latency_ms: None, message: "This provider is not set up.".to_string() });
	}
	if provider == "ml5" {
		return Ok(ml5::health(true, providers::ML5_BASE, None).await);
	}
	let key = state.secrets.get(&provider).await;
	if provider == "opencore" && key.is_none() {
		return Ok(ml5::BackendHealth { online: false, latency_ms: None, message: "Add your OpenCORe API key in Settings first.".to_string() });
	}
	Ok(ml5::health(false, &chat::base_url(&settings, &provider), key.as_deref()).await)
}

#[tauri::command]
pub async fn model_favorites() -> Result<Vec<serde_json::Value>, String> {
	let suggestions = ml5::suggestions();
	let tag = |items: Vec<ml5::Suggestion>, key: &str| -> Vec<serde_json::Value> {
		items
			.into_iter()
			.filter_map(|item| {
				let mut value = serde_json::to_value(item).ok()?;
				value.as_object_mut()?.insert(key.to_string(), serde_json::Value::Bool(true));
				Some(value)
			})
			.collect()
	};
	let mut list = tag(suggestions.featured, "featured");
	list.extend(tag(suggestions.advanced, "advanced"));
	Ok(list)
}

#[tauri::command]
pub async fn ml5_status(force: bool) -> Result<ml5::DaemonState, String> {
	Ok(ml5::state(force).await)
}

#[tauri::command]
pub async fn ml5_start(app: AppHandle) -> Result<String, String> {
	let result = ml5::start_daemon().await;
	emit_status(&app);
	result
}

#[tauri::command]
pub async fn ml5_stop(app: AppHandle) -> Result<String, String> {
	let result = ml5::stop_daemon().await;
	emit_status(&app);
	result
}

fn model_name(model: &str) -> Result<String, String> {
	let model = model.trim();
	if model.is_empty() {
		return Err("Choose a model first.".to_string());
	}
	Ok(model.to_string())
}

#[tauri::command]
pub async fn ml5_load(app: AppHandle, model: String) -> Result<(), String> {
	let result = ml5::load(&model_name(&model)?).await;
	ml5::invalidate();
	emit_status(&app);
	result
}

#[tauri::command]
pub async fn ml5_unload(app: AppHandle, model: String) -> Result<(), String> {
	let result = ml5::unload(&model_name(&model)?).await;
	ml5::invalidate();
	emit_status(&app);
	result
}

#[tauri::command]
pub async fn ml5_rename(app: AppHandle, state: State<'_, Arc<AppState>>, model: String, new_name: String) -> Result<(), String> {
	let model = model_name(&model)?;
	let new_name = new_name.trim().to_string();
	if new_name.is_empty() {
		return Err("Type a new name for the model.".to_string());
	}
	if new_name.len() > 200 || new_name.chars().any(char::is_control) {
		return Err("That name cannot be used. Pick a shorter name without special characters.".to_string());
	}
	let result = ml5::rename(&model, &new_name).await;
	ml5::invalidate();
	emit_status(&app);
	if result.is_ok() && state.settings_snapshot().await.model_for("ml5").as_deref() == Some(model.as_str()) {
		let renamed = new_name.clone();
		update_settings(&state, move |settings| {
			settings.models.insert("ml5".to_string(), renamed);
		})
		.await?;
	}
	result
}

#[tauri::command]
pub async fn ml5_rescan(app: AppHandle) -> Result<(), String> {
	let result = ml5::rescan().await;
	ml5::invalidate();
	emit_status(&app);
	result
}

#[tauri::command]
pub async fn ml5_delete(app: AppHandle, model: String) -> Result<(), String> {
	let result = ml5::delete(&model_name(&model)?).await;
	ml5::invalidate();
	emit_status(&app);
	result
}

fn pull_payload(event: &PullEvent, id: &str, target: &str) -> serde_json::Value {
	let mut value = serde_json::to_value(event).unwrap_or_else(|_| serde_json::json!({}));
	if let Some(object) = value.as_object_mut() {
		if object.get("id").and_then(|found| found.as_str()).is_none_or(str::is_empty) {
			object.insert("id".to_string(), serde_json::Value::String(id.to_string()));
		}
		object.entry("target").or_insert_with(|| serde_json::Value::String(target.to_string()));
	}
	value
}

fn event_id(event: &PullEvent) -> &str {
	match event {
		PullEvent::Started { id } | PullEvent::Stage { id, .. } | PullEvent::Progress { id, .. } | PullEvent::Done { id, .. } | PullEvent::Failed { id, .. } => id,
	}
}

#[tauri::command]
pub async fn ml5_pull(app: AppHandle, reference: String, registry: String) -> Result<String, String> {
	let reference = reference.trim().to_string();
	if reference.is_empty() {
		return Err("Type the name of a model to download.".to_string());
	}
	let (guessed, cleaned, quant) = ml5::guess_registry(&reference);
	let requested = registry.trim();
	let (registry, target, quant) = if requested.is_empty() || requested == guessed {
		(guessed.to_string(), cleaned, quant)
	} else {
		(requested.to_string(), reference.clone(), None)
	};
	if !matches!(registry.as_str(), "hf" | "core" | "routes") {
		return Err("That model source is not supported. Use Hugging Face or the OpenCORe catalog.".to_string());
	}
	let mut stream = Box::pin(ml5::pull(&target, &registry, None, quant.as_deref()).await?);
	let first = tokio::time::timeout(Duration::from_secs(90), stream.next())
		.await
		.map_err(|_| "The local engine did not start the download in time. Try again in a moment.".to_string())?;
	let first = match first {
		None => return Err("The local engine ended the download before it began.".to_string()),
		Some(Err(error)) => return Err(error),
		Some(Ok(PullEvent::Failed { message, .. })) => return Err(message),
		Some(Ok(event)) => event,
	};
	let id = match event_id(&first) {
		"" => util::new_id(),
		found => found.to_string(),
	};
	if !matches!(first, PullEvent::Started { .. }) {
		let _ = app.emit("ml5://pull", pull_payload(&PullEvent::Started { id: id.clone() }, &id, &reference));
	}
	let _ = app.emit("ml5://pull", pull_payload(&first, &id, &reference));
	let pull_id = id.clone();
	let label = reference.clone();
	tauri::async_runtime::spawn(async move {
		let mut finished = matches!(first, PullEvent::Done { .. });
		while !finished {
			match stream.next().await {
				Some(Ok(event)) => {
					finished = matches!(event, PullEvent::Done { .. } | PullEvent::Failed { .. });
					let _ = app.emit("ml5://pull", pull_payload(&event, &pull_id, &label));
				}
				Some(Err(message)) => {
					let _ = app.emit("ml5://pull", pull_payload(&PullEvent::Failed { id: pull_id.clone(), message }, &pull_id, &label));
					finished = true;
				}
				None => {
					let message = "The download stopped before it finished.".to_string();
					let _ = app.emit("ml5://pull", pull_payload(&PullEvent::Failed { id: pull_id.clone(), message }, &pull_id, &label));
					finished = true;
				}
			}
		}
		ml5::invalidate();
		emit_status(&app);
	});
	Ok(id)
}

#[tauri::command]
pub async fn ml5_pull_cancel(id: String) -> Result<(), String> {
	let id = id.trim();
	if id.is_empty() {
		return Err("There is no download to stop.".to_string());
	}
	ml5::cancel_pull(id).await
}

#[tauri::command]
pub async fn conversations_list(state: State<'_, Arc<AppState>>, query: String) -> Result<Vec<Conversation>, String> {
	state.store.list_conversations(query.trim())
}

#[tauri::command]
pub async fn conversations_create(state: State<'_, Arc<AppState>>, mode: String, provider: String, model: String) -> Result<Conversation, String> {
	let settings = state.settings_snapshot().await;
	let mode = if mode == "helper" { "helper" } else { "chat" };
	let provider = if known_provider(&settings, &provider) { provider } else { settings.provider.clone() };
	let model = if model.trim().is_empty() { settings.model_for(&provider).unwrap_or_default() } else { model };
	state.store.create_conversation(mode, &provider, &model, settings.show_thinking)
}

#[tauri::command]
pub async fn conversations_get(state: State<'_, Arc<AppState>>, id: String) -> Result<Conversation, String> {
	state.store.get_conversation(&id)
}

#[tauri::command]
pub async fn conversations_update(state: State<'_, Arc<AppState>>, conversation: Conversation) -> Result<(), String> {
	let mut stored = state.store.get_conversation(&conversation.id)?;
	let title = conversation.title.trim();
	if !title.is_empty() && title != stored.title {
		stored.title = util::truncate_end(&title.replace(['\n', '\r', '\t'], " "), MAX_TITLE);
		stored.titled = true;
	}
	stored.pinned = conversation.pinned;
	stored.mode = if conversation.mode == "helper" { "helper".to_string() } else { "chat".to_string() };
	stored.think = conversation.think;
	let settings = state.settings_snapshot().await;
	if known_provider(&settings, &conversation.provider) {
		stored.provider = conversation.provider;
		stored.model = conversation.model;
	}
	state.store.update_conversation(&stored)
}

#[tauri::command]
pub async fn conversations_delete(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
	stop_conversation(&state, &id).await;
	state.store.delete_conversation(&id)
}

#[tauri::command]
pub async fn conversations_delete_all(state: State<'_, Arc<AppState>>) -> Result<(), String> {
	let ids: Vec<String> = state.turns.lock().await.values().map(|turn| turn.conversation_id.clone()).collect();
	for id in ids {
		stop_conversation(&state, &id).await;
	}
	let running: Vec<String> = state.running.lock().await.keys().cloned().collect();
	for id in running {
		actions::cancel_running(&state, &id).await;
	}
	let waiting: Vec<String> = state.approvals.lock().await.keys().cloned().collect();
	for id in waiting {
		state.resolve_approval(&id, false).await;
	}
	state.remembered.lock().await.clear();
	state.store.delete_all_conversations()
}

fn image_url(state: &AppState, id: &str) -> Option<String> {
	if id.starts_with("data:") {
		return Some(id.to_string());
	}
	state.store.image(id).ok().flatten().map(|record| providers::data_url(&record.mime, &record.data))
}

fn with_image_urls(state: &AppState, mut message: Message) -> Message {
	message.images = message.images.iter().filter_map(|id| image_url(state, id)).collect();
	message
}

#[tauri::command]
pub async fn messages_list(state: State<'_, Arc<AppState>>, conversation_id: String) -> Result<Vec<Message>, String> {
	let stored = state.store.messages(&conversation_id)?;
	Ok(stored.into_iter().map(|message| with_image_urls(&state, message)).collect())
}

#[tauri::command]
pub async fn message_delete(state: State<'_, Arc<AppState>>, conversation_id: String, message_id: String, scope: String) -> Result<(), String> {
	let scope = DeleteScope::parse(&scope).ok_or_else(|| "Choose what to delete: this message, the whole exchange, or everything from here on.".to_string())?;
	if conversation_busy(&state, &conversation_id).await {
		return Err("This conversation is still answering. Stop it first, then delete messages.".to_string());
	}
	state.store.delete_message(&conversation_id, &message_id, scope)
}

#[tauri::command]
pub async fn message_select(state: State<'_, Arc<AppState>>, conversation_id: String, message_id: String) -> Result<Vec<Message>, String> {
	if conversation_busy(&state, &conversation_id).await {
		return Err("This conversation is still answering. Stop it first, then switch versions.".to_string());
	}
	state.store.select_message(&conversation_id, &message_id)?;
	let stored = state.store.messages(&conversation_id)?;
	Ok(stored.into_iter().map(|message| with_image_urls(&state, message)).collect())
}

#[tauri::command]
pub async fn message_revise(state: State<'_, Arc<AppState>>, conversation_id: String, message_id: String, content: String) -> Result<Vec<Message>, String> {
	let original = state.store.get_message(&message_id)?;
	if original.conversation_id != conversation_id {
		return Err("That message belongs to a different conversation.".to_string());
	}
	if original.role != "assistant" {
		return Err("Only Ash's replies can be saved as a new version this way.".to_string());
	}
	if content.trim().is_empty() {
		return Err("The reply cannot be empty.".to_string());
	}
	if conversation_busy(&state, &conversation_id).await {
		return Err("This conversation is still answering. Stop it first, then edit the reply.".to_string());
	}
	let mut meta = if original.meta.is_object() { original.meta.clone() } else { serde_json::json!({}) };
	meta["edited"] = serde_json::json!(true);
	if let Some(fields) = meta.as_object_mut() {
		fields.remove("usage");
		fields.remove("notice");
	}
	state.store.add_message(&conversation_id, original.parent_id.as_deref(), "assistant", &content, &[], None, meta)?;
	let stored = state.store.messages(&conversation_id)?;
	Ok(stored.into_iter().map(|message| with_image_urls(&state, message)).collect())
}

#[tauri::command]
pub async fn message_update(state: State<'_, Arc<AppState>>, message: Message) -> Result<(), String> {
	let mut stored = state.store.get_message(&message.id)?;
	if stored.conversation_id != message.conversation_id {
		return Err("That message belongs to a different conversation.".to_string());
	}
	if conversation_busy(&state, &stored.conversation_id).await {
		return Err("This conversation is still answering. Stop it first, then edit the message.".to_string());
	}
	stored.content = message.content;
	stored.thinking = message.thinking;
	state.store.update_message(&stored)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSend {
	#[serde(default)]
	pub conversation_id: String,
	#[serde(default)]
	pub provider: String,
	#[serde(default)]
	pub model: String,
	#[serde(default)]
	pub content: String,
	#[serde(default)]
	pub images: Vec<String>,
	#[serde(default)]
	pub think: Option<bool>,
}

fn store_image(state: &AppState, conversation_id: Option<&str>, url: &str) -> Result<String, String> {
	let (_, bytes) = providers::decode_data_url(url).ok_or_else(|| "One of the pictures could not be read. Attach it again.".to_string())?;
	if bytes.len() > MAX_IMAGE_BYTES {
		return Err("One of the pictures is larger than 20 MB. Attach a smaller picture.".to_string());
	}
	let unsupported = || "One of the attachments is not a picture Ash can read. Use PNG, JPEG, WebP, GIF or BMP.".to_string();
	let reader = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().map_err(|_| unsupported())?;
	let mime = match reader.format().ok_or_else(unsupported)? {
		image::ImageFormat::Png => "image/png",
		image::ImageFormat::Jpeg => "image/jpeg",
		image::ImageFormat::WebP => "image/webp",
		image::ImageFormat::Gif => "image/gif",
		image::ImageFormat::Bmp => "image/bmp",
		_ => return Err(unsupported()),
	};
	let (width, height) = reader.into_dimensions().map_err(|_| "One of the pictures is damaged and could not be read.".to_string())?;
	state.store.add_image(conversation_id, mime, &bytes, width, height)
}

#[tauri::command]
pub async fn chat_send(state: State<'_, Arc<AppState>>, chat: State<'_, Arc<Chat>>, approver: State<'_, Arc<Approver>>, request: ChatSend) -> Result<String, String> {
	let settings = state.settings_snapshot().await;
	let provider = if request.provider.trim().is_empty() { settings.provider.clone() } else { request.provider.trim().to_string() };
	if !known_provider(&settings, &provider) {
		return Err("That provider is not set up in Ash. Choose another one in Settings.".to_string());
	}
	let model = if request.model.trim().is_empty() { settings.model_for(&provider).unwrap_or_default() } else { request.model.trim().to_string() };
	if request.content.trim().is_empty() && request.images.is_empty() {
		return Err("Type a message first.".to_string());
	}
	if request.images.len() > MAX_IMAGES {
		return Err(format!("Attach at most {MAX_IMAGES} pictures to one message."));
	}
	if !request.conversation_id.is_empty() {
		state.store.get_conversation(&request.conversation_id)?;
		if conversation_busy(&state, &request.conversation_id).await {
			return Err("This conversation is still answering. Wait for it to finish or stop it first.".to_string());
		}
	}
	let owner = (!request.conversation_id.is_empty()).then_some(request.conversation_id.as_str());
	let mut images = Vec::with_capacity(request.images.len());
	for url in &request.images {
		images.push(store_image(&state, owner, url)?);
	}
	let start = chat::Start { conversation_id: request.conversation_id, anchor: chat::Anchor::Tail, provider, model, content: request.content, images, think: request.think };
	chat::spawn(Arc::clone(&state), Arc::clone(&chat), Arc::clone(&approver), start).await
}

#[tauri::command]
pub async fn chat_stop(state: State<'_, Arc<AppState>>, turn_id: String) -> Result<(), String> {
	chat::stop(&state, &turn_id).await;
	Ok(())
}

async fn conversation_model(state: &AppState, conversation: &Conversation) -> (String, String) {
	let settings = state.settings_snapshot().await;
	let provider = if known_provider(&settings, &conversation.provider) { conversation.provider.clone() } else { settings.provider.clone() };
	let model = if conversation.model.trim().is_empty() || provider != conversation.provider { settings.model_for(&provider).unwrap_or_default() } else { conversation.model.clone() };
	(provider, model)
}

#[tauri::command]
pub async fn chat_retry(state: State<'_, Arc<AppState>>, chat: State<'_, Arc<Chat>>, approver: State<'_, Arc<Approver>>, conversation_id: String) -> Result<String, String> {
	let conversation = state.store.get_conversation(&conversation_id)?;
	let (provider, model) = conversation_model(&state, &conversation).await;
	chat::retry(Arc::clone(&state), Arc::clone(&chat), Arc::clone(&approver), &conversation_id, provider, model, None).await
}

#[tauri::command]
pub async fn chat_regenerate(
	state: State<'_, Arc<AppState>>,
	chat: State<'_, Arc<Chat>>,
	approver: State<'_, Arc<Approver>>,
	conversation_id: String,
	message_id: String,
	think: Option<bool>,
) -> Result<String, String> {
	let conversation = state.store.get_conversation(&conversation_id)?;
	let (provider, model) = conversation_model(&state, &conversation).await;
	chat::regenerate(Arc::clone(&state), Arc::clone(&chat), Arc::clone(&approver), &conversation_id, &message_id, provider, model, think).await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn chat_edit(
	state: State<'_, Arc<AppState>>,
	chat: State<'_, Arc<Chat>>,
	approver: State<'_, Arc<Approver>>,
	conversation_id: String,
	message_id: String,
	content: String,
	think: Option<bool>,
) -> Result<String, String> {
	let conversation = state.store.get_conversation(&conversation_id)?;
	let (provider, model) = conversation_model(&state, &conversation).await;
	chat::edit(Arc::clone(&state), Arc::clone(&chat), Arc::clone(&approver), &conversation_id, &message_id, content, provider, model, think).await
}

#[tauri::command]
pub async fn chat_title(state: State<'_, Arc<AppState>>, chat: State<'_, Arc<Chat>>, conversation_id: String) -> Result<String, String> {
	let conversation = state.store.get_conversation(&conversation_id)?;
	let (provider, model) = conversation_model(&state, &conversation).await;
	if model.trim().is_empty() {
		return Err("Choose a model first, then Ash can name this chat.".to_string());
	}
	let settings = state.settings_snapshot().await;
	let key = if provider == "ml5" { None } else { state.secrets.get(&provider).await };
	if provider == "opencore" && key.is_none() {
		return Err("Add your OpenCORe API key in Settings first.".to_string());
	}
	chat::generate_title(&state, &chat, &conversation_id, provider.clone(), model, chat::base_url(&settings, &provider), key).await
}

#[tauri::command]
pub async fn tool_catalog(state: State<'_, Arc<AppState>>) -> Result<Vec<serde_json::Value>, String> {
	let tier = state.settings_snapshot().await.tier;
	Ok(tools::catalog()
		.into_iter()
		.filter(|spec| tools::allowed(spec, tier))
		.filter_map(|spec| {
			let mut value = serde_json::to_value(&spec).ok()?;
			let object = value.as_object_mut()?;
			object.remove("parameters");
			object.insert("tier".to_string(), serde_json::Value::String(if spec.mutating { "ask" } else { "observe" }.to_string()));
			Some(value)
		})
		.collect())
}

#[tauri::command]
pub async fn tool_approve(state: State<'_, Arc<AppState>>, approver: State<'_, Arc<Approver>>, action_id: String, allow: bool, remember: bool) -> Result<(), String> {
	if allow && remember {
		if let Some(action) = state.store.get_action(&action_id)? {
			let risk = tools::Risk::from_label(&action.risk);
			if !action.root && matches!(risk, tools::Risk::Low | tools::Risk::Medium) {
				state.remember(action.conversation_id.as_deref(), &action.tool).await;
			}
		}
	}
	if state.resolve_approval(&action_id, allow).await {
		return Ok(());
	}
	match state.store.get_action(&action_id)? {
		Some(mut action) if action.status == "awaiting" => {
			action.status = "stopped".to_string();
			action.finished = Some(util::now_ms());
			action.output = "This request was no longer waiting for an answer.".to_string();
			state.store.update_action(&action)?;
			approver.changed(&action);
			Err("This request is no longer waiting for an answer. Ask Ash again if it is still needed.".to_string())
		}
		Some(_) => Ok(()),
		None => Err("That request no longer exists.".to_string()),
	}
}

#[tauri::command]
pub async fn tool_cancel(state: State<'_, Arc<AppState>>, approver: State<'_, Arc<Approver>>, action_id: String) -> Result<(), String> {
	if actions::cancel_running(&state, &action_id).await {
		return Ok(());
	}
	if let Some(mut action) = state.store.get_action(&action_id)? {
		if matches!(action.status.as_str(), "pending" | "awaiting" | "running") {
			action.status = "cancelled".to_string();
			action.finished = Some(util::now_ms());
			if action.output.is_empty() {
				action.output = "The user stopped this action.".to_string();
			}
			state.store.update_action(&action)?;
			approver.changed(&action);
		}
	}
	Ok(())
}

#[tauri::command]
pub async fn tools_set_tier(state: State<'_, Arc<AppState>>, tier: Tier) -> Result<Settings, String> {
	let saved = update_settings(&state, |settings| settings.tier = tier).await?;
	if tier == Tier::Observe {
		state.remembered.lock().await.clear();
	}
	Ok(saved)
}

#[tauri::command]
pub async fn computer_status() -> Result<serde_json::Value, String> {
	Ok(status::snapshot().await)
}

#[tauri::command]
pub async fn actions_list(state: State<'_, Arc<AppState>>, limit: i64, before: Option<i64>) -> Result<Vec<Action>, String> {
	state.store.list_actions(limit.clamp(1, 1000), before)
}

#[tauri::command]
pub async fn actions_for_conversation(state: State<'_, Arc<AppState>>, conversation_id: String) -> Result<Vec<Action>, String> {
	state.store.actions_for_conversation(&conversation_id)
}

#[tauri::command]
pub async fn actions_clear(state: State<'_, Arc<AppState>>) -> Result<(), String> {
	state.store.clear_actions()
}

#[tauri::command]
pub async fn action_image(state: State<'_, Arc<AppState>>, id: String) -> Result<Option<String>, String> {
	if let Some(record) = state.store.image(&id)? {
		return Ok(Some(providers::data_url(&record.mime, &record.data)));
	}
	let Some(action) = state.store.get_action(&id)? else { return Ok(None) };
	let Some(image_id) = action.image_id else { return Ok(None) };
	Ok(state.store.image(&image_id)?.map(|record| providers::data_url(&record.mime, &record.data)))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuidedFix {
	pub id: &'static str,
	pub title: &'static str,
	pub description: &'static str,
	pub steps: Vec<&'static str>,
	#[serde(skip)]
	command: String,
	#[serde(skip)]
	root: bool,
	#[serde(skip)]
	timeout: u64,
}

const APT_OPTIONS: &str = "-o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold";

fn guided_catalog() -> Vec<GuidedFix> {
	let mut fixes = vec![
		GuidedFix {
			id: "updates",
			title: "Install all updates",
			description: "Downloads and installs every waiting update for the system and its programs, then removes packages nothing needs any more.",
			steps: vec!["Refresh the list of available updates", "Install every update", "Remove packages that are no longer needed"],
			command: format!("apt-get update && apt-get -y {APT_OPTIONS} full-upgrade && apt-get -y {APT_OPTIONS} autoremove --purge"),
			root: true,
			timeout: 3600,
		},
		GuidedFix {
			id: "repair-packages",
			title: "Repair interrupted installs",
			description: "Finishes installs or updates that were cut off, for example by a power cut, and fixes missing dependencies.",
			steps: vec!["Finish setting up half-installed packages", "Install anything that is missing", "Check that the package system is healthy"],
			command: format!("dpkg --configure -a && apt-get -y {APT_OPTIONS} -f install && apt-get check"),
			root: true,
			timeout: 1800,
		},
		GuidedFix {
			id: "disk-space",
			title: "Free up disk space",
			description: "Removes packages nothing uses, clears the download cache of installed updates and trims old system logs.",
			steps: vec!["Show how full the disk is", "Remove unused packages", "Clear the update download cache", "Keep only the last two weeks of system logs", "Show how full the disk is now"],
			command: format!("df -h / && apt-get -y {APT_OPTIONS} autoremove --purge && apt-get clean && journalctl --vacuum-time=14d && echo && df -h /"),
			root: true,
			timeout: 900,
		},
		GuidedFix {
			id: "sound",
			title: "Fix sound",
			description: "Restarts the sound system for your session. Use this when sound stopped working or a headset is not picked up.",
			steps: vec!["Restart the sound services", "Check that they are running again", "List the sound devices that were found"],
			command: "systemctl --user restart wireplumber.service pipewire.service pipewire-pulse.service && sleep 2 && systemctl --user is-active wireplumber.service pipewire.service pipewire-pulse.service && { if command -v wpctl >/dev/null 2>&1; then wpctl status | head -n 60; fi; }".to_string(),
			root: false,
			timeout: 60,
		},
		GuidedFix {
			id: "network",
			title: "Restart the network",
			description: "Restarts the service that manages Wi-Fi and wired connections. Use this when the internet stopped working.",
			steps: vec!["Restart the network service", "Wait for it to reconnect", "Show the connection state"],
			command: "systemctl restart NetworkManager.service && sleep 5 && nmcli general status && nmcli -f DEVICE,TYPE,STATE,CONNECTION device".to_string(),
			root: true,
			timeout: 120,
		},
		GuidedFix {
			id: "panel",
			title: "Restart the desktop panel",
			description: "Restarts the taskbar and desktop shell when they froze or stopped responding. Open windows stay open.",
			steps: vec!["Restart the desktop shell", "Check that it is running again"],
			command: "if systemctl --user cat plasma-plasmashell.service >/dev/null 2>&1; then systemctl --user restart plasma-plasmashell.service; else kquitapp6 plasmashell >/dev/null 2>&1; sleep 1; setsid -f plasmashell >/dev/null 2>&1 < /dev/null; fi; sleep 3; if pgrep -x plasmashell >/dev/null; then echo 'The desktop panel is running again.'; else echo 'The desktop panel did not come back.' >&2; exit 1; fi".to_string(),
			root: false,
			timeout: 60,
		},
	];
	if paths::has_program("flatpak") {
		fixes.push(GuidedFix {
			id: "flatpak",
			title: "Repair and update Flatpak apps",
			description: "Checks the apps installed for everyone from the app store for damage, updates them and removes runtimes nothing uses.",
			steps: vec!["Check every installed app for damaged files", "Update all apps", "Remove runtimes nothing uses"],
			command: "flatpak repair && flatpak update -y --noninteractive && flatpak uninstall --unused -y --noninteractive".to_string(),
			root: true,
			timeout: 3600,
		});
	}
	fixes
}

#[tauri::command]
pub async fn guided_fixes() -> Result<Vec<GuidedFix>, String> {
	Ok(guided_catalog())
}

#[tauri::command]
pub async fn guided_fix_run(state: State<'_, Arc<AppState>>, approver: State<'_, Arc<Approver>>, id: String) -> Result<String, String> {
	let fix = guided_catalog().into_iter().find(|fix| fix.id == id).ok_or_else(|| "That fix is not available on this computer.".to_string())?;
	let settings = state.settings_snapshot().await;
	if settings.tier == Tier::Observe {
		return Err("Fixes change the system, which is not allowed while the permission tier is Observe. Choose Ask or Trusted first.".to_string());
	}
	if fix.root && !tools::root::available() {
		return Err(tools::root::unavailable_reason().unwrap_or_else(|| "Administrator actions are unavailable.".to_string()));
	}
	let provider = settings.provider.clone();
	let model = settings.model_for(&provider).unwrap_or_default();
	let mut conversation = state.store.create_conversation("helper", &provider, &model, settings.show_thinking)?;
	conversation.title = fix.title.to_string();
	conversation.titled = true;
	state.store.update_conversation(&conversation)?;
	let mut intro = format!("**{}**\n\n{}\n\nThis is what will happen:\n\n", fix.title, fix.description);
	for (index, step) in fix.steps.iter().enumerate() {
		intro.push_str(&format!("{}. {step}\n", index + 1));
	}
	intro.push_str(if fix.root {
		"\nIt needs administrator rights, so your password will be asked for after you approve it."
	} else {
		"\nIt runs as you and does not need your password."
	});
	let arguments = serde_json::json!({ "command": fix.command, "root": fix.root, "timeout": fix.timeout });
	let call_id = format!("fix_{}", util::new_id());
	let mut message = state.store.add_message(&conversation.id, None, "assistant", &intro, &[], None, serde_json::json!({ "provider": provider, "model": model, "guidedFix": fix.id }))?;
	message.tool_calls = vec![StoredToolCall { id: call_id.clone(), name: "shell_execute".to_string(), arguments: arguments.to_string() }];
	state.store.update_message(&message)?;
	let proposal = actions::propose(&call_id, "shell_execute", &arguments);
	let action_id = proposal.action_id.clone();
	let title = proposal.title.clone();
	let mut feed = approver.events.subscribe();
	let runner = Arc::clone(&state);
	let live = Arc::clone(&approver);
	let conversation_id = conversation.id.clone();
	let message_id = message.id.clone();
	let message_id_for_result = message.id.clone();
	let tier = settings.tier;
	tauri::async_runtime::spawn(async move {
		let outcome = actions::run(Arc::clone(&runner), live, proposal, Some(conversation_id.clone()), Some(message_id), CancellationToken::new(), tier).await;
		let output = match outcome {
			Ok(executed) if executed.ok => executed.output,
			Ok(executed) => format!("The action did not succeed.\n{}", executed.output),
			Err(error) => format!("The action failed: {error}"),
		};
		let _ = runner.store.add_message(
			&conversation_id,
			Some(&message_id_for_result),
			"tool",
			&util::truncate_middle(&output, 24 * 1024, 8 * 1024),
			&[],
			Some(&call_id),
			serde_json::json!({ "tool": "shell_execute", "imageId": null, "title": title }),
		);
	});
	let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
	loop {
		match tokio::time::timeout_at(deadline, feed.recv()).await {
			Ok(Ok(ActionEvent::Changed { action })) if action.id == action_id => break,
			Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
			Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => break,
		}
	}
	Ok(action_id)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutostartStatus {
	pub available: bool,
	pub enabled: bool,
}

#[tauri::command]
pub async fn autostart_status() -> Result<AutostartStatus, String> {
	Ok(AutostartStatus { available: autostart::available(), enabled: autostart::enabled() })
}

#[tauri::command]
pub async fn autostart_set(enabled: bool) -> Result<AutostartStatus, String> {
	tokio::task::spawn_blocking(move || autostart::set(enabled)).await.map_err(|error| format!("Could not change starting with the desktop: {error}"))??;
	Ok(AutostartStatus { available: autostart::available(), enabled: autostart::enabled() })
}

#[tauri::command]
pub async fn clipboard_read() -> Result<String, String> {
	clipboard::read_text().await
}

#[tauri::command]
pub async fn clipboard_write(text: String) -> Result<(), String> {
	clipboard::write_text(&text).await
}

#[tauri::command]
pub async fn file_save(path: String, contents: String) -> Result<String, String> {
	let path = PathBuf::from(path.trim());
	if path.as_os_str().is_empty() || !path.is_absolute() {
		return Err("Choose where to save the file.".to_string());
	}
	if path.is_dir() {
		return Err("That is a folder. Choose a file name instead.".to_string());
	}
	let parent = path.parent().filter(|parent| parent.is_dir()).ok_or_else(|| "That folder does not exist any more. Choose another place.".to_string())?.to_path_buf();
	let target = path.clone();
	tokio::task::spawn_blocking(move || -> Result<(), String> {
		let name = target.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_else(|| "export".to_string());
		let temporary = parent.join(format!(".{name}.ash-{}", util::new_id()));
		std::fs::write(&temporary, contents.as_bytes()).map_err(|error| format!("Could not write {}: {error}", target.display()))?;
		std::fs::rename(&temporary, &target).map_err(|error| {
			let _ = std::fs::remove_file(&temporary);
			format!("Could not save {}: {error}", target.display())
		})
	})
	.await
	.map_err(|error| format!("Could not save the file: {error}"))??;
	Ok(path.to_string_lossy().to_string())
}

fn local_time(ms: i64) -> String {
	let seconds = (ms / 1000) as libc::time_t;
	let mut parts: libc::tm = unsafe { std::mem::zeroed() };
	if unsafe { libc::localtime_r(&seconds, &mut parts) }.is_null() {
		return ms.to_string();
	}
	format!("{:04}-{:02}-{:02} {:02}:{:02}", parts.tm_year + 1900, parts.tm_mon + 1, parts.tm_mday, parts.tm_hour, parts.tm_min)
}

fn fenced(text: &str) -> String {
	let (mut longest, mut run) = (0usize, 0usize);
	for character in text.chars() {
		if character == '`' {
			run += 1;
			longest = longest.max(run);
		} else {
			run = 0;
		}
	}
	let fence = "`".repeat((longest + 1).max(3));
	format!("{fence}\n{}\n{fence}\n", text.trim_end())
}

fn status_label(status: &str) -> &'static str {
	match status {
		"done" => "finished",
		"failed" => "failed",
		"denied" => "not allowed",
		"cancelled" => "stopped",
		"stopped" => "interrupted",
		"running" => "running",
		"awaiting" => "waiting for approval",
		_ => "pending",
	}
}

fn export_markdown(settings: &Settings, conversation: &Conversation, messages: &[Message], actions: &[Action]) -> String {
	let mut out = format!("# {}\n\n", conversation.title.trim());
	let model = if conversation.model.is_empty() { "no model".to_string() } else { conversation.model.clone() };
	out.push_str(&format!(
		"Exported from Ash Agent on {}. {} mode, {} on {}.\n\n---\n\n",
		local_time(util::now_ms()),
		if conversation.mode == "helper" { "Helper" } else { "Chat" },
		model,
		provider_label(settings, &conversation.provider)
	));
	let covered: std::collections::HashSet<&str> = actions.iter().filter_map(|action| action.tool_call_id.as_deref()).collect();
	for message in messages {
		match message.role.as_str() {
			"user" => {
				out.push_str(&format!("### You · {}\n\n", local_time(message.created)));
				if !message.content.trim().is_empty() {
					out.push_str(message.content.trim_end());
					out.push_str("\n\n");
				}
				if !message.images.is_empty() {
					out.push_str(&format!("_{} picture{} attached_\n\n", message.images.len(), if message.images.len() == 1 { "" } else { "s" }));
				}
			}
			"assistant" => {
				out.push_str(&format!("### Ash · {}\n\n", local_time(message.created)));
				if !message.thinking.trim().is_empty() {
					out.push_str("<details><summary>Thinking</summary>\n\n");
					out.push_str(message.thinking.trim_end());
					out.push_str("\n\n</details>\n\n");
				}
				if !message.content.trim().is_empty() {
					out.push_str(message.content.trim_end());
					out.push_str("\n\n");
				}
				for action in actions.iter().filter(|action| action.message_id.as_deref() == Some(message.id.as_str())) {
					let code = action.exit_code.map(|code| format!(", exit code {code}")).unwrap_or_default();
					out.push_str(&format!("> **{}**: {}{}{}\n\n", action.title, status_label(&action.status), code, if action.root { ", with administrator rights" } else { "" }));
					if !action.detail.trim().is_empty() {
						out.push_str(&fenced(&action.detail));
						out.push('\n');
					}
					if !action.output.trim().is_empty() {
						out.push_str(&fenced(&util::truncate_middle(action.output.trim_end(), EXPORT_OUTPUT, EXPORT_OUTPUT / 2)));
						out.push('\n');
					}
				}
			}
			"tool" => {
				if message.tool_call_id.as_deref().is_some_and(|id| covered.contains(id)) {
					continue;
				}
				let title = message.meta.get("title").and_then(|value| value.as_str()).unwrap_or("Result");
				out.push_str(&format!("> **{title}**\n\n"));
				out.push_str(&fenced(&util::truncate_middle(message.content.trim_end(), EXPORT_OUTPUT, EXPORT_OUTPUT / 2)));
				out.push('\n');
			}
			_ => {}
		}
	}
	format!("{}\n", out.trim_end())
}

#[tauri::command]
pub async fn conversations_export(state: State<'_, Arc<AppState>>, id: String, format: String) -> Result<String, String> {
	let conversation = state.store.get_conversation(&id)?;
	let messages = state.store.messages(&id)?;
	let shown: std::collections::HashSet<&str> = messages.iter().map(|message| message.id.as_str()).collect();
	let actions: Vec<Action> = state.store.actions_for_conversation(&id)?.into_iter().filter(|action| action.message_id.as_deref().is_none_or(|message_id| shown.contains(message_id))).collect();
	match format.as_str() {
		"json" => {
			let mut images: HashMap<String, String> = HashMap::new();
			let referenced = messages.iter().flat_map(|message| message.images.iter().cloned()).chain(actions.iter().filter_map(|action| action.image_id.clone()));
			for image in referenced {
				if let Some(url) = image_url(&state, &image) {
					images.insert(image, url);
				}
			}
			let document = serde_json::json!({
				"format": "ash-agent-conversation",
				"version": 1,
				"exportedAt": util::now_ms(),
				"conversation": conversation,
				"messages": messages,
				"actions": actions,
				"images": images,
			});
			serde_json::to_string_pretty(&document).map_err(|error| format!("The chat could not be exported: {error}"))
		}
		"md" | "markdown" => Ok(export_markdown(&state.settings_snapshot().await, &conversation, &messages, &actions)),
		_ => Err("Choose Markdown or JSON for the export.".to_string()),
	}
}

fn escape_cut(text: &str) -> usize {
	let Some(start) = text.rfind('\u{1b}') else { return text.len() };
	let tail = &text[start..];
	if tail.len() >= 96 {
		return text.len();
	}
	static COMPLETE: OnceLock<Regex> = OnceLock::new();
	let complete = COMPLETE.get_or_init(|| Regex::new(r"^\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[PX^_][^\x1b]*\x1b\\|[@-Z\\-_])").unwrap());
	if complete.is_match(tail) {
		return text.len();
	}
	match tail[1..].chars().next() {
		None | Some('[' | ']' | 'P' | 'X' | '^' | '_') => start,
		Some(_) => text.len(),
	}
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionOutput<'a> {
	session_id: &'a str,
	data: String,
	stream: &'a str,
}

fn forward_session(app: AppHandle, state: Arc<AppState>, session: Arc<pty::Session>) {
	let mut feed = session.events.subscribe();
	tauri::async_runtime::spawn(async move {
		let mut held = String::new();
		loop {
			match feed.recv().await {
				Ok(output) if output.stream == "out" => {
					held.push_str(&output.data);
					let cut = escape_cut(&held);
					let ready: String = held.drain(..cut).collect();
					let text = util::strip_ansi(&ready);
					if !text.is_empty() {
						let _ = app.emit("session://output", SessionOutput { session_id: &session.id, data: text, stream: "out" });
					}
				}
				Ok(_) | Err(broadcast::error::RecvError::Closed) => break,
				Err(broadcast::error::RecvError::Lagged(_)) => continue,
			}
		}
		let rest = util::strip_ansi(&held);
		if !rest.is_empty() {
			let _ = app.emit("session://output", SessionOutput { session_id: &session.id, data: rest, stream: "out" });
		}
		let code = session.exit_code().map(|code| format!(" with exit code {code}")).unwrap_or_default();
		let _ = app.emit("session://output", SessionOutput { session_id: &session.id, data: format!("\nThe shell has closed{code}.\n"), stream: "err" });
		let mut sessions = state.sessions.lock().await;
		if sessions.get(&session.id).is_some_and(|current| Arc::ptr_eq(current, &session)) {
			sessions.remove(&session.id);
		}
	});
}

async fn user_session(state: &AppState, id: &str) -> Result<Arc<pty::Session>, String> {
	match state.sessions.lock().await.get(id) {
		Some(session) if session.agent => Err("That terminal is being used by the helper. Open your own terminal instead.".to_string()),
		Some(session) if session.alive() => Ok(Arc::clone(session)),
		_ => Err("That terminal has closed. Start a new one.".to_string()),
	}
}

#[tauri::command]
pub async fn session_start(app: AppHandle, state: State<'_, Arc<AppState>>, shell: Option<String>, root: bool) -> Result<String, String> {
	if state.sessions.lock().await.values().filter(|session| !session.agent && session.alive()).count() >= MAX_USER_SESSIONS {
		return Err(format!("{MAX_USER_SESSIONS} terminals are already open. Close one first."));
	}
	let id = util::new_id();
	let starter = id.clone();
	let session = tokio::task::spawn_blocking(move || -> Result<Arc<pty::Session>, String> {
		let session = pty::Session::start(&starter, pty::Options { shell, root, agent: false, cols: 120, rows: 32, cwd: None })?;
		match session.prepare(&CancellationToken::new()) {
			Ok(()) => Ok(session),
			Err(error) => {
				session.stop();
				Err(error)
			}
		}
	})
	.await
	.map_err(|error| format!("The terminal could not be started: {error}"))??;
	state.sessions.lock().await.insert(id.clone(), Arc::clone(&session));
	forward_session(app, Arc::clone(&state), session);
	Ok(id)
}

#[tauri::command]
pub async fn session_write(state: State<'_, Arc<AppState>>, id: String, data: String) -> Result<(), String> {
	if data.len() > MAX_SESSION_WRITE {
		return Err("That is too much text to send to the terminal at once.".to_string());
	}
	let session = user_session(&state, &id).await?;
	tokio::task::spawn_blocking(move || session.write(&data)).await.map_err(|error| format!("Could not write to the terminal: {error}"))?
}

#[tauri::command]
pub async fn session_resize(state: State<'_, Arc<AppState>>, id: String, cols: u16, rows: u16) -> Result<(), String> {
	let session = user_session(&state, &id).await?;
	session.resize(cols.clamp(10, 1000), rows.clamp(2, 500))
}

#[tauri::command]
pub async fn session_stop(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
	let removed = state.sessions.lock().await.remove(&id);
	if let Some(session) = removed {
		tokio::task::spawn_blocking(move || session.stop()).await.map_err(|error| format!("Could not close the terminal: {error}"))?;
	}
	Ok(())
}

#[tauri::command]
pub async fn sessions_list(state: State<'_, Arc<AppState>>) -> Result<Vec<pty::SessionInfo>, String> {
	let sessions: Vec<Arc<pty::Session>> = state.sessions.lock().await.values().filter(|session| !session.agent).cloned().collect();
	let mut list: Vec<pty::SessionInfo> = sessions.iter().map(|session| session.info()).collect();
	list.sort_by(|left, right| left.id.cmp(&right.id));
	Ok(list)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Completion {
	pub line: String,
	pub options: Vec<String>,
}

fn word_start(line: &str) -> usize {
	let mut start = 0;
	let mut escaped = false;
	for (index, character) in line.char_indices() {
		if escaped {
			escaped = false;
		} else if character == '\\' {
			escaped = true;
		} else if character.is_whitespace() || matches!(character, '|' | ';' | '&' | '(' | ')' | '<' | '>' | '=') {
			start = index + character.len_utf8();
		}
	}
	start
}

fn unescape_word(word: &str) -> String {
	let mut out = String::with_capacity(word.len());
	let mut escaped = false;
	for character in word.chars() {
		if escaped {
			out.push(character);
			escaped = false;
		} else if character == '\\' {
			escaped = true;
		} else if character != '\'' && character != '"' {
			out.push(character);
		}
	}
	out
}

fn escape_word(word: &str) -> String {
	let (prefix, rest) = match word.strip_prefix("~/") {
		Some(rest) => ("~/", rest),
		None if word == "~" => ("~", ""),
		None => ("", word),
	};
	let mut out = prefix.to_string();
	for character in rest.chars() {
		if character.is_whitespace() || "'\"\\()&;|$`!*?[]<>{}#".contains(character) {
			out.push('\\');
		}
		out.push(character);
	}
	out
}

fn common_prefix(options: &[String]) -> String {
	let Some(first) = options.first() else { return String::new() };
	let mut end = first.len();
	for option in &options[1..] {
		let shared = first.char_indices().zip(option.chars()).take_while(|((_, left), right)| left == right).last().map(|((index, left), _)| index + left.len_utf8()).unwrap_or(0);
		end = end.min(shared);
	}
	first[..end].to_string()
}

#[tauri::command]
pub async fn session_complete(state: State<'_, Arc<AppState>>, id: String, line: String) -> Result<Completion, String> {
	let session = user_session(&state, &id).await?;
	let cwd = session.cwd();
	let start = word_start(&line);
	let (head, word) = line.split_at(start);
	let before = head.trim_end();
	let command_position = before.is_empty() || before.ends_with(['|', ';', '&', '(']) || before == "sudo" || before.ends_with(" sudo");
	let typed = unescape_word(word);
	let home = paths::home().to_string_lossy().to_string();
	let (query, tilde) = match typed.strip_prefix("~/") {
		Some(rest) => (format!("{home}/{rest}"), true),
		None if typed == "~" => (format!("{home}/"), true),
		None => (typed.clone(), false),
	};
	let mode = if command_position && !query.contains('/') { "-c" } else { "-f" };
	let directory = cwd.to_string_lossy().to_string();
	let captured = util::capture(
		"/bin/bash",
		&["--noprofile", "--norc", "-c", "cd -- \"$1\" 2>/dev/null || cd; compgen $2 -- \"$3\" 2>/dev/null | head -n 500", "ash-complete", &directory, mode, &query],
		Duration::from_secs(5),
	)
	.await;
	let home_prefix = format!("{home}/");
	let mut options: Vec<String> = Vec::new();
	for found in captured.stdout.lines().filter(|found| !found.is_empty()) {
		let mut option = found.to_string();
		if mode == "-f" {
			let resolved = if Path::new(&option).is_absolute() { PathBuf::from(&option) } else { cwd.join(&option) };
			if resolved.is_dir() && !option.ends_with('/') {
				option.push('/');
			}
			if tilde {
				if let Some(rest) = option.strip_prefix(&home_prefix) {
					option = format!("~/{rest}");
				}
			}
		}
		if !options.contains(&option) {
			options.push(option);
		}
	}
	options.sort();
	if options.is_empty() {
		return Ok(Completion { line, options });
	}
	let completed = if options.len() == 1 {
		let only = &options[0];
		if only.ends_with('/') {
			escape_word(only)
		} else {
			format!("{} ", escape_word(only))
		}
	} else {
		let shared = common_prefix(&options);
		if shared.chars().count() > typed.chars().count() {
			escape_word(&shared)
		} else {
			word.to_string()
		}
	};
	let shown = if options.len() > 1 {
		options
			.iter()
			.map(|option| {
				let trimmed = option.trim_end_matches('/');
				let name = trimmed.rsplit('/').next().filter(|name| !name.is_empty()).unwrap_or(trimmed);
				if option.ends_with('/') {
					format!("{name}/")
				} else {
					name.to_string()
				}
			})
			.collect()
	} else {
		Vec::new()
	};
	Ok(Completion { line: format!("{head}{completed}"), options: shown })
}
