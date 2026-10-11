use crate::util;
use futures_util::stream::{FuturesOrdered, StreamExt};
use std::future::Future;
use serde::Serialize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::Connection;

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const COMPONENT: &str = "org.a11y.atspi.Component";
const TEXT: &str = "org.a11y.atspi.Text";
const VALUE: &str = "org.a11y.atspi.Value";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const REGISTRY: &str = "org.a11y.atspi.Registry";
const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";
const NULL_PATH: &str = "/org/a11y/atspi/null";
const CALL_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_NODES: usize = 2500;
const MAX_CHILDREN: usize = 250;
const MAX_OUTPUT: usize = 20 * 1024;
const PARALLEL: usize = 6;
const TEXT_PREVIEW: i32 = 200;
pub const MAX_DEPTH: u32 = 30;
pub const DEFAULT_DEPTH: u32 = 12;

const STATES: &[&str] = &[
	"invalid", "active", "armed", "busy", "checked", "collapsed", "defunct", "editable", "enabled", "expandable", "expanded", "focusable", "focused", "has tooltip", "horizontal", "iconified", "modal", "multi line", "multiselectable", "opaque", "pressed", "resizable", "selectable", "selected", "sensitive", "showing", "single line", "stale", "transient", "vertical", "visible", "manages descendants", "indeterminate", "required", "truncated", "animated", "invalid entry", "supports autocompletion", "selectable text", "is default", "visited", "checkable", "has popup", "read only",
];
const REPORTED_STATES: &[&str] = &["focused", "checked", "selected", "expanded", "collapsed", "pressed", "editable", "read only", "required", "invalid entry", "is default", "modal", "indeterminate", "busy"];
const TRANSPARENT_ROLES: &[&str] = &["filler", "panel", "section", "grouping", "layered pane", "scroll pane", "viewport", "redundant object", "unknown", "root pane", "glass pane", "split pane", "internal frame", "html container"];
const STATE_ACTIVE: u32 = 1;
const STATE_ENABLED: u32 = 8;
const STATE_SHOWING: u32 = 25;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessState {
	pub available: bool,
	pub bus_address: bool,
	pub applications: usize,
	pub message: String,
}

#[derive(Clone, PartialEq, Eq)]
struct Target {
	destination: String,
	path: String,
}

struct Node {
	role: String,
	name: String,
	description: String,
	value: Option<String>,
	states: Vec<String>,
	disabled: bool,
	bounds: Option<(i32, i32, i32, i32)>,
	children: Vec<Node>,
	truncated: usize,
}

struct Walker {
	connection: Connection,
	budget: AtomicUsize,
}

