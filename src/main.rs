#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use ash_agent::actions::{ActionEvent, Approver};
use ash_agent::chat::{Chat, ChatEvent};
use ash_agent::commands::{self, Launch, MAIN_WINDOW};
use ash_agent::desktop::{self, Concealment, Instance, Notifier, Request};
use ash_agent::state::AppState;
use ash_agent::tools::screen;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tokio::sync::broadcast::error::RecvError;

struct InstanceGuard {
	_connection: Option<zbus::Connection>,
	_lock: Option<std::fs::File>,
}

type TrayHandle = Arc<tokio::sync::Mutex<Option<ksni::Handle<desktop::AshTray>>>>;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct LogPayload<'a> {
	action_id: &'a str,
	data: &'a str,
	stream: &'a str,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ImagePayload<'a> {
	action_id: &'a str,
	image_id: &'a str,
}

fn fail(message: &str) -> ! {
	eprintln!("ash-agent: {message}");
	std::process::exit(1);
}

fn remember_window(app: &AppHandle) {
	let Some(window) = app.get_webview_window(MAIN_WINDOW) else { return };
	if !window.is_visible().unwrap_or(false) || window.is_minimized().unwrap_or(false) {
		return;
	}
	let maximized = window.is_maximized().unwrap_or(false);
	let scale = window.scale_factor().unwrap_or(1.0).max(0.5);
	let size = window.inner_size().ok().map(|size| size.to_logical::<f64>(scale));
	let state = Arc::clone(app.state::<Arc<AppState>>().inner());
	tauri::async_runtime::block_on(async move {
		let current = state.settings_snapshot().await.window;
		let (width, height) = match size {
			Some(size) if !maximized => (size.width, size.height),
			_ => (current.width, current.height),
		};
		if (current.width - width).abs() < 1.0 && (current.height - height).abs() < 1.0 && current.maximized == maximized {
			return;
		}
		let _ = commands::update_settings(&state, |settings| {
			settings.window.width = width;
			settings.window.height = height;
			settings.window.maximized = maximized;
		})
		.await;
	});
}

async fn refresh_tray(state: &AppState, tray: &TrayHandle) {
	let waiting = state.approvals.lock().await.len();
	let busy = !state.turns.lock().await.is_empty() || !state.running.lock().await.is_empty();
	if let Some(handle) = tray.lock().await.as_ref() {
		handle
			.update(|item| {
				item.waiting = waiting;
				item.busy = busy;
			})
			.await;
	}
}

fn forward_chat(app: AppHandle, state: Arc<AppState>, chat: Arc<Chat>, notifier: Arc<Notifier>, tray: TrayHandle) {
	let mut feed = chat.events.subscribe();
	tauri::async_runtime::spawn(async move {
		let mut failures: HashMap<String, String> = HashMap::new();
		loop {
			let event = match feed.recv().await {
				Ok(event) => event,
				Err(RecvError::Lagged(_)) => continue,
				Err(RecvError::Closed) => break,
			};
			let _ = app.emit(event.channel(), &event);
			match &event {
				ChatEvent::Started { .. } => refresh_tray(&state, &tray).await,
				ChatEvent::Error { turn_id, message, retry_after: None } => {
					failures.insert(turn_id.clone(), message.clone());
				}
				ChatEvent::Done { turn_id, conversation_id, stopped, .. } => {
					let failure = failures.remove(turn_id);
					refresh_tray(&state, &tray).await;
					if !stopped {
						let enabled = state.settings_snapshot().await.notifications;
						notifier.reply_finished(conversation_id, failure.as_deref(), enabled).await;
					}
				}
				_ => {}
			}
		}
	});
}

fn forward_actions(app: AppHandle, state: Arc<AppState>, approver: Arc<Approver>, notifier: Arc<Notifier>, tray: TrayHandle) {
	let mut feed = approver.events.subscribe();
	tauri::async_runtime::spawn(async move {
		loop {
			let event = match feed.recv().await {
				Ok(event) => event,
				Err(RecvError::Lagged(_)) => continue,
				Err(RecvError::Closed) => break,
			};
			match &event {
				ActionEvent::Changed { action } => {
					let _ = app.emit("tool://action", action);
					refresh_tray(&state, &tray).await;
					let enabled = state.settings_snapshot().await.notifications;
					notifier.action_changed(action, enabled).await;
				}
				ActionEvent::Log { action_id, data, stream } => {
					let _ = app.emit("tool://log", LogPayload { action_id, data, stream });
				}
				ActionEvent::Image { action_id, image_id } => {
					let _ = app.emit("tool://image", ImagePayload { action_id, image_id });
				}
			}
		}
	});
}

