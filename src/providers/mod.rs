pub mod ml5;
pub mod openai;

use crate::util;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::Duration;


pub type BoxStream = std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Event, String>> + Send>>;

#[derive(Debug, Clone)]
pub struct ChatRequest {
	pub provider: String,
	pub base_url: String,
	pub api_key: Option<String>,
	pub model: String,
	pub messages: Vec<WireMessage>,
	pub tools: Option<serde_json::Value>,
	pub temperature: Option<f32>,
	pub max_tokens: Option<u32>,
	pub think: Option<bool>,
}

impl ChatRequest {
	pub fn is_local(&self) -> bool {
		self.provider == "ml5"
	}
}

fn build_local() -> reqwest::Client {
	reqwest::Client::builder()
		.connect_timeout(Duration::from_secs(10))
		.pool_idle_timeout(Duration::from_secs(90))
		.user_agent(USER_AGENT)
		.build()
		.unwrap_or_else(|_| reqwest::Client::new())
}

fn build_remote() -> reqwest::Client {
	reqwest::Client::builder()
		.connect_timeout(Duration::from_secs(20))
		.pool_idle_timeout(Duration::from_secs(90))
		.read_timeout(Duration::from_secs(90))
		.user_agent(USER_AGENT)
		.build()
		.unwrap_or_else(|_| reqwest::Client::new())
}

static LOCAL: OnceLock<reqwest::Client> = OnceLock::new();
static REMOTE: OnceLock<reqwest::Client> = OnceLock::new();

pub const USER_AGENT: &str = concat!("AshAgent/", env!("CARGO_PKG_VERSION"));

pub fn client(local: bool) -> &'static reqwest::Client {
	if local {
		LOCAL.get_or_init(build_local)
	} else {
		REMOTE.get_or_init(build_remote)
	}
}

pub async fn stream(request: ChatRequest) -> Result<BoxStream, String> {
	if request.is_local() {
		ml5::stream(request).await
	} else {
		openai::stream(request).await
	}
}

pub fn error_text(status: u16, body: &str) -> String {
	let trimmed = body.trim();
	let detail = serde_json::from_str::<serde_json::Value>(trimmed)
		.ok()
		.and_then(|value| {
			let error = value.get("error").cloned().unwrap_or(value);
			error
				.get("message")
				.and_then(|m| m.as_str())
				.or_else(|| error.as_str())
				.map(str::to_string)
		})
		.unwrap_or_else(|| trimmed.to_string());
	let detail = util::truncate_end(detail.trim(), 600);
	match status {
		401 | 403 => format!("The API key was refused. Check it in Settings. ({detail})"),
		404 => format!("That model or address does not exist. ({detail})"),
		413 => format!("The request was too large, usually because of an attached image. ({detail})"),
		429 => format!("The provider is rate limiting this account. ({detail})"),
		500 | 502 | 503 | 504 => format!("The provider is having trouble right now. ({detail})"),
		_ => detail,
	}
}

pub const ML5_BASE: &str = "http://127.0.0.1:11435";
pub const OPENCORE_BASE: &str = "https://opencore.one/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WireToolCall {
	pub id: String,
	pub name: String,
	pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WireMessage {
	pub role: String,
	#[serde(default)]
	pub content: String,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub tool_calls: Vec<WireToolCall>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub tool_call_id: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub name: Option<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub images: Vec<String>,
}