fn unwrap_value(value: Value<'_>) -> Value<'_> {
	match value {
		Value::Value(inner) => unwrap_value(*inner),
		other => other,
	}
}

fn text_of(value: &Value<'_>) -> Option<String> {
	match value {
		Value::Str(text) => Some(text.to_string()),
		Value::Value(inner) => text_of(inner),
		_ => None,
	}
}

fn number_of(value: &Value<'_>) -> Option<f64> {
	match value {
		Value::I32(number) => Some(*number as f64),
		Value::U32(number) => Some(*number as f64),
		Value::I64(number) => Some(*number as f64),
		Value::U64(number) => Some(*number as f64),
		Value::I16(number) => Some(*number as f64),
		Value::U16(number) => Some(*number as f64),
		Value::U8(number) => Some(*number as f64),
		Value::F64(number) => Some(*number),
		Value::Value(inner) => number_of(inner),
		_ => None,
	}
}

fn state_names(words: &[u32]) -> (Vec<u32>, Vec<String>) {
	let mut bits = Vec::new();
	for (index, word) in words.iter().enumerate().take(2) {
		for bit in 0..32u32 {
			if word & (1 << bit) != 0 {
				bits.push(index as u32 * 32 + bit);
			}
		}
	}
	let names = bits.iter().filter_map(|bit| STATES.get(*bit as usize)).map(|name| name.to_string()).collect();
	(bits, names)
}

async fn bounded<T, F>(futures: Vec<F>, limit: usize) -> Vec<T>
where
	F: Future<Output = T>,
{
	let mut results = Vec::with_capacity(futures.len());
	let mut pending = futures.into_iter();
	let mut running = FuturesOrdered::new();
	for future in pending.by_ref().take(limit.max(1)) {
		running.push_back(future);
	}
	while let Some(result) = running.next().await {
		results.push(result);
		if let Some(future) = pending.next() {
			running.push_back(future);
		}
	}
	results
}

impl Walker {
	async fn call<B>(&self, target: &Target, interface: &str, method: &str, body: &B) -> Option<zbus::Message>
	where
		B: serde::Serialize + zbus::zvariant::DynamicType,
	{
		self.connection
			.call_method(Some(target.destination.as_str()), target.path.as_str(), Some(interface), method, body)
			.await
			.ok()
	}

	async fn property(&self, target: &Target, interface: &str, name: &str) -> Option<OwnedValue> {
		let reply = self.call(target, PROPERTIES, "Get", &(interface, name)).await?;
		let body = reply.body();
		let value: Value = body.deserialize().ok()?;
		OwnedValue::try_from(unwrap_value(value)).ok()
	}

	async fn text_property(&self, target: &Target, interface: &str, name: &str) -> String {
		self.property(target, interface, name).await.and_then(|value| text_of(&value)).unwrap_or_default()
	}

	async fn number_property(&self, target: &Target, interface: &str, name: &str) -> Option<f64> {
		self.property(target, interface, name).await.and_then(|value| number_of(&value))
	}

	async fn role(&self, target: &Target) -> String {
		match self.call(target, ACCESSIBLE, "GetRoleName", &()).await {
			Some(reply) => reply.body().deserialize::<String>().unwrap_or_default(),
			None => String::new(),
		}
	}

	async fn state_bits(&self, target: &Target) -> Vec<u32> {
		match self.call(target, ACCESSIBLE, "GetState", &()).await {
			Some(reply) => reply.body().deserialize::<Vec<u32>>().unwrap_or_default(),
			None => Vec::new(),
		}
	}

	async fn interfaces(&self, target: &Target) -> Vec<String> {
		match self.call(target, ACCESSIBLE, "GetInterfaces", &()).await {
			Some(reply) => reply.body().deserialize::<Vec<String>>().unwrap_or_default(),
			None => Vec::new(),
		}
	}

	async fn extents(&self, target: &Target, coordinates: u32) -> Option<(i32, i32, i32, i32)> {
		let reply = self.call(target, COMPONENT, "GetExtents", &(coordinates)).await?;
		let bounds: (i32, i32, i32, i32) = reply.body().deserialize().ok()?;
		(bounds.2 > 0 && bounds.3 > 0).then_some(bounds)
	}

	async fn children(&self, target: &Target) -> (Vec<Target>, usize) {
		let count = self.number_property(target, ACCESSIBLE, "ChildCount").await.unwrap_or(0.0).max(0.0) as usize;
		if count == 0 {
			return (Vec::new(), 0);
		}
		let list: Vec<Target> = if count <= MAX_CHILDREN {
			match self.call(target, ACCESSIBLE, "GetChildren", &()).await {
				Some(reply) => reply
					.body()
					.deserialize::<Vec<(String, OwnedObjectPath)>>()
					.unwrap_or_default()
					.into_iter()
					.map(|(destination, path)| Target { destination, path: path.to_string() })
					.collect(),
				None => Vec::new(),
			}
		} else {
			let lookups: Vec<_> = (0..MAX_CHILDREN as i32)
				.map(|index| async move {
					let reply = self.call(target, ACCESSIBLE, "GetChildAtIndex", &(index)).await?;
					let (destination, path): (String, OwnedObjectPath) = reply.body().deserialize().ok()?;
					Some(Target { destination, path: path.to_string() })
				})
				.collect();
			bounded(lookups, PARALLEL * 2).await.into_iter().flatten().collect()
		};
		let list: Vec<Target> = list.into_iter().filter(|child| !child.destination.is_empty() && child.path != NULL_PATH).collect();
		let omitted = count.saturating_sub(MAX_CHILDREN);
		(list, omitted)
	}

	async fn text_preview(&self, target: &Target) -> Option<String> {
		let count = self.number_property(target, TEXT, "CharacterCount").await? as i32;
		if count <= 0 {
			return None;
		}
		let reply = self.call(target, TEXT, "GetText", &(0i32, count.min(TEXT_PREVIEW))).await?;
		let text: String = reply.body().deserialize().ok()?;
		let flattened = text.replace(['\n', '\r', '\t'], " ");
		let trimmed = flattened.trim();
		(!trimmed.is_empty()).then(|| if count > TEXT_PREVIEW { format!("{trimmed}…") } else { trimmed.to_string() })
	}

	async fn value_preview(&self, target: &Target) -> Option<String> {
		let (current, minimum, maximum) = tokio::join!(
			self.number_property(target, VALUE, "CurrentValue"),
			self.number_property(target, VALUE, "MinimumValue"),
			self.number_property(target, VALUE, "MaximumValue")
		);
		let current = current?;
		let render = |number: f64| if number.fract() == 0.0 { format!("{}", number as i64) } else { format!("{number:.2}") };
		Some(match (minimum, maximum) {
			(Some(low), Some(high)) if high > low => format!("{} (range {}–{})", render(current), render(low), render(high)),
			_ => render(current),
		})
	}

	fn walk<'a>(&'a self, target: Target, depth: u32, top: bool) -> futures_util::future::BoxFuture<'a, Option<Node>> {
		Box::pin(async move {
			if self.budget.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1)).is_err() {
				return None;
			}
			let (role, name, description, words, interfaces) = tokio::join!(
				self.role(&target),
				self.text_property(&target, ACCESSIBLE, "Name"),
				self.text_property(&target, ACCESSIBLE, "Description"),
				self.state_bits(&target),
				self.interfaces(&target)
			);
			let (bits, names) = state_names(&words);
			if !top && !words.is_empty() && !bits.contains(&STATE_SHOWING) {
				return None;
			}
			let has = |interface: &str| interfaces.iter().any(|known| known == interface);
			let password = role == "password text";
			let (bounds, text, value) = tokio::join!(
				async { if has(COMPONENT) { self.extents(&target, 1).await } else { None } },
				async { if has(TEXT) && !password { self.text_preview(&target).await } else { None } },
				async { if has(VALUE) { self.value_preview(&target).await } else { None } }
			);
			let mut node = Node {
				role,
				name: name.trim().to_string(),
				description: description.trim().to_string(),
				value: if password { Some("[hidden]".to_string()) } else { value.or(text).filter(|shown| shown != name.trim()) },
				states: names.into_iter().filter(|state| REPORTED_STATES.contains(&state.as_str())).collect(),
				disabled: !words.is_empty() && !bits.contains(&STATE_ENABLED) && !top,
				bounds,
				children: Vec::new(),
				truncated: 0,
			};
			if depth == 0 {
				return Some(node);
			}
			let (children, omitted) = self.children(&target).await;
			node.truncated = omitted;
			let walks: Vec<_> = children.into_iter().map(|child| self.walk(child, depth - 1, false)).collect();
			let built: Vec<Option<Node>> = bounded(walks, PARALLEL).await;
			node.children = built.into_iter().flatten().collect();
			Some(node)
		})
	}
}

