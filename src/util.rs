use regex::Regex;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

pub fn now_ms() -> i64 {
	SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn new_id() -> String {
	uuid::Uuid::new_v4().simple().to_string()
}

pub struct Captured {
	pub ok: bool,
	pub code: Option<i32>,
	pub stdout: String,
	pub stderr: String,
}

impl Captured {
	pub fn text(&self) -> String {
		let mut out = self.stdout.trim_end().to_string();
		let err = self.stderr.trim();
		if !err.is_empty() {
			if !out.is_empty() {
				out.push('\n');
			}
			out.push_str(err);
		}
		out
	}
}

pub async fn capture(program: &str, args: &[&str], timeout: Duration) -> Captured {
	capture_with_input(program, args, None, timeout).await
}

pub async fn capture_with_input(program: &str, args: &[&str], input: Option<&str>, timeout: Duration) -> Captured {
	let mut cmd = Command::new(program);
	cmd.args(args)
		.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.env("LC_ALL", "C.UTF-8")
		.env("NO_COLOR", "1")
		.env("SYSTEMD_PAGER", "")
		.env("PAGER", "cat")
		.kill_on_drop(true);
	let mut child = match cmd.spawn() {
		Ok(c) => c,
		Err(e) => {
			return Captured { ok: false, code: None, stdout: String::new(), stderr: format!("{program}: {e}") };
		}
	};
	if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
		let data = data.to_string();
		tokio::spawn(async move {
			let _ = stdin.write_all(data.as_bytes()).await;
			let _ = stdin.shutdown().await;
		});
	}
	match tokio::time::timeout(timeout, child.wait_with_output()).await {
		Ok(Ok(out)) => Captured {
			ok: out.status.success(),
			code: out.status.code(),
			stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
			stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
		},
		Ok(Err(e)) => Captured { ok: false, code: None, stdout: String::new(), stderr: e.to_string() },
		Err(_) => Captured { ok: false, code: None, stdout: String::new(), stderr: format!("{program} did not finish within {} seconds", timeout.as_secs()) },
	}
}

pub fn strip_ansi(input: &str) -> String {
	static RE: OnceLock<Regex> = OnceLock::new();
	let re = RE.get_or_init(|| {
		Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[PX^_][^\x1b]*\x1b\\|\x1b[@-Z\\-_]|\x1b|[\x00-\x07\x0b\x0c\x0e-\x1a\x1c-\x1f\x7f]").unwrap()
	});
	re.replace_all(input, "").into_owned()
}

pub fn clean_terminal(input: &str) -> String {
	let stripped = strip_ansi(input);
	let mut out = String::with_capacity(stripped.len());
	for (i, line) in stripped.split('\n').enumerate() {
		if i > 0 {
			out.push('\n');
		}
		let line = line.strip_suffix('\r').unwrap_or(line);
		let visible = line.rsplit('\r').find(|part| !part.is_empty()).unwrap_or("");
		let mut cleaned = String::with_capacity(visible.len());
		for ch in visible.chars() {
			if ch == '\u{8}' {
				cleaned.pop();
			} else {
				cleaned.push(ch);
			}
		}
		out.push_str(&cleaned);
	}
	out
}

pub fn truncate_middle(text: &str, head: usize, tail: usize) -> String {
	if text.len() <= head + tail + 64 {
		return text.to_string();
	}
	let head_end = floor_char(text, head);
	let tail_start = ceil_char(text, text.len() - tail);
	let skipped = text[head_end..tail_start].lines().count();
	format!("{}\n\n[… {} lines omitted …]\n\n{}", &text[..head_end], skipped.max(1), &text[tail_start..])
}

pub fn truncate_end(text: &str, max: usize) -> String {
	if text.len() <= max {
		return text.to_string();
	}
	format!("{}…", &text[..floor_char(text, max)])
}

pub fn floor_char(text: &str, mut index: usize) -> usize {
	index = index.min(text.len());
	while index > 0 && !text.is_char_boundary(index) {
		index -= 1;
	}
	index
}

pub fn ceil_char(text: &str, mut index: usize) -> usize {
	while index < text.len() && !text.is_char_boundary(index) {
		index += 1;
	}
	index.min(text.len())
}

pub fn shell_quote(value: &str) -> String {
	if !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c)) {
		return value.to_string();
	}
	format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn human_bytes(bytes: u64) -> String {
	const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
	let mut value = bytes as f64;
	let mut unit = 0;
	while value >= 1024.0 && unit < UNITS.len() - 1 {
		value /= 1024.0;
		unit += 1;
	}
	if unit == 0 {
		format!("{bytes} B")
	} else {
		format!("{value:.1} {}", UNITS[unit])
	}
}

pub fn monotonic_us() -> u64 {
	let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
	unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
	ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1000
}

pub fn kill_group(pid: u32, signal: i32) {
	if pid > 1 {
		unsafe { libc::kill(-(pid as i32), signal) };
	}
}

pub fn percent_decode(input: &str) -> Option<Vec<u8>> {
	let bytes = input.as_bytes();
	let mut out = Vec::with_capacity(bytes.len());
	let mut i = 0;
	while i < bytes.len() {
		if bytes[i] == b'%' {
			let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
			out.push(u8::from_str_radix(hex, 16).ok()?);
			i += 3;
		} else {
			out.push(bytes[i]);
			i += 1;
		}
	}
	Some(out)
}
