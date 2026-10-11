use crate::paths;
use crate::util;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use regex::Regex;
use serde::Serialize;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

const BACKLOG_LIMIT: usize = 1024 * 1024;
const SLICE: Duration = Duration::from_millis(100);
const USER_START_WAIT: Duration = Duration::from_secs(30);
const ROOT_START_WAIT: Duration = Duration::from_secs(300);
const COLLECT_LIMIT: usize = 2 * 1024 * 1024;
const COLLECT_HEAD: usize = 256 * 1024;
const COLLECT_TAIL: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Output {
	pub stream: &'static str,
	pub data: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
	pub id: String,
	pub shell: String,
	pub root: bool,
	pub agent: bool,
	pub cwd: String,
	pub alive: bool,
}

pub struct Options {
	pub shell: Option<String>,
	pub root: bool,
	pub agent: bool,
	pub cols: u16,
	pub rows: u16,
	pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
	Finished,
	Running,
	Ended,
	Cancelled,
}

pub struct RunResult {
	pub output: String,
	pub code: Option<i32>,
	pub state: RunState,
}

struct Shared {
	backlog: String,
	alive: bool,
}

pub struct Session {
	pub id: String,
	pub shell: String,
	pub root: bool,
	pub agent: bool,
	start_dir: PathBuf,
	writer: Mutex<Box<dyn Write + Send>>,
	master: Mutex<Box<dyn MasterPty + Send>>,
	child: Mutex<Box<dyn Child + Send + Sync>>,
	pid: Option<u32>,
	pub events: broadcast::Sender<Output>,
	shared: Arc<(Mutex<Shared>, Condvar)>,
	pending: Mutex<Option<String>>,
	running: Mutex<()>,
}

fn marker_pattern() -> &'static Regex {
	static PATTERN: OnceLock<Regex> = OnceLock::new();
	PATTERN.get_or_init(|| Regex::new(r"\r?\n?__ash_([0-9a-f]{32})_(\d+)__\r?\n?").unwrap())
}

fn marker_line(token: &str) -> String {
	format!("printf '\\n__ash_%s_%s__\\n' '{token}' \"$?\"\n")
}

fn same_file(left: &str, right: &str) -> bool {
	left == right || matches!((std::fs::canonicalize(left), std::fs::canonicalize(right)), (Ok(a), Ok(b)) if a == b)
}

fn find_marker(text: &str, from: usize, token: &str) -> Option<(usize, usize, Option<i32>)> {
	let from = util::floor_char(text, from);
	marker_pattern()
		.captures_iter(&text[from..])
		.find(|found| &found[1] == token)
		.and_then(|found| found.get(0).map(|whole| (from + whole.start(), from + whole.end(), found[2].parse::<i32>().ok())))
}

fn compact(collected: &mut String) {
	if collected.len() <= COLLECT_LIMIT {
		return;
	}
	let head_end = util::floor_char(collected, COLLECT_HEAD);
	let tail_start = util::ceil_char(collected, collected.len() - COLLECT_TAIL);
	let tail = collected[tail_start..].to_string();
	collected.truncate(head_end);
	collected.push_str("\n[… output omitted …]\n");
	collected.push_str(&tail);
}

pub fn strip_markers(text: &str) -> String {
	marker_pattern().replace_all(text, "\n").into_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
	Posix,
	Zsh,
	Other,
}

fn shell_family(shell: &str) -> Family {
	let resolved = std::fs::canonicalize(shell).unwrap_or_else(|_| PathBuf::from(shell));
	let name = resolved.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
	let given = std::path::Path::new(shell).file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
	let pick = |name: &str| match name {
		"bash" | "sh" | "dash" | "ksh" | "mksh" => Family::Posix,
		"zsh" => Family::Zsh,
		_ => Family::Other,
	};
	match pick(&given) {
		Family::Other => pick(&name),
		found => found,
	}
}

fn allowed_shell(requested: Option<&str>) -> String {
	let known = shells();
	match requested.map(str::trim).filter(|shell| !shell.is_empty()) {
		Some(shell) if known.iter().any(|candidate| same_file(candidate, shell)) => shell.to_string(),
		_ => {
			let fallback = default_shell();
			if shell_family(&fallback) != Family::Other && known.iter().any(|candidate| same_file(candidate, &fallback)) {
				fallback
			} else {
				"/bin/bash".to_string()
			}
		}
	}
}

