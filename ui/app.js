const globalTauri = window.__TAURI__ || {};
const coreApi = globalTauri.core || {};
const eventApi = globalTauri.event || {};

const invoke = (command, args) => {
	const call = coreApi.invoke;
	if (typeof call !== "function") return Promise.reject(new Error("Ash cannot reach the desktop backend from this window."));
	return call(command, args);
};

const listen = (event, handler) => {
	const subscribe = eventApi.listen;
	if (typeof subscribe !== "function") return Promise.reject(new Error("Ash cannot subscribe to desktop events from this window."));
	return subscribe(event, handler);
};

const VIEWS = ["chat", "models", "actions", "settings"];

const ASSISTANT_NAME = "Ash";

const TIERS = [
	{ id: "observe", label: "Observe", short: "Look only", blurb: "Ash can look at your computer (read status, files, screenshots and open windows), but it cannot change anything. Nothing moves unless you do it yourself." },
	{ id: "ask", label: "Ask", short: "Ask first", blurb: "Ash can change things, but every change is shown to you as a card you approve first. Nothing runs until you click. This is the recommended setting." },
	{ id: "trusted", label: "Trusted", short: "Fewer prompts", blurb: "Routine actions you have already approved run without stopping to ask. Administrator actions, disks and the bootloader still ask every time." }
];

const TIER_DEFAULT = "ask";

const RISK_LABELS = { low: "Low risk", medium: "Medium risk", high: "High risk", danger: "Dangerous" };


const ACTION_STATUS_LABELS = {
	pending: "Waiting to start",
	awaiting: "Needs your approval",
	running: "Running now",
	done: "Finished",
	failed: "Failed",
	denied: "You denied it",
	cancelled: "Cancelled",
	stopped: "Interrupted"
};

const MODE_LABELS = { chat: "Chat", helper: "Helper" };

const SUGGESTIONS = [
	{ title: "Explain something", text: "Explain, in plain words, how ", detail: "Assume I am new to this." },
	{ title: "Write something", text: "Write a ", detail: "Keep it friendly and short." },
	{ title: "Fix a problem", text: "Something on my computer is not working: ", detail: "Tell me what you notice." },
	{ title: "Understand a command", text: "What does this command do? ", detail: "Explain each part." },
	{ title: "Summarise a text", text: "Summarise this for me: ", detail: "Give me the key points." },
	{ title: "Learn a topic", text: "Teach me the basics of ", detail: "Start from the beginning." }
];

const GUIDED_ICONS = { audio: "sliders", display: "image", drivers: "cpu", printer: "list", updates: "download", apps: "download", wifi: "shield", network: "shield" };

const MAX_IMAGE_EDGE = 1568;

const MAX_IMAGE_BYTES = 1_400_000;

const IMAGE_TYPES = ["image/png", "image/jpeg", "image/webp", "image/gif", "image/avif", "image/bmp"];

const OUTPUT_CLAMP = 1600;

const TOOL_SCREENSHOT_NOTE = "The screenshot is attached in the next message.";

const STREAM_RENDER_MS = 80;

const DRAG_SCROLL_EDGE = 48;

const TERMINAL_FONT_FALLBACK = 8;

const state = {
	ready: false,
	view: "chat",
	sidebarCollapsed: false,
	appliedTheme: "dark",
	settings: null,
	conversations: [],
	searchQuery: "",
	conversation: null,
	messages: [],
	renderToken: 0,
	liveTurn: null,
	liveNode: null,
	autoScroll: true,
	attachments: [],
	think: false,
	models: {},
	modelErrors: {},
	health: {},
	daemon: null,
	favorites: { featured: [], advanced: [] },
	favoritesLoaded: false,
	pulls: [],
	toolCatalog: [],
	actions: [],
	actionImages: {},
	actionFilter: "all",
	answeredActionIds: {},
	guidedFixes: [],
	computerStatus: null,
	computerStatusError: "",
	sessions: [],
	activeSession: "",
	terminalHistory: [],
	terminalHistoryIndex: 0,
	terminalCols: 0,
	terminalRows: 0,
	terminalObserver: null,
	extras: "none",
	legacyOffered: false,
	pendingTitleIds: {},
};

const refs = {};

const $ = (id) => document.getElementById(id);

const el = (tag, className, text) => {
	const node = document.createElement(tag);
	if (className) node.className = className;
	if (text !== undefined && text !== null) node.textContent = String(text);
	return node;
};

const escapeText = (value) => {
	const node = document.createElement("span");
	node.textContent = value === undefined || value === null ? "" : String(value);
	return node.innerHTML;
};

const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

const asArray = (value) => (Array.isArray(value) ? value : []);

const isDataUrl = (value) => typeof value === "string" && value.startsWith("data:");

const clamp = (value, min, max) => Math.max(min, Math.min(max, value));

const numberOr = (value, fallback) => (Number.isFinite(Number(value)) ? Number(value) : fallback);

const newLocalId = () => `local-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;

const intlNumber = new Intl.NumberFormat();

const formatCount = (value) => intlNumber.format(numberOr(value, 0));

const formatBytes = (bytes) => {
	const value = numberOr(bytes, 0);
	if (value <= 0) return "0 B";
	const units = ["B", "KB", "MB", "GB", "TB", "PB"];
	let scaled = value;
	let index = 0;
	while (scaled >= 1024 && index < units.length - 1) {
		scaled /= 1024;
		index += 1;
	}
	return index === 0 ? `${Math.round(scaled)} B` : `${scaled.toFixed(1)} ${units[index]}`;
};

const formatDuration = (seconds) => {
	const total = Math.max(0, Math.round(numberOr(seconds, 0)));
	if (total < 60) return `${total} second${total === 1 ? "" : "s"}`;
	const minutes = Math.floor(total / 60);
	if (minutes < 60) return `${minutes} minute${minutes === 1 ? "" : "s"}`;
	const hours = Math.floor(minutes / 60);
	if (hours < 48) return `${hours} hour${hours === 1 ? "" : "s"} ${minutes % 60} min`;
	const days = Math.floor(hours / 24);
	return `${days} day${days === 1 ? "" : "s"} ${hours % 24} h`;
};

const formatShortDuration = (seconds) => {
	const total = Math.max(0, Math.round(numberOr(seconds, 0)));
	if (total < 60) return `${total}s`;
	const minutes = Math.floor(total / 60);
	if (minutes < 60) return `${minutes}m`;
	const hours = Math.floor(minutes / 60);
	if (hours < 48) return `${hours}h ${minutes % 60}m`;
	return `${Math.floor(hours / 24)}d`;
};

const formatClock = (milliseconds) => {
	const value = numberOr(milliseconds, 0);
	if (value <= 0) return "";
	return new Date(value).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
};

const formatDateTime = (milliseconds) => {
	const value = numberOr(milliseconds, 0);
	return value <= 0 ? "unknown time" : new Date(value).toLocaleString();
};

const relativeTime = (milliseconds) => {
	const value = numberOr(milliseconds, 0);
	if (value <= 0) return "";
	const seconds = Math.round((Date.now() - value) / 1000);
	if (seconds < 45) return "just now";
	if (seconds < 3600) return `${Math.max(1, Math.round(seconds / 60))} min ago`;
	if (seconds < 86400) return `${Math.round(seconds / 3600)} h ago`;
	if (seconds < 2592000) return `${Math.round(seconds / 86400)} d ago`;
	return new Date(value).toLocaleDateString();
};

const truncateText = (value, max) => {
	const text = value === undefined || value === null ? "" : String(value);
	return text.length <= max ? text : `${text.slice(0, Math.max(0, max - 1))}…`;
};

const prettyKey = (key) => String(key).replace(/([a-z0-9])([A-Z])/g, "$1 $2").replace(/^./, (c) => c.toUpperCase());

const show = (node, visible) => {
	if (node) node.classList.toggle("hidden", !visible);
};

const icon = (name) => {
	const span = el("span", "icon");
	span.dataset.icon = name;
	span.setAttribute("aria-hidden", "true");
	return span;
};

const button = (label, className, onClick, iconName) => {
	const node = el("button", className || "ghost-button");
	node.type = "button";
	if (iconName) node.appendChild(icon(iconName));
	if (label) node.appendChild(el("span", null, label));
	if (onClick) node.addEventListener("click", onClick);
	return node;
};

const iconButton = (label, iconName, onClick, className) => {
	const node = el("button", className || "icon-button");
	node.type = "button";
	node.setAttribute("aria-label", label);
	node.title = label;
	node.appendChild(icon(iconName));
	if (onClick) node.addEventListener("click", onClick);
	return node;
};

const flashLabel = (node, text) => {
	const target = node.querySelector("span:last-child") || node;
	const previous = target.textContent;
	target.textContent = text;
	setTimeout(() => {
		target.textContent = previous;
	}, 1400);
};

const warn = (message) => {
	const text = typeof message === "string" && message.trim() ? message.trim() : "Something went wrong.";
	const toast = el("div", "toast");
	toast.dataset.kind = "error";
	toast.setAttribute("role", "alert");
	toast.appendChild(icon("info"));
	toast.appendChild(el("div", "toast-text", text));
	toast.appendChild(iconButton("Dismiss this message", "close", () => toast.remove(), "icon-button tiny"));
	refs.toasts && refs.toasts.appendChild(toast);
	setTimeout(() => toast.remove(), text.length > 160 ? 18000 : 9000);
};

const notify = (message, kind) => {
	const toast = el("div", "toast");
	toast.dataset.kind = kind || "info";
	toast.appendChild(icon(kind === "success" ? "check" : "info"));
	toast.appendChild(el("div", "toast-text", message));
	toast.appendChild(iconButton("Dismiss this message", "close", () => toast.remove(), "icon-button tiny"));
	refs.toasts && refs.toasts.appendChild(toast);
	setTimeout(() => toast.remove(), 5200);
};

const guard = async (work, onError) => {
	try {
		return await work();
	} catch (error) {
		onError ? onError(error) : warn(error);
		return undefined;
	}
};

const jsonArgs = (value) => {
	if (value === undefined || value === null || value === "") return "";
	if (typeof value === "string") {
		try {
			return JSON.stringify(JSON.parse(value), null, 2);
		} catch (error) {
			return value;
		}
	}
	try {
		return JSON.stringify(value, null, 2);
	} catch (error) {
		return String(value);
	}
};

const settingsClone = () => JSON.parse(JSON.stringify(state.settings || {}));

const providerOptions = () => {
	const list = [
		{ id: "ml5", name: "ML5 (on this computer)" },
		{ id: "opencore", name: "OpenCORe (cloud service)" }
	];
	for (const endpoint of asArray(state.settings && state.settings.customEndpoints)) {
		list.push({ id: endpoint.id, name: `${endpoint.name || endpoint.baseUrl} (your own server)` });
	}
	return list;
};

const providerLabel = (id) => {
	const found = providerOptions().find((item) => item.id === id);
	return found ? found.name : id || "no provider";
};

const endpointFor = (id) => asArray(state.settings && state.settings.customEndpoints).find((endpoint) => endpoint.id === id);

const modelFor = (provider) => {
	const chosen = state.settings && state.settings.models ? state.settings.models[provider] : "";
	return typeof chosen === "string" ? chosen : "";
};

const activeProvider = () => (state.conversation && state.conversation.provider) || (state.settings && state.settings.provider) || "ml5";

const activeModel = () => (state.conversation && state.conversation.model) || modelFor(activeProvider());

const currentMode = () => (state.conversation && state.conversation.mode) || (state.settings && state.settings.defaultMode) || "chat";

const currentTier = () => (state.settings && state.settings.tier) || TIER_DEFAULT;

const tierInfo = (id) => TIERS.find((tier) => tier.id === id) || TIERS.find((tier) => tier.id === TIER_DEFAULT);

const isStreaming = () => Boolean(state.liveTurn);

const hexToRgb = (hex) => {
	const digits = String(hex || "").replace("#", "").trim();
	if (digits.length !== 6) return null;
	const value = Number.parseInt(digits, 16);
	if (Number.isNaN(value)) return null;
	return { r: (value >> 16) & 255, g: (value >> 8) & 255, b: value & 255 };
};

const rgbToHex = (rgb) => `#${[rgb.r, rgb.g, rgb.b].map((channel) => clamp(Math.round(channel), 0, 255).toString(16).padStart(2, "0")).join("")}`;

const mixHex = (hex, target, amount) => {
	const rgb = hexToRgb(hex);
	if (!rgb) return hex;
	return rgbToHex({ r: rgb.r + (target.r - rgb.r) * amount, g: rgb.g + (target.g - rgb.g) * amount, b: rgb.b + (target.b - rgb.b) * amount });
};

const relativeLuminance = (hex) => {
	const rgb = hexToRgb(hex);
	if (!rgb) return 0;
	const channel = (value) => {
		const ratio = value / 255;
		return ratio <= 0.03928 ? ratio / 12.92 : ((ratio + 0.055) / 1.055) ** 2.4;
	};
	return 0.2126 * channel(rgb.r) + 0.7152 * channel(rgb.g) + 0.0722 * channel(rgb.b);
};

const applyAccent = (accent) => {
	const root = document.documentElement;
	if (!hexToRgb(accent)) {
		root.style.removeProperty("--accent");
		root.style.removeProperty("--accent-strong");
		root.style.removeProperty("--accent-weak");
		root.style.removeProperty("--on-accent");
		return;
	}
	const dark = state.appliedTheme === "dark";
	const rgb = hexToRgb(accent);
	root.style.setProperty("--accent", accent);
	root.style.setProperty("--accent-strong", dark ? mixHex(accent, { r: 255, g: 255, b: 255 }, 0.3) : mixHex(accent, { r: 0, g: 0, b: 0 }, 0.25));
	root.style.setProperty("--accent-weak", `rgba(${rgb.r}, ${rgb.g}, ${rgb.b}, ${dark ? 0.18 : 0.12})`);
	root.style.setProperty("--on-accent", relativeLuminance(accent) > 0.5 ? "#0b0d12" : "#ffffff");
};

const applyTheme = (theme) => {
	const payload = isObject(theme) ? theme : {};
	state.appliedTheme = payload.scheme === "light" ? "light" : "dark";
	document.documentElement.dataset.theme = state.appliedTheme;
	applyAccent(typeof payload.accent === "string" && payload.accent ? payload.accent : "");
};

const applyFontScale = (scale) => {
	document.documentElement.style.setProperty("--scale", String(clamp(numberOr(scale, 1), 0.8, 1.4)));
};

const setBusy = (node, busy) => {
	if (!node) return;
	node.disabled = Boolean(busy);
	node.setAttribute("aria-busy", busy ? "true" : "false");
};

const focusNode = (node) => node && requestAnimationFrame(() => node.focus());

const scrollTranscriptToEnd = (smooth) => {
	const node = refs.transcript;
	if (!node) return;
	node.scrollTo({ top: node.scrollHeight, behavior: smooth ? "smooth" : "auto" });
	state.autoScroll = true;
	show(refs.jumpLatest, false);
};

const isNearBottom = () => {
	const node = refs.transcript;
	if (!node) return true;
	return node.scrollHeight - node.scrollTop - node.clientHeight < DRAG_SCROLL_EDGE;
};

const handleTranscriptScroll = () => {
	const near = isNearBottom();
	state.autoScroll = near;
	show(refs.jumpLatest, !near && (state.messages.length > 0 || isStreaming()));
};

const sanitizeFragment = (fragment) => {
	for (const node of fragment.querySelectorAll("*")) {
		for (const attribute of Array.from(node.attributes)) {
			const name = attribute.name.toLowerCase();
			if (name.startsWith("on")) node.removeAttribute(attribute.name);
			if ((name === "href" || name === "src" || name === "xlink:href") && String(attribute.value).trim().toLowerCase().startsWith("javascript:")) {
				node.removeAttribute(attribute.name);
			}
		}
	}
	return fragment;
};

const renderMarkdown = async (text) => {
	const source = typeof text === "string" ? text : "";
	if (!source.trim()) return "";
	const html = await invoke("markdown_render", { text: source });
	if (typeof html !== "string") return "";
	const template = document.createElement("template");
	template.innerHTML = html;
	return sanitizeFragment(template.content).textContent === null ? "" : serializeFragment(template.content);
};

const serializeFragment = (fragment) => {
	const holder = document.createElement("div");
	holder.appendChild(fragment.cloneNode(true));
	return holder.innerHTML;
};

const paintMarkdown = async (node, text, token) => {
	if (!node) return;
	let html = "";
	try {
		html = await renderMarkdown(text);
	} catch (error) {
		html = `<p>${escapeText(text)}</p>`;
	}
	if (token !== undefined && token !== state.renderToken) return;
	if (!html) {
		node.textContent = typeof text === "string" ? text : "";
		return;
	}
	node.innerHTML = html;
};

const attachTranscriptDelegation = () => {
	const node = refs.transcript;
	if (!node) return;
	node.addEventListener("click", (event) => {
		const copy = event.target.closest("[data-code-copy]");
		if (copy) {
			event.preventDefault();
			const block = copy.closest(".code-block");
			const code = block ? block.querySelector("pre.code") : null;
			const text = code ? code.textContent || "" : "";
			guard(async () => {
				await invoke("clipboard_write", { text });
				flashLabel(copy, "Copied");
			});
			return;
		}
		const link = event.target.closest(".msg-body a");
		if (link) {
			const href = link.getAttribute("href") || "";
			if (!href.startsWith("http://") && !href.startsWith("https://") && !href.startsWith("mailto:")) return;
			event.preventDefault();
			guard(async () => {
				await invoke("clipboard_write", { text: href });
				notify("That link was copied. Paste it into your browser to open it.", "info");
			});
			return;
		}
		const shot = event.target.closest("[data-shot-id]");
		if (shot) {
			event.preventDefault();
			openActionImage(shot.dataset.shotId, shot.dataset.shotUrl || "");
		}
	});
};

const openOverlay = (build) => {
	const root = refs.overlayRoot;
	if (!root) return;
	root.innerHTML = "";
	root.classList.remove("hidden");
	root.dataset.open = "true";
	const modal = el("div", "modal");
	modal.setAttribute("role", "dialog");
	modal.setAttribute("aria-modal", "true");
	modal.tabIndex = -1;
	root.appendChild(modal);
	build(modal, { close: () => closeOverlay() });
	const focusable = modal.querySelector("input, textarea, select, button");
	focusNode(focusable || modal);
};

const closeOverlay = () => {
	const root = refs.overlayRoot;
	if (!root || root.classList.contains("hidden")) return false;
	root.classList.add("hidden");
	root.dataset.open = "false";
	root.innerHTML = "";
	return true;
};

const overlayOpen = () => Boolean(refs.overlayRoot) && refs.overlayRoot.dataset.open === "true";

const showLightbox = (dataUrl, label) => {
	if (!isDataUrl(dataUrl)) return;
	refs.lightboxImage.src = dataUrl;
	refs.lightboxImage.alt = label || "Image preview";
	show(refs.lightbox, true);
	focusNode(refs.lightboxClose);
};

const closeLightbox = () => {
	if (!refs.lightbox) return;
	show(refs.lightbox, false);
	refs.lightboxImage.removeAttribute("src");
};

const setView = (view, options) => {
	const next = VIEWS.includes(view) ? view : "chat";
	state.view = next;
	for (const tab of document.querySelectorAll("#view-tabs .tab")) {
		tab.setAttribute("aria-current", tab.dataset.view === next ? "page" : "false");
	}
	for (const section of document.querySelectorAll(".view")) {
		show(section, section.dataset.view === next);
	}
	if (next === "models") {
		refreshModelsScreen(false);
		startDaemonPolling();
	} else {
		stopDaemonPolling();
	}
	if (next === "actions") refreshActions();
	if (next === "settings") renderSettings();
	if (next === "chat" && options && options.focus) focusNode(refs.composerInput);
	return next;
};

const applySidebar = (collapsed, persist) => {
	state.sidebarCollapsed = Boolean(collapsed);
	if (refs.app) refs.app.dataset.sidebar = state.sidebarCollapsed ? "collapsed" : "open";
	show(refs.sidebarShow, state.sidebarCollapsed);
	show(refs.sidebarScrim, !state.sidebarCollapsed && window.innerWidth <= 860);
	if (persist && state.settings && state.settings.sidebarCollapsed !== state.sidebarCollapsed) {
		const next = settingsClone();
		next.sidebarCollapsed = state.sidebarCollapsed;
		saveSettings({ silent: true, patch: next, noApply: true });
	}
};

const toggleSidebar = () => applySidebar(!state.sidebarCollapsed, true);

const renderConversationList = () => {
	const list = refs.conversationList;
	if (!list) return;
	list.innerHTML = "";
	const query = state.searchQuery.trim();
	const items = state.conversations;
	if (refs.searchNote) {
		show(refs.searchNote, query.length > 0);
		refs.searchNote.textContent = query.length > 0 ? (items.length === 1 ? "1 chat matches" : `${items.length} chats match`) : "";
	}
	if (!items.length) {
		list.appendChild(buildConversationEmpty(query));
		return;
	}
	const pinned = items.filter((conversation) => conversation.pinned);
	const rest = items.filter((conversation) => !conversation.pinned);
	if (pinned.length) list.appendChild(el("div", "search-note", "Pinned"));
	for (const conversation of pinned) list.appendChild(buildConversationRow(conversation));
	if (rest.length && pinned.length) list.appendChild(el("div", "search-note", "Recent"));
	for (const conversation of rest) list.appendChild(buildConversationRow(conversation));
};

const buildConversationEmpty = (query) => {
	const box = el("div", "empty-state");
	box.style.padding = "var(--space-5) var(--space-3)";
	box.style.gap = "var(--space-2)";
	box.appendChild(el("strong", null, query ? "No chats match that search" : "No chats yet"));
	box.appendChild(el("p", null, query ? "Try a different word, or clear the search to see everything again." : "Chats you start will be listed here, newest first."));
	box.appendChild(query ? button("Clear the search", "ghost-button small", () => clearSearch()) : button("Start a chat", "primary-button small", () => newChat(), "plus"));
	return box;
};

