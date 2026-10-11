use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const RETRY_LIMIT: u32 = 3;
const PACING_MS: u64 = 4_100;

static LAST_REQUEST: AtomicU64 = AtomicU64::new(0);
static PACING: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

fn pacing_lock() -> &'static tokio::sync::Mutex<()> {
	PACING.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn respect_pacing() {
	let _guard = pacing_lock().lock().await;
	let now = util::now_ms().max(0) as u64;
	let last = LAST_REQUEST.load(Ordering::Relaxed);
	let wait = (last + PACING_MS).saturating_sub(now);
	if wait > 0 {
		tokio::time::sleep(Duration::from_millis(wait)).await;
	}
	LAST_REQUEST.store(util::now_ms().max(0) as u64, Ordering::Relaxed);
}

fn connection_error(error: &reqwest::Error) -> String {
	if error.is_connect() {
		"There is no internet connection, so the online models cannot be reached. The local models still work offline.".to_string()
	} else if error.is_timeout() {
		"The online model stopped responding. Try again, or pick a different model.".to_string()
	} else if error.is_body() || error.is_decode() {
		"The connection to the online model ended unexpectedly. Try sending the message again.".to_string()
	} else {
		format!("The online model could not be reached: {error}")
	}
}

fn message_value(message: &WireMessage) -> serde_json::Value {
	let mut value = serde_json::json!({ "role": message.role });
	if message.images.is_empty() {
		value["content"] = serde_json::json!(message.content);
	} else {
		let mut parts: Vec<serde_json::Value> = Vec::new();
		if !message.content.is_empty() {
			parts.push(serde_json::json!({ "type": "text", "text": message.content }));
		}
		for image in &message.images {
			parts.push(serde_json::json!({ "type": "image_url", "image_url": { "url": image_url(image) } }));
		}
		value["content"] = serde_json::json!(parts);
	}
	if !message.tool_calls.is_empty() {
		let calls: Vec<serde_json::Value> = message
			.tool_calls
			.iter()
			.map(|call| {
				serde_json::json!({
					"id": call.id,
					"type": "function",
					"function": { "name": call.name, "arguments": if call.arguments.trim().is_empty() { "{}" } else { &call.arguments } }
				})
			})
			.collect();
		value["tool_calls"] = serde_json::json!(calls);
	}
	if let Some(id) = message.tool_call_id.as_ref().filter(|id| !id.is_empty()) {
		value["tool_call_id"] = serde_json::json!(id);
	}
	if let Some(name) = message.name.as_ref().filter(|name| !name.is_empty()) {
		value["name"] = serde_json::json!(name);
	}
	value
}

fn image_url(image: &str) -> String {
	if image.starts_with("data:") || image.starts_with("http://") || image.starts_with("https://") {
		return image.to_string();
	}
	let mime = match image.get(..8) {
		Some(prefix) if prefix.starts_with("iVBOR") => "image/png",
		Some(prefix) if prefix.starts_with("/9j/") => "image/jpeg",
		Some(prefix) if prefix.starts_with("R0lGOD") => "image/gif",
		Some(prefix) if prefix.starts_with("UklGR") => "image/webp",
		_ => "image/png",
	};
	format!("data:{mime};base64,{}", image.trim())
}

fn build_body(request: &ChatRequest) -> serde_json::Value {
	let messages: Vec<serde_json::Value> = request.messages.iter().map(message_value).collect();
	let mut body = serde_json::json!({
		"model": request.model,
		"messages": messages,
		"stream": true,
		"stream_options": { "include_usage": true }
	});
	if let Some(tools) = request.tools.as_ref().filter(|tools| tools.as_array().is_some_and(|items| !items.is_empty())) {
		body["tools"] = tools.clone();
		body["tool_choice"] = serde_json::json!("auto");
	}
	if let Some(temperature) = request.temperature {
		body["temperature"] = serde_json::json!(temperature);
	}
	if let Some(max_tokens) = request.max_tokens {
		body["max_completion_tokens"] = serde_json::json!(max_tokens);
	}
	body
}

fn error_of(value: &serde_json::Value) -> Option<String> {
	let error = value.get("error").filter(|error| !error.is_null())?;
	if error.is_string() && error.as_str().is_some_and(|text| text.trim().is_empty()) {
		return None;
	}
	let text = match error {
		serde_json::Value::String(text) => text.clone(),
		serde_json::Value::Object(fields) => fields
			.get("message")
			.and_then(|message| message.as_str())
			.map(str::to_string)
			.unwrap_or_else(|| serde_json::Value::Object(fields.clone()).to_string()),
		other => other.to_string(),
	};
	let text = util::truncate_end(text.trim(), 800);
	Some(if text.is_empty() { "The online model reported an error.".to_string() } else { text })
}