impl WireMessage {
	pub fn text(role: &str, content: impl Into<String>) -> Self {
		Self { role: role.into(), content: content.into(), ..Default::default() }
	}
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
	pub id: String,
	pub name: String,
	pub provider: String,
	pub capabilities: Vec<String>,
	pub vision: bool,
	pub tools: bool,
	pub thinking: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub size_bytes: Option<u64>,
	pub loaded: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub context_length: Option<u32>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub format: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
	pub prompt_tokens: u64,
	pub completion_tokens: u64,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cached_tokens: Option<u64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub generation_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Event {
	Status { text: String, completed: Option<u64>, total: Option<u64> },
	Delta { content: String, thinking: String },
	ToolCalls { calls: Vec<WireToolCall> },
	Usage { usage: Usage },
	Done { finish_reason: String },
	RateLimited { retry_after: u64, message: String },
}

#[derive(Debug, Clone)]
pub struct Frame {
	pub event: Option<String>,
	pub data: String,
}

fn find_separator(buffer: &[u8]) -> Option<(usize, usize)> {
	for i in 0..buffer.len() {
		if buffer[i] == b'\n' {
			if i + 1 < buffer.len() && buffer[i + 1] == b'\n' {
				return Some((i, 2));
			}
			if i + 2 < buffer.len() && buffer[i + 1] == b'\r' && buffer[i + 2] == b'\n' {
				return Some((i, 3));
			}
		}
	}
	None
}

fn take_frame(buffer: &mut Vec<u8>) -> Option<Frame> {
	let (at, length) = find_separator(buffer)?;
	let block: Vec<u8> = buffer.drain(..at + length).collect();
	let mut event = None;
	let mut data: Vec<String> = Vec::new();
	for line in block.split(|b| *b == b'\n') {
		let line = String::from_utf8_lossy(line);
		let line = line.trim_end_matches('\r');
		if line.starts_with(':') || line.is_empty() {
			continue;
		}
		if let Some(value) = line.strip_prefix("event:") {
			event = Some(value.trim().to_string());
		} else if let Some(value) = line.strip_prefix("data:") {
			data.push(value.strip_prefix(' ').unwrap_or(value).to_string());
		}
	}
	if event.is_none() && data.is_empty() {
		return Some(Frame { event: None, data: String::new() });
	}
	Some(Frame { event, data: data.join("\n") })
}

pub fn parse_sse<S>(stream: S) -> impl futures_util::Stream<Item = Result<Frame, String>>
where
	S: futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
	let state = (Box::pin(stream), Vec::<u8>::new(), VecDeque::<Frame>::new(), false);
	futures_util::stream::unfold(state, |(mut stream, mut buffer, mut queue, mut finished)| async move {
		loop {
			if let Some(frame) = queue.pop_front() {
				return Some((Ok(frame), (stream, buffer, queue, finished)));
			}
			if finished {
				return None;
			}
			match stream.next().await {
				None => {
					finished = true;
					if let Some(frame) = take_frame(&mut buffer) {
						queue.push_back(frame);
					}
				}
				Some(Ok(chunk)) => {
					buffer.extend_from_slice(&chunk);
					if buffer.len() > 16 * 1024 * 1024 {
						return Some((Err("The model sent more data than this app can buffer.".to_string()), (stream, buffer, queue, true)));
					}
					while let Some(frame) = take_frame(&mut buffer) {
						queue.push_back(frame);
					}
				}
				Some(Err(error)) => {
					return Some((Err(format!("The connection to the model was interrupted: {error}")), (stream, buffer, queue, true)));
				}
			}
		}
	})
}

const OPEN_TAG: &str = "<sbegin>";
const CLOSE_TAG: &str = "</sbegin>";

#[derive(Default)]
pub struct StatusFilter {
	buffer: String,
}

fn partial_tag_len(text: &str) -> usize {
	let bytes = text.as_bytes();
	let max = OPEN_TAG.len().min(bytes.len());
	(1..max).rev().find(|k| bytes[bytes.len() - k..] == OPEN_TAG.as_bytes()[..*k]).unwrap_or(0)
}

impl StatusFilter {
	pub fn feed(&mut self, text: &str) -> (String, Option<String>) {
		self.buffer.push_str(text);
		let mut visible = String::new();
		let mut status = None;
		loop {
			match self.buffer.find(OPEN_TAG) {
				Some(start) => {
					visible.push_str(&self.buffer[..start]);
					let after = start + OPEN_TAG.len();
					match self.buffer[after..].find(CLOSE_TAG) {
						Some(offset) => {
							let body = self.buffer[after..after + offset].trim().to_string();
							if !body.is_empty() {
								status = Some(body);
							}
							self.buffer.drain(..after + offset + CLOSE_TAG.len());
						}
						None => {
							self.buffer.drain(..start);
							break;
						}
					}
				}
				None => {
					let keep = partial_tag_len(&self.buffer);
					if keep < self.buffer.len() {
						let cut = self.buffer.len() - keep;
						visible.push_str(&self.buffer[..cut]);
						self.buffer.drain(..cut);
					}
					break;
				}
			}
		}
		(visible, status)
	}