fn forward_theme(app: AppHandle, state: Arc<AppState>) {
	let mut feed = state.theme_events.subscribe();
	tauri::async_runtime::spawn(async move {
		loop {
			let theme = match feed.recv().await {
				Ok(theme) => theme,
				Err(RecvError::Lagged(_)) => continue,
				Err(RecvError::Closed) => break,
			};
			if state.settings_snapshot().await.theme != "system" {
				continue;
			}
			*state.theme.write().await = theme.clone();
			let _ = app.emit("theme://change", &theme);
		}
	});
}

fn handle_requests(app: AppHandle, mut requests: tokio::sync::mpsc::UnboundedReceiver<Request>) {
	tauri::async_runtime::spawn(async move {
		while let Some(request) = requests.recv().await {
			let launch = app.state::<Launch>();
			launch.hidden.store(false, Ordering::SeqCst);
			match request {
				Request::Show => commands::show_main(&app),
				Request::NewChat => desktop::open_new_chat(&app),
			}
		}
	});
}

fn watch_signals(app: AppHandle) {
	use tokio::signal::unix::{signal, SignalKind};
	tauri::async_runtime::spawn(async move {
		let (Ok(mut terminate), Ok(mut interrupt), Ok(mut hangup)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt()), signal(SignalKind::hangup())) else { return };
		tokio::select! {
			_ = terminate.recv() => {}
			_ = interrupt.recv() => {}
			_ = hangup.recv() => {}
		}
		app.exit(0);
	});
}

fn shutdown(app: &AppHandle) {
	let state = Arc::clone(app.state::<Arc<AppState>>().inner());
	tauri::async_runtime::block_on(async move {
		let turns: Vec<String> = state.turns.lock().await.keys().cloned().collect();
		for turn in turns {
			state.cancel_turn(&turn).await;
		}
		let running: Vec<String> = state.running.lock().await.keys().cloned().collect();
		for id in running {
			ash_agent::actions::cancel_running(&state, &id).await;
		}
		let waiting: Vec<String> = state.approvals.lock().await.keys().cloned().collect();
		for id in waiting {
			state.resolve_approval(&id, false).await;
		}
		let sessions: Vec<_> = state.sessions.lock().await.drain().map(|(_, session)| session).collect();
		for session in sessions {
			session.stop();
		}
		let _ = state.store.interrupt_unfinished_actions();
	});
}

fn build_window(app: &tauri::App, settings: &ash_agent::settings::Settings) -> Result<(), String> {
	let mut builder = WebviewWindowBuilder::new(app, MAIN_WINDOW, WebviewUrl::App("index.html".into()))
		.title("Ash Agent")
		.inner_size(settings.window.width, settings.window.height)
		.min_inner_size(720.0, 520.0)
		.visible(false)
		.maximized(settings.window.maximized)
		.center();
	if let Some(icon) = desktop::window_icon() {
		builder = builder.icon(icon).map_err(|error| format!("Could not set the window icon: {error}"))?;
	}
	builder.build().map_err(|error| format!("Could not open the window: {error}"))?;
	Ok(())
}

const USAGE: &str = "Usage: ash-agent [--hidden]\n\n  --hidden                  start in the system tray without opening the window\n  --version                 print the version\n  --print-polkit-policy     print the polkit policy for the administrator helper\n  --print-desktop-entry     print the application launcher entry\n  --print-autostart-entry   print the autostart entry\n";

fn installed_exe() -> String {
	std::env::current_exe()
		.ok()
		.and_then(|path| path.canonicalize().ok())
		.map(|path| path.to_string_lossy().to_string())
		.unwrap_or_else(|| "/usr/bin/ash-agent".to_string())
}