impl Session {
	pub fn start(id: &str, options: Options) -> Result<Arc<Session>, String> {
		let shell = if options.agent { "/bin/bash".to_string() } else { allowed_shell(options.shell.as_deref()) };
		if shell_family(&shell) == Family::Other {
			return Err(format!("{shell} cannot be used here; choose bash, zsh or sh."));
		}
		let start_dir = options.cwd.filter(|dir| dir.is_dir()).unwrap_or_else(paths::home);
		let size = PtySize { rows: options.rows.max(2), cols: options.cols.max(10), pixel_width: 0, pixel_height: 0 };
		let pair = native_pty_system().openpty(size).map_err(|error| format!("Could not open a terminal: {error}"))?;
		let mut builder = if options.root {
			if !super::root::available() {
				return Err(super::root::unavailable_reason().unwrap_or_else(|| "Administrator terminals are unavailable.".to_string()));
			}
			let mut builder = CommandBuilder::new("pkexec");
			builder.arg("--disable-internal-agent");
			builder.arg(super::root::helper_path());
			builder.args(["--exec", "--cwd"]);
			builder.arg(&start_dir);
			builder.args(["--", shell.as_str(), "-l"]);
			builder
		} else {
			let mut builder = CommandBuilder::new(&shell);
			builder.arg("-l");
			builder
		};
		builder.cwd(&start_dir);
		builder.env("TERM", "dumb");
		builder.env("NO_COLOR", "1");
		builder.env("PAGER", "cat");
		builder.env("GIT_PAGER", "cat");
		builder.env("MANPAGER", "cat");
		builder.env("SYSTEMD_PAGER", "");
		builder.env("SYSTEMD_COLORS", "0");
		builder.env("DEBIAN_FRONTEND", "readline");
		if options.agent {
			builder.env("LC_ALL", "C.UTF-8");
		}
		let child = pair.slave.spawn_command(builder).map_err(|error| {
			if options.root {
				format!("Could not ask for administrator rights: {error}")
			} else {
				format!("Could not start {shell}: {error}")
			}
		})?;
		drop(pair.slave);
		let pid = child.process_id();
		let writer = pair.master.take_writer().map_err(|error| format!("Could not open the terminal for writing: {error}"))?;
		let mut reader = pair.master.try_clone_reader().map_err(|error| format!("Could not read from the terminal: {error}"))?;
		let (events, _) = broadcast::channel(1024);
		let shared = Arc::new((Mutex::new(Shared { backlog: String::new(), alive: true }), Condvar::new()));
		let session = Arc::new(Session {
			id: id.to_string(),
			shell: shell.clone(),
			root: options.root,
			agent: options.agent,
			start_dir,
			writer: Mutex::new(writer),
			master: Mutex::new(pair.master),
			child: Mutex::new(child),
			pid,
			events: events.clone(),
			shared: Arc::clone(&shared),
			pending: Mutex::new(None),
			running: Mutex::new(()),
		});
		std::thread::Builder::new()
			.name(format!("ash-pty-{}", util::truncate_end(id, 8)))
			.spawn(move || {
				let mut buffer = [0u8; 16384];
				let mut carry: Vec<u8> = Vec::new();
				loop {
					match reader.read(&mut buffer) {
						Ok(0) => break,
						Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
						Err(_) => break,
						Ok(count) => {
							carry.extend_from_slice(&buffer[..count]);
							let valid = match std::str::from_utf8(&carry) {
								Ok(_) => carry.len(),
								Err(error) if error.error_len().is_none() => error.valid_up_to(),
								Err(_) => carry.len(),
							};
							let data = String::from_utf8_lossy(&carry[..valid]).into_owned();
							carry.drain(..valid);
							if data.is_empty() {
								continue;
							}
							{
								let (lock, signal) = &*shared;
								if let Ok(mut state) = lock.lock() {
									state.backlog.push_str(&data);
									if state.backlog.len() > BACKLOG_LIMIT {
										let cut = util::ceil_char(&state.backlog, state.backlog.len() - BACKLOG_LIMIT / 2);
										state.backlog.drain(..cut);
									}
								}
								signal.notify_all();
							}
							let _ = events.send(Output { stream: "out", data });
						}
					}
				}
				let (lock, signal) = &*shared;
				if let Ok(mut state) = lock.lock() {
					if !carry.is_empty() {
						let rest = String::from_utf8_lossy(&carry).into_owned();
						state.backlog.push_str(&rest);
						let _ = events.send(Output { stream: "out", data: rest });
					}
					state.alive = false;
				}
				signal.notify_all();
				let _ = events.send(Output { stream: "exit", data: String::new() });
			})
			.map_err(|error| format!("Could not watch the terminal: {error}"))?;
		Ok(session)
	}