const buildConversationRow = (conversation) => {
	const row = el("div", "conversation-row");
	row.dataset.id = conversation.id;
	if (state.conversation && state.conversation.id === conversation.id) row.classList.add("active");
	const open = el("button", "conversation-open");
	open.type = "button";
	open.setAttribute("aria-label", `Open the chat ${conversation.title || "New chat"}`);
	open.appendChild(el("span", "conversation-title", conversation.title || "New chat"));
	const sub = el("span", "conversation-sub");
	sub.appendChild(el("span", null, relativeTime(conversation.updated) || ""));
	sub.appendChild(el("span", null, MODE_LABELS[conversation.mode] || "Chat"));
	if (conversation.pinned) sub.appendChild(icon("pin"));
	open.appendChild(sub);
	if (conversation.snippet) open.appendChild(el("span", "conversation-snippet", conversation.snippet));
	open.addEventListener("click", () => selectConversation(conversation.id));
	row.appendChild(open);
	const actions = el("div", "conversation-actions");
	actions.appendChild(iconButton(conversation.pinned ? "Unpin this chat" : "Pin this chat", "pin", (event) => {
		event.stopPropagation();
		togglePin(conversation);
	}, "icon-button tiny"));
	actions.appendChild(iconButton("Rename this chat", "edit", (event) => {
		event.stopPropagation();
		startRowRename(row, conversation);
	}, "icon-button tiny"));
	actions.appendChild(iconButton("Export this chat", "export", (event) => {
		event.stopPropagation();
		openExportDialog(conversation);
	}, "icon-button tiny"));
	actions.appendChild(iconButton("Delete this chat", "trash", (event) => {
		event.stopPropagation();
		startRowDelete(row, conversation);
	}, "icon-button tiny"));
	row.appendChild(actions);
	return row;
};

const startRowRename = (row, conversation) => {
	const host = row.querySelector(".conversation-open");
	if (!host) return;
	const input = el("input", "rename-input");
	input.value = conversation.title || "";
	input.setAttribute("aria-label", "New name for this chat");
	host.replaceWith(input);
	focusNode(input);
	input.select();
	let settled = false;
	const commit = (save) => {
		if (settled) return;
		settled = true;
		const value = input.value.trim();
		if (!save || !value || value === conversation.title) {
			renderConversationList();
			return;
		}
		guard(async () => {
			await invoke("conversations_update", { conversation: { ...conversation, title: value, snippet: null } });
			if (state.conversation && state.conversation.id === conversation.id) state.conversation.title = value;
			renderChatHeader();
			await refreshConversations();
		});
	};
	input.addEventListener("keydown", (event) => {
		if (event.key === "Enter") {
			event.preventDefault();
			commit(true);
		} else if (event.key === "Escape") {
			event.preventDefault();
			commit(false);
		}
	});
	input.addEventListener("blur", () => commit(true));
};

const startRowDelete = (row, conversation) => {
	const host = row.querySelector(".conversation-open");
	if (!host) return;
	host.remove();
	const actions = row.querySelector(".conversation-actions");
	actions && actions.remove();
	const confirm = el("div", "confirm-inline");
	confirm.appendChild(el("span", "confirm-inline-text", `Delete “${truncateText(conversation.title || "this chat", 26)}”? This cannot be undone.`));
	let settled = false;
	const finish = () => {
		if (settled) return;
		settled = true;
		renderConversationList();
	};
	confirm.appendChild(button("Delete", "danger-button small", () => {
		settled = true;
		guard(async () => {
			await invoke("conversations_delete", { id: conversation.id });
			if (state.conversation && state.conversation.id === conversation.id) {
				state.conversation = null;
				state.messages = [];
				renderChatHeader();
				await renderTranscript();
			}
			await refreshConversations();
			notify("Chat deleted.", "success");
		});
	}));
	confirm.appendChild(button("Keep it", "ghost-button small", finish));
	row.appendChild(confirm);
	focusNode(confirm.querySelector(".ghost-button"));
};

const togglePin = (conversation) => guard(async () => {
	await invoke("conversations_update", { conversation: { ...conversation, pinned: !conversation.pinned, snippet: null } });
	if (state.conversation && state.conversation.id === conversation.id) state.conversation.pinned = !conversation.pinned;
	await refreshConversations();
});

const clearSearch = () => {
	if (refs.searchInput) refs.searchInput.value = "";
	state.searchQuery = "";
	show(refs.searchClear, false);
	refreshConversations();
};

const searchDebounced = (() => {
	let timer = 0;
	return () => {
		clearTimeout(timer);
		timer = setTimeout(() => refreshConversations(), 220);
	};
})();

const refreshConversations = async () => {
	try {
		const list = await invoke("conversations_list", { query: state.searchQuery });
		state.conversations = asArray(list);
		renderConversationList();
	} catch (error) {
		state.conversations = [];
		renderConversationListError(error);
	}
};

const renderConversationListError = (error) => {
	const list = refs.conversationList;
	if (!list) return;
	list.innerHTML = "";
	const box = el("div", "empty-state");
	box.style.padding = "var(--space-5) var(--space-3)";
	box.appendChild(el("strong", null, "Chat history is unavailable"));
	box.appendChild(el("p", null, String(error || "Your conversations could not be read.")));
	box.appendChild(button("Try again", "ghost-button small", () => refreshConversations()));
	list.appendChild(box);
};

const renderChatHeader = () => {
	const conversation = state.conversation;
	const title = conversation ? conversation.title || "New chat" : "New chat";
	refs.chatTitle && (refs.chatTitle.textContent = title);
	document.title = conversation ? `${title} - Ash Agent` : "Ash Agent";
	const provider = activeProvider();
	const model = activeModel();
	const label = model || providerLabel(provider);
	refs.headModel && (refs.headModel.textContent = label);
	refs.headModel && (refs.headModel.title = `${providerLabel(provider)}${model ? ` · ${model}` : " · no model chosen yet"}`);
	refs.composerModelText && (refs.composerModelText.textContent = label);
	if (refs.modelChipText) {
		const health = state.health[provider];
		const stateText = !health ? "not checked yet" : health.online ? "connected" : "not reachable";
		refs.modelChipText.textContent = `${label} · ${stateText}`;
	}
	const helper = currentMode() === "helper";
	show(refs.headHelper, helper);
	show(refs.tierButton, helper);
	if (helper) {
		const info = tierInfo(currentTier());
		refs.tierButton.dataset.tier = info.id;
		refs.tierButton.textContent = `Permission: ${info.label}`;
		refs.tierButton.title = `${info.blurb} Click to change.`;
		refs.headHelperText && (refs.headHelperText.textContent = `Helper · ${info.short}`);
	}
	for (const option of document.querySelectorAll(".mode-option")) {
		option.setAttribute("aria-checked", option.dataset.mode === currentMode() ? "true" : "false");
	}
	refs.composerInput && (refs.composerInput.placeholder = helper ? "Describe what you want done on this computer…" : "Ask anything, or describe what you want to do…");
};

const setMode = (mode) => guard(async () => {
	const next = mode === "helper" ? "helper" : "chat";
	if (state.conversation) {
		state.conversation.mode = next;
		await invoke("conversations_update", { conversation: { ...state.conversation, snippet: null } });
		renderChatHeader();
		refreshConversations();
	} else {
		const draft = settingsClone();
		draft.defaultMode = next;
		await saveSettings({ silent: true, patch: draft, noApply: true });
		renderChatHeader();
	}
	if (next === "helper") {
		loadToolCatalog();
		notify("Helper mode is on. Ash can now use tools, and will ask before changing anything.", "info");
	}
});

const newChat = () => {
	if (isStreaming()) {
		warn("A reply is still coming. Stop it first if you want to start a new chat.");
		return;
	}
	setView("chat");
	state.conversation = null;
	state.messages = [];
	state.answeredActionIds = {};
	refs.composerInput && (refs.composerInput.value = "");
	growComposer();
	renderChatHeader();
	renderTranscript();
	refreshConversations();
	focusNode(refs.composerInput);
};

const selectConversation = async (id) => {
	if (state.conversation && state.conversation.id === id) {
		setView("chat");
		return;
	}
	if (isStreaming()) {
		warn("Finish or stop the current reply before switching chats.");
		return;
	}
	await guard(async () => {
		const conversation = await invoke("conversations_get", { id });
		const messages = await invoke("messages_list", { conversationId: id });
		state.conversation = conversation;
		state.messages = asArray(messages).filter(isObject);
		state.answeredActionIds = {};
		setView("chat");
		renderChatHeader();
		await renderTranscript();
		scrollTranscriptToEnd(false);
		if (currentMode() === "helper") loadToolCatalog();
		refreshConversations();
	});
};

const buildEmptyTranscript = () => {
	const box = el("div", "empty-state");
	const mark = el("img");
	mark.src = "logo.png";
	mark.alt = "";
	box.appendChild(mark);
	box.appendChild(el("h2", null, `Hello, I'm ${ASSISTANT_NAME}`));
	box.appendChild(el("p", null, "Ask me anything, or switch to Helper mode and I'll work on your computer with you, explaining every step and asking before I change anything."));
	const actions = el("div", "empty-actions");
	actions.appendChild(button("New chat", "primary-button small", () => newChat(), "plus"));
	actions.appendChild(button("Choose a model", "ghost-button small", () => openModelPicker(), "cpu"));
	actions.appendChild(button("Get models", "ghost-button small", () => setView("models"), "download"));
	actions.appendChild(button("Settings", "ghost-button small", () => setView("settings"), "sliders"));
	box.appendChild(actions);
	const grid = el("div", "suggestion-grid");
	for (const suggestion of SUGGESTIONS) {
		const card = el("button", "suggestion-card");
		card.type = "button";
		card.appendChild(el("span", null, suggestion.title));
		card.appendChild(el("span", null, suggestion.detail));
		card.addEventListener("click", () => {
			refs.composerInput.value = suggestion.text;
			growComposer();
			focusNode(refs.composerInput);
			refs.composerInput.setSelectionRange(suggestion.text.length, suggestion.text.length);
		});
		grid.appendChild(card);
	}
	box.appendChild(grid);
	const fixes = buildGuidedFixesCard(true);
	if (fixes) box.appendChild(fixes);
	return box;
};

const buildGuidedFixesCard = (compact) => {
	if (!state.guidedFixes.length) return null;
	const card = el("section", compact ? "card" : "card");
	const head = el("div", "card-head");
	head.appendChild(el("h2", null, "Common fixes"));
	head.appendChild(el("span", "spacer"));
	card.appendChild(head);
	card.appendChild(el("p", "card-note", "Pick whatever is wrong and Ash walks you through it, one step at a time, explaining each one in plain words. Nothing happens until you approve it."));
	const grid = el("div", "fix-grid");
	for (const fix of state.guidedFixes) {
		const item = el("div", "fix-card");
		const iconName = GUIDED_ICONS[String(fix.id || "").toLowerCase()] || "check";
		const titleRow = el("div", "card-head");
		titleRow.appendChild(icon(iconName));
		titleRow.appendChild(el("h3", null, fix.title || fix.id));
		item.appendChild(titleRow);
		item.appendChild(el("p", null, fix.description || ""));
		const steps = asArray(fix.steps);
		if (steps.length) {
			const list = el("ol", "fix-steps");
			for (const step of steps) list.appendChild(el("li", null, step));
			item.appendChild(list);
		}
		item.appendChild(button("Start this", "primary-button small", () => runGuidedFix(fix), "check"));
		grid.appendChild(item);
	}
	card.appendChild(grid);
	return card;
};

const runGuidedFix = (fix) => guard(async () => {
	if (isStreaming()) {
		warn("A reply is still coming. Stop it first, then start the fix.");
		return;
	}
	const actionId = await invoke("guided_fix_run", { id: fix.id });
	const list = asArray(await invoke("actions_list", { limit: 20, before: null }));
	const started = list.find((action) => action.id === actionId);
	if (started && started.conversationId) {
		state.conversation = null;
		await selectConversation(started.conversationId);
		upsertAction(started);
	} else {
		setView("chat");
	}
	await refreshConversations();
	notify(`“${fix.title}” is ready. Read the card and approve it when you are ready.`, "success");
});

const renderTranscript = async () => {
	const node = refs.transcript;
	if (!node) return;
	state.renderToken += 1;
	const token = state.renderToken;
	state.liveNode = null;
	const stick = isNearBottom() || state.autoScroll;
	if (!state.messages.length && !isStreaming()) {
		node.innerHTML = "";
		node.appendChild(buildEmptyTranscript());
		show(refs.jumpLatest, false);
		return;
	}
	const actions = state.conversation ? await loadConversationActions() : [];
	if (token !== state.renderToken) return;
	node.innerHTML = "";
	const rows = [];
	const liveSince = state.liveTurn ? state.liveTurn.startedAt : Infinity;
	const shownIds = new Set(state.messages.map((message) => message.id));
	const pending = actions.filter((action) => numberOr(action.created, 0) >= liveSince && !shownIds.has(action.messageId));
	const covered = new Set([...actions.filter((action) => shownIds.has(action.messageId)), ...pending].map((action) => action.toolCallId).filter(Boolean));
	for (const message of state.messages) {
		if (message.role === "tool") {
			covered.has(message.toolCallId) || rows.push(buildToolResult(message));
			continue;
		}
		const related = actions.filter((action) => action.messageId === message.id);
		rows.push(buildMessage(message, related));
	}
	for (const action of pending) rows.push(buildActionCard(action));
	for (const row of rows) node.appendChild(row);
	const live = state.liveTurn;
	if (live) {
		const row = buildLiveRow();
		node.appendChild(row);
		state.liveNode = row;
		applyLiveTurn();
	}
	if (token !== state.renderToken) return;
	stick && scrollTranscriptToEnd(false);
	show(refs.jumpLatest, false);
};

const loadConversationActions = async () => {
	if (!state.conversation) return [];
	try {
		const list = await invoke("actions_for_conversation", { conversationId: state.conversation.id });
		return asArray(list).filter(isObject);
	} catch (error) {
		return [];
	}
};

const buildActionStatus = (status) => {
	const key = ACTION_STATUS_LABELS[status] ? status : "pending";
	const badge = el("span", "badge");
	badge.dataset.status = key;
	if (key === "done") badge.classList.add("ok");
	else if (key === "failed" || key === "denied" || key === "cancelled" || key === "stopped") badge.classList.add("danger");
	else if (key === "awaiting" || key === "running" || key === "pending") badge.classList.add("warn");
	badge.textContent = ACTION_STATUS_LABELS[key];
	return badge;
};

const buildRiskBadge = (risk) => {
	const key = RISK_LABELS[risk] ? risk : "unknown";
	const badge = el("span", "badge risk-badge");
	badge.dataset.risk = key;
	badge.textContent = key === "unknown" ? "Risk unknown" : RISK_LABELS[key];
	return badge;
};

const buildOutputBlock = (output, finished) => {
	const text = typeof output === "string" ? output : "";
	const wrap = el("div");
	if (!text.trim()) return wrap;
	const truncated = text.length > OUTPUT_CLAMP;
	const pre = el("pre", "output", truncated && !finished ? truncateText(text, OUTPUT_CLAMP + 1) : text);
	truncated && !finished && pre.classList.add("clamped");
	pre.dataset.full = text;
	wrap.appendChild(pre);
	if (truncated && !finished) {
		const toggle = button("Show all", "ghost-button small", () => {
			const expanded = pre.dataset.expanded === "true";
			pre.dataset.expanded = expanded ? "false" : "true";
			pre.textContent = expanded ? truncateText(text, OUTPUT_CLAMP + 1) : text;
			pre.classList.toggle("clamped", expanded);
			toggle.querySelector("span").textContent = expanded ? "Show all" : "Show less";
		});
		wrap.appendChild(toggle);
	}
	return wrap;
};

const buildActionCard = (action) => {
	const card = el("div", "action-card");
	card.dataset.actionId = action.id;
	card.dataset.status = action.status || "pending";
	if (action.root) card.classList.add("root");
	const head = el("div", "action-head");
	head.appendChild(icon("shield"));
	head.appendChild(el("strong", null, action.title || action.tool || "Action"));
	head.appendChild(buildRiskBadge(action.risk));
	head.appendChild(buildActionStatus(action.status));
	if (action.root) head.appendChild(el("span", "badge danger", "Administrator"));
	card.appendChild(head);
	if (action.explanation) card.appendChild(el("p", "action-explain", action.explanation));
	if (action.root) {
		const note = el("div", "action-root-note");
		note.appendChild(icon("info"));
		note.appendChild(el("span", null, "This one runs with administrator rights. Ember will ask for your password in a system dialog, and nothing happens until you type it."));
		card.appendChild(note);
	}
	const detail = typeof action.detail === "string" ? action.detail.trim() : "";
	const args = action.args !== undefined && action.args !== null && action.args !== "" ? action.args : null;
	if (detail || args) {
		const block = el("div", "code-block action-detail");
		const codeHead = el("div", "code-head");
		codeHead.appendChild(el("span", "code-lang", action.tool || "action"));
		codeHead.appendChild(el("span", "badge", action.root ? "administrator" : "your account"));
		const copy = el("button", "code-copy");
		copy.type = "button";
		copy.dataset.codeCopy = "copy";
		copy.textContent = "Copy";
		codeHead.appendChild(copy);
		block.appendChild(codeHead);
		block.appendChild(el("pre", "code", detail || jsonArgs(args)));
		card.appendChild(block);
	}
	const finished = action.status === "done" || action.status === "failed" || action.status === "denied" || action.status === "cancelled" || action.status === "stopped";
	if (typeof action.output === "string" && action.output.trim()) card.appendChild(buildOutputBlock(action.output, finished));
	const foot = el("div", "action-foot");
	const answered = Boolean(state.answeredActionIds[action.id]);
	if (action.status === "awaiting" && !answered) {
		foot.appendChild(button("Approve", "primary-button small", () => resolveAction(action, true, false), "check"));
		foot.appendChild(button("Deny", "danger-button small", () => resolveAction(action, false, false), "close"));
		foot.appendChild(button("Don't ask again for this kind of action", "ghost-button small", () => resolveAction(action, true, true)));
	} else if (action.status === "awaiting" && answered) {
		foot.appendChild(el("span", "row-sub", "Your answer was sent. Waiting for the helper to continue…"));
	} else if (action.status === "running" || action.status === "pending") {
		foot.appendChild(el("span", "spinner"));
		foot.appendChild(el("span", "row-sub", action.status === "pending" ? "Getting ready…" : "Working now…"));
		foot.appendChild(button("Cancel this action", "ghost-button small danger-text", () => cancelAction(action), "stop"));
	} else {
		foot.appendChild(el("span", "row-sub", action.finished ? `Finished ${formatClock(action.finished)}` : formatClock(action.created)));
		if (Number.isFinite(Number(action.exitCode))) {
			const badge = el("span", "badge");
			badge.textContent = `exit code ${action.exitCode}`;
			action.exitCode === 0 ? badge.classList.add("ok") : badge.classList.add("danger");
			foot.appendChild(badge);
		}
	}
	card.appendChild(foot);
	action.imageId && card.appendChild(buildActionShot(action.imageId));
	return card;
};

const buildActionShot = (imageId) => {
	const shot = el("button", "shot");
	shot.type = "button";
	shot.dataset.shotId = imageId;
	const img = el("img");
	img.alt = "Screenshot taken during this action";
	img.loading = "lazy";
	shot.appendChild(img);
	shot.appendChild(el("span", "shot-label", "Screenshot (click to see it full size)"));
	loadActionImage(imageId).then((url) => {
		if (!url) return;
		img.src = url;
		shot.dataset.shotUrl = url;
	});
	return shot;
};

const buildToolResult = (message) => {
	const meta = isObject(message.meta) ? message.meta : {};
	const text = (typeof message.content === "string" ? message.content : "").replace(TOOL_SCREENSHOT_NOTE, "").trimEnd();
	const status = /^The action (did not succeed|failed)/.test(text) ? "failed" : "done";
	const card = el("div", "action-card");
	card.dataset.status = status;
	const head = el("div", "action-head");
	head.appendChild(icon("shield"));
	head.appendChild(el("strong", null, meta.title || meta.tool || "Result"));
	head.appendChild(buildActionStatus(status));
	card.appendChild(head);
	text.trim() && card.appendChild(buildOutputBlock(text, true));
	const foot = el("div", "action-foot");
	foot.appendChild(el("span", "row-sub", `Finished ${formatClock(message.created)}`));
	card.appendChild(foot);
	typeof meta.imageId === "string" && meta.imageId && card.appendChild(buildActionShot(meta.imageId));
	return card;
};

const resolveAction = (action, allow, remember) => guard(async () => {
	state.answeredActionIds[action.id] = true;
	await invoke("tool_approve", { actionId: action.id, allow, remember });
	patchActionCard({ ...action, status: allow ? "running" : "denied", finished: allow ? action.finished : Date.now() });
});

const cancelAction = (action) => guard(async () => {
	await invoke("tool_cancel", { actionId: action.id });
	patchActionCard({ ...action, status: "cancelled", finished: Date.now() });
});

const loadActionImage = async (id) => {
	if (!id) return "";
	if (state.actionImages[id]) return state.actionImages[id];
	try {
		const url = await invoke("action_image", { id });
		if (isDataUrl(url)) {
			state.actionImages[id] = url;
			return url;
		}
	} catch (error) {
		return "";
	}
	return "";
};

const openActionImage = async (id, fallback) => {
	const url = fallback || (await loadActionImage(id));
	url ? showLightbox(url, "Screenshot from the helper") : warn("That screenshot is no longer stored on this computer.");
};

const patchActionCard = (action) => {
	const rebuilt = buildActionCard(action);
	for (const node of document.querySelectorAll(`[data-action-id="${action.id}"]`)) node.replaceWith(rebuilt.cloneNode(true));
	upsertActionList(action);
	if (state.view === "actions") renderActions();
};

const upsertAction = (action) => {
	if (!isObject(action) || !action.id) return;
	upsertActionList(action);
	if (state.conversation && action.conversationId === state.conversation.id) {
		const card = buildActionCard(action);
		const existing = document.querySelector(`[data-action-id="${action.id}"]`);
		existing ? existing.replaceWith(card) : appendActionCard(card, action);
		if (state.autoScroll && isStreaming()) scrollTranscriptToEnd(false);
	}
	if (state.view === "actions") renderActions();
};

const upsertActionList = (action) => {
	const index = state.actions.findIndex((item) => item.id === action.id);
	index >= 0 ? (state.actions[index] = action) : state.actions.unshift(action);
	state.actions.sort((a, b) => numberOr(b.created, 0) - numberOr(a.created, 0));
};

const appendActionCard = (card, action) => {
	const node = refs.transcript;
	if (!node) return;
	const fresh = Boolean(state.conversation && action.conversationId === state.conversation.id);
	if (!fresh) return;
	node.querySelector(".empty-state") && (node.innerHTML = "");
	node.appendChild(card);
};

const buildThinkingBlock = (thinking, live) => {
	const details = el("details", "thinking-block");
	details.open = Boolean(live) || !state.settings || state.settings.showThinking !== false;
	const summary = el("summary");
	summary.appendChild(el("span", null, live ? `${ASSISTANT_NAME} is thinking…` : `${ASSISTANT_NAME}'s thinking`));
	details.appendChild(summary);
	details.appendChild(el("div", "thinking-body", thinking || ""));
	return details;
};

