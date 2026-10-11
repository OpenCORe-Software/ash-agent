use crate::paths;
use crate::secrets::Secrets;
use crate::settings::Settings;
use crate::store::Store;
use crate::theme::ThemeState;
use crate::tools::pty::Session;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex, RwLock};
use tokio_util::sync::CancellationToken;

pub struct Turn {
	pub cancel: CancellationToken,
	pub conversation_id: String,
	pub message_id: String,
}

pub struct AppState {
	pub settings: RwLock<Settings>,
	pub secrets: Arc<Secrets>,
	pub store: Arc<Store>,
	pub theme: RwLock<ThemeState>,
	pub theme_events: tokio::sync::broadcast::Sender<ThemeState>,
	pub turns: Mutex<HashMap<String, Turn>>,
	pub approvals: Mutex<HashMap<String, oneshot::Sender<bool>>>,
	pub sessions: Mutex<HashMap<String, Arc<Session>>>,
	pub running: Mutex<HashMap<String, CancellationToken>>,
	pub remembered: Mutex<HashSet<(String, String)>>,
	pub started: std::time::Instant,
}

impl AppState {
	pub fn new() -> Result<AppState, String> {
		let settings = Settings::load();
		let store = Store::open(&paths::data_dir().join("history.db"))?;
		let (theme_events, _) = tokio::sync::broadcast::channel(16);
		let resolved = crate::theme::resolve(&settings.theme);
		Ok(AppState {
			settings: RwLock::new(settings),
			secrets: Arc::new(Secrets::new()),
			store: Arc::new(store),
			theme: RwLock::new(resolved),
			theme_events,
			turns: Mutex::new(HashMap::new()),
			approvals: Mutex::new(HashMap::new()),
			sessions: Mutex::new(HashMap::new()),
			running: Mutex::new(HashMap::new()),
			remembered: Mutex::new(HashSet::new()),
			started: std::time::Instant::now(),
		})
	}

	pub async fn settings_snapshot(&self) -> Settings {
		self.settings.read().await.clone()
	}

	pub async fn store_settings(&self, settings: Settings) {
		let mut guard = self.settings.write().await;
		*guard = settings;
	}

	pub async fn register_approval(&self, id: &str) -> oneshot::Receiver<bool> {
		let (sender, receiver) = oneshot::channel();
		self.approvals.lock().await.insert(id.to_string(), sender);
		receiver
	}

	pub async fn resolve_approval(&self, id: &str, allow: bool) -> bool {
		match self.approvals.lock().await.remove(id) {
			Some(sender) => sender.send(allow).is_ok(),
			None => false,
		}
	}

	pub async fn cancel_approval(&self, id: &str) {
		self.approvals.lock().await.remove(id);
	}

	pub async fn register_turn(&self, id: &str, turn: Turn) {
		self.turns.lock().await.insert(id.to_string(), turn);
	}

	pub async fn finish_turn(&self, id: &str) {
		if let Some(turn) = self.turns.lock().await.remove(id) {
			turn.cancel.cancel();
		}
	}

	pub async fn cancel_turn(&self, id: &str) -> bool {
		match self.turns.lock().await.get(id) {
			Some(turn) => {
				turn.cancel.cancel();
				true
			}
			None => false,
		}
	}

	pub async fn track_action(&self, id: &str, token: CancellationToken) {
		self.running.lock().await.insert(id.to_string(), token);
	}

	pub async fn untrack_action(&self, id: &str) {
		self.running.lock().await.remove(id);
	}

	pub async fn cancel_action(&self, id: &str) -> bool {
		match self.running.lock().await.get(id) {
			Some(token) => {
				token.cancel();
				true
			}
			None => false,
		}
	}

	pub async fn remember(&self, conversation_id: Option<&str>, tool: &str) {
		self.remembered.lock().await.insert((conversation_id.unwrap_or("").to_string(), tool.to_string()));
	}

	pub async fn remembered(&self, conversation_id: Option<&str>, tool: &str, risk: crate::tools::Risk) -> bool {
		if matches!(risk, crate::tools::Risk::High | crate::tools::Risk::Danger) {
			return false;
		}
		self.remembered.lock().await.contains(&(conversation_id.unwrap_or("").to_string(), tool.to_string()))
	}

	pub async fn forget_conversation(&self, conversation_id: &str) {
		self.remembered.lock().await.retain(|(conversation, _)| conversation != conversation_id);
	}

	pub async fn conversation_of_turn(&self, id: &str) -> Option<String> {
		self.turns.lock().await.get(id).map(|turn| turn.conversation_id.clone())
	}
}