struct State {
	stream: std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Frame, String>> + Send>>,
	queue: VecDeque<Event>,
	filter: StatusFilter,
	tools: ToolCallAccumulator,
	last_usage: Option<Usage>,
	used_any: bool,
	finish: Option<String>,
	done: bool,
}

impl State {
	fn delta(&mut self, content: &str, thinking: &str) {
		if !content.is_empty() {
			self.used_any = true;
		}
		if !thinking.is_empty() {
			self.used_any = true;
		}
		let (visible, status) = self.filter.feed(content);
		if !thinking.is_empty() {
			self.queue.push_back(Event::Delta { content: String::new(), thinking: thinking.to_string() });
		}
		if !visible.is_empty() {
			self.queue.push_back(Event::Delta { content: visible, thinking: String::new() });
		}
		if let Some(status) = status {
			self.queue.push_back(Event::Status { text: status, completed: None, total: None });
		}
	}

	fn usage_of(&mut self, value: &serde_json::Value) {
		if !value.is_object() {
			return;
		}
		let usage = parse_usage(value);
		if usage.prompt_tokens == 0 && usage.completion_tokens == 0 {
			return;
		}
		self.last_usage = Some(usage.clone());
		self.queue.push_back(Event::Usage { usage });
	}

	fn choice(&mut self, value: &serde_json::Value) {
		let Some(choice) = value.get("choices").and_then(|choices| choices.get(0)) else { return };
		let delta = choice.get("delta").filter(|delta| delta.is_object());
		if let Some(delta) = delta {
			for key in ["reasoning_content", "reasoning", "thinking"] {
				if let Some(thinking) = delta.get(key).and_then(|thinking| thinking.as_str()) {
					if !thinking.is_empty() {
						self.queue.push_back(Event::Delta { content: String::new(), thinking: thinking.to_string() });
						self.used_any = true;
					}
				}
			}
			let content = delta.get("content").and_then(|content| content.as_str()).unwrap_or("");
			if let Some(calls) = delta.get("tool_calls").filter(|calls| calls.as_array().is_some_and(|items| !items.is_empty())) {
				self.tools.push(calls);
			}
			self.delta(content, "");
		}
		if let Some(reason) = choice.get("finish_reason").and_then(|reason| reason.as_str()).filter(|reason| !reason.is_empty()) {
			self.finish = Some(reason.to_string());
		}
	}

	fn finish_turn(&mut self) {
		let pending = self.filter.flush();
		if !pending.is_empty() {
			self.used_any = true;
			self.queue.push_back(Event::Delta { content: pending, thinking: String::new() });
		}
		let calls = std::mem::take(&mut self.tools).finish();
		let reason = self.finish.clone().unwrap_or_else(|| "stop".to_string());
		if !calls.is_empty() {
			self.queue.push_back(Event::ToolCalls { calls });
			self.queue.push_back(Event::Done { finish_reason: "tool_calls".to_string() });
		} else if reason == "length" {
			self.queue.push_back(Event::Done { finish_reason: reason });
		} else if !self.used_any {
			self.queue.push_back(Event::Done { finish_reason: "empty".to_string() });
		} else {
			self.queue.push_back(Event::Done { finish_reason: reason });
		}
	}

	fn mapping(&mut self, frame: Frame) -> Option<String> {
		let data = frame.data.trim();
		if data == "[DONE]" {
			if !self.done {
				self.done = true;
				self.finish_turn();
			}
			return None;
		}
		if data.is_empty() || self.done {
			return None;
		}
		let value: serde_json::Value = serde_json::from_str(data).ok()?;
		if let Some(error) = error_of(&value) {
			return Some(error);
		}
		if let Some(usage) = value.get("usage").filter(|usage| usage.is_object()) {
			self.usage_of(usage);
		}
		self.choice(&value);
		None
	}
}