const buildUsageLine = (usage) => {
	if (!isObject(usage)) return null;
	const parts = [];
	if (numberOr(usage.promptTokens, 0) > 0) parts.push(`${formatCount(usage.promptTokens)} in`);
	if (numberOr(usage.completionTokens, 0) > 0) parts.push(`${formatCount(usage.completionTokens)} out`);
	if (numberOr(usage.cachedTokens, 0) > 0) parts.push(`${formatCount(usage.cachedTokens)} cached`);
	if (numberOr(usage.generationMs, 0) > 0) parts.push(`${(numberOr(usage.generationMs, 0) / 1000).toFixed(1)}s`);
	if (!parts.length) return null;
	const line = el("div", "usage-line");
	for (const part of parts) line.appendChild(el("span", null, part));
	return line;
};

const messageUsage = (message) => (isObject(message.meta) && isObject(message.meta.usage) ? message.meta.usage : null);

const buildMessage = (message, actions) => {
	const isUser = message.role === "user";
	const row = el("div", `msg msg-${isUser ? "user" : "assistant"}`);
	row.dataset.messageId = message.id;
	row.dataset.seq = String(numberOr(message.seq, 0));
	const inner = el("div", "msg-inner");
	if (isUser) {
		const images = asArray(message.images).filter(isDataUrl);
		if (images.length) {
			const strip = el("div", "msg-attachments");
			for (const url of images) {
				const box = el("button", "attachment");
				box.type = "button";
				box.setAttribute("aria-label", "Show this image full size");
				const img = el("img");
				img.src = url;
				img.alt = "Image you attached";
				img.loading = "lazy";
				box.appendChild(img);
				box.addEventListener("click", () => showLightbox(url, "Image you attached"));
				strip.appendChild(box);
			}
			inner.appendChild(strip);
		}
		inner.appendChild(el("div", "bubble", typeof message.content === "string" ? message.content : ""));
		const bar = el("div", "msg-actions");
		versionsOf(message).length > 1 && bar.classList.add("has-versions");
		bar.appendChild(buildVersionPicker(message));
		bar.appendChild(iconButton("Copy this message", "copy", () => copyMessage(message), "icon-button tiny"));
		bar.appendChild(iconButton("Edit and send again as a new version", "edit", () => startEdit(message), "icon-button tiny"));
		bar.appendChild(iconButton("Get a new answer to this message", "retry", () => regenerateFrom(message), "icon-button tiny"));
		bar.appendChild(iconButton("Delete…", "trash", () => openDeleteDialog(message), "icon-button tiny"));
		inner.appendChild(bar);
	} else {
		inner.appendChild(buildAssistantHead(message.created));
		if (typeof message.thinking === "string" && message.thinking.trim() && (!state.settings || state.settings.showThinking !== false)) {
			inner.appendChild(buildThinkingBlock(message.thinking, false));
		}
		const calls = asArray(message.toolCalls);
		if (calls.length) {
			const chips = el("div", "tool-calls");
			for (const call of calls) {
				const chip = el("span", "tool-call-chip", call.name || "tool");
				chip.title = call.arguments || "";
				chips.appendChild(chip);
			}
			inner.appendChild(chips);
		}
		const body = el("div", "msg-body");
		inner.appendChild(body);
		paintMarkdown(body, typeof message.content === "string" ? message.content : "");
		const usage = buildUsageLine(messageUsage(message));
		usage && inner.appendChild(usage);
		const bar = el("div", "msg-actions");
		versionsOf(message).length > 1 && bar.classList.add("has-versions");
		bar.appendChild(buildVersionPicker(message));
		bar.appendChild(iconButton("Copy this reply", "copy", () => copyMessage(message), "icon-button tiny"));
		bar.appendChild(iconButton("Edit this reply as a new version", "edit", () => startEdit(message), "icon-button tiny"));
		bar.appendChild(iconButton("Write this reply again as a new version", "retry", () => regenerateFrom(message), "icon-button tiny"));
		bar.appendChild(iconButton("Delete…", "trash", () => openDeleteDialog(message), "icon-button tiny"));
		inner.appendChild(bar);
	}
	row.appendChild(inner);
	for (const action of actions) row.appendChild(buildActionCard(action));
	return row;
};

const buildAssistantHead = (created) => {
	const head = el("div", "assistant-head");
	const mark = el("img");
	mark.src = "logo.png";
	mark.alt = "";
	head.appendChild(mark);
	head.appendChild(el("strong", null, ASSISTANT_NAME));
	if (created) head.appendChild(el("span", null, formatClock(created)));
	return head;
};

const copyMessage = (message) => guard(async () => {
	await invoke("clipboard_write", { text: typeof message.content === "string" ? message.content : "" });
	notify("Copied to the clipboard.", "success");
});

const versionsOf = (message) => {
	const list = asArray(message.siblings).filter((id) => typeof id === "string" && id);
	return list.includes(message.id) ? list : [message.id];
};

const buildVersionPicker = (message) => {
	const versions = versionsOf(message);
	const wrap = el("div", "version-picker");
	if (versions.length < 2) {
		wrap.classList.add("hidden");
		return wrap;
	}
	const index = versions.indexOf(message.id);
	const previous = iconButton("Show the previous version", "chevron", () => selectVersion(versions[index - 1]), "icon-button tiny version-prev");
	const next = iconButton("Show the next version", "chevron", () => selectVersion(versions[index + 1]), "icon-button tiny version-next");
	previous.disabled = index <= 0;
	next.disabled = index >= versions.length - 1;
	wrap.appendChild(previous);
	const count = el("span", "version-count", `${index + 1} / ${versions.length}`);
	count.setAttribute("aria-label", `Version ${index + 1} of ${versions.length}`);
	wrap.appendChild(count);
	wrap.appendChild(next);
	return wrap;
};

const busyWarning = (action) => {
	if (!state.conversation) return true;
	if (!isStreaming()) return false;
	warn(`A reply is still coming. Stop it before you ${action}.`);
	return true;
};

const selectVersion = (messageId) => guard(async () => {
	if (!messageId || busyWarning("switch versions")) return;
	const messages = await invoke("message_select", { conversationId: state.conversation.id, messageId });
	state.messages = asArray(messages).filter(isObject);
	await renderTranscript();
});

const regenerateFrom = (message) => guard(async () => {
	if (busyWarning("ask again")) return;
	setComposerAlert("");
	const conversationId = state.conversation.id;
	if (message.role === "user") {
		const index = state.messages.findIndex((item) => item.id === message.id);
		state.messages = index >= 0 ? state.messages.slice(0, index + 1) : state.messages;
	} else {
		const index = state.messages.findIndex((item) => item.id === message.id);
		const anchor = index >= 0 ? state.messages.slice(0, index).map((item) => item.role).lastIndexOf("user") : -1;
		state.messages = anchor >= 0 ? state.messages.slice(0, anchor + 1) : state.messages;
	}
	await renderTranscript();
	startTurn("pending", conversationId);
	await guard(async () => {
		const turnId = await invoke("chat_regenerate", { conversationId, messageId: message.id, think: state.think ? true : null });
		if (state.liveTurn && state.liveTurn.id === "pending") state.liveTurn.id = turnId;
	}, (error) => finishTurn({ error: String(error), dropped: true }));
});

const startEdit = (message) => {
	if (busyWarning("edit a message")) return;
	const row = refs.transcript ? refs.transcript.querySelector(`.msg[data-message-id="${message.id}"]`) : null;
	if (!row || row.querySelector(".msg-edit")) return;
	const inner = row.querySelector(".msg-inner");
	if (!inner) return;
	const isUser = message.role === "user";
	const hidden = Array.from(inner.children).filter((child) => !child.classList.contains("assistant-head") && !child.classList.contains("msg-attachments"));
	for (const child of hidden) child.classList.add("hidden");
	const editor = el("div", "msg-edit");
	const area = el("textarea", "textarea msg-edit-input");
	area.value = typeof message.content === "string" ? message.content : "";
	area.setAttribute("aria-label", isUser ? "Edit your message" : "Edit Ash's reply");
	editor.appendChild(area);
	editor.appendChild(el("p", "field-help", isUser
		? "Sending keeps the original. The edited message and its new answer become another version you can switch between."
		: "Saving keeps the original reply. Your edit becomes another version you can switch between."));
	const foot = el("div", "msg-edit-foot");
	const close = () => {
		editor.remove();
		for (const child of hidden) child.classList.remove("hidden");
	};
	const submit = () => guard(async () => {
		const content = area.value;
		const unchanged = content === (typeof message.content === "string" ? message.content : "");
		if (!content.trim() && !(isUser && asArray(message.images).length)) {
			warn(isUser ? "Type a message first, or choose Cancel." : "The reply cannot be empty.");
			focusNode(area);
			return;
		}
		if (unchanged) {
			close();
			return;
		}
		if (busyWarning("edit a message")) return;
		isUser ? await submitUserEdit(message, content) : await submitReplyEdit(message, content);
	});
	foot.appendChild(button("Cancel", "ghost-button small", close));
	foot.appendChild(button(isUser ? "Send" : "Save", "primary-button small", submit, isUser ? "send" : "check"));
	editor.appendChild(foot);
	area.addEventListener("keydown", (event) => {
		if (event.key === "Escape") {
			event.preventDefault();
			event.stopPropagation();
			close();
		} else if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
			event.preventDefault();
			submit();
		}
	});
	inner.appendChild(editor);
	const fit = () => {
		area.style.height = "auto";
		area.style.height = `${Math.min(area.scrollHeight + 2, 480)}px`;
	};
	area.addEventListener("input", fit);
	requestAnimationFrame(() => {
		fit();
		area.focus();
		area.setSelectionRange(area.value.length, area.value.length);
	});
};

const submitUserEdit = async (message, content) => {
	const conversationId = state.conversation.id;
	const index = state.messages.findIndex((item) => item.id === message.id);
	const edited = { ...message, id: newLocalId(), content, siblings: [], created: Date.now() };
	state.messages = [...(index >= 0 ? state.messages.slice(0, index) : state.messages), edited];
	setComposerAlert("");
	await renderTranscript();
	startTurn("pending", conversationId);
	await guard(async () => {
		const turnId = await invoke("chat_edit", { conversationId, messageId: message.id, content, think: state.think ? true : null });
		if (state.liveTurn && state.liveTurn.id === "pending") state.liveTurn.id = turnId;
	}, (error) => finishTurn({ error: String(error), dropped: true }));
};

const submitReplyEdit = async (message, content) => {
	const messages = await invoke("message_revise", { conversationId: state.conversation.id, messageId: message.id, content });
	state.messages = asArray(messages).filter(isObject);
	await renderTranscript();
	notify("Saved as a new version.", "success");
};

const openDeleteDialog = (message) => {
	if (busyWarning("delete messages")) return;
	const isUser = message.role === "user";
	const index = state.messages.findIndex((item) => item.id === message.id);
	const asked = isUser ? message : state.messages.slice(0, Math.max(index, 0)).reverse().find((item) => item.role === "user");
	const branchNote = (count) => `Removes this version and everything that followed it. The other ${count - 1 === 1 ? "version stays" : `${count - 1} versions stay`}.`;
	const ownVersions = versionsOf(message).length;
	const askedVersions = asked ? versionsOf(asked).length : 1;
	const scopes = [
		["only", isUser ? "Only this message" : "Only this reply", ownVersions > 1 ? branchNote(ownVersions) : isUser
			? "Removes just your message. Ash's answer stays where it is."
			: "Removes just this reply, plus the action results it produced. The rest of the chat stays."],
		["pair", isUser ? "This message and Ash's answer" : "This reply and your message before it", askedVersions > 1 ? branchNote(askedVersions) : "Removes the whole exchange: your message and everything Ash did to answer it. Later messages stay."],
		["after", "This and everything after it", "Removes this message and every message that follows it in this chat."]
	];
	let chosen = "pair";
	openOverlay((modal) => {
		const head = el("div", "modal-head");
		head.appendChild(el("h2", null, isUser ? "Delete your message?" : "Delete this reply?"));
		head.appendChild(iconButton("Close", "close", () => closeOverlay()));
		modal.appendChild(head);
		const body = el("div", "modal-body");
		const list = el("div", "choice-list");
		const rows = [];
		for (const [scope, title, detail] of scopes) {
			const row = el("button", "choice");
			row.type = "button";
			row.setAttribute("aria-pressed", scope === chosen ? "true" : "false");
			const text = el("div", "choice-text");
			text.appendChild(el("strong", null, title));
			text.appendChild(el("span", null, detail));
			row.appendChild(text);
			row.addEventListener("click", () => {
				chosen = scope;
				for (const item of rows) item.setAttribute("aria-pressed", item === row ? "true" : "false");
			});
			rows.push(row);
			list.appendChild(row);
		}
		body.appendChild(list);
		body.appendChild(el("p", "field-help", "You cannot undo this."));
		modal.appendChild(body);
		const foot = el("div", "modal-foot");
		foot.appendChild(button("Cancel", "ghost-button", () => closeOverlay()));
		foot.appendChild(button("Delete", "danger-button", () => {
			closeOverlay();
			deleteMessage(message, chosen);
		}, "trash"));
		modal.appendChild(foot);
		focusNode(rows[1]);
	});
};

const deleteMessage = (message, scope) => guard(async () => {
	if (busyWarning("delete messages")) return;
	const conversationId = state.conversation.id;
	await invoke("message_delete", { conversationId, messageId: message.id, scope });
	await reloadMessages(conversationId);
	await renderTranscript();
	await refreshConversations();
	notify(scope === "only" ? "Message removed." : "Messages removed.", "success");
});

const buildLiveRow = () => {
	const row = el("div", "msg msg-assistant");
	row.dataset.messageId = "live";
	row.appendChild(el("div", "msg-inner"));
	return row;
};

const applyLiveTurn = () => {
	const turn = state.liveTurn;
	const row = state.liveNode;
	if (!turn || !row) return;
	const inner = row.querySelector(".msg-inner");
	if (!inner) return;
	let body = inner.querySelector(".msg-body");
	if (!body) {
		inner.innerHTML = "";
		inner.appendChild(buildAssistantHead(Date.now()));
		if (turn.thinking && (!state.settings || state.settings.showThinking !== false)) inner.appendChild(buildThinkingBlock(turn.thinking, true));
		body = el("div", "msg-body");
		inner.appendChild(body);
		body.textContent = turn.content;
		if (!turn.content) {
			body.innerHTML = "";
			const dots = el("div", "typing");
			dots.appendChild(el("span"));
			dots.appendChild(el("span"));
			dots.appendChild(el("span"));
			body.appendChild(dots);
		}
	}
	const thinkingBody = inner.querySelector(".thinking-body");
	if (thinkingBody && turn.thinking) thinkingBody.textContent = turn.thinking;
	if (turn.statusText) {
		let status = inner.querySelector(".stream-status");
		if (!status) {
			status = el("div", "stream-status");
			status.setAttribute("aria-live", "polite");
			inner.insertBefore(status, body);
		}
		status.textContent = turn.statusText;
	} else {
		const status = inner.querySelector(".stream-status");
		status && status.remove();
	}
	if (turn.usage) {
		const line = buildUsageLine(turn.usage);
		if (line) {
			const existing = inner.querySelector(".usage-line");
			existing ? existing.replaceWith(line) : inner.appendChild(line);
		}
	}
	const chipsHost = inner.querySelector(".tool-calls");
	if (turn.toolCalls.length) {
		const chips = chipsHost || el("div", "tool-calls");
		chips.innerHTML = "";
		for (const call of turn.toolCalls) {
			const chip = el("span", "tool-call-chip", call.name || "tool");
			chip.title = call.arguments || "";
			chips.appendChild(chip);
		}
		if (!chipsHost) inner.insertBefore(chips, body);
	} else {
		chipsHost && chipsHost.remove();
	}
};

const scheduleStreamRender = (() => {
	let timer = 0;
	return () => {
		if (timer) return;
		timer = setTimeout(() => {
			timer = 0;
			const turn = state.liveTurn;
			if (!turn || !state.liveNode) return;
			const stick = state.autoScroll && isNearBottom();
			applyLiveTurn();
			const body = state.liveNode.querySelector(".msg-body");
			if (body && turn.content) {
				const token = turn.renderStamp;
				paintMarkdown(body, turn.content, token);
			}
			if (stick) scrollTranscriptToEnd(false);
			else if (turn.content) show(refs.jumpLatest, true);
		}, STREAM_RENDER_MS);
	};
})();

const startTurn = (turnId, conversationId) => {
	state.liveTurn = {
		id: turnId,
		conversationId,
		messageId: "",
		content: "",
		thinking: "",
		statusText: "",
		usage: null,
		toolCalls: [],
		renderStamp: state.renderToken,
		startedAt: Date.now()
	};
	const node = refs.transcript;
	if (node) {
		node.querySelector(".empty-state") && (node.innerHTML = "");
		const row = buildLiveRow();
		node.appendChild(row);
		state.liveNode = row;
	}
	show(refs.stopButton, true);
	setBusy(refs.sendButton, true);
	setBusy(refs.composerInput, false);
	state.autoScroll = true;
	scrollTranscriptToEnd(true);
};

const finishTurn = (options) => {
	const turn = state.liveTurn;
	show(refs.stopButton, false);
	setBusy(refs.sendButton, false);
	if (!turn) return;
	const settings = options || {};
	const message = {
		id: turn.messageId || newLocalId(),
		conversationId: turn.conversationId,
		seq: state.messages.length + 1,
		role: "assistant",
		content: turn.content,
		thinking: turn.thinking,
		toolCalls: turn.toolCalls,
		toolCallId: null,
		images: [],
		meta: turn.usage ? { usage: turn.usage } : {},
		created: Date.now()
	};
	state.liveTurn = null;
	state.liveNode = null;
	state.renderToken += 1;
	if (!settings.dropped && (turn.content || turn.thinking || !settings.error)) state.messages.push(message);
	if (settings.error) setComposerAlert(settings.error);
	else setComposerAlert("");
	guard(async () => {
		await reloadMessages(turn.conversationId);
		await renderTranscript();
		await refreshConversations();
		growComposer();
		if (!settings.error) focusNode(refs.composerInput);
	});
};

const reloadMessages = async (conversationId) => {
	const id = conversationId || (state.conversation ? state.conversation.id : "");
	if (!id || !state.conversation || state.conversation.id !== id) return;
	try {
		const messages = await invoke("messages_list", { conversationId: id });
		if (state.conversation && state.conversation.id === id) state.messages = asArray(messages).filter(isObject);
	} catch (error) {
		return;
	}
};

const adoptConversation = async (conversationId) => {
	if (!conversationId) return;
	if (state.liveTurn && !state.liveTurn.conversationId) state.liveTurn.conversationId = conversationId;
	if (state.conversation && state.conversation.id === conversationId) return;
	if (state.conversation) return;
	try {
		state.conversation = await invoke("conversations_get", { id: conversationId });
		for (const message of state.messages) if (!message.conversationId) message.conversationId = conversationId;
		renderChatHeader();
		refreshConversations();
	} catch (error) {
		return;
	}
};

const setComposerAlert = (text) => {
	const box = refs.composerAlert;
	if (!box) return;
	box.innerHTML = "";
	if (!text) {
		show(box, false);
		return;
	}
	box.appendChild(icon("info"));
	const wrap = el("div");
	wrap.appendChild(el("div", null, String(text)));
	box.appendChild(wrap);
	box.appendChild(iconButton("Dismiss this message", "close", () => setComposerAlert(""), "icon-button tiny"));
	show(box, true);
};

const sendMessage = async () => {
	if (isStreaming()) return;
	const input = refs.composerInput;
	const content = (input.value || "").trim();
	const images = state.attachments.slice();
	if (!content && !images.length) return;
	const provider = activeProvider();
	const model = activeModel();
	const think = state.think;
	const conversationId = state.conversation ? state.conversation.id : "";
	const userMessage = {
		id: newLocalId(),
		conversationId,
		seq: state.messages.length + 1,
		role: "user",
		content,
		thinking: "",
		toolCalls: [],
		toolCallId: null,
		images,
		meta: {},
		created: Date.now()
	};
	state.messages.push(userMessage);
	state.attachments = [];
	renderAttachments();
	input.value = "";
	growComposer();
	setComposerAlert("");
	const stick = isNearBottom() || state.autoScroll;
	const node = refs.transcript;
	if (node) {
		node.querySelector(".empty-state") && (node.innerHTML = "");
		node.appendChild(buildMessage(userMessage, []));
	}
	stick && scrollTranscriptToEnd(true);
	startTurn("pending", conversationId);
	const rollback = (error) => {
		state.messages = state.messages.filter((item) => item.id !== userMessage.id);
		state.attachments = images;
		renderAttachments();
		input.value = content;
		growComposer();
		finishTurn({ error, dropped: true });
	};
	await guard(async () => {
		const turnId = await invoke("chat_send", { request: { conversationId, provider, model, content, images, think: think ? true : null } });
		if (state.liveTurn) state.liveTurn.id = turnId;
		await afterSend();
	}, rollback);
};

const afterSend = async () => {
	await refreshConversations();
	if (!state.conversation) return;
	try {
		const fresh = await invoke("conversations_get", { id: state.conversation.id });
		if (state.conversation && state.conversation.id === fresh.id) state.conversation = fresh;
		renderChatHeader();
	} catch (error) {
		return;
	}
};

const titleConversation = async (conversationId) => {
	state.pendingTitleIds[conversationId] = true;
	await guard(async () => {
		const title = await invoke("chat_title", { conversationId });
		if (typeof title === "string" && title.trim() && state.conversation && state.conversation.id === conversationId) {
			state.conversation.title = title.trim();
			state.conversation.titled = true;
			renderChatHeader();
		}
		await refreshConversations();
	});
	delete state.pendingTitleIds[conversationId];
};

const stopTurn = () => guard(async () => {
	const turn = state.liveTurn;
	if (!turn || turn.id === "pending") return;
	await invoke("chat_stop", { turnId: turn.id });
	notify("Stopping…", "info");
});

const growComposer = () => {
	const input = refs.composerInput;
	if (!input) return;
	input.style.height = "auto";
	input.style.height = `${Math.min(input.scrollHeight, 260)}px`;
};

const renderAttachments = () => {
	const strip = refs.attachStrip;
	if (!strip) return;
	strip.innerHTML = "";
	show(strip, state.attachments.length > 0);
	state.attachments.forEach((url, index) => {
		const box = el("div", "attachment");
		const img = el("img");
		img.src = url;
		img.alt = `Attached image ${index + 1}`;
		box.appendChild(img);
		box.appendChild(iconButton("Remove this image", "close", () => {
			state.attachments.splice(index, 1);
			renderAttachments();
		}, "attachment-remove"));
		strip.appendChild(box);
	});
};