	pub fn start_agent(id: &str, root: bool, cancel: &CancellationToken) -> Result<Arc<Session>, String> {
		let session = Session::start(id, Options { shell: None, root, agent: true, cols: 200, rows: 50, cwd: None })?;
		match session.prepare(cancel) {
			Ok(()) => Ok(session),
			Err(error) => {
				session.stop();
				Err(error)
			}
		}
	}

	pub fn prepare(&self, cancel: &CancellationToken) -> Result<(), String> {
		let token = util::new_id();
		let quiet = match shell_family(&self.shell) {
			Family::Zsh => "stty -echo 2>/dev/null; unsetopt zle prompt_cr prompt_sp 2>/dev/null; unset HISTFILE; precmd_functions=(); preexec_functions=(); PROMPT=''; RPROMPT=''; PS2=''",
			_ => "stty -echo 2>/dev/null; set +o emacs +o vi 2>/dev/null; unset HISTFILE PROMPT_COMMAND; PS1=''; PS2=''; PS0=''",
		};
		self.write(&format!("{quiet}; export TERM=dumb NO_COLOR=1 PAGER=cat GIT_PAGER=cat MANPAGER=cat SYSTEMD_PAGER= SYSTEMD_COLORS=0\n{}", marker_line(&token)))?;
		let wait = if self.root { ROOT_START_WAIT } else { USER_START_WAIT };
		let (_, state, text) = self.wait_for(&token, wait, cancel);
		match state {
			RunState::Finished => Ok(()),
			RunState::Cancelled => Err("The terminal was not opened because it was stopped.".to_string()),
			RunState::Ended => {
				let text = util::clean_terminal(&text);
				let detail = text.trim();
				let lowered = detail.to_lowercase();
				Err(if self.root && (lowered.contains("no authentication agent") || lowered.contains("no session for cookie")) {
					"No password prompt could be shown in this desktop session, so administrator rights could not be requested. Log out and back in, then try again.".to_string()
				} else if self.root && (detail.is_empty() || lowered.contains("not authorized") || lowered.contains("dismissed") || lowered.contains("authentication")) {
					super::root::denied_message()
				} else if detail.is_empty() {
					"The terminal closed as soon as it started.".to_string()
				} else {
					format!("The terminal closed as soon as it started: {}", util::truncate_end(detail, 400))
				})
			}
			RunState::Running => Err(if self.root {
				"Administrator rights were not granted in time, so the terminal was not opened.".to_string()
			} else {
				"The terminal did not become ready in time.".to_string()
			}),
		}
	}

	pub fn read_available(&self, wait: Duration) -> Option<String> {
		let (lock, signal) = &*self.shared;
		let mut state = lock.lock().ok()?;
		if state.backlog.is_empty() && state.alive {
			state = signal.wait_timeout(state, wait).ok()?.0;
		}
		if state.backlog.is_empty() && !state.alive {
			return None;
		}
		Some(std::mem::take(&mut state.backlog))
	}

	pub fn write(&self, data: &str) -> Result<(), String> {
		let mut writer = self.writer.lock().map_err(|_| "The terminal is unavailable.".to_string())?;
		writer.write_all(data.as_bytes()).map_err(|error| format!("Could not write to the terminal: {error}"))?;
		writer.flush().map_err(|error| format!("Could not write to the terminal: {error}"))
	}

	pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
		let master = self.master.lock().map_err(|_| "The terminal is unavailable.".to_string())?;
		master
			.resize(PtySize { rows: rows.max(2), cols: cols.max(10), pixel_width: 0, pixel_height: 0 })
			.map_err(|error| format!("Could not resize the terminal: {error}"))
	}

	pub fn alive(&self) -> bool {
		self.shared.0.lock().map(|state| state.alive).unwrap_or(false)
	}

	pub fn stop(&self) {
		if let Ok(mut child) = self.child.lock() {
			if self.root {
				let _ = self.write("\u{3}\u{4}");
			}
			let _ = child.kill();
		}
		if let Some(pid) = self.pid {
			util::kill_group(pid, libc::SIGHUP);
		}
		let (lock, signal) = &*self.shared;
		if let Ok(mut state) = lock.lock() {
			state.alive = false;
		}
		signal.notify_all();
	}

	pub fn pid(&self) -> Option<u32> {
		self.pid
	}

	pub fn cwd(&self) -> PathBuf {
		self.pid
			.and_then(|pid| std::fs::read_link(format!("/proc/{pid}/cwd")).ok())
			.unwrap_or_else(|| self.start_dir.clone())
	}

	pub fn info(&self) -> SessionInfo {
		SessionInfo {
			id: self.id.clone(),
			shell: self.shell.clone(),
			root: self.root,
			agent: self.agent,
			cwd: self.cwd().to_string_lossy().to_string(),
			alive: self.alive(),
		}
	}

	pub fn busy(&self) -> bool {
		let Some(pid) = self.pid else { return false };
		let leader = self.master.lock().ok().and_then(|master| master.process_group_leader());
		matches!(leader, Some(group) if group > 0 && group as u32 != pid)
	}

	pub fn exit_code(&self) -> Option<i32> {
		self.child.lock().ok().and_then(|mut child| child.try_wait().ok().flatten()).map(|status| status.exit_code() as i32)
	}

	fn take_backlog(&self) -> String {
		self.shared.0.lock().map(|mut state| std::mem::take(&mut state.backlog)).unwrap_or_default()
	}

	fn wait_for(&self, token: &str, wait: Duration, cancel: &CancellationToken) -> (Option<i32>, RunState, String) {
		let deadline = Instant::now() + wait;
		let (lock, signal) = &*self.shared;
		let mut collected = String::new();
		let Ok(mut state) = lock.lock() else { return (None, RunState::Ended, collected) };
		loop {
			let scan_from = collected.len().saturating_sub(96);
			collected.push_str(&std::mem::take(&mut state.backlog));
			if let Some((start, end, code)) = find_marker(&collected, scan_from, token) {
				let rest = collected[end..].to_string();
				state.backlog.insert_str(0, &rest);
				collected.truncate(start);
				return (code, RunState::Finished, strip_markers(&collected));
			}
			compact(&mut collected);
			if !state.alive {
				return (None, RunState::Ended, strip_markers(&collected));
			}
			if cancel.is_cancelled() {
				return (None, RunState::Cancelled, strip_markers(&collected));
			}
			let now = Instant::now();
			if now >= deadline {
				return (None, RunState::Running, strip_markers(&collected));
			}
			state = match signal.wait_timeout(state, SLICE.min(deadline - now)) {
				Ok((guard, _)) => guard,
				Err(_) => return (None, RunState::Ended, strip_markers(&collected)),
			};
		}
	}

	pub fn run_once(&self, command: &str, wait: Duration, cancel: &CancellationToken) -> RunResult {
		let Ok(_exclusive) = self.running.lock() else {
			return RunResult { output: "The terminal is unavailable.".to_string(), code: None, state: RunState::Ended };
		};
		if !self.alive() {
			return RunResult { output: self.take_backlog(), code: self.exit_code(), state: RunState::Ended };
		}
		let mut pending = match self.pending.lock() {
			Ok(pending) => pending,
			Err(_) => return RunResult { output: "The terminal is unavailable.".to_string(), code: None, state: RunState::Ended },
		};
		let continuing = pending.is_some() && self.busy();
		let mut preface = String::new();
		if !continuing {
			if let Some(previous) = pending.take() {
				let earlier = self.take_backlog();
				let (body, code) = match find_marker(&earlier, 0, &previous) {
					Some((start, end, code)) => {
						let mut body = earlier[..start].to_string();
						body.push_str(&earlier[end..]);
						(body, code)
					}
					None => (earlier, None),
				};
				let body = util::clean_terminal(&strip_markers(&body));
				let summary = match code {
					Some(code) => format!("The previous command finished with exit code {code}."),
					None => "The previous command is no longer running.".to_string(),
				};
				preface = if body.trim().is_empty() { format!("{summary}\n\n") } else { format!("{summary} Its remaining output:\n{}\n\n", body.trim_end()) };
			}
		}
		let token = if continuing {
			let token = pending.clone().unwrap_or_default();
			if !command.is_empty() {
				let line = if command.ends_with('\n') { command.to_string() } else { format!("{command}\n") };
				if let Err(error) = self.write(&line) {
					return RunResult { output: error, code: None, state: RunState::Ended };
				}
			}
			token
		} else {
			if command.trim().is_empty() {
				let rest = util::clean_terminal(&strip_markers(&self.take_backlog()));
				let output = format!("{preface}{rest}").trim().to_string();
				return RunResult { output: if output.is_empty() { "Nothing is running in this terminal.".to_string() } else { output }, code: None, state: RunState::Finished };
			}
			let token = util::new_id();
			let line = format!("eval -- {}; {}", util::shell_quote(command), marker_line(&token));
			if let Err(error) = self.write(&line) {
				return RunResult { output: error, code: None, state: RunState::Ended };
			}
			*pending = Some(token.clone());
			token
		};
		drop(pending);
		let (code, state, text) = self.wait_for(&token, wait, cancel);
		let mut state = state;
		match state {
			RunState::Finished => {
				if let Ok(mut pending) = self.pending.lock() {
					*pending = None;
				}
			}
			RunState::Cancelled => {
				let _ = self.write("\u{3}");
				std::thread::sleep(Duration::from_millis(300));
				if !self.busy() {
					if let Ok(mut pending) = self.pending.lock() {
						*pending = None;
					}
				}
			}
			RunState::Running => {
				if !self.busy() && !self.alive() {
					state = RunState::Ended;
				}
			}
			RunState::Ended => {}
		}
		let code = if state == RunState::Ended { self.exit_code() } else { code };
		RunResult { output: format!("{preface}{}", util::clean_terminal(&text)), code, state }
	}
}