async fn attempt(request: &ChatRequest, base: &str, key: Option<&str>, out: &tokio::sync::mpsc::Sender<Result<Event, String>>) -> Result<BoxStream, String> {
	let mut tries = 0u32;
	loop {
		if request.provider == "opencore" {
			respect_pacing().await;
		}
		let mut builder = client(false)
			.post(format!("{base}/chat/completions"))
			.header(reqwest::header::ACCEPT, "text/event-stream")
			.json(&build_body(request));
		if let Some(key) = key.filter(|key| !key.is_empty()) {
			builder = builder.bearer_auth(key);
		}
		let response = match builder.send().await {
			Ok(response) => response,
			Err(error) => {
				let message = connection_error(&error);
				if tries < RETRY_LIMIT && (error.is_connect() || error.is_timeout()) {
					tries += 1;
					let wait = 1_500 * tries as u64;
					let _ = out.try_send(Ok(retry_status(wait, tries)));
					continue;
				}
				return Err(message);
			}
		};
		let status = response.status();
		if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
			let wait = retry_after(response.headers()).unwrap_or(5).clamp(1, 90);
			let body = response.text().await.unwrap_or_default();
			if tries < RETRY_LIMIT {
				tries += 1;
				let _ = out.try_send(Ok(retry_status(wait, tries)));
				continue;
			}
			return Err(error_text(status.as_u16(), &body));
		}
		if !status.is_success() {
			let body = response.text().await.unwrap_or_default();
			return Err(error_text(status.as_u16(), &body));
		}
		let _ = out.try_send(Ok(Event::Status { text: "Thinking…".to_string(), completed: None, total: None }));
		let state = State {
			stream: Box::pin(parse_sse(response.bytes_stream())),
			queue: VecDeque::new(),
			filter: StatusFilter::default(),
			tools: ToolCallAccumulator::default(),
			last_usage: None,
			used_any: false,
			finish: None,
			done: false,
		};
		return Ok(Box::pin(run(state)));
	}
}

fn retry_status(wait: u64, attempt: u32) -> Event {
	Event::Status {
		text: format!("The provider is rate limiting this account; retrying in {wait}s (attempt {attempt})."),
		completed: None,
		total: None,
	}
}

fn run(state: State) -> impl futures_util::Stream<Item = Result<Event, String>> + Send {
	futures_util::stream::unfold(Some(state), |state| async move {
		let mut state = state?;
		loop {
			if let Some(event) = state.queue.pop_front() {
				if matches!(event, Event::Done { .. }) && state.queue.is_empty() {
					return Some((Ok(event), None));
				}
				return Some((Ok(event), Some(state)));
			}
			match tokio::time::timeout(IDLE_TIMEOUT, state.stream.next()).await {
				Err(_) => return Some((Err("The online model went quiet and the request was stopped. Try again.".to_string()), None)),
				Ok(None) => {
					if state.done {
						return None;
					}
					if state.finish.is_none() && state.last_usage.is_none() && !state.used_any && state.tools.is_empty() {
						return Some((Err("The connection to the online model ended early. Try sending the message again.".to_string()), None));
					}
					state.done = true;
					state.finish_turn();
				}
				Ok(Some(Err(error))) => return Some((Err(error), None)),
				Ok(Some(Ok(frame))) => {
					if let Some(error) = state.mapping(frame) {
						return Some((Err(error), None));
					}
				}
			}
		}
	})
}

pub async fn stream(request: ChatRequest) -> Result<BoxStream, String> {
	if request.model.trim().is_empty() {
		return Err("Choose a model in Settings first.".to_string());
	}
	let base = request.base_url.trim().trim_end_matches('/').to_string();
	let key = request.api_key.clone();
	let (sender, stream) = channel();
	let out = sender.clone();
	tokio::spawn(async move {
		let deadline = tokio::time::Instant::now() + TOTAL_TIMEOUT;
		let outcome = tokio::select! {
			_ = tokio::time::sleep_until(deadline) => {
				Err("The online model took longer than fifteen minutes, so the request was stopped.".to_string())
			}
			outcome = attempt(&request, &base, key.as_deref(), &out) => outcome,
		};
		match outcome {
			Err(error) => {
				let _ = out.send(Err(error)).await;
			}
			Ok(mut inner) => loop {
				let next = tokio::select! {
					_ = tokio::time::sleep_until(deadline) => {
						let _ = out.send(Err("The online model took longer than fifteen minutes, so the request was stopped.".to_string())).await;
						return;
					}
					next = inner.next() => next,
				};
				match next {
					None => return,
					Some(item) => {
						if out.send(item).await.is_err() {
							return;
						}
					}
				}
			},
		}
	});
	drop(sender);
	Ok(stream)
}