	pub fn flush(&mut self) -> String {
		let pending = std::mem::take(&mut self.buffer);
		if let Some(start) = pending.find(OPEN_TAG) {
			return pending[..start].to_string();
		}
		pending
	}
}

pub fn data_url(mime: &str, bytes: &[u8]) -> String {
	use base64::Engine;
	format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
}

pub fn decode_data_url(url: &str) -> Option<(String, Vec<u8>)> {
	use base64::Engine;
	let rest = url.strip_prefix("data:")?;
	let (meta, payload) = rest.split_once(",")?;
	let mime = meta.split(';').next().unwrap_or("image/png").to_string();
	let bytes = if meta.contains(";base64") {
		base64::engine::general_purpose::STANDARD.decode(payload).ok()?
	} else {
		util::percent_decode(payload)?
	};
	Some((mime, bytes))
}

pub fn parse_tool_arguments(arguments: &str) -> serde_json::Value {
	serde_json::from_str(arguments).unwrap_or_else(|_| {
		if arguments.trim().is_empty() {
			serde_json::json!({})
		} else {
			serde_json::Value::String(arguments.to_string())
		}
	})
}

pub fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
	let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?.trim();
	if let Ok(seconds) = value.parse::<f64>() {
		return Some(seconds.max(0.0) as u64);
	}
	let target = httpdate::parse_http_date(value).ok()?;
	let now = std::time::SystemTime::now();
	target.duration_since(now).ok().map(|d| d.as_secs())
}

pub fn channel() -> (tokio::sync::mpsc::Sender<Result<Event, String>>, BoxStream) {
	let (sender, receiver) = tokio::sync::mpsc::channel(64);
	let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
		receiver.recv().await.map(|item| (item, receiver))
	});
	(sender, Box::pin(stream))
}

pub fn parse_usage(value: &serde_json::Value) -> Usage {
	Usage {
		prompt_tokens: value.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
		completion_tokens: value.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
		cached_tokens: value
			.get("cached_tokens")
			.and_then(|v| v.as_u64())
			.or_else(|| value.get("prompt_tokens_details").and_then(|d| d.get("cached_tokens")).and_then(|v| v.as_u64())),
		generation_ms: value.get("generation_ms").and_then(|v| v.as_u64()),
	}
}

#[derive(Default)]
pub struct ToolCallAccumulator {
	calls: Vec<WireToolCall>,
}

impl ToolCallAccumulator {
	pub fn push(&mut self, deltas: &serde_json::Value) {
		let Some(items) = deltas.as_array() else { return };
		for (position, delta) in items.iter().enumerate() {
			let index = delta.get("index").and_then(|i| i.as_u64()).map(|i| i as usize).unwrap_or(position);
			while self.calls.len() <= index {
				self.calls.push(WireToolCall { id: String::new(), name: String::new(), arguments: String::new() });
			}
			let call = &mut self.calls[index];
			let function = delta.get("function").filter(|f| f.is_object()).unwrap_or(delta);
			if let Some(id) = delta.get("id").and_then(|i| i.as_str()).filter(|i| !i.is_empty()) {
				call.id = id.to_string();
			}
			if let Some(name) = function.get("name").and_then(|n| n.as_str()) {
				call.name.push_str(name);
			}
			match function.get("arguments") {
				Some(serde_json::Value::String(text)) => call.arguments.push_str(text),
				Some(serde_json::Value::Null) | None => {}
				Some(other) => call.arguments.push_str(&other.to_string()),
			}
		}
	}

	pub fn is_empty(&self) -> bool {
		self.calls.iter().all(|call| call.name.is_empty())
	}

	pub fn finish(mut self) -> Vec<WireToolCall> {
		self.calls.retain(|call| !call.name.is_empty());
		for call in &mut self.calls {
			if call.id.is_empty() {
				call.id = format!("call_{}", util::new_id());
			}
		}
		self.calls
	}
}

pub fn is_opaque(text: &str) -> bool {
	let trimmed = text.trim();
	trimmed.len() > 1024
		&& !trimmed.contains(char::is_whitespace)
		&& trimmed.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_'))
}
