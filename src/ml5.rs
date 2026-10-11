use crate::paths;
use crate::providers::{self, ML5_BASE};
use crate::util;
use futures_util::StreamExt;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex as StdMutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, Mutex};

const UNIT: &str = "ml5d.service";
const CATALOG: &str = include_str!("../data/ml5-models.json");
const STATUS_TTL: Duration = Duration::from_secs(2);
const STATUS_TIMEOUT: Duration = Duration::from_secs(4);
const PING_TIMEOUT: Duration = Duration::from_secs(2);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
const QUICK: Duration = Duration::from_secs(15);
const RENAME_TIMEOUT: Duration = Duration::from_secs(30);
const RESCAN_TIMEOUT: Duration = Duration::from_secs(300);
const LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const LOAD_TIMEOUT: Duration = Duration::from_secs(35 * 60);
const PULL_START_TIMEOUT: Duration = Duration::from_secs(60);
const PULL_STALL: Duration = Duration::from_secs(120);
const REMOTE_TIMEOUT: Duration = Duration::from_secs(10);
const REMOTE_LIST_TIMEOUT: Duration = Duration::from_secs(20);
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_TIMEOUT: Duration = Duration::from_secs(120);
const START_WAIT: Duration = Duration::from_secs(60);
const STOP_WAIT: Duration = Duration::from_secs(20);
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const LOG_LIMIT: u64 = 8 * 1024 * 1024;
const NOT_RUNNING: &str = "The local engine (ML5) is not running. Start it from the Models panel and try again.";

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DaemonState {
	pub running: bool,
	pub version: Option<String>,
	pub models_dir: Option<String>,
	pub log_dir: Option<String>,
	pub uptime_secs: Option<u64>,
	pub max_loaded_models: Option<u64>,
	pub keep_alive_secs: Option<i64>,
	pub n_parallel: Option<u64>,
	pub auto_ctx_max: Option<u32>,
	pub runtime: Option<String>,
	pub runtime_release: Option<String>,
	pub model_count: Option<u64>,
	pub loading: Vec<String>,
	pub loaded: Vec<LoadedModel>,
	pub pulls: Vec<PullProgress>,
	pub memory: Option<Memory>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LoadedModel {
	pub name: String,
	pub display_name: String,
	pub backend: String,
	pub context: u64,
	pub parallel: u64,
	pub gpu_layers: String,
	pub vision: bool,
	pub embeddings: bool,
	pub active_requests: u64,
	pub idle_secs: u64,
	pub expires_in_secs: Option<i64>,
	pub memory_bytes: u64,
	pub loaded_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PullProgress {
	pub id: String,
	pub target: String,
	pub file: Option<String>,
	pub stage: String,
	pub phase: String,
	pub downloaded: u64,
	pub total: Option<u64>,
	pub started_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
	pub total_bytes: u64,
	pub available_bytes: u64,
	pub gpu_total_bytes: u64,
	pub gpu_free_bytes: u64,
	pub committed_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Model {
	pub id: String,
	pub name: String,
	pub capabilities: Vec<String>,
	pub vision: bool,
	pub tools: bool,
	pub thinking: bool,
	pub embedding: bool,
	pub size_bytes: u64,
	pub format: String,
	pub loaded: bool,
	pub loaded_context: Option<u64>,
	pub context_length: Option<u64>,
	pub parameter_size: Option<String>,
	pub quantization: Option<String>,
	pub architecture: Option<String>,
	pub modified_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PullEvent {
	Started { id: String },
	Stage { id: String, text: String },
	Progress { id: String, downloaded: u64, total: Option<u64>, file: Option<String> },
	Done { id: String, model: String },
	Failed { id: String, message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
	pub reference: String,
	pub name: String,
	pub description: String,
	#[serde(default)]
	pub size: String,
	#[serde(default)]
	pub tags: Vec<String>,
	#[serde(default)]
	pub registry: String,
	#[serde(default)]
	pub featured: bool,
	#[serde(default)]
	pub advanced: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Suggestions {
	pub featured: Vec<Suggestion>,
	pub advanced: Vec<Suggestion>,
}

impl Suggestions {
	pub fn all(&self) -> Vec<Suggestion> {
		self.featured.iter().chain(self.advanced.iter()).cloned().collect()
	}
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendHealth {
	pub online: bool,
	pub latency_ms: Option<u64>,
	pub message: String,
}

struct Cached {
	at: Instant,
	generation: u64,
	state: DaemonState,
}

#[derive(Clone)]
struct Spawned {
	signal: mpsc::UnboundedSender<libc::c_int>,
	exited: watch::Receiver<bool>,
}

enum Failure {
	Down,
	Busy,
	Broken,
	Unexpected,
	Http(u16, String),
}

#[derive(PartialEq, Eq)]
enum Unit {
	Present,
	Masked,
	Missing,
}

static CACHE: StdMutex<Option<Cached>> = StdMutex::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static FETCH: Mutex<()> = Mutex::const_new(());
static DAEMON_OP: Mutex<()> = Mutex::const_new(());
static SPAWNED: StdMutex<Option<Spawned>> = StdMutex::new(None);

fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
	mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn client() -> &'static reqwest::Client {
	providers::client(true)
}

fn text(value: &Value, key: &str) -> Option<String> {
	value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn number(value: &Value, key: &str) -> Option<u64> {
	value.get(key).and_then(Value::as_u64)
}

fn flag(value: &Value, key: &str) -> bool {
	value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn strings(value: &Value, key: &str) -> Vec<String> {
	value
		.get(key)
		.and_then(Value::as_array)
		.map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_string)).collect())
		.unwrap_or_default()
}

fn sentence(text: &str) -> String {
	let trimmed = text.trim();
	let mut chars = trimmed.chars();
	let Some(first) = chars.next() else { return String::new() };
	let mut out: String = first.to_uppercase().chain(chars).collect();
	if !out.ends_with(['.', '!', '?', '…']) {
		out.push('.');
	}
	out
}

fn plain(detail: &str) -> String {
	let kept: Vec<&str> = detail.split_inclusive(". ").map(str::trim).filter(|part| !part.is_empty() && !part.contains('`')).collect();
	sentence(&util::truncate_end(kept.join(" ").trim(), 300))
}

fn strip_kind(detail: &str) -> (Option<&'static str>, &str) {
	const KINDS: [&str; 8] = ["model not found", "model already exists", "backend error", "no backend available for capability", "download failed", "invalid request", "io error", "json error"];
	let trimmed = detail.trim();
	for kind in KINDS {
		if let Some(rest) = trimmed.strip_prefix(kind).and_then(|rest| rest.strip_prefix(':')) {
			return (Some(kind), rest.trim());
		}
	}
	(None, trimmed)
}

fn friendly(detail: &str) -> String {
	let lower = detail.to_ascii_lowercase();
	if lower.contains("download cancelled") {
		return "The download was cancelled.".to_string();
	}
	if lower.contains("needs hugging face access") {
		return "Hugging Face did not allow that download. Check that the model name is spelled correctly. If the model is private or needs its license accepted, accept it on huggingface.co, add a Hugging Face access token, and try again.".to_string();
	}
	if lower.contains("is not a known model name") {
		return "There is no model with that name in the catalogue. Try a Hugging Face name such as owner/model instead.".to_string();
	}
	if lower.contains("was not found on hugging face") {
		return "That model could not be found on Hugging Face. Check the name and try again.".to_string();
	}
	if lower.contains("contains no gguf model files") {
		return "That Hugging Face repository has no model files the local engine can use.".to_string();
	}
	if lower.contains("no gguf file matching quantization") {
		return "That model is not available in the requested quality level.".to_string();
	}
	if lower.contains("no prebuilt runtime") {
		return "The local engine has no ready-made runtime for this computer, so it cannot run models yet.".to_string();
	}
	if lower.contains("runtime download") || lower.contains("runtime stream error") {
		return "The local engine could not download the runtime it needs. Check the internet connection and try again.".to_string();
	}
	if lower.contains("sha-256") || lower.contains("incomplete") {
		return "The download was damaged on the way. Try again.".to_string();
	}
	if lower.contains("model slots are busy") {
		return "All model slots are busy right now. Wait until a reply finishes, then try again.".to_string();
	}
	if lower.contains("is busy") || lower.contains("serving other requests") {
		return "That model is busy answering a request. Wait until it finishes, then try again.".to_string();
	}
	if lower.contains("not enough memory") || lower.contains("out of memory") {
		return "There is not enough free memory to load that model. Close other programs or pick a smaller model.".to_string();
	}
	if lower.contains("no space left") {
		return "The disk is full. Free up some space and try again.".to_string();
	}
	if lower.contains("pull ended unexpectedly") {
		return "The download stopped unexpectedly. Try again.".to_string();
	}
	let (kind, rest) = strip_kind(detail);
	let cleaned = plain(rest);
	match kind {
		Some("model not found") => "That model is not installed on this computer.".to_string(),
		Some("model already exists") => "A model with that name already exists.".to_string(),
		Some("download failed") if cleaned.is_empty() => "The download failed. Try again.".to_string(),
		Some("download failed") => format!("The download failed: {cleaned}"),
		Some("backend error") | Some("no backend available for capability") if cleaned.is_empty() => "The local engine could not run that model.".to_string(),
		Some("backend error") | Some("no backend available for capability") => format!("The local engine could not run that model: {cleaned}"),
		Some("io error") => "The local engine could not read or write its files. Check that the disk is not full and try again.".to_string(),
		_ if cleaned.is_empty() => "The local engine ran into a problem. Try again.".to_string(),
		_ => cleaned,
	}
}

fn error_detail(body: &str) -> Option<String> {
	let value: Value = serde_json::from_str(body.trim()).ok()?;
	let error = value.get("error")?;
	error
		.as_str()
		.or_else(|| error.get("message").and_then(Value::as_str))
		.map(str::trim)
		.filter(|detail| !detail.is_empty())
		.map(str::to_string)
}

fn engine_error(status: u16, body: &str) -> String {
	match (status, error_detail(body)) {
		(404, None) => "This version of the local engine does not support that action. Update ML5 and try again.".to_string(),
		(413, _) => "The request was too large for the local engine.".to_string(),
		(415 | 422, _) => "The local engine did not understand the request. Update ML5 and try again.".to_string(),
		(_, Some(detail)) => friendly(&detail),
		(500..=599, None) => "The local engine ran into a problem. Try again, or restart it from the Models panel.".to_string(),
		_ => format!("The local engine refused the request (error {status})."),
	}
}

fn failure_text(failure: Failure) -> String {
	match failure {
		Failure::Down => NOT_RUNNING.to_string(),
		Failure::Busy => "The local engine took too long to answer. It may be busy loading a model, so try again in a moment.".to_string(),
		Failure::Broken => "The connection to the local engine was interrupted. Try again.".to_string(),
		Failure::Unexpected => "The local engine sent a reply this app does not understand. Update ML5 and try again.".to_string(),
		Failure::Http(status, body) => engine_error(status, &body),
	}
}

fn transport(error: &reqwest::Error) -> Failure {
	if error.is_connect() {
		Failure::Down
	} else if error.is_timeout() {
		Failure::Busy
	} else {
		Failure::Broken
	}
}

async fn call(method: Method, path: &str, body: Option<&Value>, timeout: Duration) -> Result<Value, Failure> {
	let mut request = client().request(method, format!("{ML5_BASE}{path}")).timeout(timeout);
	if let Some(body) = body {
		request = request.json(body);
	}
	let response = request.send().await.map_err(|error| transport(&error))?;
	let status = response.status();
	let body = response.text().await.map_err(|error| transport(&error))?;
	if !status.is_success() {
		return Err(Failure::Http(status.as_u16(), body));
	}
	if body.trim().is_empty() {
		return Ok(Value::Null);
	}
	serde_json::from_str(&body).map_err(|_| Failure::Unexpected)
}

async fn action(method: Method, path: &str, body: Option<&Value>, timeout: Duration) -> Result<Value, String> {
	let result = call(method, path, body, timeout).await.map_err(failure_text);
	invalidate();
	result
}

fn require_model(model: &str) -> Result<&str, String> {
	let model = model.trim();
	if model.is_empty() {
		Err("Choose a model first.".to_string())
	} else {
		Ok(model)
	}
}

pub fn suggestions() -> Suggestions {
	static PARSED: OnceLock<Suggestions> = OnceLock::new();
	PARSED
		.get_or_init(|| {
			let mut parsed: Suggestions = serde_json::from_str(CATALOG).unwrap_or_default();
			let tag = |items: &mut Vec<Suggestion>, featured: bool| {
				items.retain(|item| !item.reference.trim().is_empty());
				for item in items.iter_mut() {
					item.featured = featured;
					item.advanced = !featured;
					if item.registry.trim().is_empty() {
						item.registry = guess_registry(&item.reference).0.to_string();
					}
				}
			};
			tag(&mut parsed.featured, true);
			tag(&mut parsed.advanced, false);
			parsed
		})
		.clone()
}

fn stage_label(phase: &str) -> String {
	match phase {
		"starting" => "Getting ready…",
		"resolving" => "Looking up the model…",
		"downloading" => "Downloading…",
		"verifying" => "Checking the download…",
		"done" => "Finishing up…",
		"error" => "The download failed.",
		_ => "Working…",
	}
	.to_string()
}

fn parse_loaded(item: &Value) -> LoadedModel {
	let name = text(item, "name").unwrap_or_default();
	LoadedModel {
		display_name: text(item, "friendly_name").filter(|friendly| !friendly.trim().is_empty()).unwrap_or_else(|| name.clone()),
		name,
		backend: text(item, "backend").unwrap_or_default(),
		context: number(item, "n_ctx").unwrap_or(0),
		parallel: number(item, "n_parallel").unwrap_or(0),
		gpu_layers: text(item, "gpu_layers").unwrap_or_default(),
		vision: flag(item, "vision"),
		embeddings: flag(item, "embeddings"),
		active_requests: number(item, "active_requests").unwrap_or(0),
		idle_secs: number(item, "idle_secs").unwrap_or(0),
		expires_in_secs: item.get("expires_in_secs").and_then(Value::as_i64),
		memory_bytes: number(item, "memory_bytes").unwrap_or(0),
		loaded_at: text(item, "loaded_at"),
	}
}

fn parse_pull(item: &Value) -> PullProgress {
	let phase = text(item, "stage").unwrap_or_default();
	PullProgress {
		id: text(item, "id").unwrap_or_default(),
		target: text(item, "target").unwrap_or_default(),
		file: text(item, "file"),
		stage: stage_label(&phase),
		phase,
		downloaded: number(item, "downloaded").unwrap_or(0),
		total: number(item, "total"),
		started_at: text(item, "started_at"),
	}
}

fn parse_state(value: &Value) -> DaemonState {
	let list = |key: &str| value.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
	let memory = value.get("memory").filter(|memory| memory.is_object()).map(|memory| Memory {
		total_bytes: number(memory, "total_bytes").unwrap_or(0),
		available_bytes: number(memory, "available_bytes").unwrap_or(0),
		gpu_total_bytes: number(memory, "gpu_total_bytes").unwrap_or(0),
		gpu_free_bytes: number(memory, "gpu_free_bytes").unwrap_or(0),
		committed_bytes: number(memory, "committed_bytes").unwrap_or(0),
	});
	let runtime = value.get("runtime").filter(|runtime| runtime.is_object());
	DaemonState {
		running: true,
		version: text(value, "version"),
		models_dir: text(value, "models_dir"),
		log_dir: text(value, "log_dir"),
		uptime_secs: number(value, "uptime_secs"),
		max_loaded_models: number(value, "max_loaded_models"),
		keep_alive_secs: value.get("keep_alive_secs").and_then(Value::as_i64),
		n_parallel: number(value, "n_parallel"),
		auto_ctx_max: number(value, "auto_ctx_max").and_then(|max| u32::try_from(max).ok()),
		runtime: runtime.and_then(|runtime| text(runtime, "server")),
		runtime_release: runtime.and_then(|runtime| text(runtime, "release_tag")),
		model_count: number(value, "model_count"),
		loading: strings(value, "loading"),
		loaded: list("loaded").iter().map(parse_loaded).collect(),
		pulls: list("pulls").iter().map(parse_pull).filter(|pull| !pull.id.is_empty()).collect(),
		memory,
	}
}

fn fresh(since: Option<Instant>) -> Option<DaemonState> {
	let cache = lock(&CACHE);
	let cached = cache.as_ref()?;
	let current = cached.generation == GENERATION.load(Ordering::Acquire) && cached.at.elapsed() < STATUS_TTL;
	let recent = since.is_none_or(|since| cached.at >= since);
	(current && recent).then(|| cached.state.clone())
}

pub async fn state(force: bool) -> DaemonState {
	let requested = Instant::now();
	if !force {
		if let Some(state) = fresh(None) {
			return state;
		}
	}
	let _flight = FETCH.lock().await;
	if let Some(state) = fresh(Some(requested)) {
		return state;
	}
	let generation = GENERATION.load(Ordering::Acquire);
	let state = match call(Method::GET, "/api/status", None, STATUS_TIMEOUT).await {
		Ok(value) if value.is_object() => parse_state(&value),
		Ok(_) | Err(Failure::Unexpected) | Err(Failure::Http(..)) => DaemonState { running: true, ..DaemonState::default() },
		Err(Failure::Busy) => lock(&CACHE)
			.as_ref()
			.map(|cached| cached.state.clone())
			.filter(|previous| previous.running)
			.unwrap_or_else(|| DaemonState { running: true, ..DaemonState::default() }),
		Err(Failure::Down) | Err(Failure::Broken) => DaemonState::default(),
	};
	*lock(&CACHE) = Some(Cached { at: Instant::now(), generation, state: state.clone() });
	state
}

pub fn invalidate() {
	GENERATION.fetch_add(1, Ordering::AcqRel);
	*lock(&CACHE) = None;
}

async fn ping() -> Option<String> {
	let value = call(Method::GET, "/ollama/api/version", None, PING_TIMEOUT).await.ok()?;
	text(&value, "version")
}

fn is_executable(path: &Path) -> bool {
	path.metadata().map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

fn ml5d_binary() -> Option<PathBuf> {
	paths::find_program("ml5d").or_else(|| {
		[PathBuf::from("/usr/local/bin/ml5d"), PathBuf::from("/usr/bin/ml5d"), paths::home().join(".local/bin/ml5d")]
			.into_iter()
			.find(|candidate| is_executable(candidate))
	})
}

async fn binary_version() -> Option<String> {
	let program = ml5d_binary()?;
	let captured = util::capture(&program.to_string_lossy(), &["--version"], VERSION_TIMEOUT).await;
	if !captured.ok {
		return None;
	}
	captured.stdout.split_whitespace().last().map(str::to_string).filter(|version| version.chars().any(|c| c.is_ascii_digit()))
}

pub async fn version() -> Option<String> {
	if let Some(version) = state(false).await.version {
		return Some(version);
	}
	binary_version().await
}

async fn systemctl(args: &[&str], timeout: Duration) -> util::Captured {
	let mut full = vec!["--user"];
	full.extend_from_slice(args);
	util::capture("systemctl", &full, timeout).await
}

async fn unit_state() -> Unit {
	let captured = systemctl(&["show", UNIT, "--property=LoadState", "--value"], SYSTEMCTL_TIMEOUT).await;
	if !captured.ok {
		return Unit::Missing;
	}
	match captured.stdout.trim() {
		"" | "not-found" => Unit::Missing,
		"masked" => Unit::Masked,
		_ => Unit::Present,
	}
}

struct UnitStatus {
	active: String,
	sub: String,
	restarts: u64,
}

async fn unit_status() -> UnitStatus {
	let captured = systemctl(&["show", UNIT, "--property=ActiveState,SubState,NRestarts"], SYSTEMCTL_TIMEOUT).await;
	let mut status = UnitStatus { active: String::new(), sub: String::new(), restarts: 0 };
	for line in captured.stdout.lines() {
		match line.split_once('=') {
			Some(("ActiveState", value)) => status.active = value.trim().to_string(),
			Some(("SubState", value)) => status.sub = value.trim().to_string(),
			Some(("NRestarts", value)) => status.restarts = value.trim().parse().unwrap_or(0),
			_ => {}
		}
	}
	status
}

fn pick_reason(log: &str) -> Option<String> {
	let lines: Vec<&str> = log.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
	lines
		.iter()
		.rev()
		.find(|line| line.to_ascii_lowercase().starts_with("error"))
		.or_else(|| lines.iter().rev().find(|line| !line.contains(".service")))
		.map(|line| line.to_string())
}

async fn unit_reason() -> Option<String> {
	let captured = util::capture("journalctl", &["--user", "--unit", UNIT, "--lines", "40", "--output", "cat", "--no-pager"], SYSTEMCTL_TIMEOUT).await;
	captured.ok.then(|| pick_reason(&captured.stdout)).flatten()
}

fn file_reason(path: &Path) -> Option<String> {
	let mut file = std::fs::File::open(path).ok()?;
	let length = file.metadata().ok()?.len();
	file.seek(SeekFrom::Start(length.saturating_sub(8192))).ok()?;
	let mut bytes = Vec::new();
	file.read_to_end(&mut bytes).ok()?;
	pick_reason(&util::strip_ansi(&String::from_utf8_lossy(&bytes)))
}

fn start_failure(reason: Option<String>) -> String {
	let Some(reason) = reason.map(|reason| reason.trim().to_string()).filter(|reason| !reason.is_empty()) else {
		return "The local engine could not be started. Try again, and restart the computer if it keeps happening.".to_string();
	};
	let lower = reason.to_ascii_lowercase();
	if lower.contains("cannot listen") || lower.contains("address already in use") || lower.contains("address in use") {
		return "The local engine could not start because another program is already using its network port (11435).".to_string();
	}
	if lower.contains("already running") {
		return "Another copy of the local engine is already starting. Wait a moment and try again.".to_string();
	}
	if lower.contains("permission denied") {
		return "The local engine could not start because it is not allowed to open its files. Check the permissions of the .ml5 folder in your home folder.".to_string();
	}
	if lower.contains("no space left") {
		return "The local engine could not start because the disk is full.".to_string();
	}
	let detail = reason.strip_prefix("Error:").unwrap_or(&reason);
	let cleaned = plain(detail);
	if cleaned.is_empty() {
		"The local engine could not be started. Try again, and restart the computer if it keeps happening.".to_string()
	} else {
		format!("The local engine could not be started: {cleaned}")
	}
}

async fn start_unit() -> Result<String, String> {
	let before = unit_status().await.restarts;
	let captured = systemctl(&["start", UNIT], SYSTEMCTL_TIMEOUT).await;
	if !captured.ok {
		return Err(start_failure(unit_reason().await));
	}
	let deadline = Instant::now() + START_WAIT;
	let mut checked = Instant::now();
	while Instant::now() < deadline {
		if ping().await.is_some() {
			return Ok("started".to_string());
		}
		if checked.elapsed() >= Duration::from_secs(2) {
			checked = Instant::now();
			let status = unit_status().await;
			if matches!(status.active.as_str(), "failed" | "inactive") || status.sub == "auto-restart" || status.restarts > before {
				let reason = unit_reason().await;
				systemctl(&["stop", UNIT], STOP_TIMEOUT).await;
				return Err(start_failure(reason));
			}
		}
		tokio::time::sleep(Duration::from_millis(500)).await;
	}
	Err("The local engine was started but did not answer within a minute. It may still be getting ready, so check again shortly.".to_string())
}

fn open_log() -> Result<(PathBuf, std::fs::File), String> {
	let failed = |_| "The local engine could not be started because its log file could not be created.".to_string();
	let dir = paths::runtime_dir();
	paths::ensure_private_dir(&dir).map_err(failed)?;
	let path = dir.join("ml5d.log");
	if path.metadata().is_ok_and(|meta| meta.len() > LOG_LIMIT) {
		let _ = std::fs::rename(&path, dir.join("ml5d.log.1"));
	}
	let file = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&path).map_err(failed)?;
	Ok((path, file))
}

fn spawn_detached(program: &Path, log: std::fs::File) -> Result<Spawned, String> {
	let errors = log.try_clone().map_err(|_| "The local engine could not be started because its log file could not be opened.".to_string())?;
	let mut command = tokio::process::Command::new(program);
	command.stdin(Stdio::null()).stdout(Stdio::from(log)).stderr(Stdio::from(errors)).kill_on_drop(false);
	unsafe {
		command.pre_exec(|| if libc::setsid() == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) });
	}
	let mut child = command.spawn().map_err(|_| "The local engine could not be started because its program could not be run.".to_string())?;
	let pid = child.id().and_then(|pid| libc::pid_t::try_from(pid).ok());
	let (signal, mut signals) = mpsc::unbounded_channel::<libc::c_int>();
	let (exit_sender, exited) = watch::channel(false);
	tokio::spawn(async move {
		loop {
			tokio::select! {
				_ = child.wait() => break,
				Some(number) = signals.recv() => {
					if let Some(pid) = pid {
						unsafe { libc::kill(pid, number) };
					}
				}
			}
		}
		let _ = exit_sender.send(true);
	});
	Ok(Spawned { signal, exited })
}

async fn start_direct() -> Result<String, String> {
	let program = ml5d_binary().ok_or_else(|| "The local engine (ML5) is not installed on this computer.".to_string())?;
	let (log_path, log) = open_log()?;
	let spawned = spawn_detached(&program, log)?;
	let mut exited = spawned.exited.clone();
	*lock(&SPAWNED) = Some(spawned);
	let deadline = Instant::now() + START_WAIT;
	loop {
		if ping().await.is_some() {
			return Ok("started".to_string());
		}
		if *exited.borrow() {
			return Err(start_failure(file_reason(&log_path)));
		}
		if Instant::now() >= deadline {
			return Err("The local engine was started but did not answer within a minute. It may still be getting ready, so check again shortly.".to_string());
		}
		tokio::select! {
			_ = exited.changed() => {}
			_ = tokio::time::sleep(Duration::from_millis(500)) => {}
		}
	}
}

pub async fn start_daemon() -> Result<String, String> {
	let _op = DAEMON_OP.lock().await;
	invalidate();
	if ping().await.is_some() {
		return Ok("already running".to_string());
	}
	let result = match unit_state().await {
		Unit::Present => start_unit().await,
		Unit::Masked => Err("The local engine has been switched off for this account in the system settings, so it cannot be started from here.".to_string()),
		Unit::Missing => start_direct().await,
	};
	invalidate();
	result
}

fn spawned() -> Option<Spawned> {
	lock(&SPAWNED).as_ref().filter(|spawned| !*spawned.exited.borrow()).cloned()
}

async fn wait_exit(exited: &mut watch::Receiver<bool>, limit: Duration) -> bool {
	tokio::time::timeout(limit, exited.wait_for(|done| *done)).await.is_ok()
}

async fn stop_spawned(spawned: Spawned) {
	let mut exited = spawned.exited.clone();
	let _ = spawned.signal.send(libc::SIGTERM);
	if !wait_exit(&mut exited, STOP_WAIT).await {
		let _ = spawned.signal.send(libc::SIGKILL);
		wait_exit(&mut exited, Duration::from_secs(5)).await;
	}
	lock(&SPAWNED).take();
}

async fn wait_down(limit: Duration) -> bool {
	let deadline = Instant::now() + limit;
	loop {
		if ping().await.is_none() {
			return true;
		}
		if Instant::now() >= deadline {
			return false;
		}
		tokio::time::sleep(Duration::from_millis(300)).await;
	}
}

async fn stop_inner() -> Result<String, String> {
	let unit = unit_state().await;
	let unit_running = unit == Unit::Present && matches!(unit_status().await.active.as_str(), "active" | "activating" | "reloading");
	let own = spawned();
	if !unit_running && own.is_none() && ping().await.is_none() {
		return Ok("already stopped".to_string());
	}
	if unit_running {
		systemctl(&["stop", UNIT], STOP_TIMEOUT).await;
		if wait_down(Duration::from_secs(3)).await {
			return Ok("stopped".to_string());
		}
	}
	if let Some(own) = own {
		stop_spawned(own).await;
		if wait_down(Duration::from_secs(3)).await {
			return Ok("stopped".to_string());
		}
	}
	if let Some(program) = ml5d_binary() {
		util::capture(&program.to_string_lossy(), &["--stop"], STOP_TIMEOUT).await;
	}
	if wait_down(STOP_WAIT).await {
		Ok("stopped".to_string())
	} else {
		Err("The local engine did not stop. It may be busy unloading a model, so try again in a moment.".to_string())
	}
}

pub async fn stop_daemon() -> Result<String, String> {
	let _op = DAEMON_OP.lock().await;
	invalidate();
	let result = stop_inner().await;
	invalidate();
	result
}

fn parse_model(item: &Value) -> Model {
	let capabilities = strings(item, "capabilities");
	let has = |capability: &str| capabilities.iter().any(|c| c == capability);
	let details = item.get("details").filter(|details| details.is_object());
	let detail = |key: &str| details.and_then(|details| text(details, key)).filter(|value| !value.is_empty());
	let name = text(item, "name").unwrap_or_default();
	Model {
		name: text(item, "display_name")
			.or_else(|| text(item, "friendly_name"))
			.filter(|display| !display.trim().is_empty())
			.unwrap_or_else(|| name.clone()),
		id: name,
		vision: item.get("vision").and_then(Value::as_bool).unwrap_or_else(|| has("vision")),
		tools: has("tools"),
		thinking: has("thinking"),
		embedding: has("embed") && !has("chat"),
		size_bytes: number(item, "size_bytes").unwrap_or(0),
		format: text(item, "format").unwrap_or_default(),
		loaded: flag(item, "loaded"),
		loaded_context: number(item, "n_ctx"),
		context_length: details.and_then(|details| number(details, "context_length")),
		parameter_size: detail("parameter_size"),
		quantization: detail("quantization"),
		architecture: detail("architecture"),
		modified_at: text(item, "modified_at"),
		capabilities,
	}
}

pub async fn models() -> Result<Vec<Model>, String> {
	let value = call(Method::GET, "/api/models", None, QUICK).await.map_err(failure_text)?;
	let mut models: Vec<Model> = value
		.get("models")
		.and_then(Value::as_array)
		.map(|items| items.iter().map(parse_model).filter(|model| !model.id.is_empty()).collect())
		.unwrap_or_default();
	models.sort_by_cached_key(|model| model.name.to_lowercase());
	Ok(models)
}

pub async fn load(model: &str) -> Result<(), String> {
	let model = require_model(model)?;
	action(Method::POST, "/api/load", Some(&serde_json::json!({ "model": model })), LOAD_TIMEOUT).await.map(|_| ())
}

pub async fn unload(model: &str) -> Result<(), String> {
	let model = require_model(model)?;
	action(Method::POST, "/api/unload", Some(&serde_json::json!({ "model": model })), LIFECYCLE_TIMEOUT).await.map(|_| ())
}

pub async fn rename(model: &str, new_name: &str) -> Result<(), String> {
	let model = require_model(model)?;
	let new_name = new_name.trim();
	if new_name.is_empty() {
		return Err("Type a new name for the model.".to_string());
	}
	action(Method::POST, "/api/rename", Some(&serde_json::json!({ "model": model, "new_name": new_name })), RENAME_TIMEOUT).await.map(|_| ())
}

pub async fn rescan() -> Result<(), String> {
	action(Method::POST, "/api/rescan", None, RESCAN_TIMEOUT).await.map(|_| ())
}

pub async fn delete(model: &str) -> Result<(), String> {
	let model = require_model(model)?;
	action(Method::DELETE, "/api/delete", Some(&serde_json::json!({ "model": model })), LIFECYCLE_TIMEOUT).await.map(|_| ())
}

pub async fn cancel_pull(id: &str) -> Result<(), String> {
	let id = id.trim();
	if id.is_empty() {
		return Ok(());
	}
	let result = match call(Method::POST, "/api/pull/cancel", Some(&serde_json::json!({ "id": id })), QUICK).await {
		Ok(_) => Ok(()),
		Err(Failure::Http(400, body)) if error_detail(&body).is_some_and(|detail| detail.contains("no running download")) => Ok(()),
		Err(failure) => Err(failure_text(failure)),
	};
	invalidate();
	result
}

fn stage_text(event: &str, value: &Value) -> Option<String> {
	match event {
		"resolving" => {
			let target = text(value, "target").unwrap_or_default();
			let shown = target.split(" -> ").next().unwrap_or("").trim().trim_start_matches("hf:").to_string();
			Some(if shown.is_empty() { "Looking up the model…".to_string() } else { format!("Looking up {shown}…") })
		}
		"downloading" => Some(match text(value, "file").filter(|file| !file.is_empty()) {
			Some(file) => format!("Downloading {file}…"),
			None => "Downloading…".to_string(),
		}),
		"verifying" => Some(match text(value, "file").filter(|file| !file.is_empty()) {
			Some(file) => format!("Checking {file}…"),
			None => "Checking the download…".to_string(),
		}),
		_ => None,
	}
}

fn is_hugging_face_link(reference: &str) -> bool {
	let lower = reference.to_ascii_lowercase();
	["https://huggingface.co/", "http://huggingface.co/", "https://hf.co/", "http://hf.co/", "huggingface.co/", "hf.co/", "hf:"].iter().any(|prefix| lower.starts_with(prefix))
}

fn pull_failure(error: &reqwest::Error) -> String {
	if error.is_connect() {
		"The local engine (ML5) is not running, so the download cannot start. Start it from the Models panel and try again.".to_string()
	} else if error.is_timeout() {
		"The local engine took too long to start the download. Try again in a moment.".to_string()
	} else {
		"The connection to the local engine was interrupted before the download started. Try again.".to_string()
	}
}

pub async fn pull(
	reference: &str,
	registry: &str,
	token: Option<&str>,
	quant: Option<&str>,
) -> Result<impl futures_util::Stream<Item = Result<PullEvent, String>>, String> {
	let reference = reference.trim();
	if reference.is_empty() {
		return Err("Type the name of a model to download.".to_string());
	}
	let (guessed, guessed_reference, guessed_quant) = guess_registry(reference);
	let (registry, target, implied_quant) = match registry.trim().to_ascii_lowercase().as_str() {
		"" | "auto" | "core" | "coreml5" => (guessed, guessed_reference, guessed_quant),
		"routes" => ("routes", reference.to_string(), None),
		"hf" | "huggingface" => ("hf", reference.to_string(), None),
		_ => return Err("That download source is not supported. Pick a model from the catalogue or use a Hugging Face name.".to_string()),
	};
	if registry == "hf" && target.contains("://") && !is_hugging_face_link(&target) {
		return Err("Only Hugging Face links can be downloaded. Paste a link that starts with https://huggingface.co/.".to_string());
	}
	let mut body = serde_json::json!({ "model": target, "registry": registry });
	if let Some(token) = token.map(str::trim).filter(|token| !token.is_empty()) {
		body["token"] = serde_json::json!(token);
	}
	if let Some(quant) = quant.map(str::trim).filter(|quant| !quant.is_empty()).map(str::to_string).or(implied_quant) {
		body["quant"] = serde_json::json!(quant);
	}
	let response = match tokio::time::timeout(PULL_START_TIMEOUT, client().post(format!("{ML5_BASE}/api/pull")).json(&body).send()).await {
		Err(_) => return Err("The local engine took too long to start the download. Try again in a moment.".to_string()),
		Ok(Err(error)) => return Err(pull_failure(&error)),
		Ok(Ok(response)) => response,
	};
	let status = response.status();
	if !status.is_success() {
		let body = tokio::time::timeout(QUICK, response.text()).await.ok().and_then(Result::ok).unwrap_or_default();
		return Err(engine_error(status.as_u16(), &body));
	}
	invalidate();
	let mut frames = Box::pin(providers::parse_sse(response.bytes_stream()));
	let (sender, receiver) = mpsc::channel::<Result<PullEvent, String>>(64);
	let stream = futures_util::stream::unfold(receiver, |mut receiver| async move { receiver.recv().await.map(|item| (item, receiver)) });
	let fallback = target.clone();
	tokio::spawn(async move {
		let mut id = String::new();
		let mut file: Option<String> = None;
		let message = loop {
			let frame = match tokio::time::timeout(PULL_STALL, frames.next()).await {
				Err(_) => break "The local engine stopped answering during the download. Check that it is still running, then try again.".to_string(),
				Ok(None) => break "The download stopped unexpectedly. Try again.".to_string(),
				Ok(Some(Err(_))) => break "The connection to the local engine was lost. The download may still be running in the background; check the Downloads list.".to_string(),
				Ok(Some(Ok(frame))) => frame,
			};
			let data = frame.data.trim();
			if data.is_empty() {
				continue;
			}
			let Ok(value) = serde_json::from_str::<Value>(data) else { continue };
			let event = value.get("event").and_then(Value::as_str).or(frame.event.as_deref()).unwrap_or("").to_string();
			let item = match event.as_str() {
				"error" => break text(&value, "error").map(|error| friendly(&error)).unwrap_or_else(|| "The download failed. Try again.".to_string()),
				"done" => {
					invalidate();
					let model = text(&value, "model").filter(|model| !model.is_empty()).unwrap_or_else(|| fallback.clone());
					let _ = sender.send(Ok(PullEvent::Done { id: id.clone(), model })).await;
					return;
				}
				"started" => {
					if let Some(started) = text(&value, "id").filter(|started| !started.is_empty()) {
						id = started;
					}
					PullEvent::Started { id: id.clone() }
				}
				"progress" => PullEvent::Progress {
					id: id.clone(),
					downloaded: number(&value, "downloaded").unwrap_or(0),
					total: number(&value, "total").filter(|total| *total > 0),
					file: text(&value, "file").or_else(|| file.clone()),
				},
				other => {
					if matches!(other, "downloading" | "verifying") {
						file = text(&value, "file").filter(|name| !name.is_empty()).or(file);
					}
					match stage_text(other, &value) {
						Some(text) => PullEvent::Stage { id: id.clone(), text },
						None => continue,
					}
				}
			};
			if sender.send(Ok(item)).await.is_err() {
				return;
			}
		};
		invalidate();
		let _ = sender.send(Ok(PullEvent::Failed { id, message })).await;
	});
	Ok(stream)
}

pub fn guess_registry(reference: &str) -> (&'static str, String, Option<String>) {
	let trimmed = reference.trim();
	if let Some(rest) = trimmed.strip_prefix("routes:").or_else(|| trimmed.strip_prefix("core:")) {
		return ("routes", rest.trim().to_string(), None);
	}
	let lower = trimmed.to_ascii_lowercase();
	let hugging_face = trimmed.starts_with("hf:") || lower.contains("huggingface.co/") || lower.contains("hf.co/") || (trimmed.contains('/') && !trimmed.contains("://"));
	if !hugging_face {
		return ("routes", trimmed.to_string(), None);
	}
	let repo = trimmed.strip_prefix("hf:").unwrap_or(trimmed).trim();
	match repo.rsplit_once(':') {
		Some((base, quant)) if !quant.is_empty() && !quant.contains('/') && base.contains('/') => ("hf", base.to_string(), Some(quant.to_string())),
		_ => ("hf", repo.to_string(), None),
	}
}

fn remote_failure(error: &reqwest::Error) -> String {
	if error.is_builder() {
		"The server address is not valid. Check it in Settings.".to_string()
	} else if error.is_timeout() {
		"The server took too long to answer. Try again in a moment.".to_string()
	} else if error.is_connect() {
		"Could not connect to the server. Check the address and the internet connection.".to_string()
	} else {
		"The connection to the server was interrupted. Try again.".to_string()
	}
}

fn offline(latency_ms: Option<u64>, message: impl Into<String>) -> BackendHealth {
	BackendHealth { online: false, latency_ms, message: message.into() }
}

pub async fn health(local: bool, base_url: &str, api_key: Option<&str>) -> BackendHealth {
	let started = Instant::now();
	let elapsed = || Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
	if local {
		return match call(Method::GET, "/ollama/api/version", None, HEALTH_TIMEOUT).await {
			Ok(value) if text(&value, "version").is_some() => BackendHealth { online: true, latency_ms: elapsed(), message: "Connected".to_string() },
			Ok(_) | Err(Failure::Unexpected) | Err(Failure::Http(..)) => offline(elapsed(), "Another program is using the local engine's address, so ML5 cannot be reached."),
			Err(Failure::Down) => offline(None, "The local engine is not running."),
			Err(Failure::Busy) => offline(None, "The local engine is not answering right now. It may be busy; try again in a moment."),
			Err(Failure::Broken) => offline(None, "The connection to the local engine was interrupted. Try again."),
		};
	}
	let base = base_url.trim().trim_end_matches('/');
	if base.is_empty() {
		return offline(None, "No server address is set. Add one in Settings.");
	}
	let mut request = providers::client(false).get(format!("{base}/models")).timeout(REMOTE_TIMEOUT);
	if let Some(key) = api_key.map(str::trim).filter(|key| !key.is_empty()) {
		request = request.bearer_auth(key);
	}
	match request.send().await {
		Err(error) => offline(None, remote_failure(&error)),
		Ok(response) => {
			let status = response.status();
			if status.is_success() {
				return BackendHealth { online: true, latency_ms: elapsed(), message: "Connected".to_string() };
			}
			let latency = elapsed();
			if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
				return offline(latency, "The API key was refused. Check it in Settings.");
			}
			let body = tokio::time::timeout(REMOTE_TIMEOUT, response.text()).await.ok().and_then(Result::ok).unwrap_or_default();
			offline(latency, providers::error_text(status.as_u16(), &body))
		}
	}
}

pub async fn remote_models(base_url: &str, api_key: Option<&str>) -> Result<Vec<String>, String> {
	let base = base_url.trim().trim_end_matches('/');
	if base.is_empty() {
		return Err("No server address is set. Add one in Settings.".to_string());
	}
	let mut request = providers::client(false).get(format!("{base}/models")).timeout(REMOTE_LIST_TIMEOUT);
	if let Some(key) = api_key.map(str::trim).filter(|key| !key.is_empty()) {
		request = request.bearer_auth(key);
	}
	let response = request.send().await.map_err(|error| remote_failure(&error))?;
	let status = response.status();
	let body = response.text().await.map_err(|error| remote_failure(&error))?;
	if !status.is_success() {
		return Err(providers::error_text(status.as_u16(), &body));
	}
	let value: Value = serde_json::from_str(&body).map_err(|_| "The server sent a model list this app does not understand.".to_string())?;
	let items = value
		.get("data")
		.or_else(|| value.get("models"))
		.or(Some(&value))
		.and_then(Value::as_array)
		.cloned()
		.unwrap_or_default();
	let mut ids: Vec<String> = items
		.iter()
		.filter_map(|item| {
			item.as_str()
				.or_else(|| item.get("id").and_then(Value::as_str))
				.or_else(|| item.get("name").and_then(Value::as_str))
				.or_else(|| item.get("model").and_then(Value::as_str))
				.map(str::trim)
				.filter(|id| !id.is_empty())
				.map(str::to_string)
		})
		.collect();
	ids.sort();
	ids.dedup();
	Ok(ids)
}