const pickImages = () => {
	const input = el("input");
	input.type = "file";
	input.accept = IMAGE_TYPES.join(",");
	input.multiple = true;
	input.addEventListener("change", () => {
		const files = Array.from(input.files || []);
		files.length && addImageFiles(files);
	});
	input.click();
};

const addImageFiles = (files) => {
	for (const file of files) {
		if (!file.type.startsWith("image/")) {
			warn(`${file.name} is not a picture, so it was left out.`);
			continue;
		}
		if (!IMAGE_TYPES.includes(file.type)) {
			warn(`${file.name} is in a format Ash cannot send (${file.type}). A PNG, JPEG or WebP picture works.`);
			continue;
		}
		guard(async () => {
			const url = await prepareImage(file);
			if (!url) {
				warn(`${file.name} could not be read.`);
				return;
			}
			state.attachments.push(url);
			renderAttachments();
			focusNode(refs.composerInput);
		});
	}
};

const prepareImage = (file) => new Promise((resolve) => {
	const reader = new FileReader();
	reader.onerror = () => resolve("");
	reader.onload = () => {
		const raw = typeof reader.result === "string" ? reader.result : "";
		if (!isDataUrl(raw)) {
			resolve("");
			return;
		}
		const image = new Image();
		image.onerror = () => resolve(raw);
		image.onload = () => {
			const width = image.naturalWidth || 1;
			const height = image.naturalHeight || 1;
			const longest = Math.max(width, height);
			const scale = longest > MAX_IMAGE_EDGE ? MAX_IMAGE_EDGE / longest : 1;
			if (scale === 1 && raw.length <= MAX_IMAGE_BYTES) {
				resolve(raw);
				return;
			}
			const targetWidth = Math.max(1, Math.round(width * scale));
			const targetHeight = Math.max(1, Math.round(height * scale));
			const canvas = document.createElement("canvas");
			canvas.width = targetWidth;
			canvas.height = targetHeight;
			const context = canvas.getContext("2d");
			if (!context) {
				resolve(raw);
				return;
			}
			context.drawImage(image, 0, 0, targetWidth, targetHeight);
			const keepsTransparency = file.type === "image/png" || file.type === "image/webp" || file.type === "image/gif";
			let scaled = "";
			try {
				scaled = keepsTransparency ? canvas.toDataURL("image/png") : canvas.toDataURL("image/jpeg", 0.85);
			} catch (error) {
				resolve(raw);
				return;
			}
			resolve(scaled && scaled.length < raw.length ? scaled : raw);
		};
		image.src = raw;
	};
	reader.readAsDataURL(file);
});

const handlePaste = (event) => {
	const items = event.clipboardData ? Array.from(event.clipboardData.items || []) : [];
	const files = items.filter((item) => item.kind === "file" && item.type.startsWith("image/")).map((item) => item.getAsFile()).filter(Boolean);
	if (!files.length) return;
	event.preventDefault();
	addImageFiles(files);
};

const sendWithEnter = () => !state.settings || state.settings.sendWithEnter !== false;

const handleComposerKeydown = (event) => {
	if (event.key !== "Enter") return;
	const enterSends = sendWithEnter();
	if (event.shiftKey || event.ctrlKey || event.metaKey) {
		if (!enterSends && !event.shiftKey && !event.ctrlKey && !event.metaKey) return;
		if (!enterSends) {
			event.preventDefault();
			sendMessage();
		}
		return;
	}
	if (enterSends) {
		event.preventDefault();
		sendMessage();
	}
};

const renderComposerHints = () => {
	const enterSends = sendWithEnter();
	const hint = enterSends ? "Enter sends, Shift and Enter adds a line." : "Shift and Enter sends, Enter adds a line.";
	if (refs.composerHintText) refs.composerHintText.textContent = hint;
	if (refs.sendHint) refs.sendHint.textContent = enterSends ? "Ctrl and Enter also sends" : "Enter adds a line";
};

const openModelPicker = async (initialProvider) => {
	const provider = providerOptions().some((option) => option.id === initialProvider) ? initialProvider : activeProvider();
	openOverlay((modal) => {
		const head = el("div", "modal-head");
		head.appendChild(el("h2", null, "Choose a model"));
		head.appendChild(iconButton("Close", "close", () => closeOverlay()));
		modal.appendChild(head);
		const body = el("div", "modal-body");
		const providerField = el("div", "field");
		providerField.appendChild(el("label", "field-label", "Where the model runs"));
		const select = el("select", "select");
		select.setAttribute("aria-label", "Provider");
		for (const option of providerOptions()) {
			const item = el("option", null, option.name);
			item.value = option.id;
			select.appendChild(item);
		}
		select.value = provider;
		providerField.appendChild(select);
		body.appendChild(providerField);
		const search = el("input", "text-input");
		search.type = "search";
		search.placeholder = "Filter models";
		search.spellcheck = false;
		search.setAttribute("aria-label", "Filter models");
		const searchField = el("div", "field hidden");
		searchField.appendChild(search);
		body.appendChild(searchField);
		const list = el("div", "option-list");
		body.appendChild(list);
		const note = el("p", "card-note");
		body.appendChild(note);
		modal.appendChild(body);
		const manage = button("Manage local models", "ghost-button", () => {
			closeOverlay();
			setView("models");
		}, "cpu");
		let items = [];
		let current = provider;
		const paintList = () => {
			const query = search.value.trim().toLowerCase();
			const chosen = modelFor(current);
			const visible = query ? items.filter((model) => `${model.name || ""} ${model.id}`.toLowerCase().includes(query)) : items;
			list.innerHTML = "";
			if (!visible.length && items.length) {
				list.appendChild(el("p", "card-note", `No model matches “${search.value.trim()}”.`));
				return;
			}
			for (const model of visible) {
				const row = el("button", "option");
				row.type = "button";
				row.setAttribute("aria-selected", model.id === chosen ? "true" : "false");
				const text = el("span", "option-text");
				text.appendChild(el("strong", null, model.name || model.id));
				const details = [];
				model.parameterSize && details.push(model.parameterSize);
				model.quantization && details.push(model.quantization);
				model.format && details.push(model.format);
				numberOr(model.sizeBytes, 0) > 0 && details.push(formatBytes(model.sizeBytes));
				model.loaded && details.push("loaded right now");
				model.contextLength && details.push(`context ${formatCount(model.contextLength)}`);
				const capabilities = [];
				model.vision && capabilities.push("sees images");
				model.tools && capabilities.push("tools");
				model.thinking && capabilities.push("thinking");
				const line = details.concat(capabilities).join(" · ");
				line && text.appendChild(el("span", null, line));
				row.appendChild(text);
				row.appendChild(el("span", "option-check", model.id === chosen ? "✓" : ""));
				row.addEventListener("click", () => {
					closeOverlay();
					chooseModel(current, model.id);
				});
				list.appendChild(row);
			}
		};
		const paint = async (providerId) => {
			current = providerId;
			items = [];
			search.value = "";
			show(searchField, false);
			show(manage, providerId === "ml5");
			list.innerHTML = "";
			const loading = el("div", "row");
			loading.appendChild(el("span", "spinner"));
			loading.appendChild(el("span", "row-sub", "Looking for models…"));
			list.appendChild(loading);
			const found = await loadModels(providerId, true);
			if (current !== providerId) return;
			items = found;
			list.innerHTML = "";
			const error = state.modelErrors[providerId];
			if (!items.length) {
				const empty = el("div", "empty-state");
				empty.style.padding = "var(--space-5) 0";
				empty.appendChild(el("strong", null, error ? "The model list could not be read" : "No models are listed yet"));
				empty.appendChild(el("p", null, error || (providerId === "ml5" ? "The local engine has no models installed. Download one and it will appear here." : "This provider did not return any models.")));
				providerId === "ml5" && empty.appendChild(button("Get models", "ghost-button small", () => {
					closeOverlay();
					setView("models");
				}, "download"));
				list.appendChild(empty);
			} else {
				show(searchField, items.length > 8);
				paintList();
			}
			note.textContent = providerId === "ml5" ? "Local models run on this computer and keep working without an internet connection." : "Messages go to the provider you choose here.";
		};
		search.addEventListener("input", () => paintList());
		search.addEventListener("keydown", (event) => {
			if (event.key !== "Enter") return;
			event.preventDefault();
			const first = list.querySelector(".option");
			first && first.click();
		});
		select.addEventListener("change", () => paint(select.value));
		const foot = el("div", "modal-foot");
		foot.appendChild(manage);
		foot.appendChild(el("span", "spacer"));
		foot.appendChild(button("Close", "ghost-button", () => closeOverlay()));
		modal.appendChild(foot);
		paint(provider);
	});
};

const loadModels = async (provider, force) => {
	if (!force && state.models[provider]) return state.models[provider];
	try {
		const list = await invoke("models_available", { provider });
		state.models[provider] = asArray(list).filter(isObject);
		delete state.modelErrors[provider];
	} catch (error) {
		state.modelErrors[provider] = String(error);
		state.models[provider] = [];
	}
	return state.models[provider];
};

const chooseModel = (provider, model) => guard(async () => {
	const draft = settingsClone();
	draft.provider = provider;
	draft.models = { ...(draft.models || {}), [provider]: model };
	if (state.conversation && state.conversation.provider === provider) {
		state.conversation.model = model;
		await invoke("conversations_update", { conversation: { ...state.conversation, snippet: null } });
	}
	await saveSettings({ silent: true, patch: draft });
	renderChatHeader();
	renderModelsScreen();
	notify(`${model} is now the model in use.`, "success");
});

const loadToolCatalog = () => guard(async () => {
	const list = await invoke("tool_catalog");
	state.toolCatalog = asArray(list).filter(isObject);
	if (state.extras === "tools") renderExtras();
});

const openTierDialog = () => {
	openOverlay((modal) => {
		const head = el("div", "modal-head");
		head.appendChild(el("h2", null, "What may Ash do on this computer?"));
		head.appendChild(iconButton("Close", "close", () => closeOverlay()));
		modal.appendChild(head);
		const body = el("div", "modal-body");
		body.appendChild(el("p", "field-help", "This decides how much Ash is allowed to change. You can change it whenever you like; the toolbar always shows the current setting."));
		const cards = el("div", "tier-cards");
		body.appendChild(cards);
		const paint = (current) => {
			cards.innerHTML = "";
			for (const tier of TIERS) {
				const card = el("button", "tier-card");
				card.type = "button";
				card.setAttribute("aria-pressed", tier.id === current ? "true" : "false");
				card.appendChild(el("strong", null, `${tier.label}: ${tier.short}`));
				card.appendChild(el("span", null, tier.blurb));
				card.addEventListener("click", () => guard(async () => {
					const saved = await invoke("tools_set_tier", { tier: tier.id });
					state.settings = saved;
					paint(tier.id);
					renderChatHeader();
					if (state.view === "settings") renderSettings();
					notify(`Permission tier set to ${tier.label}.`, "success");
				}));
				cards.appendChild(card);
			}
		};
		paint(currentTier());
		modal.appendChild(body);
		const foot = el("div", "modal-foot");
		foot.appendChild(button("Done", "primary-button", () => closeOverlay()));
		modal.appendChild(foot);
	});
};

const renderExtras = () => {
	const chatView = refs.viewChat;
	if (!chatView) return;
	const existing = chatView.querySelector(".extras-panel");
	existing && existing.remove();
	if (state.extras === "none") return;
	const showingTools = state.extras === "tools";
	const panel = el("aside", "extras-panel");
	panel.setAttribute("aria-label", showingTools ? "What Ash may do" : "What Ash can see");
	const head = el("header", "panel-head");
	const title = el("div", "panel-title");
	title.appendChild(icon(showingTools ? "shield" : "image"));
	title.appendChild(el("span", null, showingTools ? "What Ash may do" : "What Ash can see"));
	head.appendChild(title);
	const headActions = el("div", "panel-head-actions");
	headActions.appendChild(iconButton("Close this panel", "close", () => {
		state.extras = "none";
		renderExtras();
	}));
	head.appendChild(headActions);
	panel.appendChild(head);
	const scroll = el("div", "extras-scroll");
	panel.appendChild(scroll);
	if (showingTools) {
		const info = tierInfo(currentTier());
		scroll.appendChild(el("h3", null, "Permission right now"));
		const card = el("div", "card");
		card.appendChild(el("h2", null, `${info.label}: ${info.short}`));
		card.appendChild(el("p", "card-note", info.blurb));
		card.appendChild(button("Change this", "ghost-button small", () => openTierDialog(), "sliders"));
		scroll.appendChild(card);
		scroll.appendChild(el("h3", null, "Tools available now"));
		const list = el("div", "tools-list");
		if (!state.toolCatalog.length) {
			const empty = el("div", "empty-state");
			empty.style.padding = "var(--space-5) 0";
			empty.appendChild(el("p", null, "No tools are available at this permission level. Raising it lets Ash do more, and Ash will still ask before changing anything below Trusted."));
			empty.appendChild(button("Raise the permission level", "primary-button small", () => openTierDialog(), "shield"));
			list.appendChild(empty);
		}
		const groups = new Map();
		for (const tool of state.toolCatalog) {
			const key = tool.group || "General";
			groups.has(key) ? groups.get(key).push(tool) : groups.set(key, [tool]);
		}
		for (const [group, tools] of groups) {
			list.appendChild(el("h3", null, prettyKey(group)));
			for (const tool of tools) {
				const row = el("div", "tool-row");
				const text = el("div", "row-sub");
				text.appendChild(el("strong", null, tool.name || "tool"));
				text.appendChild(el("span", "row-sub", tool.description || ""));
				row.appendChild(text);
				tool.root && row.appendChild(el("span", "badge danger", "administrator"));
				row.appendChild(buildRiskBadge(tool.risk));
				list.appendChild(row);
			}
		}
		scroll.appendChild(list);
		const refresh = button("Check again", "ghost-button small", () => loadToolCatalog(), "retry");
		scroll.appendChild(refresh);
		return;
	}
	scroll.appendChild(el("h3", null, "This computer"));
	const card = el("div", "card");
	const status = state.computerStatus;
	if (isObject(status)) {
		const kv = el("dl", "kv");
		const pushPairs = (source, prefix) => {
			for (const [key, value] of Object.entries(source)) {
				if (isObject(value)) {
					pushPairs(value, prefix ? `${prefix} · ${prettyKey(key)}` : prettyKey(key));
					continue;
				}
				if (Array.isArray(value)) {
					kv.appendChild(el("dt", null, prefix ? `${prefix} · ${prettyKey(key)}` : prettyKey(key)));
					kv.appendChild(el("dd", null, `${value.length} entries`));
					continue;
				}
				kv.appendChild(el("dt", null, prefix ? `${prefix} · ${prettyKey(key)}` : prettyKey(key)));
				kv.appendChild(el("dd", null, String(value)));
			}
		};
		pushPairs(status, "");
		kv.children.length ? card.appendChild(kv) : card.appendChild(el("p", "card-note", "This machine reported no details."));
	} else {
		card.appendChild(el("p", "card-note", state.computerStatusError || "Ask Ash for a summary of this computer and the details will appear here."));
	}
	card.appendChild(button(state.computerStatus ? "Refresh" : "Look now", "ghost-button small", () => refreshComputerStatus(), "retry"));
	scroll.appendChild(card);
	scroll.appendChild(el("h3", null, "Pictures Ash has seen"));
	const shots = el("div", "row-list");
	const recent = state.actions.filter((action) => action.imageId).slice(0, 8);
	if (!recent.length) {
		const empty = el("div", "empty-state");
		empty.style.padding = "var(--space-5) 0";
		empty.appendChild(el("p", null, "When Ash looks at your screen, the picture shows up here so you can check exactly what it saw."));
		empty.appendChild(button("Ask Ash to look now", "ghost-button small", () => {
			refs.composerInput.value = "Please look at my screen and tell me what you see.";
			growComposer();
			focusNode(refs.composerInput);
		}, "image"));
		shots.appendChild(empty);
	}
	for (const action of recent) {
		const shot = el("button", "shot");
		shot.type = "button";
		shot.dataset.shotId = action.imageId;
		const img = el("img");
		img.alt = "Screenshot from an earlier action";
		img.loading = "lazy";
		shot.appendChild(img);
		shot.appendChild(el("span", "shot-label", `${action.title || action.tool} · ${relativeTime(action.created)}`));
		shot.addEventListener("click", () => openActionImage(action.imageId, shot.dataset.shotUrl || ""));
		loadActionImage(action.imageId).then((url) => {
			if (!url) return;
			img.src = url;
			shot.dataset.shotUrl = url;
		});
		shots.appendChild(shot);
	}
	scroll.appendChild(shots);
	if (state.conversation) {
		scroll.appendChild(el("h3", null, "Actions in this chat"));
		const list = el("div", "row-list");
		const related = state.actions.filter((action) => action.conversationId === state.conversation.id);
		if (!related.length) {
			list.appendChild(el("p", "card-note", "Nothing has been changed in this chat yet."));
		}
		for (const action of related.slice(0, 10)) list.appendChild(buildActionLogRow(action));
		scroll.appendChild(list);
	}
};

const refreshComputerStatus = () => guard(async () => {
	const status = await invoke("computer_status");
	state.computerStatus = status;
	state.computerStatusError = "";
	if (state.extras === "view") renderExtras();
}, (error) => {
	state.computerStatusError = String(error);
	state.computerStatus = null;
	if (state.extras === "view") renderExtras();
	warn(error);
});

const daemonTimer = { id: 0 };

const startDaemonPolling = () => {
	stopDaemonPolling();
	daemonTimer.id = setInterval(() => {
		if (state.view !== "models") {
			stopDaemonPolling();
			return;
		}
		guard(async () => {
			const status = await invoke("ml5_status", { force: true });
			state.daemon = status;
			if (state.view === "models") renderModelsScreen();
		}, () => {
			stopDaemonPolling();
		});
	}, 2000);
};

const stopDaemonPolling = () => {
	if (daemonTimer.id) {
		clearInterval(daemonTimer.id);
		daemonTimer.id = 0;
	}
};

const refreshModelsScreen = async (force) => {
	const daemon = await guard(async () => invoke("ml5_status", { force: Boolean(force) }));
	if (daemon) state.daemon = daemon;
	if (!state.favoritesLoaded) {
		await guard(async () => {
			const list = asArray(await invoke("model_favorites"));
			const featured = list.filter((item) => item.featured);
			const advanced = list.filter((item) => item.advanced);
			state.favorites = featured.length || advanced.length ? { featured, advanced } : { featured: list.slice(0, 6), advanced: list.slice(6) };
			state.favoritesLoaded = true;
		});
	}
	if (state.daemon && state.daemon.running) await loadModels("ml5", force);
	if (state.view === "models") renderModelsScreen();
};

const renderModelsScreen = () => {
	const root = refs.viewModels;
	if (!root) return;
	root.innerHTML = "";
	const screen = el("div", "screen");
	const head = el("div", "screen-head");
	const titleBox = el("div");
	titleBox.appendChild(el("h1", null, "Models"));
	titleBox.appendChild(el("p", null, "Ash thinks with the ML5 engine on this computer, or with a cloud provider you connect. Everything here is under your control."));
	head.appendChild(titleBox);
	const headActions = el("div", "screen-head-actions");
	headActions.appendChild(button("Refresh", "ghost-button", () => refreshModelsScreen(true), "retry"));
	head.appendChild(headActions);
	screen.appendChild(head);
	screen.appendChild(buildDaemonCard());
	screen.appendChild(buildInstalledCard());
	screen.appendChild(buildCatalogCard());
	screen.appendChild(buildProvidersCard());
	root.appendChild(screen);
};

const buildDaemonCard = () => {
	const card = el("section", "card");
	const head = el("div", "card-head");
	head.appendChild(el("h2", null, "Local engine (ML5)"));
	const daemon = state.daemon;
	const running = Boolean(daemon && daemon.running);
	const badge = el("span", "badge");
	badge.classList.add(running ? "ok" : "danger");
	badge.textContent = running ? "Running" : "Not running";
	head.appendChild(badge);
	if (!state.daemon) head.appendChild(el("span", "spinner"));
	head.appendChild(el("span", "spacer"));
	const actions = el("div", "row-actions");
	const start = button("Start", "primary-button small", () => daemonAction("ml5_start", "The local engine is running."));
	const stop = button("Stop", "ghost-button small", () => daemonAction("ml5_stop", "The local engine stopped."));
	start.disabled = running;
	stop.disabled = !running;
	actions.appendChild(start);
	actions.appendChild(stop);
	head.appendChild(actions);
	card.appendChild(head);
	if (!running) {
		card.appendChild(el("p", "card-note", "The local engine is not running. Start it to use models stored on this computer; nothing leaves your machine. If it is not installed at all, Start will say so."));
		const help = el("div", "empty-actions");
		help.appendChild(button("Check again", "ghost-button small", () => refreshModelsScreen(true), "retry"));
		help.appendChild(button("Use a cloud provider instead", "ghost-button small", () => setView("settings"), "sliders"));
		card.appendChild(help);
		return card;
	}
	const facts = el("div", "stat-grid");
	const add = (label, value) => {
		const stat = el("div", "stat");
		stat.appendChild(el("span", "stat-label", label));
		stat.appendChild(el("span", "stat-value", value));
		facts.appendChild(stat);
	};
	add("Version", daemon.version || "unknown");
	add("Uptime", daemon.uptimeSecs ? formatShortDuration(daemon.uptimeSecs) : "just started");
	if (daemon.runtime) add("Runtime", daemon.runtime);
	add("Parallel slots", daemon.nParallel ? formatCount(daemon.nParallel) : "automatic");
	add("Max loaded", daemon.maxLoadedModels ? formatCount(daemon.maxLoadedModels) : "automatic");
	add("Context ceiling", daemon.autoCtxMax ? formatCount(daemon.autoCtxMax) : "automatic");
	daemon.keepAliveSecs ? add("Keep alive", formatShortDuration(daemon.keepAliveSecs)) : null;
	daemon.modelsDir && add("Model folder", daemon.modelsDir);
	card.appendChild(facts);
	if (daemon.memory) card.appendChild(buildMemorySection(daemon.memory));
	card.appendChild(buildPullsSection());
	const loading = asArray(daemon.loading);
	if (loading.length) {
		card.appendChild(el("h2", null, "Loading now"));
		const list = el("div", "row-list");
		for (const name of loading) {
			const row = el("div", "row");
			row.appendChild(el("span", "spinner"));
			row.appendChild(el("span", "row-title", name));
			row.appendChild(el("span", "row-sub", "Reading the model into memory…"));
			list.appendChild(row);
		}
		card.appendChild(list);
	}
	card.appendChild(el("h2", null, "Models in memory"));
	const list = el("div", "row-list");
	const loaded = asArray(daemon.loaded);
	if (!loaded.length) {
		const empty = el("div", "empty-state");
		empty.style.padding = "var(--space-5) 0";
		empty.appendChild(el("p", null, "No model is loaded at the moment. The engine loads one when you send a message, or you can load one below."));
		empty.appendChild(button("Load one now", "ghost-button small", () => openModelPicker(), "cpu"));
		list.appendChild(empty);
	}
	for (const model of loaded) list.appendChild(buildLoadedRow(model));
	card.appendChild(list);
	return card;
};

