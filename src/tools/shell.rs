use super::root::LogSink;
use super::Risk;
use crate::util;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

const DANGEROUS: &[&str] = &[
	"rm -rf /",
	"mkfs",
	"dd if=",
	"dd of=/dev",
	":(){:|:&};:",
	"chmod -R 777 /",
	"chown -R",
	"> /dev/sd",
	"wipefs",
	"shred /dev",
	"fdisk",
	"parted",
	"mkswap /dev",
	"dd of=/dev/sd",
];

const HIGH: &[&str] = &[
	"rm -rf",
	"rm -fr",
	"rm -r",
	"apt purge",
	"apt-get purge",
	"apt remove",
	"apt-get remove",
	"dpkg -r",
	"snap remove",
	"flatpak uninstall",
	"systemctl disable",
	"systemctl mask",
	"killall",
	"pkill",
	"userdel",
	"groupdel",
	"iptables",
	"nft ",
	"ufw disable",
	"grub-install",
	"update-grub",
	"grub-mkconfig",
	"mount ",
	"umount ",
	"losetup",
	"cryptsetup",
	"lvremove",
	"vgremove",
	"zpool",
	"btrfs subvolume delete",
	"passwd",
	"usermod",
	"visudo",
	"crontab -r",
];

const MEDIUM: &[&str] = &[
	"apt install",
	"apt-get install",
	"apt upgrade",
	"apt-get upgrade",
	"apt full-upgrade",
	"apt autoremove",
	"snap install",
	"flatpak install",
	"pip install",
	"npm install -g",
	"systemctl start",
	"systemctl stop",
	"systemctl restart",
	"systemctl enable",
	"systemctl --user",
	"nmcli",
	"cp ",
	"mv ",
	"mkdir ",
	"touch ",
	"tee ",
	"sed -i",
	"truncate",
	"git clone",
	"curl -o",
	"wget -O",
	"chmod",
	"chown",
	"ln -s",
	"tar -x",
	"unzip",
	"kill ",
];

const READ_ONLY: &[&str] = &[
	"ls",
	"cat",
	"head",
	"tail",
	"less",
	"grep",
	"rg",
	"find",
	"stat",
	"file",
	"du",
	"df",
	"free",
	"uptime",
	"uname",
	"whoami",
	"id",
	"pwd",
	"echo",
	"which",
	"type",
	"env",
	"printenv",
	"date",
	"systemctl status",
	"systemctl list",
	"systemctl is-active",
	"journalctl",
	"dmesg",
	"lsblk",
	"lscpu",
	"lsusb",
	"lspci",
	"ip a",
	"ip addr",
	"ip route",
	"nmcli dev",
	"nmcli con show",
	"hostnamectl",
	"localectl",
	"timedatectl",
	"free -h",
	"ps",
	"top",
	"htop",
	"ping",
	"ss",
	"netstat",
	"lsof",
	"wc",
	"sort",
	"uniq",
	"cut",
	"awk",
	"sed ",
	"jq",
	"dpkg -l",
	"apt list",
	"apt-cache",
	"flatpak list",
	"snap list",
	"pactl list",
	"wpctl status",
	"aplay -l",
	"xrandr",
	"kscreen-doctor -o",
	"fc-list",
	"gh --version",
	"lsblk -f",
];

const SPLITTERS: &[&str] = &["&&", "||", ";", "|", "\n"];

fn commands(command: &str) -> Vec<String> {
	let mut parts = vec![command.to_string()];
	for splitter in SPLITTERS {
		parts = parts.iter().flat_map(|part| part.split(splitter).map(str::to_string)).collect();
	}
	parts.into_iter().map(|part| part.trim().to_string()).filter(|part| !part.is_empty()).collect()
}

fn head_words(command: &str) -> String {
	let stripped = command.trim_start_matches(|c: char| c.is_whitespace());
	let stripped = stripped.strip_prefix("sudo ").unwrap_or(stripped).trim_start();
	let words: Vec<&str> = stripped.split_whitespace().take(3).collect();
	words.join(" ")
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
	let lowered = haystack.to_lowercase();
	needles.iter().any(|needle| lowered.contains(&needle.to_lowercase()))
}

pub fn classify(command: &str, root: bool) -> Risk {
	let trimmed = command.trim();
	if trimmed.is_empty() {
		return Risk::Low;
	}
	if contains_any(trimmed, DANGEROUS) {
		return Risk::Danger;
	}
	let mut risk = Risk::Low;
	for single in commands(trimmed) {
		let single_risk = single_risk(&single, root);
		if single_risk as u8 > risk as u8 {
			risk = single_risk;
		}
	}
	risk
}