fn main() {
	let arguments: Vec<String> = std::env::args().skip(1).collect();
	let mut hidden = false;
	for argument in &arguments {
		match argument.as_str() {
			"--hidden" => hidden = true,
			"--version" => {
				println!("ash-agent {}", env!("CARGO_PKG_VERSION"));
				return;
			}
			"--help" | "-h" => {
				print!("{USAGE}");
				return;
			}
			"--print-polkit-policy" => {
				print!("{}", ash_agent::tools::root::policy_xml());
				return;
			}
			"--print-desktop-entry" => {
				print!("{}", screen::desktop_entry(&installed_exe()));
				return;
			}
			"--print-autostart-entry" => {
				print!("{}", ash_agent::autostart::system_entry_text());
				return;
			}
			other => {
				eprint!("ash-agent: unknown option {other}\n\n{USAGE}");
				std::process::exit(2);
			}
		}
	}
	let instance = tauri::async_runtime::block_on(desktop::claim(hidden)).unwrap_or_else(|error| fail(&error));
	let (connection, lock, requests) = match instance {
		Instance::Secondary => return,
		Instance::Primary { connection, lock, requests } => (connection, lock, requests),
	};
	let state = Arc::new(AppState::new().unwrap_or_else(|error| fail(&error)));
	if let Err(error) = state.store.interrupt_unfinished_actions() {
		eprintln!("ash-agent: {error}");
	}
	let settings = tauri::async_runtime::block_on(state.settings_snapshot());
	let chat = Arc::new(Chat::new());
	let approver = Arc::new(Approver::new());
	let tray_online = Arc::new(AtomicBool::new(false));
	let tray: TrayHandle = Arc::new(tokio::sync::Mutex::new(None));
	let app = tauri::Builder::default()
		.plugin(tauri_plugin_dialog::init())
		.manage(Arc::clone(&state))
		.manage(Arc::clone(&chat))
		.manage(Arc::clone(&approver))
		.manage(Launch::new(hidden, Arc::clone(&tray_online)))
		.manage(InstanceGuard { _connection: connection, _lock: lock })
		.invoke_handler(tauri::generate_handler![
			commands::ui_ready,
			commands::settings_get,
			commands::settings_set,
			commands::secret_status,
			commands::secret_set,
			commands::secret_delete,
			commands::legacy_import,
			commands::legacy_apply,
			commands::legacy_dismiss,
			commands::theme_get,
			commands::theme_watch,
			commands::markdown_render,
			commands::models_available,
			commands::model_health,
			commands::model_favorites,
			commands::ml5_status,
			commands::ml5_start,
			commands::ml5_stop,
			commands::ml5_load,
			commands::ml5_unload,
			commands::ml5_rename,
			commands::ml5_rescan,
			commands::ml5_delete,
			commands::ml5_pull,
			commands::ml5_pull_cancel,
			commands::conversations_list,
			commands::conversations_create,
			commands::conversations_get,
			commands::conversations_update,
			commands::conversations_delete,
			commands::conversations_delete_all,
			commands::conversations_export,
			commands::messages_list,
			commands::message_delete,
			commands::message_select,
			commands::message_revise,
			commands::message_update,
			commands::chat_send,
			commands::chat_stop,
			commands::chat_retry,
			commands::chat_regenerate,
			commands::chat_edit,
			commands::chat_title,
			commands::tool_catalog,
			commands::tool_approve,
			commands::tool_cancel,
			commands::tools_set_tier,
			commands::computer_status,
			commands::actions_list,
			commands::actions_for_conversation,
			commands::actions_clear,
			commands::action_image,
			commands::guided_fixes,
			commands::guided_fix_run,
			commands::autostart_status,
			commands::autostart_set,
			commands::clipboard_read,
			commands::clipboard_write,
			commands::file_save,
			commands::session_start,
			commands::session_write,
			commands::session_resize,
			commands::session_stop,
			commands::session_complete,
			commands::sessions_list,
		])
		.setup({
			let state = Arc::clone(&state);
			let chat = Arc::clone(&chat);
			let approver = Arc::clone(&approver);
			let tray = Arc::clone(&tray);
			let tray_online = Arc::clone(&tray_online);
			move |app| {
				build_window(app, &settings)?;
				let handle = app.handle().clone();
				let concealment = Concealment::new(handle.clone());
				approver.set_concealer(Arc::new(move |hide| concealment.conceal(hide)));
				let notifier = Notifier::new(handle.clone());
				forward_chat(handle.clone(), Arc::clone(&state), Arc::clone(&chat), Arc::clone(&notifier), Arc::clone(&tray));
				forward_actions(handle.clone(), Arc::clone(&state), Arc::clone(&approver), notifier, Arc::clone(&tray));
				forward_theme(handle.clone(), Arc::clone(&state));
				handle_requests(handle.clone(), requests);
				let tray_app = handle.clone();
				tauri::async_runtime::spawn(async move {
					let spawned = desktop::spawn_tray(tray_app, tray_online).await;
					*tray.lock().await = spawned;
				});
				tauri::async_runtime::spawn(async {
					let _ = tokio::task::spawn_blocking(desktop::ensure_icon).await;
					screen::ensure_authorized().await;
				});
				watch_signals(handle.clone());
				Ok(())
			}
		})
		.build(tauri::generate_context!())
		.unwrap_or_else(|error| fail(&format!("Ash Agent could not start: {error}")));
	app.run(move |app, event| match event {
		RunEvent::WindowEvent { label, event: WindowEvent::CloseRequested { api, .. }, .. } if label == MAIN_WINDOW => {
			remember_window(app);
			let keep = state.settings.try_read().map(|settings| settings.close_to_tray).unwrap_or(true);
			if keep && tray_online.load(Ordering::SeqCst) {
				api.prevent_close();
				desktop::hide_main(app);
			}
		}
		RunEvent::ExitRequested { code: None, api, .. } if tray_online.load(Ordering::SeqCst) && state.settings.try_read().map(|settings| settings.close_to_tray).unwrap_or(true) && app.get_webview_window(MAIN_WINDOW).is_some() => {
			api.prevent_exit();
		}
		RunEvent::Exit => {
			remember_window(app);
			shutdown(app);
		}
		_ => {}
	});
}