const STATUS_SERVICE: &str = "org.a11y.Bus";
const STATUS_PATH: &str = "/org/a11y/bus";
const STATUS_INTERFACE: &str = "org.a11y.Status";
const REGISTER_WAIT: Duration = Duration::from_secs(4);
const REGISTER_POLL: Duration = Duration::from_millis(250);

async fn switched_on(session: &Connection) -> Option<bool> {
	let reply = tokio::time::timeout(CALL_TIMEOUT, session.call_method(Some(STATUS_SERVICE), STATUS_PATH, Some(PROPERTIES), "Get", &(STATUS_INTERFACE, "IsEnabled"))).await.ok()?.ok()?;
	let body = reply.body();
	let value: OwnedValue = body.deserialize().ok()?;
	match unwrap_value(Value::from(value)) {
		Value::Bool(enabled) => Some(enabled),
		_ => None,
	}
}

async fn switch_on(session: &Connection) -> bool {
	let call = session.call_method(Some(STATUS_SERVICE), STATUS_PATH, Some(PROPERTIES), "Set", &(STATUS_INTERFACE, "IsEnabled", Value::Bool(true)));
	matches!(tokio::time::timeout(CALL_TIMEOUT, call).await, Ok(Ok(_)))
}

async fn wait_for_registrations(walker: &Walker) {
	let deadline = tokio::time::Instant::now() + REGISTER_WAIT;
	let mut last = applications(walker).await.len();
	let mut steady = 0;
	while tokio::time::Instant::now() < deadline && steady < 3 {
		tokio::time::sleep(REGISTER_POLL).await;
		let count = applications(walker).await.len();
		steady = if count == last { steady + 1 } else { 0 };
		last = count;
	}
}

