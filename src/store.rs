use crate::util::{new_id, now_ms};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS conversations (
	id TEXT PRIMARY KEY,
	title TEXT NOT NULL,
	created INTEGER NOT NULL,
	updated INTEGER NOT NULL,
	pinned INTEGER NOT NULL DEFAULT 0,
	mode TEXT NOT NULL DEFAULT 'chat',
	provider TEXT NOT NULL DEFAULT '',
	model TEXT NOT NULL DEFAULT '',
	think INTEGER NOT NULL DEFAULT 1,
	titled INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS messages (
	id TEXT PRIMARY KEY,
	conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
	seq INTEGER NOT NULL,
	role TEXT NOT NULL,
	content TEXT NOT NULL DEFAULT '',
	thinking TEXT NOT NULL DEFAULT '',
	tool_calls TEXT NOT NULL DEFAULT '[]',
	tool_call_id TEXT,
	images TEXT NOT NULL DEFAULT '[]',
	meta TEXT NOT NULL DEFAULT '{}',
	created INTEGER NOT NULL,
	parent_id TEXT,
	chosen INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS messages_conv ON messages(conversation_id, seq);
CREATE TABLE IF NOT EXISTS images (
	id TEXT PRIMARY KEY,
	conversation_id TEXT,
	mime TEXT NOT NULL,
	data BLOB NOT NULL,
	width INTEGER NOT NULL,
	height INTEGER NOT NULL,
	created INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS images_conv ON images(conversation_id);
CREATE TABLE IF NOT EXISTS actions (
	id TEXT PRIMARY KEY,
	conversation_id TEXT,
	message_id TEXT,
	tool_call_id TEXT,
	tool TEXT NOT NULL,
	title TEXT NOT NULL,
	explanation TEXT NOT NULL DEFAULT '',
	detail TEXT NOT NULL DEFAULT '',
	args TEXT NOT NULL DEFAULT '{}',
	risk TEXT NOT NULL,
	root INTEGER NOT NULL DEFAULT 0,
	status TEXT NOT NULL,
	output TEXT NOT NULL DEFAULT '',
	image_id TEXT,
	exit_code INTEGER,
	created INTEGER NOT NULL,
	finished INTEGER
);
CREATE INDEX IF NOT EXISTS actions_conv ON actions(conversation_id, created);
CREATE INDEX IF NOT EXISTS actions_created ON actions(created);
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(content, message_id UNINDEXED, conversation_id UNINDEXED, tokenize = 'unicode61 remove_diacritics 2');
CREATE TRIGGER IF NOT EXISTS messages_fts_insert AFTER INSERT ON messages BEGIN
	INSERT INTO messages_fts(content, message_id, conversation_id) SELECT new.content, new.id, new.conversation_id WHERE new.role IN ('user', 'assistant');
END;
CREATE TRIGGER IF NOT EXISTS messages_fts_update AFTER UPDATE OF content ON messages BEGIN
	DELETE FROM messages_fts WHERE message_id = old.id;
	INSERT INTO messages_fts(content, message_id, conversation_id) SELECT new.content, new.id, new.conversation_id WHERE new.role IN ('user', 'assistant');
END;
CREATE TRIGGER IF NOT EXISTS messages_fts_delete AFTER DELETE ON messages BEGIN
	DELETE FROM messages_fts WHERE message_id = old.id;
END;
CREATE TRIGGER IF NOT EXISTS conversations_images_delete AFTER DELETE ON conversations BEGIN
	DELETE FROM images WHERE conversation_id = old.id;
	UPDATE actions SET conversation_id = NULL, message_id = NULL WHERE conversation_id = old.id;
END;
";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
	pub id: String,
	pub title: String,
	pub created: i64,
	pub updated: i64,
	pub pinned: bool,
	pub mode: String,
	pub provider: String,
	pub model: String,
	pub think: bool,
	pub titled: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub snippet: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoredToolCall {
	pub id: String,
	pub name: String,
	pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
	pub id: String,
	pub conversation_id: String,
	pub seq: i64,
	pub role: String,
	pub content: String,
	pub thinking: String,
	pub tool_calls: Vec<StoredToolCall>,
	pub tool_call_id: Option<String>,
	pub images: Vec<String>,
	pub meta: Value,
	pub created: i64,
	#[serde(default)]
	pub parent_id: Option<String>,
	#[serde(default)]
	pub siblings: Vec<String>,
	#[serde(skip)]
	pub chosen: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteScope {
	Only,
	Pair,
	After,
}

impl DeleteScope {
	pub fn parse(value: &str) -> Option<DeleteScope> {
		match value {
			"only" => Some(DeleteScope::Only),
			"pair" => Some(DeleteScope::Pair),
			"after" => Some(DeleteScope::After),
			_ => None,
		}
	}
}

struct Tree {
	nodes: HashMap<String, Message>,
	children: HashMap<Option<String>, Vec<String>>,
}

impl Tree {
	fn build(all: Vec<Message>) -> Tree {
		let known: HashSet<String> = all.iter().map(|message| message.id.clone()).collect();
		let mut children: HashMap<Option<String>, Vec<String>> = HashMap::new();
		let mut nodes = HashMap::with_capacity(all.len());
		for mut message in all {
			if message.parent_id.as_ref().is_some_and(|parent| !known.contains(parent)) {
				message.parent_id = None;
			}
			children.entry(message.parent_id.clone()).or_default().push(message.id.clone());
			nodes.insert(message.id.clone(), message);
		}
		Tree { nodes, children }
	}

	fn kids(&self, parent: &Option<String>) -> &[String] {
		self.children.get(parent).map(Vec::as_slice).unwrap_or(&[])
	}

	fn pick(&self, kids: &[String]) -> Option<String> {
		kids.iter().filter_map(|id| self.nodes.get(id)).max_by_key(|message| (message.chosen, message.seq)).map(|message| message.id.clone())
	}

	fn walk_from(&self, start: Option<String>) -> Vec<Message> {
		let mut out = Vec::new();
		let mut key = start;
		while out.len() < self.nodes.len() {
			let kids = self.kids(&key);
			let Some(next) = self.pick(kids) else { break };
			let mut message = self.nodes[&next].clone();
			message.siblings = kids.to_vec();
			out.push(message);
			key = Some(next);
		}
		out
	}

	fn lineage(&self, id: &str) -> Vec<Message> {
		let mut out = Vec::new();
		let mut cursor = self.nodes.get(id);
		while let Some(message) = cursor {
			if out.len() > self.nodes.len() {
				break;
			}
			out.push(message.clone());
			cursor = message.parent_id.as_ref().and_then(|parent| self.nodes.get(parent));
		}
		out.reverse();
		out
	}

	fn subtree(&self, id: &str) -> HashSet<String> {
		let mut found = HashSet::new();
		let mut stack = vec![id.to_string()];
		while let Some(current) = stack.pop() {
			if !found.insert(current.clone()) {
				continue;
			}
			stack.extend(self.kids(&Some(current)).iter().cloned());
		}
		found
	}

	fn with_tool_results(&self, id: &str) -> HashSet<String> {
		let mut found = HashSet::new();
		let mut stack = vec![id.to_string()];
		while let Some(current) = stack.pop() {
			if !found.insert(current.clone()) {
				continue;
			}
			stack.extend(self.kids(&Some(current)).iter().filter(|child| self.nodes.get(*child).is_some_and(|message| message.role == "tool")).cloned());
		}
		found
	}

	fn exchange_start(&self, id: &str) -> String {
		let mut current = id.to_string();
		for _ in 0..=self.nodes.len() {
			let Some(message) = self.nodes.get(&current) else { break };
			if message.role == "user" {
				break;
			}
			match message.parent_id.as_ref().filter(|parent| self.nodes.contains_key(*parent)) {
				Some(parent) => current = parent.clone(),
				None => break,
			}
		}
		current
	}

	fn removal(&self, id: &str, scope: DeleteScope) -> (HashSet<String>, Vec<(String, Option<String>)>) {
		let reattach_children = |removed: &HashSet<String>, parent: Option<String>| -> Vec<(String, Option<String>)> {
			removed
				.iter()
				.flat_map(|gone| self.kids(&Some(gone.clone())).iter())
				.filter(|child| !removed.contains(*child))
				.map(|child| (child.clone(), parent.clone()))
				.collect()
		};
		let has_versions = |id: &str| self.kids(&self.nodes[id].parent_id).len() > 1;
		match scope {
			DeleteScope::After => (self.subtree(id), Vec::new()),
			DeleteScope::Only if has_versions(id) => (self.subtree(id), Vec::new()),
			DeleteScope::Only => {
				let removed = self.with_tool_results(id);
				let parent = self.nodes[id].parent_id.clone();
				let moves = reattach_children(&removed, parent);
				(removed, moves)
			}
			DeleteScope::Pair => {
				let start = self.exchange_start(id);
				if has_versions(&start) {
					return (self.subtree(&start), Vec::new());
				}
				let parent = self.nodes[&start].parent_id.clone();
				let mut removed = self.subtree(&start);
				let path = self.walk_from(Some(start.clone()));
				let Some(position) = path.iter().position(|message| message.role == "user") else { return (removed, Vec::new()) };
				let last = if position == 0 { start.clone() } else { path[position - 1].id.clone() };
				let mut moves = Vec::new();
				for next in self.kids(&Some(last)).iter().filter(|child| self.nodes[*child].role == "user") {
					for kept in self.subtree(next) {
						removed.remove(&kept);
					}
					moves.push((next.clone(), parent.clone()));
				}
				(removed, moves)
			}
		}
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Action {
	pub id: String,
	pub conversation_id: Option<String>,
	pub message_id: Option<String>,
	pub tool_call_id: Option<String>,
	pub tool: String,
	pub title: String,
	pub explanation: String,
	pub detail: String,
	pub args: Value,
	pub risk: String,
	pub root: bool,
	pub status: String,
	pub output: String,
	pub image_id: Option<String>,
	pub exit_code: Option<i64>,
	pub created: i64,
	pub finished: Option<i64>,
}

pub struct ImageRecord {
	pub mime: String,
	pub data: Vec<u8>,
	pub width: u32,
	pub height: u32,
}

pub struct Store {
	conn: Mutex<Connection>,
}

fn conversation_from(row: &Row) -> rusqlite::Result<Conversation> {
	Ok(Conversation {
		id: row.get(0)?,
		title: row.get(1)?,
		created: row.get(2)?,
		updated: row.get(3)?,
		pinned: row.get::<_, i64>(4)? != 0,
		mode: row.get(5)?,
		provider: row.get(6)?,
		model: row.get(7)?,
		think: row.get::<_, i64>(8)? != 0,
		titled: row.get::<_, i64>(9)? != 0,
		snippet: None,
	})
}

const CONVERSATION_COLUMNS: &str = "id, title, created, updated, pinned, mode, provider, model, think, titled";
const MESSAGE_COLUMNS: &str = "id, conversation_id, seq, role, content, thinking, tool_calls, tool_call_id, images, meta, created, parent_id, chosen";
const ACTION_COLUMNS: &str = "id, conversation_id, message_id, tool_call_id, tool, title, explanation, detail, args, risk, root, status, output, image_id, exit_code, created, finished";

fn message_from(row: &Row) -> rusqlite::Result<Message> {
	let tool_calls: String = row.get(6)?;
	let images: String = row.get(8)?;
	let meta: String = row.get(9)?;
	Ok(Message {
		id: row.get(0)?,
		conversation_id: row.get(1)?,
		seq: row.get(2)?,
		role: row.get(3)?,
		content: row.get(4)?,
		thinking: row.get(5)?,
		tool_calls: serde_json::from_str(&tool_calls).unwrap_or_default(),
		tool_call_id: row.get(7)?,
		images: serde_json::from_str(&images).unwrap_or_default(),
		meta: serde_json::from_str(&meta).unwrap_or(Value::Object(Default::default())),
		created: row.get(10)?,
		parent_id: row.get(11)?,
		siblings: Vec::new(),
		chosen: row.get(12)?,
	})
}

fn action_from(row: &Row) -> rusqlite::Result<Action> {
	let args: String = row.get(8)?;
	Ok(Action {
		id: row.get(0)?,
		conversation_id: row.get(1)?,
		message_id: row.get(2)?,
		tool_call_id: row.get(3)?,
		tool: row.get(4)?,
		title: row.get(5)?,
		explanation: row.get(6)?,
		detail: row.get(7)?,
		args: serde_json::from_str(&args).unwrap_or(Value::Null),
		risk: row.get(9)?,
		root: row.get::<_, i64>(10)? != 0,
		status: row.get(11)?,
		output: row.get(12)?,
		image_id: row.get(13)?,
		exit_code: row.get(14)?,
		created: row.get(15)?,
		finished: row.get(16)?,
	})
}

fn fts_query(input: &str) -> Option<String> {
	let terms: Vec<String> = input
		.split_whitespace()
		.map(|t| t.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '.').collect::<String>())
		.filter(|t| !t.is_empty())
		.map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
		.collect();
	(!terms.is_empty()).then(|| terms.join(" "))
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
	let has_parent = conn.prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name = 'parent_id'")?.exists([])?;
	if !has_parent {
		let tx = conn.unchecked_transaction()?;
		tx.execute_batch(
			"ALTER TABLE messages ADD COLUMN parent_id TEXT;
			ALTER TABLE messages ADD COLUMN chosen INTEGER NOT NULL DEFAULT 0;
			UPDATE messages SET parent_id = (SELECT earlier.id FROM messages earlier WHERE earlier.conversation_id = messages.conversation_id AND earlier.seq < messages.seq ORDER BY earlier.seq DESC LIMIT 1), chosen = seq;",
		)?;
		tx.commit()?;
	}
	conn.execute_batch("CREATE INDEX IF NOT EXISTS messages_parent ON messages(conversation_id, parent_id);")
}

fn err(e: rusqlite::Error) -> String {
	format!("History database error: {e}")
}

impl Store {
	pub fn open(path: &Path) -> Result<Store, String> {
		if let Some(dir) = path.parent() {
			crate::paths::ensure_private_dir(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
		}
		let conn = Connection::open(path).map_err(err)?;
		conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(err)?;
		conn.execute_batch(SCHEMA).map_err(err)?;
		migrate(&conn).map_err(err)?;
		let _ = std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600));
		let day_ago = now_ms() - 86_400_000;
		conn.execute("DELETE FROM images WHERE conversation_id IS NULL AND created < ?1", params![day_ago]).map_err(err)?;
		Ok(Store { conn: Mutex::new(conn) })
	}

	fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
		let conn = self.conn.lock().map_err(|_| "History database is unavailable".to_string())?;
		f(&conn).map_err(err)
	}

	pub fn create_conversation(&self, mode: &str, provider: &str, model: &str, think: bool) -> Result<Conversation, String> {
		let now = now_ms();
		let conv = Conversation {
			id: new_id(),
			title: "New chat".into(),
			created: now,
			updated: now,
			pinned: false,
			mode: mode.into(),
			provider: provider.into(),
			model: model.into(),
			think,
			titled: false,
			snippet: None,
		};
		self.with(|c| {
			c.execute(
				"INSERT INTO conversations (id, title, created, updated, pinned, mode, provider, model, think, titled) VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, ?8, 0)",
				params![conv.id, conv.title, conv.created, conv.updated, conv.mode, conv.provider, conv.model, conv.think as i64],
			)
		})?;
		Ok(conv)
	}

	pub fn get_conversation(&self, id: &str) -> Result<Conversation, String> {
		self.with(|c| {
			c.query_row(&format!("SELECT {CONVERSATION_COLUMNS} FROM conversations WHERE id = ?1"), params![id], conversation_from).optional()
		})?
		.ok_or_else(|| "This conversation no longer exists.".to_string())
	}

	pub fn list_conversations(&self, query: &str) -> Result<Vec<Conversation>, String> {
		let query = query.trim();
		self.with(|c| {
			if query.is_empty() {
				let mut stmt = c.prepare(&format!("SELECT {CONVERSATION_COLUMNS} FROM conversations ORDER BY pinned DESC, updated DESC LIMIT 2000"))?;
				let rows = stmt.query_map([], conversation_from)?;
				return rows.collect();
			}
			let like = format!("%{}%", query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
			let mut found: Vec<Conversation> = {
				let mut stmt = c.prepare(&format!("SELECT {CONVERSATION_COLUMNS} FROM conversations WHERE title LIKE ?1 ESCAPE '\\' ORDER BY updated DESC LIMIT 500"))?;
				let rows = stmt.query_map(params![like], conversation_from)?;
				rows.collect::<rusqlite::Result<Vec<_>>>()?
			};
			if let Some(fts) = fts_query(query) {
				let mut stmt = c.prepare(
					"SELECT conversation_id, snippet(messages_fts, 0, char(1), char(2), '…', 12) FROM messages_fts WHERE messages_fts MATCH ?1 ORDER BY rank LIMIT 400",
				)?;
				let hits = stmt.query_map(params![fts], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
				let mut seen: std::collections::HashSet<String> = found.iter().map(|f| f.id.clone()).collect();
				for hit in hits.flatten() {
					if let Some(existing) = found.iter_mut().find(|f| f.id == hit.0) {
						if existing.snippet.is_none() {
							existing.snippet = Some(hit.1);
						}
						continue;
					}
					if !seen.insert(hit.0.clone()) {
						continue;
					}
					let conv = c
						.query_row(&format!("SELECT {CONVERSATION_COLUMNS} FROM conversations WHERE id = ?1"), params![hit.0], conversation_from)
						.optional()?;
					if let Some(mut conv) = conv {
						conv.snippet = Some(hit.1);
						found.push(conv);
					}
				}
			}
			found.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.updated.cmp(&a.updated)));
			Ok(found)
		})
	}

	pub fn update_conversation(&self, conv: &Conversation) -> Result<(), String> {
		self.with(|c| {
			c.execute(
				"UPDATE conversations SET title = ?2, pinned = ?3, mode = ?4, provider = ?5, model = ?6, think = ?7, titled = ?8 WHERE id = ?1",
				params![conv.id, conv.title, conv.pinned as i64, conv.mode, conv.provider, conv.model, conv.think as i64, conv.titled as i64],
			)
		})?;
		Ok(())
	}

	pub fn touch_conversation(&self, id: &str) -> Result<i64, String> {
		let now = now_ms();
		self.with(|c| c.execute("UPDATE conversations SET updated = ?2 WHERE id = ?1", params![id, now]))?;
		Ok(now)
	}

	pub fn delete_conversation(&self, id: &str) -> Result<(), String> {
		self.with(|c| c.execute("DELETE FROM conversations WHERE id = ?1", params![id]))?;
		Ok(())
	}

	pub fn delete_all_conversations(&self) -> Result<(), String> {
		self.with(|c| c.execute_batch("DELETE FROM conversations; DELETE FROM images; DELETE FROM messages_fts;"))?;
		Ok(())
	}

	#[allow(clippy::too_many_arguments)]
	pub fn add_message(&self, conversation_id: &str, parent_id: Option<&str>, role: &str, content: &str, images: &[String], tool_call_id: Option<&str>, meta: Value) -> Result<Message, String> {
		let id = new_id();
		let now = now_ms();
		let images_json = serde_json::to_string(images).unwrap_or_else(|_| "[]".into());
		let meta_json = meta.to_string();
		self.with(|c| {
			let (seq, chosen): (i64, i64) = c.query_row(
				"SELECT COALESCE(MAX(seq), 0) + 1, COALESCE(MAX(chosen), 0) + 1 FROM messages WHERE conversation_id = ?1",
				params![conversation_id],
				|r| Ok((r.get(0)?, r.get(1)?)),
			)?;
			c.execute(
				"INSERT INTO messages (id, conversation_id, seq, role, content, images, tool_call_id, meta, created, parent_id, chosen) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
				params![id, conversation_id, seq, role, content, images_json, tool_call_id, meta_json, now, parent_id, chosen],
			)?;
			for image in images {
				c.execute("UPDATE images SET conversation_id = ?2 WHERE id = ?1", params![image, conversation_id])?;
			}
			c.execute("UPDATE conversations SET updated = ?2 WHERE id = ?1", params![conversation_id, now])?;
			c.query_row(&format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE id = ?1"), params![id], message_from)
		})
	}

	pub fn update_message(&self, msg: &Message) -> Result<(), String> {
		let tool_calls = serde_json::to_string(&msg.tool_calls).unwrap_or_else(|_| "[]".into());
		let images = serde_json::to_string(&msg.images).unwrap_or_else(|_| "[]".into());
		self.with(|c| {
			c.execute(
				"UPDATE messages SET content = ?2, thinking = ?3, tool_calls = ?4, images = ?5, meta = ?6 WHERE id = ?1",
				params![msg.id, msg.content, msg.thinking, tool_calls, images, msg.meta.to_string()],
			)?;
			for image in &msg.images {
				c.execute("UPDATE images SET conversation_id = ?2 WHERE id = ?1", params![image, msg.conversation_id])?;
			}
			Ok(())
		})
	}

	pub fn get_message(&self, id: &str) -> Result<Message, String> {
		self.with(|c| c.query_row(&format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE id = ?1"), params![id], message_from).optional())?
			.ok_or_else(|| "That message no longer exists.".to_string())
	}

	pub fn all_messages(&self, conversation_id: &str) -> Result<Vec<Message>, String> {
		self.with(|c| {
			let mut stmt = c.prepare(&format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE conversation_id = ?1 ORDER BY seq"))?;
			let rows = stmt.query_map(params![conversation_id], message_from)?;
			rows.collect()
		})
	}

	fn tree(&self, conversation_id: &str) -> Result<Tree, String> {
		Ok(Tree::build(self.all_messages(conversation_id)?))
	}

	pub fn messages(&self, conversation_id: &str) -> Result<Vec<Message>, String> {
		Ok(self.tree(conversation_id)?.walk_from(None))
	}

	pub fn lineage(&self, conversation_id: &str, message_id: &str) -> Result<Vec<Message>, String> {
		let tree = self.tree(conversation_id)?;
		if !tree.nodes.contains_key(message_id) {
			return Err("That message no longer exists.".to_string());
		}
		Ok(tree.lineage(message_id))
	}

	pub fn select_message(&self, conversation_id: &str, message_id: &str) -> Result<(), String> {
		let changed = self.with(|c| {
			c.execute(
				"UPDATE messages SET chosen = (SELECT COALESCE(MAX(chosen), 0) + 1 FROM messages WHERE conversation_id = ?1) WHERE id = ?2 AND conversation_id = ?1",
				params![conversation_id, message_id],
			)
		})?;
		if changed == 0 {
			return Err("That version no longer exists.".to_string());
		}
		Ok(())
	}

	pub fn delete_message(&self, conversation_id: &str, message_id: &str, scope: DeleteScope) -> Result<(), String> {
		let tree = self.tree(conversation_id)?;
		if !tree.nodes.contains_key(message_id) {
			return Err("That message no longer exists.".to_string());
		}
		let (removed, moves) = tree.removal(message_id, scope);
		self.remove(&removed, &moves)
	}

	pub fn delete_subtree(&self, conversation_id: &str, message_id: &str) -> Result<(), String> {
		let tree = self.tree(conversation_id)?;
		if !tree.nodes.contains_key(message_id) {
			return Ok(());
		}
		self.remove(&tree.subtree(message_id), &[])
	}

	fn remove(&self, removed: &HashSet<String>, moves: &[(String, Option<String>)]) -> Result<(), String> {
		self.with(|c| {
			let tx = c.unchecked_transaction()?;
			for (child, parent) in moves {
				tx.execute("UPDATE messages SET parent_id = ?2 WHERE id = ?1", params![child, parent])?;
			}
			for id in removed {
				tx.execute("UPDATE actions SET message_id = NULL WHERE message_id = ?1", params![id])?;
				tx.execute("DELETE FROM messages WHERE id = ?1", params![id])?;
			}
			tx.commit()
		})
	}

	pub fn add_image(&self, conversation_id: Option<&str>, mime: &str, data: &[u8], width: u32, height: u32) -> Result<String, String> {
		let id = new_id();
		self.with(|c| {
			c.execute(
				"INSERT INTO images (id, conversation_id, mime, data, width, height, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
				params![id, conversation_id, mime, data, width, height, now_ms()],
			)
		})?;
		Ok(id)
	}

	pub fn image(&self, id: &str) -> Result<Option<ImageRecord>, String> {
		self.with(|c| {
			c.query_row("SELECT mime, data, width, height FROM images WHERE id = ?1", params![id], |r| {
				Ok(ImageRecord { mime: r.get(0)?, data: r.get(1)?, width: r.get(2)?, height: r.get(3)? })
			})
			.optional()
		})
	}

	pub fn insert_action(&self, a: &Action) -> Result<(), String> {
		self.with(|c| {
			c.execute(
				&format!("INSERT INTO actions ({ACTION_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)"),
				params![
					a.id,
					a.conversation_id,
					a.message_id,
					a.tool_call_id,
					a.tool,
					a.title,
					a.explanation,
					a.detail,
					a.args.to_string(),
					a.risk,
					a.root as i64,
					a.status,
					a.output,
					a.image_id,
					a.exit_code,
					a.created,
					a.finished
				],
			)
		})?;
		Ok(())
	}

	pub fn update_action(&self, a: &Action) -> Result<(), String> {
		self.with(|c| {
			c.execute(
				"UPDATE actions SET status = ?2, output = ?3, image_id = ?4, exit_code = ?5, finished = ?6, risk = ?7, root = ?8, detail = ?9 WHERE id = ?1",
				params![a.id, a.status, a.output, a.image_id, a.exit_code, a.finished, a.risk, a.root as i64, a.detail],
			)
		})?;
		Ok(())
	}

	pub fn actions_for_conversation(&self, conversation_id: &str) -> Result<Vec<Action>, String> {
		self.with(|c| {
			let mut stmt = c.prepare(&format!("SELECT {ACTION_COLUMNS} FROM actions WHERE conversation_id = ?1 ORDER BY created"))?;
			let rows = stmt.query_map(params![conversation_id], action_from)?;
			rows.collect()
		})
	}

	pub fn list_actions(&self, limit: i64, before: Option<i64>) -> Result<Vec<Action>, String> {
		self.with(|c| {
			let mut stmt = c.prepare(&format!("SELECT {ACTION_COLUMNS} FROM actions WHERE created < ?1 ORDER BY created DESC LIMIT ?2"))?;
			let rows = stmt.query_map(params![before.unwrap_or(i64::MAX), limit], action_from)?;
			rows.collect()
		})
	}

	pub fn get_action(&self, id: &str) -> Result<Option<Action>, String> {
		self.with(|c| c.query_row(&format!("SELECT {ACTION_COLUMNS} FROM actions WHERE id = ?1"), params![id], action_from).optional())
	}

	pub fn interrupt_unfinished_actions(&self) -> Result<usize, String> {
		self.with(|c| {
			c.execute(
				"UPDATE actions SET status = 'stopped', finished = ?1, output = CASE WHEN output = '' THEN 'Ash Agent was closed before this action finished.' ELSE output END WHERE status IN ('pending', 'awaiting', 'running')",
				params![now_ms()],
			)
		})
	}

	pub fn clear_actions(&self) -> Result<(), String> {
		self.with(|c| c.execute("DELETE FROM actions WHERE status NOT IN ('pending', 'awaiting', 'running')", []))?;
		Ok(())
	}
}