impl Drop for Session {
	fn drop(&mut self) {
		if self.alive() {
			self.stop();
		}
	}
}

pub fn default_shell() -> String {
	std::env::var("SHELL").ok().filter(|shell| !shell.trim().is_empty()).unwrap_or_else(|| "/bin/bash".to_string())
}

pub fn shells() -> Vec<String> {
	let mut found: Vec<String> = std::fs::read_to_string("/etc/shells")
		.unwrap_or_default()
		.lines()
		.map(str::trim)
		.filter(|line| line.starts_with('/') && !line.ends_with("nologin") && !line.ends_with("false"))
		.filter(|line| std::path::Path::new(line).is_file() && shell_family(line) != Family::Other)
		.map(str::to_string)
		.collect();
	for candidate in ["/bin/bash", "/usr/bin/bash", "/bin/zsh", "/usr/bin/zsh", "/bin/sh"] {
		if std::path::Path::new(candidate).is_file() && shell_family(candidate) != Family::Other && !found.iter().any(|shell| shell == candidate) {
			found.push(candidate.to_string());
		}
	}
	let mut unique: Vec<String> = Vec::new();
	for shell in found {
		let resolved = std::fs::canonicalize(&shell).unwrap_or_else(|_| PathBuf::from(&shell));
		if !unique.iter().any(|known| std::fs::canonicalize(known).unwrap_or_else(|_| PathBuf::from(known)) == resolved) {
			unique.push(shell);
		}
	}
	if let Some(index) = unique.iter().position(|shell| shell.ends_with("/bash")) {
		let bash = unique.remove(index);
		unique.insert(0, bash);
	}
	unique
}
