use crate::commands::{self, MAIN_WINDOW};
use crate::paths;
use crate::state::AppState;
use crate::store::Action;
use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem, Status, ToolTip, Tray, TrayMethods};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;

pub const BUS_NAME: &str = "one.opencore.AshAgent";
pub const BUS_PATH: &str = "/one/opencore/AshAgent";
pub const BUS_INTERFACE: &str = "one.opencore.AshAgent.App";
const RESHOW_DELAY: Duration = Duration::from_millis(1200);
const ICON_SMALL: &[u8] = include_bytes!("../icons/32x32.png");
const ICON_LARGE: &[u8] = include_bytes!("../icons/128x128.png");
const ICON_WINDOW: &[u8] = include_bytes!("../icons/icon.png");
const ICON_DOUBLE: &[u8] = include_bytes!("../icons/128x128@2x.png");
const THEME_ICONS: [(u32, &[u8]); 4] = [(32, ICON_SMALL), (128, ICON_LARGE), (256, ICON_DOUBLE), (512, ICON_WINDOW)];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
	Show,
	NewChat,
}

struct AppInterface {
	sender: mpsc::UnboundedSender<Request>,
}

#[zbus::interface(name = "one.opencore.AshAgent.App")]
impl AppInterface {
	fn activate(&self, hidden: bool) {
		if !hidden {
			let _ = self.sender.send(Request::Show);
		}
	}

	fn new_chat(&self) {
		let _ = self.sender.send(Request::NewChat);
	}
}

pub enum Instance {
	Primary { connection: Option<zbus::Connection>, lock: Option<std::fs::File>, requests: mpsc::UnboundedReceiver<Request> },
	Secondary,
}

fn lock_file() -> Result<Option<std::fs::File>, String> {
	use std::os::unix::io::AsRawFd;
	let dir = paths::runtime_dir();
	paths::ensure_private_dir(&dir).map_err(|error| format!("Could not prepare {}: {error}", dir.display()))?;
	let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("instance.lock")).map_err(|error| format!("Could not open the instance lock: {error}"))?;
	let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
	Ok(locked.then_some(file))
}

async fn ask_existing(connection: &zbus::Connection, hidden: bool) -> bool {
	let call = connection.call_method(Some(BUS_NAME), BUS_PATH, Some(BUS_INTERFACE), "Activate", &(hidden));
	matches!(tokio::time::timeout(Duration::from_secs(5), call).await, Ok(Ok(_)))
}

pub async fn claim(hidden: bool) -> Result<Instance, String> {
	let (sender, requests) = mpsc::unbounded_channel();
	let Ok(connection) = zbus::Connection::session().await else {
		return match lock_file()? {
			Some(lock) => Ok(Instance::Primary { connection: None, lock: Some(lock), requests }),
			None => Ok(Instance::Secondary),
		};
	};
	if connection.object_server().at(BUS_PATH, AppInterface { sender: sender.clone() }).await.is_err() {
		return match lock_file()? {
			Some(lock) => Ok(Instance::Primary { connection: None, lock: Some(lock), requests }),
			None => Ok(Instance::Secondary),
		};
	}
	let flags = zbus::fdo::RequestNameFlags::DoNotQueue.into();
	match connection.request_name_with_flags(BUS_NAME, flags).await {
		Ok(zbus::fdo::RequestNameReply::PrimaryOwner) | Ok(zbus::fdo::RequestNameReply::AlreadyOwner) => Ok(Instance::Primary { connection: Some(connection), lock: lock_file().ok().flatten(), requests }),
		Ok(_) | Err(zbus::Error::NameTaken) => {
			if ask_existing(&connection, hidden).await {
				Ok(Instance::Secondary)
			} else {
				Err("Another copy of Ash Agent is running but did not respond. Close it and try again.".to_string())
			}
		}
		Err(error) => Err(format!("Could not check for another copy of Ash Agent: {error}")),
	}
}

fn icon_installed(base: &std::path::Path) -> bool {
	let hicolor = base.join("icons/hicolor");
	["scalable/apps/ash-agent.svg", "512x512/apps/ash-agent.png", "256x256/apps/ash-agent.png", "128x128/apps/ash-agent.png", "48x48/apps/ash-agent.png"]
		.iter()
		.any(|relative| hicolor.join(relative).is_file())
}

