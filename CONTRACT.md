# Ash Agent: frozen frontend/backend contract

This file is the single source of truth for the boundary between the Rust backend
(`src/`) and the UI (`ui/`). Both sides are written against it. Do not change a name
here without changing both sides.

Transport: Tauri v2 IPC. `withGlobalTauri` is `true`, so the frontend reaches the
backend through `window.__TAURI__.core.invoke` and `window.__TAURI__.event.listen`.
There is no bundler and no npm. `ui/` is plain static files served from disk.

## Window

- One window, label `main`. Built programmatically in `src/main.rs` (the config ships
  `app.windows: []`). Default size comes from `Settings.window` (1180x780). The window
  is created hidden and shown once the frontend signals `ui_ready`, to avoid a white flash.

## Commands

Every command returns `Result<T, String>`. Errors are already human-readable sentences
written for a non-technical user; the UI shows them verbatim in a toast or an inline
banner. The UI never invents its own wording for a backend failure.

### Settings and secrets

- `settings_get() -> Settings`
- `settings_set(settings: Settings) -> Settings`: validates, persists, returns the
  normalized copy the UI should adopt.
- `secret_status(provider: String) -> SecretStatus`: `{ hasKey: bool, backend: String }`
  where `backend` is `"wallet"`, `"file"`, or `""`.
- `secret_set(provider: String, value: String) -> SecretStatus`
- `secret_delete(provider: String) -> SecretStatus`
- `legacy_import() -> Option<LegacyImport>`: `{ provider: Option<String>, hasKey: bool }`.
  Present only when the old Python agent's `config.json` is on disk.

### Theme

- `theme_get() -> ThemeState`: `{ scheme: "light"|"dark", accent: Option<String>, source: String }`.
- `theme_watch(enable: bool) -> ()`: start/stop forwarding KDE portal theme changes.
- Event `theme://change` with the same `ThemeState` payload.

### Models and providers

- `models_available(provider: String) -> Vec<ModelInfo>`: for `ml5` this is the daemon's
  local list; for `opencore` and custom endpoints it is the remote `GET /v1/models` list.
- `model_health(provider: String) -> BackendHealth`: `{ online, latencyMs, message }`.
- `model_favorites() -> Vec<Suggestion>`: the featured/advanced catalog from
  `data/ml5-models.json`, for the "Get models" screen.

### ML5 daemon

- `ml5_status(force: bool) -> DaemonState`
- `ml5_start() -> String`
- `ml5_stop() -> String`
- `ml5_load(model: String) -> ()`
- `ml5_unload(model: String) -> ()`
- `ml5_rename(model: String, newName: String) -> ()`
- `ml5_rescan() -> ()`
- `ml5_delete(model: String) -> ()`
- `ml5_pull(reference: String, registry: String) -> String`: returns a `pullId`;
  progress arrives as events (below). Registry is `hf`, `core`, or `routes`.
- `ml5_pull_cancel(id: String) -> ()`
- Events: `ml5://pull` carrying `PullEvent`, and `ml5://status` carrying `DaemonState`
  on a 2 s poll while the Models panel is open.

### Conversations

- `conversations_list(query: String) -> Vec<Conversation>`
- `conversations_create(mode: String, provider: String, model: String) -> Conversation`
- `conversations_get(id: String) -> Conversation`
- `conversations_update(conversation: Conversation) -> ()`
- `conversations_delete(id: String) -> ()`
- `conversations_delete_all() -> ()`
- `conversations_export(id: String, format: String) -> String`: `format` is `md` or `json`;
  returns the text to save. Saving uses the `dialog:allow-save` permission from the frontend.
- `messages_list(conversationId: String) -> Vec<Message>`: the active branch only, in
  order. Messages form a tree through `parentId`; each returned message carries
  `siblings` (ids of every version at that point, oldest first, including itself).
- `message_select(conversationId: String, messageId: String) -> Vec<Message>`: makes that
  version active and returns the new active branch.
- `message_revise(conversationId: String, messageId: String, content: String) -> Vec<Message>`:
  saves an edited copy of an assistant reply as a new version beside it.
- `message_delete(conversationId: String, messageId: String, scope: String) -> ()`:
  `scope` is `only` (this message and its tool results; later messages are re-attached),
  `pair` (the whole exchange from the user message; later messages stay), or `after`
  (this message and everything below it). When the target has other versions, `only`
  and `pair` remove that version's branch and the other versions remain.
- `message_update(message: Message) -> ()`

### Chat

- `chat_send(request: ChatSend) -> String`: returns a `turnId` immediately. Tokens
  arrive as events. `ChatSend` is
  `{ conversationId, provider, model, content, images: Vec<String>, think: Option<bool> }`
  where each image is a `data:` URL. If `conversationId` is empty the conversation is
  created first.