async fn accessibility_bus() -> Result<Connection, String> {
	let session = Connection::session().await.map_err(|_| "There is no desktop session to inspect.".to_string())?;
	let reply = session.call_method(Some("org.a11y.Bus"), "/org/a11y/bus", Some("org.a11y.Bus"), "GetAddress", &()).await.map_err(|error| {
		let text = error.to_string();
		if text.contains("ServiceUnknown") || text.contains("NameHasNoOwner") {
			"Accessibility support is not running on this desktop, so applications cannot be read through it.".to_string()
		} else {
			format!("Accessibility support could not be reached: {text}")
		}
	})?;
	let address = reply.body().deserialize::<String>().map_err(|_| "Accessibility support gave an unusable address.".to_string())?;
	if address.trim().is_empty() {
		return Err("Accessibility support is not running on this desktop.".to_string());
	}
	zbus::connection::Builder::address(address.as_str())
		.map_err(|error| format!("Accessibility support could not be opened: {error}"))?
		.method_timeout(CALL_TIMEOUT)
		.build()
		.await
		.map_err(|error| format!("Accessibility support could not be opened: {error}"))
}

struct Application {
	target: Target,
	name: String,
	pid: Option<u32>,
}

struct Frame {
	application: usize,
	target: Target,
	name: String,
	role: String,
	active: bool,
	screen: Option<(i32, i32, i32, i32)>,
	local: Option<(i32, i32, i32, i32)>,
}

async fn applications(walker: &Walker) -> Vec<Application> {
	let registry = Target { destination: REGISTRY.to_string(), path: ROOT_PATH.to_string() };
	let roots: Vec<Target> = match walker.call(&registry, ACCESSIBLE, "GetChildren", &()).await {
		Some(reply) => reply
			.body()
			.deserialize::<Vec<(String, OwnedObjectPath)>>()
			.unwrap_or_default()
			.into_iter()
			.filter(|(destination, path)| !destination.is_empty() && path.as_str() != NULL_PATH)
			.map(|(destination, path)| Target { destination, path: path.to_string() })
			.collect(),
		None => Vec::new(),
	};
	let lookups: Vec<_> = roots
		.into_iter()
		.map(|target| async move {
			let name = walker.text_property(&target, ACCESSIBLE, "Name").await;
			let pid = walker
				.connection
				.call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus"), "GetConnectionUnixProcessID", &(target.destination.as_str()))
				.await
				.ok()
				.and_then(|reply| reply.body().deserialize::<u32>().ok());
			Application { target, name, pid }
		})
		.collect();
	bounded(lookups, PARALLEL * 2).await
}

async fn frames(walker: &Walker, apps: &[Application]) -> Vec<Frame> {
	let lookups: Vec<_> = apps
		.iter()
		.enumerate()
		.map(|(index, app)| async move {
			let (children, _) = walker.children(&app.target).await;
			let mut found = Vec::new();
			for target in children.into_iter().take(20) {
				let (name, role, words) = tokio::join!(walker.text_property(&target, ACCESSIBLE, "Name"), walker.role(&target), walker.state_bits(&target));
				let (bits, _) = state_names(&words);
				if !bits.contains(&STATE_SHOWING) && !bits.contains(&STATE_ACTIVE) {
					continue;
				}
				let (screen, local) = tokio::join!(walker.extents(&target, 0), walker.extents(&target, 1));
				found.push(Frame { application: index, target, name: name.trim().to_string(), role, active: bits.contains(&STATE_ACTIVE), screen, local });
			}
			found
		})
		.collect();
	bounded(lookups, PARALLEL * 2).await.into_iter().flatten().collect()
}