pub fn ensure_icon() {
	let system = std::env::var("XDG_DATA_DIRS")
		.ok()
		.filter(|dirs| !dirs.trim().is_empty())
		.unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
	if system.split(':').filter(|dir| !dir.is_empty()).any(|dir| icon_installed(std::path::Path::new(dir))) {
		return;
	}
	let Some(data) = dirs::data_dir() else { return };
	let hicolor = data.join("icons/hicolor");
	let mut changed = false;
	for (size, bytes) in THEME_ICONS {
		let target = hicolor.join(format!("{size}x{size}/apps/ash-agent.png"));
		if std::fs::read(&target).map(|existing| existing == bytes).unwrap_or(false) {
			continue;
		}
		let Some(parent) = target.parent() else { continue };
		if std::fs::create_dir_all(parent).is_err() {
			continue;
		}
		let temporary = parent.join(format!(".ash-agent-{}.png", std::process::id()));
		if std::fs::write(&temporary, bytes).is_ok() && std::fs::rename(&temporary, &target).is_ok() {
			changed = true;
		} else {
			let _ = std::fs::remove_file(&temporary);
		}
	}
	if changed {
		let _ = std::fs::File::open(&hicolor).and_then(|directory| directory.set_modified(std::time::SystemTime::now()));
	}
}

pub fn window_icon() -> Option<tauri::image::Image<'static>> {
	let decoded = image::load_from_memory(ICON_WINDOW).ok()?.to_rgba8();
	let (width, height) = decoded.dimensions();
	Some(tauri::image::Image::new_owned(decoded.into_raw(), width, height))
}

fn pixmap(bytes: &[u8]) -> Option<Icon> {
	let decoded = image::load_from_memory(bytes).ok()?.to_rgba8();
	let (width, height) = decoded.dimensions();
	let mut data = Vec::with_capacity((width * height * 4) as usize);
	for pixel in decoded.pixels() {
		let [red, green, blue, alpha] = pixel.0;
		data.extend_from_slice(&[alpha, red, green, blue]);
	}
	Some(Icon { width: width as i32, height: height as i32, data })
}

pub fn visible(app: &AppHandle) -> bool {
	app.get_webview_window(MAIN_WINDOW).map(|window| window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(false)).unwrap_or(false)
}

pub fn focused(app: &AppHandle) -> bool {
	app.get_webview_window(MAIN_WINDOW).map(|window| window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false)).unwrap_or(false)
}

pub fn hide_main(app: &AppHandle) {
	if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
		let _ = window.hide();
	}
}

pub fn toggle_main(app: &AppHandle) {
	if focused(app) {
		hide_main(app);
	} else {
		commands::show_main(app);
	}
}

pub struct Concealment {
	app: AppHandle,
	hidden: AtomicBool,
	generation: AtomicU64,
}

impl Concealment {
	pub fn new(app: AppHandle) -> Arc<Concealment> {
		Arc::new(Concealment { app, hidden: AtomicBool::new(false), generation: AtomicU64::new(0) })
	}

	pub fn conceal(self: &Arc<Self>, hide: bool) -> bool {
		let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
		if hide {
			if self.hidden.load(Ordering::SeqCst) {
				return false;
			}
			if !visible(&self.app) {
				return false;
			}
			self.hidden.store(true, Ordering::SeqCst);
			hide_main(&self.app);
			return true;
		}
		if !self.hidden.load(Ordering::SeqCst) {
			return false;
		}
		let this = Arc::clone(self);
		tauri::async_runtime::spawn(async move {
			tokio::time::sleep(RESHOW_DELAY).await;
			if this.generation.load(Ordering::SeqCst) == generation {
				this.reveal();
			}
		});
		false
	}

	pub fn reveal(&self) {
		self.generation.fetch_add(1, Ordering::SeqCst);
		if self.hidden.swap(false, Ordering::SeqCst) {
			if let Some(window) = self.app.get_webview_window(MAIN_WINDOW) {
				let _ = window.show();
			}
		}
	}

	pub fn active(&self) -> bool {
		self.hidden.load(Ordering::SeqCst)
	}
}

pub struct AshTray {
	pub app: AppHandle,
	pub online: Arc<AtomicBool>,
	pub waiting: usize,
	pub busy: bool,
}

fn stop_everything(app: &AppHandle) {
	let state = Arc::clone(app.state::<Arc<AppState>>().inner());
	tauri::async_runtime::spawn(async move {
		let turns: Vec<String> = state.turns.lock().await.keys().cloned().collect();
		for turn in turns {
			state.cancel_turn(&turn).await;
		}
		let running: Vec<String> = state.running.lock().await.keys().cloned().collect();
		for id in running {
			crate::actions::cancel_running(&state, &id).await;
		}
		let waiting: Vec<String> = state.approvals.lock().await.keys().cloned().collect();
		for id in waiting {
			state.resolve_approval(&id, false).await;
		}
	});
}