const buildLoadedRow = (model) => {
	const row = el("div", "row");
	const main = el("div", "row-main");
	const title = el("span", "row-title", model.displayName || model.name || "model");
	model.vision && title.appendChild(el("span", "badge accent", "sees images"));
	numberOr(model.activeRequests, 0) > 0 && title.appendChild(el("span", "badge ok", `${formatCount(model.activeRequests)} working now`));
	main.appendChild(title);
	const bits = [];
	numberOr(model.context, 0) > 0 && bits.push(`context ${formatCount(model.context)}`);
	numberOr(model.parallel, 0) > 0 && bits.push(`${formatCount(model.parallel)} slots`);
	model.gpuLayers && bits.push(`GPU layers ${model.gpuLayers}`);
	model.backend && bits.push(model.backend);
	numberOr(model.memoryBytes, 0) > 0 && bits.push(`${formatBytes(model.memoryBytes)} of memory`);
	Number.isFinite(Number(model.expiresInSecs)) && Number(model.expiresInSecs) >= 0 && bits.push(`unloads in ${formatShortDuration(model.expiresInSecs)}`);
	numberOr(model.idleSecs, 0) > 0 && bits.push(`idle ${formatShortDuration(model.idleSecs)}`);
	main.appendChild(el("span", "row-sub", bits.filter(Boolean).join(" · ") || model.name));
	row.appendChild(main);
	const actions = el("div", "row-actions");
	actions.appendChild(button("Unload", "ghost-button small", () => modelAction("ml5_unload", model.name, "Model unloaded from memory.")));
	row.appendChild(actions);
	return row;
};

const buildMemorySection = (memory) => {
	const wrap = el("div");
	wrap.style.marginTop = "var(--space-5)";
	const total = numberOr(memory.totalBytes, 0);
	const available = numberOr(memory.availableBytes, 0);
	const gpuTotal = numberOr(memory.gpuTotalBytes, 0);
	const gpuFree = numberOr(memory.gpuFreeBytes, 0);
	const used = Math.max(0, total - available);
	const head = el("div", "card-head");
	head.appendChild(el("h2", null, "Memory"));
	head.appendChild(el("span", "spacer"));
	head.appendChild(el("span", "badge", total ? `${formatBytes(used)} of ${formatBytes(total)} in use` : "no memory reported"));
	wrap.appendChild(head);
	if (total) {
		const bar = el("div", "bar");
		const usedSeg = el("div", "bar-seg soft");
		usedSeg.style.width = `${clamp((used / total) * 100, 0, 100)}%`;
		const freeSeg = el("div", "bar-seg accent");
		freeSeg.style.width = `${clamp((available / total) * 100, 0, 100)}%`;
		bar.appendChild(usedSeg);
		bar.appendChild(freeSeg);
		wrap.appendChild(bar);
		const legend = el("div", "bar-legend");
		legend.appendChild(legendItem("var(--accent)", `Free ${formatBytes(available)}`));
		legend.appendChild(legendItem("var(--surface-3)", `In use ${formatBytes(used)}`));
		numberOr(memory.committedBytes, 0) > 0 && legend.appendChild(legendItem("var(--warn)", `Committed ${formatBytes(memory.committedBytes)}`));
		wrap.appendChild(legend);
	}
	if (gpuTotal) {
		const gpuBar = el("div", "bar");
		gpuBar.style.marginTop = "var(--space-3)";
		const seg = el("div", "bar-seg gpu");
		seg.style.width = `${clamp(((gpuTotal - gpuFree) / gpuTotal) * 100, 0, 100)}%`;
		gpuBar.appendChild(seg);
		wrap.appendChild(gpuBar);
		const legend = el("div", "bar-legend");
		legend.appendChild(el("span", "legend-item", `Graphics memory: ${formatBytes(gpuFree)} free of ${formatBytes(gpuTotal)}`));
		wrap.appendChild(legend);
	} else {
		wrap.appendChild(el("p", "card-note", "No graphics memory was reported, so models run on the processor."));
	}
	return wrap;
};

const legendItem = (color, text) => {
	const node = el("span", "legend-item");
	const swatch = el("span", "legend-swatch");
	swatch.style.background = color;
	node.appendChild(swatch);
	node.appendChild(el("span", null, text));
	return node;
};

const buildPullsSection = () => {
	const wrap = el("div");
	const fromDaemon = asArray(state.daemon && state.daemon.pulls);
	const merged = new Map();
	for (const pull of fromDaemon.concat(state.pulls)) merged.set(pull.id, { ...(merged.get(pull.id) || {}), ...pull });
	const items = Array.from(merged.values());
	if (!items.length) return wrap;
	wrap.appendChild(el("h2", null, "Downloads"));
	const list = el("div", "row-list");
	for (const pull of items) {
		const row = el("div", "pull-row");
		const total = numberOr(pull.total, 0);
		const downloaded = numberOr(pull.downloaded, 0);
		const head = el("div", "pull-head");
		head.appendChild(el("span", "pull-name", pull.file || pull.target || pull.id || "download"));
		const meta = el("span", "pull-meta");
		meta.textContent = total ? `${formatBytes(downloaded)} of ${formatBytes(total)} · ${Math.round(clamp((downloaded / total) * 100, 0, 100))}%` : formatBytes(downloaded);
		head.appendChild(meta);
		row.appendChild(head);
		const progress = el("div", "progress");
		const fill = el("div", "progress-fill");
		total ? (fill.style.width = `${clamp((downloaded / total) * 100, 0, 100)}%`) : fill.classList.add("indeterminate");
		progress.appendChild(fill);
		row.appendChild(progress);
		const foot = el("div", "pull-head");
		foot.appendChild(el("span", "row-sub", pull.stage || "Downloading…"));
		foot.appendChild(el("span", "spacer"));
		foot.appendChild(button("Cancel", "ghost-button small danger-text", () => cancelPull(pull.id), "stop"));
		row.appendChild(foot);
		list.appendChild(row);
	}
	wrap.appendChild(list);
	return wrap;
};

const cancelPull = (id) => guard(async () => {
	await invoke("ml5_pull_cancel", { id });
	state.pulls = state.pulls.filter((pull) => pull.id !== id);
	renderModelsScreen();
	notify("Download cancelled.", "info");
});

const startPull = (item) => guard(async () => {
	if (!state.daemon || !state.daemon.running) {
		warn("Start the local engine first, then download models.");
		return;
	}
	const pullId = await invoke("ml5_pull", { reference: item.reference, registry: item.registry || "core" });
	state.pulls = [{ id: pullId, target: item.reference, file: item.name || item.reference, stage: "Getting ready…", downloaded: 0, total: null }].concat(state.pulls.filter((pull) => pull.id !== pullId));
	renderModelsScreen();
	notify(`Downloading ${item.name || item.reference}. You can keep using Ash meanwhile.`, "info");
});

const buildInstalledCard = () => {
	const card = el("section", "card");
	const models = state.models.ml5 || [];
	const head = el("div", "card-head");
	head.appendChild(el("h2", null, "Models on this computer"));
	head.appendChild(el("span", "badge", models.length === 1 ? "1 model" : `${models.length} models`));
	head.appendChild(el("span", "spacer"));
	head.appendChild(button("Rescan folder", "ghost-button small", () => guard(async () => {
		await invoke("ml5_rescan");
		await loadModels("ml5", true);
		await refreshModelsScreen(true);
		notify("Model folder rescanned.", "success");
	}), "retry"));
	card.appendChild(head);
	const list = el("div", "row-list");
	if (!models.length) {
		const empty = el("div", "empty-state");
		empty.style.padding = "var(--space-5) 0";
		empty.appendChild(el("strong", null, state.modelErrors.ml5 ? "The model list could not be read" : "No models downloaded yet"));
		empty.appendChild(el("p", null, state.modelErrors.ml5 || "The local engine has no models yet. Pick one of the recommendations below; they work offline once downloaded."));
		empty.appendChild(button("See recommendations", "primary-button small", () => {
			const catalog = refs.viewModels && refs.viewModels.querySelector('[data-catalog="true"]');
			catalog ? catalog.scrollIntoView({ block: "start", behavior: "smooth" }) : null;
		}, "download"));
		list.appendChild(empty);
	}
	for (const model of models) list.appendChild(buildInstalledRow(model));
	card.appendChild(list);
	card.appendChild(el("p", "card-note", "Renaming only changes the label you see here; the file itself is untouched."));
	return card;
};

const buildInstalledRow = (model) => {
	const row = el("div", "row");
	const main = el("div", "row-main");
	const title = el("span", "row-title", model.name || model.id);
	model.loaded && title.appendChild(el("span", "badge ok", "loaded"));
	main.appendChild(title);
	const bits = [];
	model.parameterSize && bits.push(model.parameterSize);
	model.quantization && bits.push(model.quantization);
	model.format && bits.push(model.format);
	numberOr(model.sizeBytes, 0) > 0 && bits.push(formatBytes(model.sizeBytes));
	model.contextLength && bits.push(`context ${formatCount(model.contextLength)}`);
	model.architecture && bits.push(model.architecture);
	main.appendChild(el("span", "row-sub", bits.filter(Boolean).join(" · ") || model.id));
	const chips = el("div", "tag-row");
	model.vision && chips.appendChild(el("span", "badge accent", "sees images"));
	model.tools && chips.appendChild(el("span", "badge accent", "uses tools"));
	model.thinking && chips.appendChild(el("span", "badge accent", "thinks first"));
	model.modifiedAt && chips.appendChild(el("span", "badge", `changed ${relativeTime(Date.parse(model.modifiedAt)) || model.modifiedAt}`));
	chips.children.length && main.appendChild(chips);
	row.appendChild(main);
	const actions = el("div", "row-actions");
	model.loaded
		? actions.appendChild(button("Unload", "ghost-button small", () => modelAction("ml5_unload", model.id, "Model unloaded from memory.")))
		: actions.appendChild(button("Load", "primary-button small", () => modelAction("ml5_load", model.id, "Model loaded into memory.")));
	actions.appendChild(button("Rename", "ghost-button small", () => promptDialog({
		title: "Rename this model",
		label: "New name",
		value: model.name || model.id,
		help: `The file stays as ${model.id}. Only the label changes.`,
		required: true,
		onSubmit: async (value) => {
			await invoke("ml5_rename", { model: model.id, newName: value });
			await loadModels("ml5", true);
			await refreshModelsScreen(true);
			notify("Model renamed.", "success");
		}
	}), "edit"));
	actions.appendChild(button("Delete file", "danger-button small", () => confirmDialog({
		title: "Delete this model file?",
		body: `“${model.name || model.id}” will be removed from this computer to free up space. You can download it again from the list below.`,
		confirmLabel: "Delete the file",
		danger: true,
		onConfirm: async () => {
			await invoke("ml5_delete", { model: model.id });
			await loadModels("ml5", true);
			await refreshModelsScreen(true);
			notify("Model deleted.", "success");
		}
	}), "trash"));
	row.appendChild(actions);
	return row;
};

const buildCatalogCard = () => {
	const card = el("section", "card");
	card.dataset.catalog = "true";
	const head = el("div", "card-head");
	head.appendChild(el("h2", null, "Get a model"));
	head.appendChild(el("span", "spacer"));
	card.appendChild(head);
	const groups = [
		["featured", "Recommended", "A good place to start. These run well on ordinary laptops."],
		["advanced", "More options", "Smaller, larger or more specialised models."]
	];
	let painted = false;
	for (const [key, title, note] of groups) {
		const items = asArray(state.favorites[key]);
		if (!items.length) continue;
		painted = true;
		const group = el("div", "catalog-group");
		group.appendChild(el("h3", "catalog-title", title));
		group.appendChild(el("p", "card-note", note));
		const grid = el("div", "model-grid");
		for (const item of items) grid.appendChild(buildCatalogItem(item));
		group.appendChild(grid);
		card.appendChild(group);
	}
	if (!painted) {
		const empty = el("div", "empty-state");
		empty.style.padding = "var(--space-5) 0";
		empty.appendChild(el("strong", null, "The recommendation list is unavailable"));
		empty.appendChild(el("p", null, "The built-in list of models could not be read. You can still connect a cloud provider below."));
		empty.appendChild(button("Set up a provider", "ghost-button small", () => setView("settings"), "sliders"));
		card.appendChild(empty);
	}
	return card;
};

const buildCatalogItem = (item) => {
	const card = el("div", "model-card");
	card.appendChild(el("h3", null, item.name || item.reference));
	card.appendChild(el("p", null, item.description || ""));
	const chips = el("div", "tag-row");
	item.size && chips.appendChild(el("span", "badge accent", item.size));
	for (const tag of asArray(item.tags)) chips.appendChild(el("span", "badge", tag === "opencore" ? "by CORe" : tag));
	card.appendChild(chips);
	const foot = el("div", "model-card-foot");
	const get = button("Get", "primary-button small", () => startPull(item), "download");
	if (!state.daemon || !state.daemon.running) {
		get.disabled = true;
		get.title = "Start the local engine first";
	}
	foot.appendChild(get);
	const source = el("span", "model-source", item.reference);
	source.title = item.registry === "hf" ? `${item.reference} on Hugging Face` : item.reference;
	foot.appendChild(source);
	card.appendChild(foot);
	return card;
};

const buildProvidersCard = () => {
	const card = el("section", "card");
	const head = el("div", "card-head");
	head.appendChild(el("h2", null, "Cloud and other providers"));
	head.appendChild(el("span", "spacer"));
	head.appendChild(button("Add a provider", "primary-button small", () => openEndpointEditor(null), "plus"));
	card.appendChild(head);
	card.appendChild(el("p", "card-note", "Use a cloud provider for stronger answers, or a server you run yourself that speaks the OpenAI chat API. Your key goes into your system keyring; if there is no keyring, Ash keeps it in a private file only you can read."));
	const list = el("div", "row-list");
	for (const provider of providerOptions()) list.appendChild(buildProviderRow(provider));
	card.appendChild(list);
	return card;
};

const buildProviderRow = (provider) => {
	const row = el("div", "row");
	const main = el("div", "row-main");
	const title = el("span", "row-title", provider.name);
	const health = state.health[provider.id];
	const badge = el("span", "badge");
	if (!health) {
		badge.textContent = "not checked yet";
	} else if (health.online) {
		badge.classList.add("ok");
		badge.textContent = Number.isFinite(Number(health.latencyMs)) ? `connected · ${formatCount(health.latencyMs)} ms` : "connected";
	} else {
		badge.classList.add("danger");
		badge.textContent = "not reachable";
	}
	title.appendChild(badge);
	state.settings && state.settings.provider === provider.id && title.appendChild(el("span", "badge accent", "in use now"));
	main.appendChild(title);
	const detail = [];
	const chosen = modelFor(provider.id);
	detail.push(chosen ? `model: ${chosen}` : "no model chosen yet");
	const endpoint = endpointFor(provider.id);
	if (endpoint) detail.push(endpoint.baseUrl);
	if (provider.id === "ml5") detail.push("runs on this computer");
	if (provider.id === "opencore") detail.push("cloud service over the internet");
	if (health && !health.online && health.message) detail.push(health.message);
	main.appendChild(el("span", "row-sub", detail.join(" · ")));
	if (endpoint) {
		const chips = el("div", "tag-row");
		endpoint.tools && chips.appendChild(el("span", "badge", "tools"));
		endpoint.vision && chips.appendChild(el("span", "badge", "sees images"));
		chips.children.length && main.appendChild(chips);
	}
	row.appendChild(main);
	const actions = el("div", "row-actions");
	actions.appendChild(button("Test", "ghost-button small", () => testProvider(provider.id), "check"));
	actions.appendChild(button("Choose model", "ghost-button small", () => openModelPicker(provider.id), "cpu"));
	if (provider.id === "opencore") actions.appendChild(button("API key", "ghost-button small", () => openKeyDialog(provider.id), "shield"));
	if (endpoint) {
		actions.appendChild(button("Edit", "ghost-button small", () => openEndpointEditor(endpoint), "edit"));
		actions.appendChild(iconButton(`Remove ${provider.name}`, "trash", () => removeEndpoint(endpoint), "icon-button danger"));
	}
	const inUse = state.settings && state.settings.provider === provider.id;
	const use = button(inUse ? "In use" : "Use this one", inUse ? "ghost-button small" : "primary-button small", () => chooseProvider(provider.id));
	use.disabled = Boolean(inUse);
	actions.appendChild(use);
	row.appendChild(actions);
	return row;
};

const chooseProvider = (provider) => guard(async () => {
	const draft = settingsClone();
	draft.provider = provider;
	await saveSettings({ silent: true, patch: draft });
	await testProvider(provider, true);
	renderModelsScreen();
	renderChatHeader();
	notify(`${providerLabel(provider)} is now the provider Ash uses.`, "success");
});

const testProvider = (provider, quiet) => guard(async () => {
	const result = await invoke("model_health", { provider });
	state.health[provider] = result;
	renderModelsScreen();
	if (!quiet) result.online ? notify(`${providerLabel(provider)}: ${result.message}`, "success") : warn(`${providerLabel(provider)}: ${result.message}`);
	return result;
});

const openEndpointEditor = async (endpoint) => {
	const editing = Boolean(endpoint);
	const draft = endpoint ? { ...endpoint } : { id: "", name: "", baseUrl: "", tools: true, vision: false };
	const keyStatus = editing ? await guard(async () => invoke("secret_status", { provider: endpoint.id })) : null;
	const hasKey = Boolean(keyStatus && keyStatus.hasKey);
	openOverlay((modal) => {
		const head = el("div", "modal-head");
		head.appendChild(el("h2", null, editing ? `Edit ${endpoint.name || "this provider"}` : "Add a provider"));
		head.appendChild(iconButton("Close", "close", () => closeOverlay()));
		modal.appendChild(head);
		const body = el("div", "modal-body");
		body.appendChild(el("p", "field-help", "Any server that speaks the OpenAI chat API works here: a cloud service, a machine on your home network, or a local proxy."));
		const textField = (labelText, value, placeholder, help, aria) => {
			const field = el("div", "field");
			field.appendChild(el("label", "field-label", labelText));
			const input = el("input", "text-input");
			input.value = value || "";
			input.placeholder = placeholder;
			input.spellcheck = false;
			input.setAttribute("aria-label", aria);
			field.appendChild(input);
			help && field.appendChild(el("span", "field-help", help));
			body.appendChild(field);
			return input;
		};
		const nameInput = textField("Name", draft.name, "My home server", "", "Provider name");
		const urlInput = textField("Address", draft.baseUrl, "http://192.168.1.10:8080/v1", "The address that ends in /v1. Ash adds the rest of the path itself.", "Base address");
		const keyInput = textField(
			"API key (optional)",
			"",
			hasKey ? "A key is saved. Type a new one to replace it." : "sk-…",
			hasKey ? `Saved in ${keyStatus.backend === "wallet" ? "your system keyring" : "a private file only you can read"}. Ash never shows a saved key again.` : "Leave this empty if the server does not need one, for example a proxy that adds its own.",
			"API key"
		);
		keyInput.type = "password";
		keyInput.autocomplete = "off";
		let removeKey = false;
		if (hasKey) {
			const forget = button("Remove the saved key", "ghost-button small", () => {
				removeKey = !removeKey;
				forget.querySelector("span:last-child").textContent = removeKey ? "The key will be removed when you save" : "Remove the saved key";
				forget.classList.toggle("danger", removeKey);
				keyInput.disabled = removeKey;
			}, "trash");
			keyInput.parentNode.appendChild(forget);
		}
		const toggles = el("div", "field toggle-list");
		for (const item of [{ key: "tools", label: "This server can use tools", help: "Lets Ash work on your computer through this provider in Helper mode." }, { key: "vision", label: "This server can see pictures", help: "Lets you attach images and lets the helper look at screenshots." }]) {
			const label = el("label", "switch");
			const input = el("input");
			input.type = "checkbox";
			input.checked = Boolean(draft[item.key]);
			input.addEventListener("change", () => (draft[item.key] = input.checked));
			label.appendChild(input);
			label.appendChild(el("span", "switch-track"));
			const text = el("span", "switch-text");
			text.appendChild(el("strong", null, item.label));
			text.appendChild(el("span", "row-sub", item.help));
			label.appendChild(text);
			toggles.appendChild(label);
		}
		body.appendChild(toggles);
		modal.appendChild(body);
		const foot = el("div", "modal-foot");
		foot.appendChild(button("Cancel", "ghost-button", () => closeOverlay()));
		foot.appendChild(button(editing ? "Save changes" : "Add and choose a model", "primary-button", () => {
			const name = nameInput.value.trim();
			const baseUrl = urlInput.value.trim().replace(/\/+$/, "");
			const key = keyInput.value.trim();
			if (!baseUrl) {
				warn("An address is needed, for example http://192.168.1.10:8080/v1.");
				focusNode(urlInput);
				return;
			}
			if (!/^https?:\/\//i.test(baseUrl)) {
				warn("The address should begin with http:// or https://.");
				focusNode(urlInput);
				return;
			}
			if (/\s/.test(key)) {
				warn("API keys cannot contain spaces or line breaks. Paste the key again.");
				focusNode(keyInput);
				return;
			}
			const id = editing && draft.id ? draft.id : newLocalId();
			const draftSettings = settingsClone();
			draftSettings.customEndpoints = asArray(draftSettings.customEndpoints).filter((item) => item.id !== id);
			draftSettings.customEndpoints.push({ id, name: name || baseUrl, baseUrl, tools: Boolean(draft.tools), vision: Boolean(draft.vision) });
			draftSettings.models = { ...(draftSettings.models || {}), [id]: (draftSettings.models && draftSettings.models[id]) || "" };
			editing || (draftSettings.provider = id);
			closeOverlay();
			guard(async () => {
				await saveSettings({ silent: true, patch: draftSettings });
				if (removeKey) await invoke("secret_delete", { provider: id });
				else if (key) await invoke("secret_set", { provider: id, value: key });
				delete state.models[id];
				renderModelsScreen();
				renderChatHeader();
				const health = await testProvider(id, true);
				if (editing) {
					notify("Provider updated.", "success");
					return;
				}
				health && health.online ? openModelPicker(id) : warn(`${name || baseUrl} was added, but it did not answer: ${health ? health.message : "no reply"}. Check the address and key with Edit.`);
			});
		}, "check"));
		modal.appendChild(foot);
		focusNode(editing ? urlInput : nameInput);
	});
};