fn origin_for(frame: &Frame, app: &Application, windows: &[super::window::Window]) -> Option<(i32, i32)> {
	let pid = app.pid? as i64;
	let candidates: Vec<&super::window::Window> = windows.iter().filter(|window| window.pid == pid).collect();
	if candidates.is_empty() {
		return None;
	}
	let size = frame.local.or(frame.screen).map(|bounds| (bounds.2, bounds.3));
	let by_title = |window: &&super::window::Window| !frame.name.is_empty() && (window.title == frame.name || window.title.contains(&frame.name));
	let by_size = |window: &&super::window::Window| size.map(|(width, height)| (window.client_width - width).abs() <= 2 && (window.client_height - height).abs() <= 2).unwrap_or(false);
	let chosen = candidates
		.iter()
		.find(|window| by_title(window) && by_size(window))
		.or_else(|| candidates.iter().find(|window| by_size(window)))
		.or_else(|| candidates.iter().find(|window| by_title(window)))
		.or_else(|| (candidates.len() == 1).then(|| &candidates[0]))?;
	Some((chosen.client_x, chosen.client_y))
}

struct Renderer {
	lines: Vec<String>,
	size: usize,
	clipped: bool,
	origin: Option<(i32, i32)>,
	fallback: Option<(i32, i32)>,
}

impl Renderer {
	fn push(&mut self, line: String) {
		if self.size + line.len() + 1 > MAX_OUTPUT {
			self.clipped = true;
			return;
		}
		self.size += line.len() + 1;
		self.lines.push(line);
	}

	fn describe(&self, node: &Node) -> String {
		let mut line = node.role.clone();
		if !node.name.is_empty() {
			line.push_str(&format!(" \"{}\"", util::truncate_end(&node.name, 120)));
		}
		if let Some(value) = &node.value {
			line.push_str(&format!(" = \"{}\"", util::truncate_end(value, 200)));
		}
		if !node.description.is_empty() && node.description != node.name {
			line.push_str(&format!(" ({})", util::truncate_end(&node.description, 120)));
		}
		let mut flags = node.states.clone();
		if node.disabled {
			flags.insert(0, "disabled".to_string());
		}
		if !flags.is_empty() {
			line.push_str(&format!(" [{}]", flags.join(", ")));
		}
		if let Some((x, y, width, height)) = node.bounds {
			let (left, top) = match self.origin.or(self.fallback) {
				Some((origin_x, origin_y)) => (origin_x + x, origin_y + y),
				None => (x, y),
			};
			line.push_str(&format!(" at {},{} size {width}x{height}", left + width / 2, top + height / 2));
		}
		line
	}

	fn render(&mut self, node: &Node, indent: usize) {
		if self.clipped {
			return;
		}
		let transparent = node.name.is_empty() && node.value.is_none() && node.states.is_empty() && TRANSPARENT_ROLES.contains(&node.role.as_str());
		let next = if transparent && indent > 0 {
			indent
		} else {
			let line = format!("{}- {}", "  ".repeat(indent), self.describe(node));
			self.push(line);
			indent + 1
		};
		for child in &node.children {
			self.render(child, next);
		}
		if node.truncated > 0 {
			self.push(format!("{}- … {} more items not shown", "  ".repeat(next), node.truncated));
		}
	}
}

