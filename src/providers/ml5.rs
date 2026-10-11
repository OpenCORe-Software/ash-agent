use super::*;

const IDLE_TIMEOUT: Duration = Duration::from_secs(600);

fn connection_error(error: &reqwest::Error) -> String {
	if error.is_connect() {
		"Could not reach ML5 at 127.0.0.1:11435. Start the local engine from the Models panel, or check that the ml5d service is running.".to_string()
	} else if error.is_timeout() {
		"The local model stopped responding. It may be loading a large model; try again in a moment.".to_string()
	} else if error.is_body() || error.is_decode() {
		"The connection to the local model ended unexpectedly. Try sending the message again.".to_string()
	} else {
		format!("The local model could not be reached: {error}")
	}
}

fn message_value(message: &WireMessage) -> serde_json::Value {
	let mut value = serde_json::json!({ "role": message.role, "content": message.content });
	if !message.images.is_empty() {
		value["images"] = serde_json::json!(message.images);
	}
	if !message.tool_calls.is_empty() {
		value["tool_calls"] = serde_json::to_value(&message.tool_calls).unwrap_or(serde_json::Value::Array(Vec::new()));
	}
	if let Some(id) = message.tool_call_id.as_ref().filter(|id| !id.is_empty()) {
		value["tool_call_id"] = serde_json::json!(id);
	}
	if let Some(name) = message.name.as_ref().filter(|name| !name.is_empty()) {
		value["name"] = serde_json::json!(name);
	}
	value
}

fn build_body(request: &ChatRequest) -> serde_json::Value {
	let messages: Vec<serde_json::Value> = request.messages.iter().map(message_value).collect();
	let mut body = serde_json::json!({ "model": request.model, "messages": messages, "stream": true });
	if let Some(tools) = request.tools.as_ref().filter(|tools| tools.as_array().is_some_and(|items| !items.is_empty())) {
		body["tools"] = tools.clone();
		body["tool_choice"] = serde_json::json!("auto");
	}
	if let Some(temperature) = request.temperature {
		body["temperature"] = serde_json::json!(temperature);
	}
	if let Some(max_tokens) = request.max_tokens {
		body["max_tokens"] = serde_json::json!(max_tokens);
	}
	if let Some(think) = request.think {
		body["think"] = serde_json::json!(think);
	}
	body
}

fn error_of(value: &serde_json::Value) -> Option<String> {
	let error = value.get("error").filter(|error| !error.is_null())?;
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
	Some(if text.is_empty() { "The local model reported an error.".to_string() } else { text })
}

fn status_text(value: &serde_json::Value, model: &str) -> String {
	match value.get("status").and_then(|status| status.as_str()).unwrap_or_default() {
		"loading_model" => value
			.get("message")
			.and_then(|message| message.as_str())
			.map(str::to_string)
			.unwrap_or_else(|| format!("Loading {model}…")),
		"queued" => "Waiting for the model…".to_string(),
		"processing_prompt" => {
			let completed = value.get("completed").and_then(|number| number.as_u64()).unwrap_or(0);
			match value.get("total").and_then(|number| number.as_u64()).filter(|total| *total > 0) {
				Some(total) => format!("Reading the conversation… {}%", (completed.min(total) * 100) / total),
				None => "Reading the conversation…".to_string(),
			}
		}
		other => other.replace('_', " "),
	}
}

struct State {
	stream: std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Frame, String>> + Send>>,
	queue: VecDeque<Event>,
	calls: Vec<WireToolCall>,
	finish: Option<String>,
	done: bool,
	model: String,
}