const removeEndpoint = (endpoint) => confirmDialog({
	title: "Remove this provider?",
	body: `“${endpoint.name || endpoint.baseUrl}” will be removed from Ash, together with any key you saved for it.`,
	confirmLabel: "Remove it",
	danger: true,
	onConfirm: async () => {
		const draft = settingsClone();
		draft.customEndpoints = asArray(draft.customEndpoints).filter((item) => item.id !== endpoint.id);
		if (draft.models) delete draft.models[endpoint.id];
		if (draft.provider === endpoint.id) draft.provider = "ml5";
		await saveSettings({ silent: true, patch: draft });
		await guard(async () => {
			await invoke("secret_delete", { provider: endpoint.id });
		});
		delete state.models[endpoint.id];
		renderModelsScreen();
		renderChatHeader();
		notify("Provider removed.", "success");
	}
});

const openKeyDialog = (provider) => guard(async () => {
	const status = await invoke("secret_status", { provider });
	const backendText = status.backend === "wallet" ? "your system keyring (KDE Wallet)" : status.backend === "file" ? "a private file that only you can read" : "";
	openOverlay((modal) => {
		const head = el("div", "modal-head");
		head.appendChild(el("h2", null, `${providerLabel(provider)} key`));
		head.appendChild(iconButton("Close", "close", () => closeOverlay()));
		modal.appendChild(head);
		const body = el("div", "modal-body");
		if (status.hasKey) {
			body.appendChild(el("p", "field-help", `A key is saved in ${backendText}. For your safety Ash never shows a saved key again; it only uses it.`));
			const actions = el("div", "empty-actions");
			actions.appendChild(button("Replace the key", "primary-button small", () => {
				closeOverlay();
				openKeyEntry(provider);
			}, "edit"));
			actions.appendChild(button("Delete the key", "danger-button small", () => {
				closeOverlay();
				confirmDialog({
					title: "Delete the saved key?",
					body: "Ash will not be able to reach this provider until you add a key again.",
					confirmLabel: "Delete the key",
					danger: true,
					onConfirm: async () => {
						await invoke("secret_delete", { provider });
						state.health[provider] = null;
						renderModelsScreen();
						notify("Key deleted.", "success");
					}
				});
			}, "trash"));
			body.appendChild(actions);
		} else {
			body.appendChild(el("p", "field-help", "No key is saved for this provider yet."));
			const actions = el("div", "empty-actions");
			actions.appendChild(button("Add a key", "primary-button small", () => {
				closeOverlay();
				openKeyEntry(provider);
			}, "shield"));
			body.appendChild(actions);
		}
		modal.appendChild(body);
		const foot = el("div", "modal-foot");
		foot.appendChild(button("Done", "ghost-button", () => closeOverlay()));
		modal.appendChild(foot);
	});
});

const openKeyEntry = (provider) => {
	openOverlay((modal) => {
		const head = el("div", "modal-head");
		head.appendChild(el("h2", null, `Add the ${providerLabel(provider)} key`));
		head.appendChild(iconButton("Close", "close", () => closeOverlay()));
		modal.appendChild(head);
		const body = el("div", "modal-body");
		body.appendChild(el("p", "field-help", "The key goes straight into your system keyring. It is never written into your chats or your settings, and it cannot be read back afterwards."));
		const field = el("div", "field");
		field.appendChild(el("label", "field-label", "API key"));
		const input = el("input", "text-input");
		input.type = "password";
		input.autocomplete = "off";
		input.spellcheck = false;
		input.placeholder = "sk-…";
		input.setAttribute("aria-label", "API key");
		field.appendChild(input);
		body.appendChild(field);
		modal.appendChild(body);
		const foot = el("div", "modal-foot");
		foot.appendChild(button("Cancel", "ghost-button", () => closeOverlay()));
		const save = button("Save the key", "primary-button", () => {
			const value = input.value.trim();
			if (!value) {
				warn("Paste the key first, or choose Cancel.");
				focusNode(input);
				return;
			}
			closeOverlay();
			guard(async () => {
				const result = await invoke("secret_set", { provider, value });
				notify(result.backend === "wallet" ? "Key saved in your system keyring." : "Key saved in a private file.", "success");
				await testProvider(provider, true);
				renderModelsScreen();
			});
		}, "check");
		foot.appendChild(save);
		modal.appendChild(foot);
		input.addEventListener("keydown", (event) => {
			if (event.key === "Enter") {
				event.preventDefault();
				save.click();
			}
		});
		focusNode(input);
	});
};

const daemonAction = (command, successMessage) => guard(async () => {
	await invoke(command);
	state.daemon = await invoke("ml5_status", { force: true });
	await loadModels("ml5", true);
	renderModelsScreen();
	notify(successMessage, "success");
});

const modelAction = (command, model, successMessage) => guard(async () => {
	await invoke(command, { model });
	state.daemon = await invoke("ml5_status", { force: true });
	await loadModels("ml5", true);
	renderModelsScreen();
	notify(successMessage, "success");
});

const renderSettings = () => {
	const root = refs.viewSettings;
	if (!root) return;
	root.innerHTML = "";
	const screen = el("div", "screen");
	const head = el("div", "screen-head");
	const titleBox = el("div");
	titleBox.appendChild(el("h1", null, "Settings"));
	titleBox.appendChild(el("p", null, "Every change here is saved on this computer the moment you make it. Nothing in this screen is sent anywhere."));
	head.appendChild(titleBox);
	screen.appendChild(head);
	if (!state.settings) {
		const card = el("section", "card");
		card.appendChild(el("span", "spinner"));
		card.appendChild(el("p", "card-note", "Settings are still loading…"));
		screen.appendChild(card);
		root.appendChild(screen);
		return;
	}
	screen.appendChild(buildSettingsAnswers());
	screen.appendChild(buildSettingsHelper());
	screen.appendChild(buildSettingsAppearance());
	screen.appendChild(buildSettingsComputer());
	screen.appendChild(buildSettingsCareful());
	screen.appendChild(buildSettingsAbout());
	root.appendChild(screen);
};

const settingsCard = (title, note) => {
	const card = el("section", "card settings-card");
	card.appendChild(el("h2", null, title));
	if (note) card.appendChild(el("p", "card-note", note));
	return card;
};

const settingsField = (label, help, control) => {
	const field = el("div", "field");
	field.appendChild(el("label", "field-label", label));
	control.setAttribute("aria-label", label);
	field.appendChild(control);
	if (help) field.appendChild(el("span", "field-help", help));
	return field;
};

const settingsToggle = (label, help, value, onChange) => {
	const wrap = el("div", "field");
	const row = el("label", "switch");
	const input = el("input");
	input.type = "checkbox";
	input.checked = Boolean(value);
	input.addEventListener("change", () => onChange(input.checked));
	row.appendChild(input);
	row.appendChild(el("span", "switch-track"));
	const text = el("span", "switch-text");
	text.appendChild(el("strong", null, label));
	if (help) text.appendChild(el("span", "row-sub", help));
	row.appendChild(text);
	wrap.appendChild(row);
	return wrap;
};

const saveSettings = async (options) => {
	const payload = options && options.patch ? options.patch : state.settings;
	if (!payload) return;
	try {
		const saved = await invoke("settings_set", { settings: payload });
		state.settings = saved;
		if (!(options && options.noApply)) applySettingsToUi();
		if (state.view === "settings" && !(options && options.silent)) renderSettings();
		if (state.view === "models" && !(options && options.silent)) renderModelsScreen();
	} catch (error) {
		warn(error);
	}
};

const applySettingsToUi = () => {
	const settings = state.settings;
	if (!settings) return;
	applyFontScale(settings.fontScale);
	applySidebar(settings.sidebarCollapsed, false);
	renderComposerHints();
	renderChatHeader();
	if (settings.theme === "system") {
		guard(async () => {
			applyTheme(await invoke("theme_get"));
		});
	} else {
		applyTheme({ scheme: settings.theme === "light" ? "light" : "dark", accent: null });
	}
};

const buildSettingsAnswers = () => {
	const card = settingsCard("How Ash answers", "These shape the replies you get.");
	const providerSelect = el("select", "select");
	for (const provider of providerOptions()) {
		const option = el("option", null, provider.name);
		option.value = provider.id;
		providerSelect.appendChild(option);
	}
	providerSelect.value = state.settings.provider;
	providerSelect.addEventListener("change", async () => {
		const draft = settingsClone();
		draft.provider = providerSelect.value;
		await saveSettings({ patch: draft });
	});
	card.appendChild(settingsField("Provider", "Where answers come from. Local models keep everything on this computer.", providerSelect));
	const modelSelect = el("select", "select");
	modelSelect.setAttribute("aria-label", "Model");
	card.appendChild(settingsField("Model", "The model Ash uses with this provider.", modelSelect));
	fillModelSelect(state.settings.provider, modelSelect);
	const promptArea = el("textarea", "textarea");
	promptArea.value = state.settings.systemPrompt || "";
	promptArea.placeholder = "For example: answer briefly, and use British spelling.";
	promptArea.addEventListener("change", async () => {
		const draft = settingsClone();
		draft.systemPrompt = promptArea.value;
		await saveSettings({ patch: draft });
	});
	card.appendChild(settingsField("Extra instructions", "Added to the instructions Ash always follows. Leave empty if you have none.", promptArea));
	const temperatureRow = el("div", "range-row");
	const range = el("input");
	range.type = "range";
	range.min = "0";
	range.max = "2";
	range.step = "0.1";
	range.value = state.settings.temperature === null || state.settings.temperature === undefined ? "0.7" : String(state.settings.temperature);
	const readout = el("span", "field-value", Number(range.value).toFixed(1));
	range.addEventListener("input", () => (readout.textContent = Number(range.value).toFixed(1)));
	range.addEventListener("change", async () => {
		const draft = settingsClone();
		draft.temperature = Number(range.value);
		await saveSettings({ patch: draft });
	});
	temperatureRow.appendChild(range);
	temperatureRow.appendChild(readout);
	card.appendChild(settingsField("Creativity", "Low keeps answers steady and repeatable. High makes them more varied and surprising.", temperatureRow));
	card.appendChild(settingsToggle("Send with Enter", "When this is off, Enter starts a new line and Shift and Enter sends the message.", state.settings.sendWithEnter, async (value) => {
		const draft = settingsClone();
		draft.sendWithEnter = value;
		await saveSettings({ patch: draft });
	}));
	card.appendChild(settingsToggle("Show Ash's thinking", "Shows the reasoning a model produces before it answers, when the model supports it.", state.settings.showThinking, async (value) => {
		const draft = settingsClone();
		draft.showThinking = value;
		await saveSettings({ patch: draft });
	}));
	const modeSelect = el("select", "select");
	for (const mode of ["chat", "helper"]) {
		const option = el("option", null, MODE_LABELS[mode]);
		option.value = mode;
		modeSelect.appendChild(option);
	}
	modeSelect.value = state.settings.defaultMode;
	modeSelect.addEventListener("change", async () => {
		const draft = settingsClone();
		draft.defaultMode = modeSelect.value;
		await saveSettings({ patch: draft });
	});
	card.appendChild(settingsField("Mode for new chats", "Chat is a conversation. Helper works on your computer with tools.", modeSelect));
	return card;
};

const fillModelSelect = (provider, select) => {
	select.innerHTML = "";
	const pending = el("option", null, "Loading models…");
	pending.value = "";
	select.appendChild(pending);
	select.disabled = true;
	guard(async () => {
		const items = await loadModels(provider);
		select.innerHTML = "";
		if (!items.length) {
			const empty = el("option", null, state.modelErrors[provider] ? "Models unavailable" : "No models available");
			empty.value = "";
			select.appendChild(empty);
			select.disabled = false;
			return;
		}
		const chosen = modelFor(provider);
		for (const model of items) {
			const option = el("option", null, model.name || model.id);
			option.value = model.id;
			select.appendChild(option);
		}
		chosen && items.some((model) => model.id === chosen) ? (select.value = chosen) : null;
		select.disabled = false;
		select.addEventListener("change", async () => {
			const draft = settingsClone();
			draft.models = { ...(draft.models || {}), [provider]: select.value };
			if (state.conversation && state.conversation.provider === provider) {
				state.conversation.model = select.value;
				await invoke("conversations_update", { conversation: { ...state.conversation, snippet: null } });
			}
			await saveSettings({ silent: true, patch: draft });
			renderChatHeader();
		});
	});
};

const buildSettingsHelper = () => {
	const card = settingsCard("Helper permissions", "In Helper mode Ash can work on your computer. This decides how much it may do without asking.");
	const cards = el("div", "tier-cards");
	for (const tier of TIERS) {
		const option = el("button", "tier-card");
		option.type = "button";
		option.setAttribute("aria-pressed", currentTier() === tier.id ? "true" : "false");
		option.appendChild(el("strong", null, `${tier.label}: ${tier.short}`));
		option.appendChild(el("span", null, tier.blurb));
		option.addEventListener("click", () => guard(async () => {
			state.settings = await invoke("tools_set_tier", { tier: tier.id });
			renderSettings();
			renderChatHeader();
			notify(`Permission tier set to ${tier.label}.`, "success");
		}));
		cards.appendChild(option);
	}
	card.appendChild(cards);
	card.appendChild(settingsToggle("Hide the window while Ash is controlling the computer", "Useful when Ash needs to see the screen without this window in the way. The tray icon stays available.", state.settings.hideWhileControlling, async (value) => {
		const draft = settingsClone();
		draft.hideWhileControlling = value;
		await saveSettings({ patch: draft });
	}));
	card.appendChild(settingsToggle("Show notifications", "Tells you when a longer task finishes while you are looking at something else.", state.settings.notifications, async (value) => {
		const draft = settingsClone();
		draft.notifications = value;
		await saveSettings({ patch: draft });
	}));
	const actions = el("div", "empty-actions");
	actions.appendChild(button("See what Ash may do right now", "ghost-button small", () => {
		state.extras = "tools";
		setView("chat");
		loadToolCatalog();
		renderExtras();
	}, "shield"));
	actions.appendChild(button("See what Ash can see", "ghost-button small", () => {
		state.extras = "view";
		setView("chat");
		refreshComputerStatus();
		renderExtras();
	}, "image"));
	actions.appendChild(button("Open the action log", "ghost-button small", () => setView("actions"), "list"));
	actions.appendChild(button("Open the terminal", "ghost-button small", () => openTerminal(), "terminal"));
	card.appendChild(actions);
	return card;
};

const buildSettingsAppearance = () => {
	const card = settingsCard("Look and feel", "Ash follows your desktop by default, including the accent colour you have chosen there.");
	const themeSelect = el("select", "select");
	for (const theme of [["system", "Follow my desktop"], ["light", "Always light"], ["dark", "Always dark"]]) {
		const option = el("option", null, theme[1]);
		option.value = theme[0];
		themeSelect.appendChild(option);
	}
	themeSelect.value = state.settings.theme;
	themeSelect.addEventListener("change", async () => {
		const draft = settingsClone();
		draft.theme = themeSelect.value;
		await saveSettings({ patch: draft });
	});
	card.appendChild(settingsField("Theme", "Following your desktop keeps Ash in step with the rest of Ember.", themeSelect));
	const scaleRow = el("div", "range-row");
	const scale = el("input");
	scale.type = "range";
	scale.min = "0.8";
	scale.max = "1.4";
	scale.step = "0.05";
	scale.value = String(state.settings.fontScale);
	const readout = el("span", "field-value", `${Math.round(numberOr(state.settings.fontScale, 1) * 100)}%`);
	scale.addEventListener("input", () => {
		readout.textContent = `${Math.round(Number(scale.value) * 100)}%`;
		applyFontScale(scale.value);
	});
	scale.addEventListener("change", async () => {
		const draft = settingsClone();
		draft.fontScale = Number(scale.value);
		await saveSettings({ patch: draft });
	});
	scaleRow.appendChild(scale);
	scaleRow.appendChild(readout);
	card.appendChild(settingsField("Text size", "Makes everything in Ash bigger or smaller.", scaleRow));
	card.appendChild(settingsToggle("Keep the sidebar hidden", "Gives the conversation the whole window. Ctrl+B brings the chat list back.", state.settings.sidebarCollapsed, async (value) => {
		applySidebar(value, false);
		const draft = settingsClone();
		draft.sidebarCollapsed = value;
		await saveSettings({ patch: draft, noApply: true });
	}));
	return card;
};

const buildSettingsComputer = () => {
	const card = settingsCard("This computer", "Small conveniences for living with Ash day to day.");
	const autostartRow = el("label", "switch");
	const autostartInput = el("input");
	autostartInput.type = "checkbox";
	autostartInput.disabled = true;
	autostartRow.appendChild(autostartInput);
	autostartRow.appendChild(el("span", "switch-track"));
	const autostartText = el("span", "switch-text");
	autostartText.appendChild(el("strong", null, "Start Ash when I log in"));
	const autostartHelp = el("span", "row-sub", "Checking how this computer starts applications…");
	autostartText.appendChild(autostartHelp);
	autostartRow.appendChild(autostartText);
	card.appendChild(el("div", "field")).appendChild(autostartRow);
	guard(async () => {
		const status = await invoke("autostart_status");
		autostartInput.disabled = !status.available;
		autostartInput.checked = Boolean(status.enabled);
		autostartHelp.textContent = status.available ? "Ash waits quietly in the tray so the desktop can ask it anything straight away." : "This computer has no autostart entry for Ash, so this switch is not available.";
		autostartInput.addEventListener("change", async () => {
			const wanted = autostartInput.checked;
			autostartInput.disabled = true;
			await guard(async () => {
				const result = await invoke("autostart_set", { enabled: wanted });
				autostartInput.checked = Boolean(result.enabled);
				notify(result.enabled ? "Ash will start with your session." : "Ash will no longer start automatically.", "success");
			}, (error) => {
				autostartInput.checked = !wanted;
				warn(error);
			});
			autostartInput.disabled = false;
		});
	}, () => {
		autostartHelp.textContent = "Autostart cannot be changed from this build, so Ash leaves it alone.";
	});
	card.appendChild(settingsToggle("Close to the tray instead of quitting", "Closing the window keeps Ash ready in the tray. Choose Quit from the tray menu to stop it completely.", state.settings.closeToTray, async (value) => {
		const draft = settingsClone();
		draft.closeToTray = value;
		await saveSettings({ patch: draft });
	}));
	const actions = el("div", "empty-actions");
	actions.appendChild(button("Show the welcome guide again", "ghost-button small", () => openOnboarding(true), "info"));
	actions.appendChild(button("Import settings from the old agent", "ghost-button small", () => offerLegacyImport(true), "download"));
	card.appendChild(actions);
	return card;
};

const buildSettingsCareful = () => {
	const card = settingsCard("Careful zone", "These cannot be undone, so Ash asks before doing them.");
	card.classList.add("danger-zone");
	const actions = el("div", "empty-actions");
	actions.appendChild(button("Delete all conversations", "danger-button small", () => deleteAllConversations(), "trash"));
	actions.appendChild(button("Clear the action log", "ghost-button small", () => clearActionLog(), "eraser"));
	card.appendChild(actions);
	return card;
};

const buildSettingsAbout = () => {
	const card = settingsCard("About Ash Agent");
	card.appendChild(el("p", null, "Ash Agent is the assistant built into Ember. It chats, explains and writes things for you, and when you ask, it works on this computer too: looking at your screen, running a command, or changing a setting, always explaining itself as it goes."));
	const kv = el("dl", "kv");
	kv.appendChild(el("dt", null, "Version"));
	kv.appendChild(el("dd", null, "1.0.0"));
	kv.appendChild(el("dt", null, "Provider"));
	kv.appendChild(el("dd", null, providerLabel(activeProvider())));
	kv.appendChild(el("dt", null, "Model"));
	kv.appendChild(el("dd", null, activeModel() || "none chosen yet"));
	kv.appendChild(el("dt", null, "Permission"));
	kv.appendChild(el("dd", null, tierInfo(currentTier()).label));
	kv.appendChild(el("dt", null, "Privacy"));
	kv.appendChild(el("dd", null, "Chats, keys and screenshots stay on this computer."));
	card.appendChild(kv);
	return card;
};

const deleteAllConversations = () => confirmDialog({
	title: "Delete every conversation?",
	body: "This erases your whole chat history from this computer. Your models, settings and API key are not touched. You cannot undo this.",
	confirmLabel: "Delete everything",
	danger: true,
	onConfirm: async () => {
		await invoke("conversations_delete_all");
		state.conversation = null;
		state.messages = [];
		renderChatHeader();
		await renderTranscript();
		await refreshConversations();
		notify("All conversations deleted.", "success");
	}
});

const clearActionLog = () => confirmDialog({
	title: "Clear the action log?",
	body: "This removes the record of what the helper has done on this computer. Actions still waiting for your approval are kept.",
	confirmLabel: "Clear the log",
	danger: true,
	onConfirm: async () => {
		await invoke("actions_clear");
		state.actions = [];
		state.answeredActionIds = {};
		renderActions();
		renderTranscript();
		notify("Action log cleared.", "success");
	}
});