fn single_risk(command: &str, root: bool) -> Risk {
	let bare = command.strip_prefix("sudo ").unwrap_or(command);
	if contains_any(bare, DANGEROUS) {
		return Risk::Danger;
	}
	if contains_any(bare, HIGH) {
		return Risk::High;
	}
	let head = head_words(bare);
	let reads = READ_ONLY.iter().any(|verb| {
		let verb = verb.trim();
		if verb.contains(' ') {
			head.starts_with(verb)
		} else {
			head.split_whitespace().next() == Some(verb)
		}
	});
	if reads {
		return Risk::Low;
	}
	if contains_any(bare, MEDIUM) {
		return Risk::Medium;
	}
	if root {
		return Risk::High;
	}
	if head.starts_with("rm") || head.starts_with("mv") || head.starts_with("dd") {
		return Risk::High;
	}
	Risk::Medium
}

pub fn title_for(command: &str, root: bool) -> String {
	let head = head_words(command.trim_start_matches("sudo "));
	let first = head.split_whitespace().next().unwrap_or("command").to_string();
	let verb = match first.as_str() {
		"ls" | "cat" | "head" | "tail" | "less" => "Look at",
		"grep" | "rg" | "find" => "Search for",
		"df" | "du" | "lsblk" | "free" => "Check",
		"systemctl" => "Manage a service",
		"apt" | "apt-get" | "dpkg" => "Install or change software",
		"nmcli" => "Change the network settings",
		"cp" | "mv" | "mkdir" | "touch" => "Change files",
		"rm" => "Delete files",
		"journalctl" | "dmesg" => "Read the system log",
		"pactl" | "wpctl" => "Change the sound settings",
		"chmod" | "chown" => "Change file permissions",
		"pip" | "npm" | "cargo" => "Install software",
		_ => "Run a command",
	};
	if root {
		format!("{verb} as administrator")
	} else {
		verb.to_string()
	}
}

pub fn explain(command: &str, root: bool, risk: Risk) -> String {
	let head = head_words(command.trim_start_matches("sudo "));
	let first = head.split_whitespace().next().unwrap_or("");
	let what = match first {
		"ls" => "This lists the files in a directory so the assistant can see what is there.",
		"cat" | "head" | "tail" => "This shows the contents of a file.",
		"grep" | "rg" => "This searches inside files for a word or pattern.",
		"find" => "This searches the file system for files that match.",
		"df" | "du" => "This reports how much disk space is used.",
		"free" => "This reports how much memory is in use.",
		"lsblk" => "This lists the disks and their partitions.",
		"systemctl" => "This asks the system service manager to change a background service.",
		"journalctl" | "dmesg" => "This reads the system log to find out what went wrong.",
		"apt" | "apt-get" => "This installs, updates or removes software using Ubuntu's package system.",
		"dpkg" => "This works with installed software packages directly.",
		"nmcli" => "This changes the network settings, such as Wi-Fi or a connection profile.",
		"pactl" | "wpctl" => "This changes the sound settings.",
		"cp" | "mv" | "mkdir" | "touch" | "ln" => "This changes files on disk.",
		"rm" => "This deletes files. Deleted files usually cannot be recovered.",
		"chmod" | "chown" => "This changes who is allowed to read or change a file.",
		"mount" | "umount" => "This attaches or detaches a disk or partition.",
		"grub-install" | "update-grub" | "grub-mkconfig" => "This changes the boot loader, which is what starts the system when you turn the computer on.",
		"cryptsetup" => "This works with disk encryption.",
		"pip" | "npm" | "cargo" => "This installs software from a programming package index.",
		"curl" | "wget" => "This downloads something from the internet.",
		"kill" | "pkill" | "killall" => "This stops a running program.",
		_ => "This runs a program on the computer.",
	};
	let mut text = what.to_string();
	if root {
		text.push_str(" It runs with administrator rights, which means it is allowed to change any part of the system.");
	}
	match risk {
		Risk::Danger => text.push_str(" This one is dangerous: it can destroy data or make the system unbootable. Read the command before allowing it."),
		Risk::High => text.push_str(" This one can change or remove things in a way that is hard to undo."),
		Risk::Medium => text.push_str(" It makes a change to the system, but a reversible one."),
		Risk::Low => text.push_str(" It only reads information and does not change anything."),
	}
	text
}