pub async fn tree(depth: u32, window: Option<&str>) -> Result<String, String> {
	let connection = accessibility_bus().await?;
	let walker = Walker { connection, budget: AtomicUsize::new(MAX_NODES) };
	if let Ok(session) = Connection::session().await {
		if switched_on(&session).await == Some(false) && switch_on(&session).await {
			wait_for_registrations(&walker).await;
		}
	}
	let apps = applications(&walker).await;
	if apps.is_empty() {
		return Err(access_hint());
	}
	let found = frames(&walker, &apps).await;
	let query = window.map(str::trim).filter(|query| !query.is_empty()).map(str::to_lowercase);
	let selected: Vec<&Frame> = match &query {
		Some(query) => found.iter().filter(|frame| frame.name.to_lowercase().contains(query) || apps[frame.application].name.to_lowercase().contains(query)).collect(),
		None => found.iter().filter(|frame| frame.active).collect(),
	};
	if selected.is_empty() {
		let mut listing: Vec<String> = found
			.iter()
			.map(|frame| {
				let app = &apps[frame.application];
				let title = if frame.name.is_empty() { "(untitled)".to_string() } else { format!("\"{}\"", frame.name) };
				format!("- {} {title} from {}", frame.role, if app.name.is_empty() { "an unnamed application" } else { app.name.as_str() })
			})
			.collect();
		listing.sort();
		listing.dedup();
		let lead = match &query {
			Some(query) => format!("No window matching \"{query}\" is publishing its controls."),
			None => "The focused window is not publishing its controls.".to_string(),
		};
		if listing.is_empty() {
			return Err(format!("{lead} {}", access_hint()));
		}
		return Ok(format!("{lead} These windows can be read; ask for one by name:\n{}", listing.join("\n")));
	}
	let windows = super::window::list().await.unwrap_or_default();
	let depth = depth.clamp(1, MAX_DEPTH);
	let mut sections: Vec<String> = Vec::new();
	for frame in selected.into_iter().take(4) {
		let app = &apps[frame.application];
		let Some(node) = walker.walk(frame.target.clone(), depth, true).await else { break };
		let origin = origin_for(frame, app, &windows);
		let fallback = match (frame.screen, frame.local) {
			(Some(screen), Some(local)) => Some((screen.0 - local.0, screen.1 - local.1)),
			(Some(screen), None) => Some((screen.0, screen.1)),
			_ => None,
		};
		let mut renderer = Renderer { lines: Vec::new(), size: 0, clipped: false, origin, fallback };
		renderer.render(&node, 0);
		let mut text = format!(
			"Application: {}{}\n",
			if app.name.is_empty() { "unnamed" } else { app.name.as_str() },
			app.pid.map(|pid| format!(" (process {pid})")).unwrap_or_default()
		);
		text.push_str(&renderer.lines.join("\n"));
		if renderer.clipped {
			text.push_str("\n- … the rest was cut off; ask for a smaller depth or a specific window to see more");
		}
		sections.push(text);
	}
	let mut output = sections.join("\n\n");
	if walker.budget.load(Ordering::SeqCst) == 0 {
		output.push_str("\n\nThe window has more controls than could be read at once; ask for a smaller depth to see an overview.");
	}
	output.push_str("\n\n\"at x,y\" is the centre of each control in screen pixels, ready for mouse_actions.");
	Ok(output)
}

pub fn access_hint() -> String {
	"No application is publishing its controls right now. Ash switches accessibility on when it needs it; KDE and GTK applications follow straight away, but Firefox, Chromium based browsers and Electron applications that were already open only publish their controls after they are restarted.".to_string()
}

pub async fn state() -> AccessState {
	match accessibility_bus().await {
		Ok(connection) => {
			let walker = Walker { connection, budget: AtomicUsize::new(0) };
			let count = applications(&walker).await.len();
			let enabled = match Connection::session().await {
				Ok(session) => switched_on(&session).await,
				Err(_) => None,
			};
			let message = match enabled {
				Some(false) => "available; accessibility is switched off for now and Ash switches it on the first time it reads a window".to_string(),
				_ if count == 0 => access_hint(),
				_ => format!("{count} applications are publishing their controls."),
			};
			AccessState { available: count > 0 || enabled == Some(false), bus_address: true, applications: count, message }
		}
		Err(error) => AccessState { available: false, bus_address: false, applications: 0, message: error },
	}
}