const renderActions = () => {
	const root = refs.viewActions;
	if (!root) return;
	root.innerHTML = "";
	const screen = el("div", "screen");
	const head = el("div", "screen-head");
	const titleBox = el("div");
	titleBox.appendChild(el("h1", null, "Action log"));
	titleBox.appendChild(el("p", null, "Everything Ash has done on this computer, newest first. Anything still waiting for your approval sits at the top."));
	head.appendChild(titleBox);
	const headActions = el("div", "screen-head-actions");
	headActions.appendChild(button("Refresh", "ghost-button", () => refreshActions(), "retry"));
	headActions.appendChild(button("Clear the log", "ghost-button danger-text", () => clearActionLog(), "eraser"));
	head.appendChild(headActions);
	screen.appendChild(head);
	const filters = el("div", "filter-row");
	const options = [["all", "Everything"], ["awaiting", "Waiting for you"], ["running", "Running"], ["done", "Finished"], ["failed", "Failed"], ["denied", "Denied"], ["cancelled", "Cancelled"]];
	for (const [key, label] of options) {
		const chip = el("button", "filter-chip", label);
		chip.type = "button";
		chip.setAttribute("aria-pressed", state.actionFilter === key ? "true" : "false");
		chip.addEventListener("click", () => {
			state.actionFilter = key;
			renderActions();
		});
		filters.appendChild(chip);
	}
	screen.appendChild(filters);
	const waiting = state.actions.filter((action) => action.status === "awaiting");
	if (waiting.length && state.actionFilter !== "awaiting") {
		const banner = el("div", "action-card");
		banner.dataset.status = "awaiting";
		const head2 = el("div", "action-head");
		head2.appendChild(icon("shield"));
		head2.appendChild(el("strong", null, waiting.length === 1 ? "One action is waiting for your answer" : `${waiting.length} actions are waiting for your answer`));
		banner.appendChild(head2);
		banner.appendChild(el("p", "action-explain", "Ash cannot continue until you approve or deny these."));
		const jump = button("Show them", "primary-button small", () => {
			state.actionFilter = "awaiting";
			renderActions();
		}, "check");
		banner.appendChild(jump);
		screen.appendChild(banner);
	}
	const items = state.actionFilter === "all" ? state.actions : state.actions.filter((action) => action.status === state.actionFilter);
	if (!items.length) {
		const empty = el("div", "empty-state");
		empty.appendChild(el("strong", null, state.actions.length ? "Nothing in this filter" : "Ash has not changed anything yet"));
		empty.appendChild(el("p", null, state.actions.length ? "Pick a different filter to see the rest of the log." : "When you ask Ash to change something on this computer, it is recorded here with the outcome, so you can always check what happened."));
		empty.appendChild(button("Switch to Helper mode", "primary-button small", () => {
			setMode("helper");
			setView("chat");
		}, "shield"));
		screen.appendChild(empty);
		root.appendChild(screen);
		return;
	}
	const list = el("div", "row-list");
	for (const action of items) list.appendChild(buildActionLogRow(action));
	screen.appendChild(list);
	root.appendChild(screen);
};

const buildActionLogRow = (action) => {
	const row = el("div", "log-row");
	row.tabIndex = 0;
	row.setAttribute("role", "button");
	row.setAttribute("aria-expanded", "false");
	const time = el("span", "mono", relativeTime(action.created) || "unknown");
	time.title = formatDateTime(action.created);
	const tool = el("span", "mono", action.tool || "action");
	tool.title = action.tool || "";
	const risk = el("span", null, RISK_LABELS[action.risk] || "Risk unknown");
	risk.dataset.risk = RISK_LABELS[action.risk] ? action.risk : "unknown";
	const status = el("span", null, ACTION_STATUS_LABELS[action.status] || action.status || "unknown");
	const exit = el("span", "mono", Number.isFinite(Number(action.exitCode)) ? `exit ${action.exitCode}` : "no exit code");
	const title = el("span", null, truncateText(action.title || action.detail || "Action", 70));
	row.appendChild(time);
	row.appendChild(tool);
	row.appendChild(risk);
	row.appendChild(status);
	row.appendChild(exit);
	row.appendChild(title);
	const detail = el("div", "log-detail hidden");
	detail.appendChild(buildActionCard(action));
	row.appendChild(detail);
	const toggle = () => {
		const opening = detail.classList.contains("hidden");
		detail.classList.toggle("hidden", !opening);
		row.setAttribute("aria-expanded", opening ? "true" : "false");
	};
	row.addEventListener("click", (event) => {
		if (event.target.closest(".log-detail")) {
			const shot = event.target.closest("[data-shot-id]");
			shot && openActionImage(shot.dataset.shotId, shot.dataset.shotUrl || "");
			return;
		}
		toggle();
	});
	row.addEventListener("keydown", (event) => {
		if (event.target !== row) return;
		if (event.key === "Enter" || event.key === " ") {
			event.preventDefault();
			toggle();
		}
	});
	return row;
};

const refreshActions = () => guard(async () => {
	const list = await invoke("actions_list", { limit: 200, before: null });
	const merged = new Map();
	for (const action of asArray(list).filter(isObject).concat(state.actions)) merged.set(action.id, action);
	state.actions = Array.from(merged.values()).sort((a, b) => numberOr(b.created, 0) - numberOr(a.created, 0));
	if (state.view === "actions") renderActions();
});

const confirmDialog = (options) => {
	openOverlay((modal) => {
		const head = el("div", "modal-head");
		head.appendChild(el("h2", null, options.title));
		const close = iconButton("Close", "close", () => closeOverlay());
		head.appendChild(close);
		modal.appendChild(head);
		const body = el("div", "modal-body");
		body.appendChild(el("p", "field-help", options.body));
		if (options.detail) body.appendChild(el("pre", "output", options.detail));
		modal.appendChild(body);
		const foot = el("div", "modal-foot");
		const dismiss = () => {
			closeOverlay();
			typeof options.onCancel === "function" && options.onCancel();
		};
		close.addEventListener("click", () => typeof options.onCancel === "function" && options.onCancel());
		foot.appendChild(button(options.cancelLabel || "Cancel", "ghost-button", dismiss));
		const accept = button(options.confirmLabel || "Confirm", options.danger ? "danger-button" : "primary-button", () => {
			closeOverlay();
			guard(async () => {
				await options.onConfirm();
			});
		});
		foot.appendChild(accept);
		modal.appendChild(foot);
		focusNode(options.danger ? accept : close);
	});
};

const promptDialog = (options) => {
	openOverlay((modal) => {
		const head = el("div", "modal-head");
		head.appendChild(el("h2", null, options.title));
		head.appendChild(iconButton("Close", "close", () => closeOverlay()));
		modal.appendChild(head);
		const body = el("div", "modal-body");
		if (options.help) body.appendChild(el("p", "field-help", options.help));
		const field = el("div", "field");
		field.appendChild(el("label", "field-label", options.label));
		const input = el("input", "text-input");
		input.type = options.type || "text";
		input.value = options.value || "";
		input.setAttribute("aria-label", options.label);
		field.appendChild(input);
		body.appendChild(field);
		modal.appendChild(body);
		const foot = el("div", "modal-foot");
		foot.appendChild(button("Cancel", "ghost-button", () => closeOverlay()));
		const submit = () => {
			const value = input.value.trim();
			if (!value) {
				warn("Type a name first, or choose Cancel.");
				focusNode(input);
				return;
			}
			closeOverlay();
			guard(async () => {
				await options.onSubmit(value);
			});
		};
		const accept = button(options.confirmLabel || "Save", "primary-button", submit);
		foot.appendChild(accept);
		modal.appendChild(foot);
		input.addEventListener("keydown", (event) => {
			if (event.key === "Enter") {
				event.preventDefault();
				submit();
			}
		});
		focusNode(input);
		input.select();
	});
};

const openExportDialog = (conversation) => {
	openOverlay((modal) => {
		const head = el("div", "modal-head");
		head.appendChild(el("h2", null, "Export this chat"));
		head.appendChild(iconButton("Close", "close", () => closeOverlay()));
		modal.appendChild(head);
		const body = el("div", "modal-body");
		body.appendChild(el("p", "field-help", "Pick a format and Ash prepares the file, then you choose where to put it. Markdown is readable text anywhere; JSON keeps every detail."));
		const list = el("div", "option-list");
		for (const option of [
			{ id: "md", label: "Markdown", detail: "A readable document you can open in any text editor." },
			{ id: "json", label: "JSON", detail: "A complete copy, useful for backups or other tools." }
		]) {
			const row = el("button", "option");
			row.type = "button";
			row.dataset.format = option.id;
			row.setAttribute("aria-selected", option.id === "md" ? "true" : "false");
			const text = el("span", "option-text");
			text.appendChild(el("strong", null, option.label));
			text.appendChild(el("span", null, option.detail));
			row.appendChild(text);
			row.appendChild(el("span", "option-check", option.id === "md" ? "✓" : ""));
			row.addEventListener("click", () => {
				for (const other of list.querySelectorAll(".option")) {
					const selected = other === row;
					other.setAttribute("aria-selected", selected ? "true" : "false");
					other.querySelector(".option-check").textContent = selected ? "✓" : "";
				}
			});
			list.appendChild(row);
		}
		body.appendChild(list);
		modal.appendChild(body);
		const foot = el("div", "modal-foot");
		foot.appendChild(button("Cancel", "ghost-button", () => closeOverlay()));
		foot.appendChild(button("Prepare the file", "primary-button", () => {
			const selected = list.querySelector('.option[aria-selected="true"]');
			const format = selected ? selected.dataset.format : "md";
			closeOverlay();
			exportConversation(conversation, format);
		}, "download"));
		modal.appendChild(foot);
	});
};

const exportConversation = async (conversation, format) => {
	await guard(async () => {
		const payload = await invoke("conversations_export", { id: conversation.id, format });
		const body = typeof payload === "string" ? payload : String(payload === undefined || payload === null ? "" : payload);
		if (!body) {
			warn("The backend returned an empty export, so there is nothing to save.");
			return;
		}
		const safeName = (conversation.title || "ash-agent-chat").replace(/[^\w\-. ]+/g, "").trim().replace(/\s+/g, "-") || "ash-agent-chat";
		const extension = format === "json" ? "json" : "md";
		const dialog = window.__TAURI__ && window.__TAURI__.dialog;
		if (!dialog || typeof dialog.save !== "function") {
			warn("Saving a file is not available in this build, so the export cannot be written to disk.");
			return;
		}
		const chosen = await dialog.save({
			title: "Save this export",
			defaultPath: `${safeName}.${extension}`,
			filters: [{ name: format === "json" ? "JSON file" : "Markdown file", extensions: [extension] }]
		});
		if (!chosen) return;
		const path = typeof chosen === "string" ? chosen : chosen.path;
		if (!path) return;
		const saved = await invoke("file_save", { path, contents: body });
		notify(`Export saved to ${typeof saved === "string" && saved ? saved : path}`, "success");
	});
};

const offerLegacyImport = async (manual) => {
	const legacy = await guard(async () => invoke("legacy_import"));
	if (!legacy) {
		manual && notify("No settings from the old agent were found on this computer.", "info");
		return;
	}
	if (state.legacyOffered && !manual) return;
	state.legacyOffered = true;
	const backend = legacy.provider === "opencore" ? "OpenCORe" : legacy.provider === "ml5" ? "the local engine" : "its old provider";
	if (!manual && state.settings && state.settings.legacyHandled) return;
	confirmDialog({
		title: "Use the settings from your old assistant?",
		cancelLabel: "No thanks",
		onCancel: () => guard(async () => {
			state.settings = await invoke("legacy_dismiss");
		}),
		body: `The previous agent on this computer used ${backend}${legacy.hasKey ? ", and saved an API key" : ""}. Ash can bring that choice across so you do not have to set it up again.`,
		confirmLabel: "Bring them across",
		onConfirm: async () => {
			state.settings = await invoke("legacy_apply");
			applySettingsToUi();
			notify(legacy.hasKey ? "Settings and the saved API key from the old agent were brought across." : "Settings from the old agent were brought across.", "success");
		}
	});
};

const openOnboarding = (reopen) => {
	const root = refs.onboardingRoot;
	if (!root) return;
	root.innerHTML = "";
	show(root, true);
	const draft = {
		index: 0,
		backend: state.settings && state.settings.provider === "opencore" ? "opencore" : "ml5",
		tier: state.settings ? currentTier() : TIER_DEFAULT,
		key: ""
	};
	const card = el("div", "onboarding-card");
	const head = el("div", "onboarding-head");
	const mark = el("img");
	mark.src = "logo.png";
	mark.alt = "";
	head.appendChild(mark);
	const headText = el("div");
	headText.appendChild(el("h1", null, `Welcome to ${ASSISTANT_NAME}`));
	headText.appendChild(el("p", null, "Your assistant for Ember: part conversation, part pair of hands."));
	head.appendChild(headText);
	card.appendChild(head);
	const dots = el("div", "onboarding-progress");
	const marks = [];
	for (let index = 0; index < 3; index += 1) {
		const dot = el("span", "onboarding-dot");
		dots.appendChild(dot);
		marks.push(dot);
	}
	card.appendChild(dots);
	const stage = el("div");
	card.appendChild(stage);
	const foot = el("div", "onboarding-foot");
	const steps = ["What Ash is", "Where it thinks", "What it may do"];
	const label = el("span", "field-value");
	const back = button("Back", "ghost-button", () => paint(draft.index - 1));
	const next = button("Continue", "primary-button", () => (draft.index < 2 ? paint(draft.index + 1) : finish()));
	const skip = button("Skip this", "ghost-button", () => finish());
	foot.appendChild(skip);
	foot.appendChild(el("span", "spacer"));
	foot.appendChild(label);
	foot.appendChild(back);
	foot.appendChild(next);
	card.appendChild(foot);
	root.appendChild(card);
	const paint = (index) => {
		draft.index = clamp(index, 0, 2);
		marks.forEach((dot, position) => (dot.dataset.active = position <= draft.index ? "true" : "false"));
		label.textContent = `${draft.index + 1} of 3: ${steps[draft.index]}`;
		back.disabled = draft.index === 0;
		next.querySelector("span").textContent = draft.index === 2 ? `Start using ${ASSISTANT_NAME}` : "Continue";
		stage.innerHTML = "";
		if (draft.index === 0) {
			stage.appendChild(el("p", "field-help", `Ash is the assistant built into Ember. It chats with you about anything, and it can also work on this computer when you ask, explaining each step and asking before it changes anything.`));
			const list = el("div", "choice-list");
			for (const item of [
				["Ask anything", "Writing, explaining, code, planning: Ash answers like any chat assistant."],
				["Get help with this computer", "Switch to Helper mode and Ash can look at your screen, run a command for you, or change a setting."],
				["Stay private", "Your conversations are stored on this computer. Local models never send anything over the internet."]
			]) {
				const row = el("div", "choice");
				row.appendChild(icon("check"));
				const text = el("div", "choice-text");
				text.appendChild(el("strong", null, item[0]));
				text.appendChild(el("span", null, item[1]));
				row.appendChild(text);
				list.appendChild(row);
			}
			stage.appendChild(list);
		} else if (draft.index === 1) {
			stage.appendChild(el("p", "field-help", "Where should Ash think? You can change this at any time under Models."));
			const list = el("div", "choice-list");
			for (const option of [
				["ml5", "On this computer (private)", "Uses the ML5 engine on your own machine. It works offline and nothing leaves the computer. You download a model first; recommendations are under Models."],
				["opencore", "In the cloud (stronger answers)", "Uses OpenCORe over the internet. More capable and nothing to download, but your messages are sent to that service and you need an API key."]
			]) {
				const row = el("button", "choice");
				row.type = "button";
				row.setAttribute("aria-pressed", draft.backend === option[0] ? "true" : "false");
				row.appendChild(icon(option[0] === "ml5" ? "cpu" : "sliders"));
				const text = el("div", "choice-text");
				text.appendChild(el("strong", null, option[1]));
				text.appendChild(el("span", null, option[2]));
				row.appendChild(text);
				row.addEventListener("click", () => {
					draft.backend = option[0];
					paint(1);
				});
				list.appendChild(row);
			}
			stage.appendChild(list);
			if (draft.backend === "opencore") {
				const field = el("div", "field");
				field.appendChild(el("label", "field-label", "OpenCORe API key"));
				const input = el("input", "text-input");
				input.type = "password";
				input.autocomplete = "off";
				input.spellcheck = false;
				input.placeholder = "sk-…";
				input.value = draft.key;
				input.setAttribute("aria-label", "OpenCORe API key");
				input.addEventListener("input", () => (draft.key = input.value));
				field.appendChild(input);
				field.appendChild(el("span", "field-help", "Kept in your system keyring, never in your chats. You can replace or delete it later under Models."));
				stage.appendChild(field);
			}
		} else {
			stage.appendChild(el("p", "field-help", "Last of all: how much may Ash do on this computer? You can change it whenever you like."));
			const list = el("div", "choice-list");
			for (const tier of TIERS) {
				const row = el("button", "choice");
				row.type = "button";
				row.setAttribute("aria-pressed", draft.tier === tier.id ? "true" : "false");
				row.appendChild(icon("shield"));
				const text = el("div", "choice-text");
				text.appendChild(el("strong", null, `${tier.label}: ${tier.short}`));
				text.appendChild(el("span", null, tier.blurb));
				row.appendChild(text);
				row.addEventListener("click", () => {
					draft.tier = tier.id;
					paint(2);
				});
				list.appendChild(row);
			}
			stage.appendChild(list);
		}
	};
	const finish = async () => {
		show(root, false);
		root.innerHTML = "";
		await guard(async () => {
			const settings = settingsClone();
			settings.onboarded = true;
			settings.provider = draft.backend;
			await saveSettings({ silent: true, patch: settings });
			if (draft.backend === "opencore" && draft.key.trim()) {
				await guard(async () => {
					const result = await invoke("secret_set", { provider: "opencore", value: draft.key.trim() });
					notify(result.backend === "wallet" ? "Key saved in your system keyring." : "Key saved in a private file.", "success");
				});
			}
			if (state.settings && state.settings.tier !== draft.tier) {
				await guard(async () => {
					state.settings = await invoke("tools_set_tier", { tier: draft.tier });
				});
			}
		});
		applySettingsToUi();
		renderChatHeader();
		reopen ? notify("The welcome guide finished. You can open it again from Settings.", "success") : notify("All set. Ask Ash anything.", "success");
	};
	paint(0);
};

const appendTerminal = (text, stream) => {
	const node = refs.terminalOutput;
	if (!node || !text) return;
	const line = el("span", stream === "err" ? "terminal-line err" : "terminal-line", text);
	node.appendChild(line);
	node.scrollTop = node.scrollHeight;
};

const openTerminal = async () => {
	show(refs.terminalPanel, true);
	await refreshSessions();
	if (!state.activeSession) await startSession(false);
	setupTerminalResize();
	focusNode(refs.terminalInput);
};

const closeTerminal = () => {
	show(refs.terminalPanel, false);
	teardownTerminalResize();
};

const refreshSessions = () => guard(async () => {
	const list = await invoke("sessions_list");
	state.sessions = asArray(list).filter(isObject).filter((session) => session.alive !== false);
	const select = refs.sessionSelect;
	select.innerHTML = "";
	if (!state.sessions.length) {
		const option = el("option", null, "No sessions are open");
		option.value = "";
		select.appendChild(option);
		state.activeSession = "";
		return;
	}
	if (!state.activeSession || !state.sessions.some((session) => session.id === state.activeSession)) state.activeSession = state.sessions[0].id;
	for (const session of state.sessions) {
		const option = el("option", null, `${session.shell || "shell"}${session.root ? " (administrator)" : ""} · ${truncateText(session.cwd || "unknown folder", 40)}`);
		option.value = session.id;
		select.appendChild(option);
	}
	select.value = state.activeSession;
});

const startSession = (root) => guard(async () => {
	const id = await invoke("session_start", { shell: null, root: Boolean(root) });
	state.activeSession = id;
	appendTerminal(root ? "Ash opened a shell with administrator rights.\n" : "Ash opened a shell on this computer.\n", "out");
	await refreshSessions();
	reportTerminalSize();
});

const stopSession = () => guard(async () => {
	if (!state.activeSession) {
		warn("There is no open session to close.");
		return;
	}
	await invoke("session_stop", { id: state.activeSession });
	appendTerminal("Session closed.\n", "out");
	state.activeSession = "";
	await refreshSessions();
});

const writeSession = (data) => {
	if (!state.activeSession) {
		warn("There is no open terminal session. Start one first.");
		return;
	}
	guard(async () => {
		await invoke("session_write", { id: state.activeSession, data });
	});
};

const handleTerminalSubmit = (event) => {
	event.preventDefault();
	const input = refs.terminalInput;
	const line = input.value;
	input.value = "";
	state.terminalHistory.push(line);
	state.terminalHistoryIndex = state.terminalHistory.length;
	appendTerminal(`$ ${line}\n`, "out");
	writeSession(`${line}\n`);
};

const handleTerminalKeys = (event) => {
	const input = refs.terminalInput;
	if (event.key === "ArrowUp") {
		event.preventDefault();
		if (!state.terminalHistory.length) return;
		state.terminalHistoryIndex = Math.max(0, state.terminalHistoryIndex - 1);
		input.value = state.terminalHistory[state.terminalHistoryIndex] || "";
		input.setSelectionRange(input.value.length, input.value.length);
		return;
	}
	if (event.key === "ArrowDown") {
		event.preventDefault();
		state.terminalHistoryIndex = Math.min(state.terminalHistory.length, state.terminalHistoryIndex + 1);
		input.value = state.terminalHistory[state.terminalHistoryIndex] || "";
		input.setSelectionRange(input.value.length, input.value.length);
		return;
	}
	if (event.key === "Escape") {
		event.stopPropagation();
		input.value = "";
		return;
	}
	if (event.ctrlKey && (event.key === "l" || event.key === "L")) {
		event.preventDefault();
		refs.terminalOutput.innerHTML = "";
		return;
	}
	if (event.ctrlKey && (event.key === "c" || event.key === "C")) {
		event.preventDefault();
		appendTerminal("^C\n", "out");
		writeSession("\u0003");
		return;
	}
	if (event.key === "Tab") {
		event.preventDefault();
		completeTerminalLine();
	}
};

const completeTerminalLine = () => guard(async () => {
	const input = refs.terminalInput;
	if (!state.activeSession) return;
	const cursor = input.selectionStart === null ? input.value.length : input.selectionStart;
	const before = input.value.slice(0, cursor);
	const after = input.value.slice(cursor);
	const result = await invoke("session_complete", { id: state.activeSession, line: before });
	if (!isObject(result)) return;
	const line = typeof result.line === "string" ? result.line : before;
	input.value = `${line}${after}`;
	input.setSelectionRange(line.length, line.length);
	const options = asArray(result.options).filter((item) => typeof item === "string");
	options.length > 1 && appendTerminal(`${options.join("  ")}\n`, "out");
});

const measureTerminal = () => {
	const node = refs.terminalOutput;
	if (!node) return { cols: 80, rows: 24 };
	const style = window.getComputedStyle(node);
	const charWidth = measureCharWidth(style, node) || TERMINAL_FONT_FALLBACK;
	const lineHeight = Number.parseFloat(style.lineHeight) || 16;
	const horizontal = (Number.parseFloat(style.paddingLeft) || 0) + (Number.parseFloat(style.paddingRight) || 0);
	const vertical = (Number.parseFloat(style.paddingTop) || 0) + (Number.parseFloat(style.paddingBottom) || 0);
	return {
		cols: Math.max(20, Math.floor((node.clientWidth - horizontal) / charWidth)),
		rows: Math.max(4, Math.floor((node.clientHeight - vertical) / lineHeight))
	};
};