pub fn resolve_cwd(value: Option<&str>) -> std::path::PathBuf {
	match value.map(str::trim).filter(|value| !value.is_empty()) {
		Some(path) => {
			let path = if let Some(rest) = path.strip_prefix("~/") {
				crate::paths::home().join(rest)
			} else {
				std::path::PathBuf::from(path)
			};
			if path.is_dir() {
				path
			} else {
				crate::paths::home()
			}
		}
		None => crate::paths::home(),
	}
}

pub struct Executed {
	pub output: String,
	pub code: Option<i32>,
	pub ok: bool,
	pub duration_ms: u64,
}

struct GroupGuard {
	pid: Option<u32>,
}

impl Drop for GroupGuard {
	fn drop(&mut self) {
		if let Some(pid) = self.pid.take() {
			util::kill_group(pid, libc::SIGTERM);
			std::thread::spawn(move || {
				std::thread::sleep(Duration::from_secs(2));
				util::kill_group(pid, libc::SIGKILL);
			});
		}
	}
}

async fn pump<R>(reader: Option<R>, stream: &'static str, log: Option<LogSink>, collected: Arc<std::sync::Mutex<String>>)
where
	R: tokio::io::AsyncRead + Unpin,
{
	let Some(reader) = reader else { return };
	let mut reader = BufReader::new(reader);
	let mut line = Vec::new();
	loop {
		line.clear();
		match reader.read_until(b'\n', &mut line).await {
			Ok(0) | Err(_) => break,
			Ok(_) => {
				let text = String::from_utf8_lossy(&line).into_owned();
				if let Some(sink) = &log {
					sink(stream, &text);
				}
				if let Ok(mut all) = collected.lock() {
					if all.len() < OUTPUT_LIMIT {
						all.push_str(&text);
					}
				}
			}
		}
	}
}

async fn run_user(command: &str, cwd: &std::path::Path, timeout: Duration, log: Option<LogSink>) -> util::Captured {
	let mut child = match tokio::process::Command::new("/bin/bash")
		.arg("-lc")
		.arg(command)
		.current_dir(cwd)
		.stdin(Stdio::null())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.env("LC_ALL", "C.UTF-8")
		.env("NO_COLOR", "1")
		.env("PAGER", "cat")
		.env("GIT_PAGER", "cat")
		.env("SYSTEMD_PAGER", "")
		.env("SYSTEMD_COLORS", "0")
		.process_group(0)
		.kill_on_drop(true)
		.spawn()
	{
		Ok(child) => child,
		Err(error) => return util::Captured { ok: false, code: None, stdout: String::new(), stderr: format!("The command could not be started: {error}") },
	};
	let mut guard = GroupGuard { pid: child.id() };
	let collected = Arc::new(std::sync::Mutex::new(String::new()));
	let readers = tokio::spawn({
		let collected = Arc::clone(&collected);
		let stdout = child.stdout.take();
		let stderr = child.stderr.take();
		async move {
			tokio::join!(pump(stdout, "stdout", log.clone(), Arc::clone(&collected)), pump(stderr, "stderr", log, collected));
		}
	});
	let waited = tokio::time::timeout(timeout, child.wait()).await;
	let (ok, code, note) = match waited {
		Ok(Ok(status)) => {
			guard.pid = None;
			(status.success(), status.code().or_else(|| std::os::unix::process::ExitStatusExt::signal(&status).map(|signal| 128 + signal)), String::new())
		}
		Ok(Err(error)) => (false, None, format!("The command could not be waited for: {error}")),
		Err(_) => (false, None, format!("The command did not finish within {} seconds and was stopped.", timeout.as_secs())),
	};
	drop(guard);
	let _ = tokio::time::timeout(Duration::from_secs(3), readers).await;
	let output = collected.lock().map(|text| text.clone()).unwrap_or_default();
	util::Captured { ok, code, stdout: output, stderr: note }
}

pub async fn execute(command: &str, cwd: &std::path::Path, root: bool, timeout: Duration, log: Option<LogSink>) -> Executed {
	let started = std::time::Instant::now();
	let captured = if root { super::root::run_root(command, cwd, timeout, log).await } else { run_user(command, cwd, timeout, log).await };
	let duration_ms = started.elapsed().as_millis() as u64;
	let mut output = util::clean_terminal(&util::truncate_middle(&captured.text(), 24 * 1024, 8 * 1024));
	if output.trim().is_empty() {
		output = if captured.ok { "The command finished without printing anything.".to_string() } else { format!("The command failed with exit code {} and printed nothing.", captured.code.map(|code| code.to_string()).unwrap_or_else(|| "unknown".to_string())) };
	}
	Executed { ok: captured.ok, code: captured.code, output, duration_ms }
}