- `chat_stop(turnId: String) -> ()`
- `chat_retry(conversationId: String) -> String`: answers the last user message again as
  a new version; nothing is deleted.
- `chat_regenerate(conversationId: String, messageId: String, think: Option<bool>) -> String`:
  for a user message, adds a new answer version to it; for an assistant reply, writes a
  new version of the answer to the user message it belongs to.
- `chat_edit(conversationId: String, messageId: String, content: String, think: Option<bool>) -> String`:
  adds the edited user message as a new version beside the original (same pictures) and
  answers it.
- `chat_title(conversationId: String) -> String`: generates a title with the active model.
- Events during a turn, all tagged with `turnId`:
  - `chat://delta`: `{ turnId, messageId, content: String, thinking: String }`
  - `chat://status`: `{ turnId, text: String }` (reasoning/status line, incl. OpenCORe `<sbegin>`)
  - `chat://tool_calls`: `{ turnId, calls: Vec<WireToolCall> }`
  - `chat://usage`: `{ turnId, usage: Usage }`
  - `chat://done`: `{ turnId, messageId, finishReason, stopped: bool }`
  - `chat://error`: `{ turnId, message: String, retryAfter: Option<u64> }`

The backend persists every delta into `store`, so a reload of the conversation from the
database always matches what was streamed.

### Helper mode / tools

- `tool_catalog() -> Vec<ToolSpec>`: `{ name, description, group, tier, risk, root }`.
  The backend only advertises tools the current tier allows.
- `tool_approve(actionId: String, allow: bool, remember: bool) -> ()`: answers a
  pending confirmation card.
- `tool_cancel(actionId: String) -> ()`
- `tools_set_tier(tier: Tier) -> Settings`
- `computer_status() -> serde_json::Value`: session/desktop/memory/disk summary.
- `actions_list(limit: i64, before: Option<i64>) -> Vec<Action>`
- `actions_for_conversation(conversationId: String) -> Vec<Action>`
- `actions_clear() -> ()`
- `action_image(id: String) -> Option<String>`: screenshot as a `data:` URL.
- `guided_fixes() -> Vec<GuidedFix>`: `{ id, title, description, steps: Vec<String> }`.
- `guided_fix_run(id: String) -> String`: returns an `actionId`; runs through the
  normal approval path.
- Events: `tool://action` carrying `Action` on every state change (pending, awaiting,
  running, done, failed, denied, cancelled), and `tool://log` for live process output.

### Shell sessions (interactive)

- `session_start(shell: Option<String>, root: bool) -> String`: returns a `sessionId`.
- `session_write(id: String, data: String) -> ()`
- `session_resize(id: String, cols: u16, rows: u16) -> ()`
- `session_stop(id: String) -> ()`
- `sessions_list() -> Vec<SessionInfo>`: `{ id, shell, root, cwd, alive }`.
- Event `session://output` carrying `{ sessionId, data: String, stream: "out"|"err" }`.

### Clipboard and screenshots from the UI

- `clipboard_read() -> String`
- `clipboard_write(text: String) -> ()`

## Types shared with the frontend

All of these serialize with `camelCase` field names and are declared in `src/util.rs`,
`src/settings.rs`, `src/store.rs`, `src/providers/mod.rs`, and `src/ml5.rs`. The UI must
not redefine them; it consumes the JSON as produced.

`Settings`, `Tier` (`"observe" | "ask" | "trusted"`), `CustomEndpoint`, `Conversation`,
`Message`, `Action`, `ModelInfo`, `Usage`, `BackendHealth`, `DaemonState`, `LoadedModel`,
`PullProgress`, `PullEvent`, `Memory`, `Model`, `Suggestion`, `WireToolCall`,
`ThemeState`.

## Frontend files

- `ui/index.html`: the single page.
- `ui/app.js`: the application. Plain ES modules, no framework, no build step.
- `ui/styles.css`: the stylesheet.
- `ui/logo.png`: already present; the app icon and the empty-state mark.

The CSP in `tauri.conf.json` forbids inline scripts and remote resources:
`script-src 'self'`, `style-src 'self' 'unsafe-inline'`, `img-src 'self' data: blob: ash:`.
Everything must be local. Inline `onclick=` attributes are not allowed; bind listeners in JS.

## Markdown

Assistant message bodies are rendered by the backend, not the frontend:
`markdown_render(text: String) -> String` returns sanitized HTML with syntax-highlighted
code blocks already applied, using the active theme. The UI injects it with
`innerHTML`. Code blocks arrive as
`<div class="code-block" data-language="…"><div class="code-head">…<button class="code-copy" data-code-copy="copy">Copy</button></div><pre class="code">…</pre></div>`,
so the UI only has to delegate clicks on `[data-code-copy]`.