const measureCharWidth = (style, host) => {
	const probe = el("span", null, "0000000000");
	probe.style.position = "absolute";
	probe.style.visibility = "hidden";
	probe.style.whiteSpace = "pre";
	probe.style.fontFamily = style.fontFamily;
	probe.style.fontSize = style.fontSize;
	probe.style.letterSpacing = style.letterSpacing;
	host.appendChild(probe);
	const width = probe.getBoundingClientRect().width / 10;
	probe.remove();
	return width;
};

const reportTerminalSize = () => {
	if (!state.activeSession || refs.terminalPanel.classList.contains("hidden")) return;
	const { cols, rows } = measureTerminal();
	if (cols === state.terminalCols && rows === state.terminalRows) return;
	state.terminalCols = cols;
	state.terminalRows = rows;
	guard(async () => {
		await invoke("session_resize", { id: state.activeSession, cols, rows });
	});
};

const setupTerminalResize = () => {
	if (state.terminalObserver) return;
	state.terminalObserver = new ResizeObserver(() => reportTerminalSize());
	refs.terminalOutput && state.terminalObserver.observe(refs.terminalOutput);
	refs.terminalPanel && state.terminalObserver.observe(refs.terminalPanel);
	reportTerminalSize();
};

const teardownTerminalResize = () => {
	if (!state.terminalObserver) return;
	state.terminalObserver.disconnect();
	state.terminalObserver = null;
};

const attachStreamListeners = async () => {
	await listen("chat://started", async (event) => {
		const payload = event.payload || {};
		const turn = state.liveTurn;
		if (!turn) return;
		if (turn.id === "pending") turn.id = payload.turnId;
		if (payload.turnId !== turn.id) return;
		await adoptConversation(payload.conversationId);
	});
	await listen("chat://title", (event) => {
		const payload = event.payload || {};
		if (!payload.conversationId || typeof payload.title !== "string") return;
		if (state.conversation && state.conversation.id === payload.conversationId) {
			state.conversation.title = payload.title;
			state.conversation.titled = true;
			renderChatHeader();
		}
		refreshConversations();
	});
	await listen("chat://delta", async (event) => {
		const payload = event.payload || {};
		const turn = state.liveTurn;
		if (!turn || (turn.id !== "pending" && payload.turnId !== turn.id)) return;
		if (payload.messageId && turn.messageId !== payload.messageId) {
			const nextStep = Boolean(turn.messageId);
			turn.messageId = payload.messageId;
			if (nextStep) {
				turn.content = "";
				turn.thinking = "";
				turn.toolCalls = [];
				turn.statusText = "";
				turn.usage = null;
				await reloadMessages(turn.conversationId);
				state.messages = state.messages.filter((message) => message.id !== payload.messageId);
				await renderTranscript();
			}
			state.liveNode && (state.liveNode.dataset.messageId = payload.messageId);
		}
		if (typeof payload.content === "string") turn.content += payload.content;
		if (typeof payload.thinking === "string") turn.thinking += payload.thinking;
		turn.renderStamp += 1;
		scheduleStreamRender();
	});
	await listen("chat://status", (event) => {
		const payload = event.payload || {};
		const turn = state.liveTurn;
		if (!turn || (turn.id !== "pending" && payload.turnId !== turn.id)) return;
		turn.statusText = typeof payload.text === "string" ? payload.text : "";
		const inner = state.liveNode ? state.liveNode.querySelector(".msg-inner") : null;
		if (!inner) return;
		let node = inner.querySelector(".stream-status");
		if (turn.statusText) {
			if (!node) {
				node = el("div", "stream-status");
				node.setAttribute("aria-live", "polite");
				inner.insertBefore(node, inner.querySelector(".msg-body"));
			}
			node.textContent = turn.statusText;
		} else {
			node && node.remove();
		}
		if (state.autoScroll) scrollTranscriptToEnd(false);
	});
	await listen("chat://tool_calls", (event) => {
		const payload = event.payload || {};
		const turn = state.liveTurn;
		if (!turn || (turn.id !== "pending" && payload.turnId !== turn.id)) return;
		turn.toolCalls = asArray(payload.calls).filter(isObject);
		applyLiveTurn();
	});
	await listen("chat://usage", (event) => {
		const payload = event.payload || {};
		const turn = state.liveTurn;
		if (!turn || (turn.id !== "pending" && payload.turnId !== turn.id)) return;
		turn.usage = isObject(payload.usage) ? payload.usage : null;
		applyLiveTurn();
	});
	await listen("chat://done", (event) => {
		const payload = event.payload || {};
		const turn = state.liveTurn;
		if (!turn || (turn.id !== "pending" && payload.turnId !== turn.id)) return;
		if (payload.messageId) turn.messageId = payload.messageId;
		const stopped = Boolean(payload.stopped);
		finishTurn(stopped ? { stopped: true } : {});
		if (stopped) notify("Stopped.", "info");
	});
	await listen("chat://error", (event) => {
		const payload = event.payload || {};
		const turn = state.liveTurn;
		if (!turn || (turn.id !== "pending" && payload.turnId !== turn.id)) return;
		const wait = numberOr(payload.retryAfter, 0);
		const suffix = wait > 0 ? ` Try again in about ${formatDuration(wait)}.` : "";
		finishTurn({ error: `${payload.message || "The model could not answer."}${suffix}` });
	});
};

const attachToolListeners = async () => {
	await listen("tool://action", (event) => {
		isObject(event.payload) && upsertAction(event.payload);
	});
	await listen("tool://log", (event) => {
		const payload = event.payload || {};
		const actionId = payload.actionId || payload.id;
		if (!actionId) return;
		const data = typeof payload.data === "string" ? payload.data : typeof payload.text === "string" ? payload.text : "";
		if (!data) return;
		const found = state.actions.find((action) => action.id === actionId);
		if (found) found.output = `${found.output || ""}${data}`;
		for (const card of document.querySelectorAll(`[data-action-id="${actionId}"]`)) {
			const wrap = card.querySelector("pre.output");
			if (!wrap) continue;
			const full = `${wrap.dataset.full || ""}${data}`;
			wrap.dataset.full = full;
			wrap.textContent = full.length > OUTPUT_CLAMP && wrap.dataset.expanded !== "true" ? truncateText(full, OUTPUT_CLAMP + 1) : full;
			wrap.classList.toggle("clamped", full.length > OUTPUT_CLAMP && wrap.dataset.expanded !== "true");
		}
		if (state.view === "actions") renderActions();
	});
};

const attachSessionListeners = async () => {
	await listen("session://output", (event) => {
		const payload = event.payload || {};
		if (payload.sessionId !== state.activeSession) return;
		appendTerminal(typeof payload.data === "string" ? payload.data : "", payload.stream === "err" ? "err" : "out");
	});
};

const attachMl5Listeners = async () => {
	await listen("ml5://pull", (event) => {
		const payload = event.payload || {};
		const kind = payload.type || "";
		const id = payload.id || "";
		if (kind === "started") {
			if (!state.pulls.some((pull) => pull.id === id)) state.pulls.unshift({ id, target: payload.target || id, file: payload.file || id, stage: "Getting ready…", downloaded: 0, total: null });
		} else if (kind === "stage") {
			const pull = state.pulls.find((item) => item.id === id);
			pull ? (pull.stage = payload.text || "") : state.pulls.unshift({ id, target: id, file: id, stage: payload.text || "", downloaded: 0, total: null });
		} else if (kind === "progress") {
			const pull = state.pulls.find((item) => item.id === id);
			if (pull) {
				pull.downloaded = numberOr(payload.downloaded, 0);
				pull.total = payload.total === null || payload.total === undefined ? null : numberOr(payload.total, 0);
				if (payload.file) pull.file = payload.file;
			} else {
				state.pulls.unshift({ id, target: id, file: payload.file || id, stage: "Downloading…", downloaded: numberOr(payload.downloaded, 0), total: payload.total === null || payload.total === undefined ? null : numberOr(payload.total, 0) });
			}
		} else if (kind === "done") {
			state.pulls = state.pulls.filter((pull) => pull.id !== id);
			state.models.ml5 = null;
			notify(`${payload.model || "The model"} finished downloading and is ready to use.`, "success");
			state.view === "models" && refreshModelsScreen(true);
		} else if (kind === "failed") {
			state.pulls = state.pulls.filter((pull) => pull.id !== id);
			warn(payload.message || "The download failed.");
			state.view === "models" && refreshModelsScreen(true);
		}
		state.view === "models" && renderModelsScreen();
	});
	await listen("ml5://status", (event) => {
		if (!isObject(event.payload)) return;
		state.daemon = event.payload;
		if (state.view === "models") renderModelsScreen();
	});
};

const attachAppListeners = async () => {
	await listen("app://new-chat", () => {
		closeOverlay();
		newChat();
		focusNode(refs.composerInput);
	});
	await listen("app://open-conversation", (event) => {
		const id = event.payload && event.payload.conversationId;
		if (!id) return;
		closeOverlay();
		selectConversation(id);
	});
};

const attachThemeListeners = async () => {
	await listen("theme://change", (event) => {
		const payload = event.payload || {};
		if (state.settings && state.settings.theme !== "system") return;
		applyTheme(payload);
	});
};

const handleGlobalKeys = (event) => {
	const target = event.target;
	const typing = Boolean(target) && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable);
	const modifier = event.ctrlKey || event.metaKey;
	if (event.key === "Escape") {
		if (refs.lightbox && !refs.lightbox.classList.contains("hidden")) {
			closeLightbox();
			return;
		}
		if (overlayOpen()) {
			closeOverlay();
			return;
		}
		if (state.extras !== "none") {
			state.extras = "none";
			renderExtras();
			return;
		}
		if (refs.onboardingRoot && !refs.onboardingRoot.classList.contains("hidden")) return;
		if (refs.terminalPanel && !refs.terminalPanel.classList.contains("hidden") && typing && target === refs.terminalInput) return;
		if (refs.terminalPanel && !refs.terminalPanel.classList.contains("hidden")) {
			closeTerminal();
			return;
		}
		if (state.view !== "chat") {
			setView("chat", { focus: true });
			return;
		}
		if (!state.sidebarCollapsed && window.innerWidth <= 860) {
			applySidebar(true, false);
			return;
		}
		if (typing && target === refs.composerInput) {
			target.blur();
			return;
		}
		if (typing && isStreaming()) {
			stopTurn();
			return;
		}
		if (typing) target.blur();
		return;
	}
	if (!modifier) return;
	const key = (event.key || "").toLowerCase();
	if (key === "n") {
		event.preventDefault();
		newChat();
		return;
	}
	if (key === "k" || key === "f") {
		event.preventDefault();
		setView("chat");
		window.innerWidth <= 860 && applySidebar(false, false);
		focusNode(refs.searchInput);
		refs.searchInput && refs.searchInput.select();
		return;
	}
	if (key === "b") {
		event.preventDefault();
		toggleSidebar();
		return;
	}
	if (key === ",") {
		event.preventDefault();
		setView("settings");
		return;
	}
	if (key === "m") {
		event.preventDefault();
		openModelPicker();
		return;
	}
	if (key === "j") {
		event.preventDefault();
		refs.terminalPanel.classList.contains("hidden") ? openTerminal() : closeTerminal();
		return;
	}
	if (key === "l" && !typing && state.view === "actions") {
		event.preventDefault();
		clearActionLog();
		return;
	}
	if (key === "enter" && typing && target === refs.composerInput) {
		event.preventDefault();
		isStreaming() ? stopTurn() : sendMessage();
	}
};

const handleWindowResize = () => {
	if (window.innerWidth > 860) {
		show(refs.sidebarScrim, false);
	} else {
		show(refs.sidebarScrim, !state.sidebarCollapsed);
	}
	reportTerminalSize();
};

const cacheRefs = () => {
	refs.app = $("app");
	refs.sidebarToggle = $("sidebar-toggle");
	refs.sidebarShow = $("sidebar-show");
	refs.sidebarScrim = $("sidebar-scrim");
	refs.newChat = $("new-chat");
	refs.searchInput = $("search-input");
	refs.searchClear = $("search-clear");
	refs.searchNote = $("search-note");
	refs.conversationList = $("conversation-list");
	refs.modelChip = $("model-chip");
	refs.providerDot = $("provider-dot");
	refs.modelChipText = $("model-chip-text");
	refs.openModels = $("open-models");
	refs.openSettings = $("open-settings");
	refs.chatTitle = $("chat-title");
	refs.titleButton = $("title-button");
	refs.headModel = $("head-model");
	refs.headHelper = $("head-helper");
	refs.headHelperText = $("head-helper-text");
	refs.tierButton = $("tier-button");
	refs.openTerminal = $("open-terminal");
	refs.openActions = $("open-actions");
	refs.clearFromHere = $("clear-from-here");
	refs.transcript = $("transcript");
	refs.jumpLatest = $("jump-latest");
	refs.composerAlert = $("composer-alert");
	refs.attachStrip = $("attach-strip");
	refs.composer = $("composer");
	refs.composerInput = $("composer-input");
	refs.attachButton = $("attach-button");
	refs.thinkButton = $("think-button");
	refs.composerModel = $("composer-model");
	refs.composerModelText = $("composer-model-text");
	refs.sendHint = $("send-hint");
	refs.stopButton = $("stop-button");
	refs.sendButton = $("send-button");
	refs.composerHintText = $("composer-hint-text");
	refs.viewChat = $("view-chat");
	refs.viewModels = $("view-models");
	refs.viewActions = $("view-actions");
	refs.viewSettings = $("view-settings");
	refs.terminalPanel = $("terminal-panel");
	refs.sessionSelect = $("session-select");
	refs.sessionNew = $("session-new");
	refs.sessionStop = $("session-stop");
	refs.terminalClose = $("terminal-close");
	refs.terminalOutput = $("terminal-output");
	refs.terminalForm = $("terminal-form");
	refs.terminalInput = $("terminal-input");
	refs.overlayRoot = $("overlay-root");
	refs.lightbox = $("lightbox");
	refs.lightboxImage = $("lightbox-image");
	refs.lightboxClose = $("lightbox-close");
	refs.toasts = $("toasts");
	refs.onboardingRoot = $("onboarding-root");
};

const attachStaticListeners = () => {
	refs.sidebarToggle && refs.sidebarToggle.addEventListener("click", () => toggleSidebar());
	refs.sidebarShow && refs.sidebarShow.addEventListener("click", () => toggleSidebar());
	refs.sidebarScrim && refs.sidebarScrim.addEventListener("click", () => applySidebar(true, false));
	refs.newChat && refs.newChat.addEventListener("click", () => newChat());
	refs.openModels && refs.openModels.addEventListener("click", () => setView("models"));
	refs.openSettings && refs.openSettings.addEventListener("click", () => setView("settings"));
	refs.openTerminal && refs.openTerminal.addEventListener("click", () => openTerminal());
	refs.openActions && refs.openActions.addEventListener("click", () => setView("actions"));
	refs.clearFromHere && refs.clearFromHere.addEventListener("click", () => {
		if (!state.conversation || !state.messages.length) {
			warn("There is nothing in this chat to clear yet.");
			return;
		}
		const last = [...state.messages].reverse().find((message) => message.role === "user" || message.role === "assistant");
		last ? openDeleteDialog(last) : warn("There is nothing in this chat to clear yet.");
	});
	refs.modelChip && refs.modelChip.addEventListener("click", () => openModelPicker());
	refs.composerModel && refs.composerModel.addEventListener("click", () => openModelPicker());
	refs.tierButton && refs.tierButton.addEventListener("click", () => openTierDialog());
	refs.titleButton && refs.titleButton.addEventListener("click", () => renameChatTitle());
	refs.attachButton && refs.attachButton.addEventListener("click", () => pickImages());
	refs.thinkButton && refs.thinkButton.addEventListener("click", () => {
		state.think = !state.think;
		refs.thinkButton.setAttribute("aria-pressed", state.think ? "true" : "false");
		refs.thinkButton.title = state.think ? "Thinking is on: Ash reasons before answering" : "Ask the model to think first";
	});
	refs.sendButton && refs.sendButton.addEventListener("click", () => sendMessage());
	refs.stopButton && refs.stopButton.addEventListener("click", () => stopTurn());
	refs.jumpLatest && refs.jumpLatest.addEventListener("click", () => scrollTranscriptToEnd(true));
	refs.transcript && refs.transcript.addEventListener("scroll", handleTranscriptScroll);
	refs.composerInput && refs.composerInput.addEventListener("input", () => growComposer());
	refs.composerInput && refs.composerInput.addEventListener("keydown", handleComposerKeydown);
	refs.composerInput && refs.composerInput.addEventListener("paste", handlePaste);
	if (refs.composer) {
		refs.composer.addEventListener("dragover", (event) => {
			event.preventDefault();
			refs.composer.classList.add("dragging");
		});
		refs.composer.addEventListener("dragleave", () => refs.composer.classList.remove("dragging"));
		refs.composer.addEventListener("drop", (event) => {
			event.preventDefault();
			refs.composer.classList.remove("dragging");
			const files = Array.from(event.dataTransfer ? event.dataTransfer.files || [] : []);
			files.length && addImageFiles(files);
		});
	}
	if (refs.searchInput) {
		refs.searchInput.addEventListener("input", () => {
			state.searchQuery = refs.searchInput.value;
			show(refs.searchClear, state.searchQuery.length > 0);
			searchDebounced();
		});
		refs.searchInput.addEventListener("keydown", (event) => {
			if (event.key === "Escape") {
				event.stopPropagation();
				clearSearch();
			}
		});
	}
	refs.searchClear && refs.searchClear.addEventListener("click", () => clearSearch());
	for (const option of document.querySelectorAll(".mode-option")) option.addEventListener("click", () => setMode(option.dataset.mode));
	for (const tab of document.querySelectorAll("#view-tabs .tab")) tab.addEventListener("click", () => setView(tab.dataset.view));
	refs.terminalClose && refs.terminalClose.addEventListener("click", () => closeTerminal());
	refs.terminalForm && refs.terminalForm.addEventListener("submit", handleTerminalSubmit);
	refs.terminalInput && refs.terminalInput.addEventListener("keydown", handleTerminalKeys);
	refs.sessionNew && refs.sessionNew.addEventListener("click", () => startSession(false));
	refs.sessionNew && refs.sessionNew.addEventListener("contextmenu", (event) => {
		event.preventDefault();
		startSession(true);
	});
	refs.sessionStop && refs.sessionStop.addEventListener("click", () => stopSession());
	refs.sessionSelect && refs.sessionSelect.addEventListener("change", () => {
		state.activeSession = refs.sessionSelect.value;
		state.terminalCols = 0;
		state.terminalRows = 0;
		reportTerminalSize();
		focusNode(refs.terminalInput);
	});
	refs.lightboxClose && refs.lightboxClose.addEventListener("click", () => closeLightbox());
	refs.lightbox && refs.lightbox.addEventListener("click", (event) => {
		event.target === refs.lightbox && closeLightbox();
	});
	refs.overlayRoot && refs.overlayRoot.addEventListener("click", (event) => {
		event.target === refs.overlayRoot && closeOverlay();
	});
	document.addEventListener("keydown", handleGlobalKeys, true);
	window.addEventListener("resize", handleWindowResize);
	refs.transcript && attachTranscriptDelegation();
};

const renameChatTitle = () => {
	if (!state.conversation) {
		warn("Start a chat first, then you can give it a name.");
		return;
	}
	const host = refs.chatTitle;
	if (!host) return;
	const input = el("input", "rename-input");
	input.value = state.conversation.title || "";
	input.setAttribute("aria-label", "New name for this chat");
	host.replaceWith(input);
	focusNode(input);
	input.select();
	let settled = false;
	const restore = (text) => {
		const span = el("span");
		span.id = "chat-title";
		span.textContent = text;
		input.replaceWith(span);
		refs.chatTitle = span;
	};
	const commit = (save) => {
		if (settled) return;
		settled = true;
		const value = input.value.trim();
		restore(state.conversation ? state.conversation.title || "New chat" : "New chat");
		if (!save || !value || !state.conversation || value === state.conversation.title) return;
		guard(async () => {
			await invoke("conversations_update", { conversation: { ...state.conversation, title: value, snippet: null } });
			state.conversation.title = value;
			renderChatHeader();
			await refreshConversations();
		});
	};
	input.addEventListener("keydown", (event) => {
		if (event.key === "Enter") {
			event.preventDefault();
			commit(true);
		} else if (event.key === "Escape") {
			event.preventDefault();
			commit(false);
		}
	});
	input.addEventListener("blur", () => commit(true));
};

const updateProviderDot = (online, message) => {
	if (refs.providerDot) {
		refs.providerDot.dataset.state = online ? "online" : "offline";
		refs.providerDot.title = message || "";
	}
	if (refs.modelChipText) {
		const provider = activeProvider();
		const chosen = activeModel();
		refs.modelChipText.textContent = `${chosen || providerLabel(provider)} · ${online ? "connected" : "not reachable"}`;
	}
};

const boot = async () => {
	cacheRefs();
	applyFontScale(1);
	attachStaticListeners();
	state.settings = await guard(async () => invoke("settings_get"));
	if (!state.settings) {
		warn("Ash could not read its settings, so it started with the defaults. Changes may not be saved.");
	}
	applySettingsToUi();
	setView("chat");
	await Promise.all([
		attachStreamListeners(),
		attachToolListeners(),
		attachSessionListeners(),
		attachMl5Listeners(),
		attachThemeListeners(),
		attachAppListeners()
	]);
	await guard(async () => {
		applyTheme(await invoke("theme_get"));
		await invoke("theme_watch", { enable: true });
	});
	await Promise.all([
		refreshConversations(),
		guard(async () => {
			state.guidedFixes = asArray(await invoke("guided_fixes")).filter(isObject);
		}),
		guard(async () => {
			state.daemon = await invoke("ml5_status", { force: false });
			state.daemon && state.daemon.running && (await loadModels("ml5", false));
		}),
		guard(async () => {
			const provider = activeProvider();
			const health = await invoke("model_health", { provider });
			state.health[provider] = health;
			updateProviderDot(health.online, `${providerLabel(provider)}: ${health.message}`);
		}),
		guard(async () => {
			await loadModels(activeProvider(), false);
		})
	]);
	await guard(async () => {
		await invoke("ui_ready");
	});
	state.ready = true;
	if (state.settings && !state.settings.onboarded) openOnboarding(false);
	else offerLegacyImport(false);
	focusNode(refs.composerInput);
};

boot().catch((error) => {
	warn(`Ash could not finish starting up. ${error && error.message ? error.message : error}`);
});