impl Tray for AshTray {
	fn id(&self) -> String {
		"ash-agent".to_string()
	}

	fn title(&self) -> String {
		"Ash Agent".to_string()
	}

	fn category(&self) -> ksni::Category {
		ksni::Category::ApplicationStatus
	}

	fn status(&self) -> Status {
		if self.waiting > 0 {
			Status::NeedsAttention
		} else {
			Status::Active
		}
	}

	fn icon_name(&self) -> String {
		"ash-agent".to_string()
	}

	fn icon_pixmap(&self) -> Vec<Icon> {
		[ICON_SMALL, ICON_LARGE].into_iter().filter_map(pixmap).collect()
	}

	fn attention_icon_name(&self) -> String {
		"ash-agent".to_string()
	}

	fn attention_icon_pixmap(&self) -> Vec<Icon> {
		self.icon_pixmap()
	}

	fn tool_tip(&self) -> ToolTip {
		let description = if self.waiting == 1 {
			"Waiting for you to approve an action".to_string()
		} else if self.waiting > 1 {
			format!("Waiting for you to approve {} actions", self.waiting)
		} else if self.busy {
			"Working on your request".to_string()
		} else {
			"Ready when you are".to_string()
		};
		ToolTip { icon_name: "ash-agent".to_string(), icon_pixmap: Vec::new(), title: "Ash Agent".to_string(), description }
	}

	fn activate(&mut self, _x: i32, _y: i32) {
		toggle_main(&self.app);
	}

	fn watcher_online(&self) {
		self.online.store(true, Ordering::SeqCst);
	}

	fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
		self.online.store(false, Ordering::SeqCst);
		true
	}

	fn menu(&self) -> Vec<MenuItem<Self>> {
		let mut items: Vec<MenuItem<Self>> = vec![
			StandardItem {
				label: "Open Ash Agent".to_string(),
				icon_name: "ash-agent".to_string(),
				activate: Box::new(|tray: &mut Self| commands::show_main(&tray.app)),
				..Default::default()
			}
			.into(),
			StandardItem {
				label: "New chat".to_string(),
				icon_name: "document-new".to_string(),
				activate: Box::new(|tray: &mut Self| open_new_chat(&tray.app)),
				..Default::default()
			}
			.into(),
		];
		if self.busy || self.waiting > 0 {
			items.push(MenuItem::Separator);
			items.push(
				StandardItem {
					label: "Stop everything Ash is doing".to_string(),
					icon_name: "process-stop".to_string(),
					activate: Box::new(|tray: &mut Self| stop_everything(&tray.app)),
					..Default::default()
				}
				.into(),
			);
		}
		items.push(MenuItem::Separator);
		items.push(
			StandardItem {
				label: "Quit".to_string(),
				icon_name: "application-exit".to_string(),
				activate: Box::new(|tray: &mut Self| tray.app.exit(0)),
				..Default::default()
			}
			.into(),
		);
		items
	}
}

pub fn open_new_chat(app: &AppHandle) {
	commands::show_main(app);
	let _ = app.emit("app://new-chat", ());
}

pub async fn spawn_tray(app: AppHandle, online: Arc<AtomicBool>) -> Option<ksni::Handle<AshTray>> {
	let tray = AshTray { app, online, waiting: 0, busy: false };
	tray.assume_sni_available(true).spawn().await.ok()
}

#[zbus::proxy(interface = "org.freedesktop.Notifications", default_service = "org.freedesktop.Notifications", default_path = "/org/freedesktop/Notifications")]
trait Notifications {
	#[allow(clippy::too_many_arguments)]
	fn notify(
		&self,
		app_name: &str,
		replaces_id: u32,
		app_icon: &str,
		summary: &str,
		body: &str,
		actions: &[&str],
		hints: HashMap<&str, zbus::zvariant::Value<'_>>,
		expire_timeout: i32,
	) -> zbus::Result<u32>;

	fn close_notification(&self, id: u32) -> zbus::Result<()>;

	#[zbus(signal)]
	fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;
}

#[derive(Debug, Clone)]
enum Target {
	Action(String),
	Conversation(String),
}

pub struct Notifier {
	app: AppHandle,
	proxy: tokio::sync::OnceCell<Option<NotificationsProxy<'static>>>,
	shown: Mutex<HashMap<u32, Target>>,
	approvals: Mutex<HashMap<String, u32>>,
}