impl State {
	fn absorb(&mut self, value: &serde_json::Value) {
		if let Some(text) = value.get("response").and_then(|response| response.as_str()) {
			if !text.is_empty() {
				self.queue.push_back(Event::Delta { content: text.to_string(), thinking: String::new() });
			}
		}
		let Some(message) = value.get("message").filter(|message| message.is_object()) else { return };
		let content = message.get("content").and_then(|content| content.as_str()).unwrap_or("");
		let thinking = message.get("thinking").and_then(|thinking| thinking.as_str()).unwrap_or("");
		if !content.is_empty() || !thinking.is_empty() {
			self.queue.push_back(Event::Delta { content: content.to_string(), thinking: thinking.to_string() });
		}
		if let Some(calls) = message.get("tool_calls").and_then(|calls| calls.as_array()) {
			for call in calls {
				let function = call.get("function").filter(|function| function.is_object()).unwrap_or(call);
				let Some(name) = function.get("name").and_then(|name| name.as_str()).filter(|name| !name.is_empty()) else { continue };
				let arguments = match function.get("arguments") {
					Some(serde_json::Value::String(text)) => text.clone(),
					Some(serde_json::Value::Null) | None => "{}".to_string(),
					Some(other) => other.to_string(),
				};
				let id = call
					.get("id")
					.and_then(|id| id.as_str())
					.filter(|id| !id.is_empty())
					.map(str::to_string)
					.unwrap_or_else(|| format!("call_{}", util::new_id()));
				if !self.calls.iter().any(|known| known.id == id) {
					self.calls.push(WireToolCall { id, name: name.to_string(), arguments });
				}
			}
		}
		if let Some(reason) = value.get("finish_reason").and_then(|reason| reason.as_str()) {
			self.finish = Some(reason.to_string());
		}
		if value.get("usage").is_some_and(|usage| usage.is_object()) {
			self.queue.push_back(Event::Usage { usage: parse_usage(&value["usage"]) });
		}
		if value.get("done").and_then(|done| done.as_bool()).unwrap_or(false) {
			self.done = true;
		}
	}

	fn finish_turn(&mut self) {
		let calls = std::mem::take(&mut self.calls);
		let finish_reason = if calls.is_empty() {
			self.finish.clone().unwrap_or_else(|| "stop".to_string())
		} else {
			self.queue.push_back(Event::ToolCalls { calls });
			"tool_calls".to_string()
		};
		self.queue.push_back(Event::Done { finish_reason });
	}

	fn mapping(&mut self, frame: Frame) -> Option<String> {
		if frame.data.trim().is_empty() {
			return None;
		}
		let value: serde_json::Value = serde_json::from_str(frame.data.trim()).ok()?;
		if let Some(error) = error_of(&value) {
			return Some(error);
		}
		if value.get("status").is_some() {
			self.queue.push_back(Event::Status {
				text: status_text(&value, &self.model),
				completed: value.get("completed").and_then(|value| value.as_u64()),
				total: value.get("total").and_then(|value| value.as_u64()),
			});
			return None;
		}
		self.absorb(&value);
		if self.done {
			self.finish_turn();
		}
		None
	}
}

pub async fn stream(request: ChatRequest) -> Result<BoxStream, String> {
	if request.model.trim().is_empty() {
		return Err("Choose a local model in Settings first.".to_string());
	}
	let base = if request.base_url.trim().is_empty() { ML5_BASE.to_string() } else { request.base_url.trim().trim_end_matches('/').to_string() };
	let response = client(true)
		.post(format!("{base}/api/chat"))
		.json(&build_body(&request))
		.send()
		.await
		.map_err(|error| connection_error(&error))?;
	let status = response.status();
	if !status.is_success() {
		let body = response.text().await.unwrap_or_default();
		return Err(error_text(status.as_u16(), &body));
	}
	let state = State {
		stream: Box::pin(parse_sse(response.bytes_stream())),
		queue: VecDeque::new(),
		calls: Vec::new(),
		finish: None,
		done: false,
		model: request.model.clone(),
	};
	let stream = futures_util::stream::unfold(Some(state), |state| async move {
		let mut state = state?;
		loop {
			if let Some(event) = state.queue.pop_front() {
				if matches!(event, Event::Done { .. }) {
					let done = state.queue.is_empty();
					return Some((Ok(event), if done { None } else { Some(state) }));
				}
				return Some((Ok(event), Some(state)));
			}
			match tokio::time::timeout(IDLE_TIMEOUT, state.stream.next()).await {
				Err(_) => return Some((Err("The local model stopped responding. It may still be loading; try again once it is ready.".to_string()), None)),
				Ok(None) => {
					return Some((Err("The local model ended the response without finishing. Try sending the message again.".to_string()), None));
				}
				Ok(Some(Err(error))) => return Some((Err(error), None)),
				Ok(Some(Ok(frame))) => {
					if let Some(error) = state.mapping(frame) {
						return Some((Err(error), None));
					}
				}
			}
		}
	});
	Ok(Box::pin(stream))
}