fn escape_markup(text: &str) -> String {
	text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

impl Notifier {
	pub fn new(app: AppHandle) -> Arc<Notifier> {
		Arc::new(Notifier { app, proxy: tokio::sync::OnceCell::new(), shown: Mutex::new(HashMap::new()), approvals: Mutex::new(HashMap::new()) })
	}

	async fn proxy(self: &Arc<Self>) -> Option<NotificationsProxy<'static>> {
		self.proxy
			.get_or_init(|| async {
				let connection = zbus::Connection::session().await.ok()?;
				let proxy = NotificationsProxy::new(&connection).await.ok()?;
				let listener = proxy.clone();
				let this = Arc::clone(self);
				tauri::async_runtime::spawn(async move {
					use futures_util::StreamExt;
					let Ok(mut stream) = listener.receive_action_invoked().await else { return };
					while let Some(signal) = stream.next().await {
						if let Ok(args) = signal.args() {
							this.invoked(args.id, &args.action_key);
						}
					}
				});
				Some(proxy)
			})
			.await
			.clone()
	}

	fn invoked(&self, id: u32, _key: &str) {
		let target = self.shown.lock().ok().and_then(|mut shown| shown.remove(&id));
		let Some(target) = target else { return };
		commands::show_main(&self.app);
		let conversation = match target {
			Target::Conversation(conversation) => Some(conversation),
			Target::Action(action) => {
				let state = self.app.state::<Arc<AppState>>();
				state.store.get_action(&action).ok().flatten().and_then(|found| found.conversation_id)
			}
		};
		if let Some(conversation) = conversation {
			let _ = self.app.emit("app://open-conversation", serde_json::json!({ "conversationId": conversation }));
		}
	}

	async fn send(self: &Arc<Self>, summary: &str, body: &str, urgency: u8, target: Target) -> Option<u32> {
		let proxy = self.proxy().await?;
		let mut hints: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
		hints.insert("desktop-entry", zbus::zvariant::Value::from("ash-agent"));
		hints.insert("urgency", zbus::zvariant::Value::U8(urgency));
		let escaped = escape_markup(body);
		let call = proxy.notify("Ash Agent", 0, "ash-agent", summary, &escaped, &["default", "Open"], hints, if urgency >= 2 { 0 } else { -1 });
		let id = tokio::time::timeout(Duration::from_secs(5), call).await.ok()?.ok()?;
		if let Ok(mut shown) = self.shown.lock() {
			if shown.len() > 64 {
				shown.clear();
			}
			shown.insert(id, target);
		}
		Some(id)
	}

	async fn close(self: &Arc<Self>, id: u32) {
		if let Ok(mut shown) = self.shown.lock() {
			shown.remove(&id);
		}
		if let Some(proxy) = self.proxy().await {
			let _ = tokio::time::timeout(Duration::from_secs(5), proxy.close_notification(id)).await;
		}
	}

	pub async fn action_changed(self: &Arc<Self>, action: &Action, enabled: bool) {
		let existing = self.approvals.lock().ok().and_then(|mut approvals| if action.status == "awaiting" { None } else { approvals.remove(&action.id) });
		if let Some(id) = existing {
			self.close(id).await;
			return;
		}
		if action.status != "awaiting" || !enabled || focused(&self.app) {
			return;
		}
		if self.approvals.lock().map(|approvals| approvals.contains_key(&action.id)).unwrap_or(true) {
			return;
		}
		let summary = if action.root { "Ash needs your OK for an administrator action" } else { "Ash needs your OK" };
		if let Some(id) = self.send(summary, &action.title, if action.root { 2 } else { 1 }, Target::Action(action.id.clone())).await {
			let late = match self.approvals.lock() {
				Ok(mut approvals) => {
					approvals.insert(action.id.clone(), id);
					false
				}
				Err(_) => true,
			};
			if late {
				self.close(id).await;
			}
		}
	}

	pub async fn reply_finished(self: &Arc<Self>, conversation_id: &str, failed: Option<&str>, enabled: bool) {
		if !enabled || focused(&self.app) {
			return;
		}
		let state = Arc::clone(self.app.state::<Arc<AppState>>().inner());
		let title = state.store.get_conversation(conversation_id).map(|conversation| conversation.title).unwrap_or_else(|_| "Your chat".to_string());
		let (summary, body) = match failed {
			Some(message) => ("Ash could not finish its answer".to_string(), format!("{title}: {message}")),
			None => ("Ash has answered".to_string(), title),
		};
		self.send(&summary, &crate::util::truncate_end(&body, 300), 1, Target::Conversation(conversation_id.to_string())).await;
	}
}
