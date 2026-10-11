use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const SAFE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const GRACE: Duration = Duration::from_secs(3);

static PLAIN: AtomicBool = AtomicBool::new(false);

const STRIPPED_ENV: &[&str] = &[
	"LD_PRELOAD",
	"LD_LIBRARY_PATH",
	"LD_AUDIT",
	"LD_DEBUG",
	"LD_PROFILE",
	"PYTHONPATH",
	"PYTHONHOME",
	"PYTHONSTARTUP",
	"PERL5LIB",
	"PERL5OPT",
	"RUBYLIB",
	"RUBYOPT",
	"GEM_PATH",
	"NODE_PATH",
	"NODE_OPTIONS",
	"JAVA_TOOL_OPTIONS",
	"_JAVA_OPTIONS",
	"GCONV_PATH",
	"BASH_ENV",
	"ENV",
	"CDPATH",
	"GLOBIGNORE",
	"SHELLOPTS",
	"BASHOPTS",
	"PS4",
	"PROMPT_COMMAND",
	"IFS",
	"BASH_FUNC_",
];

fn strip_environment(pipe_mode: bool) {
	for (key, _) in std::env::vars_os() {
		let key_text = key.to_string_lossy().to_string();
		let blocked = STRIPPED_ENV.iter().any(|bad| if bad.ends_with('_') { key_text.starts_with(bad) } else { key_text == *bad });
		if blocked {
			std::env::remove_var(&key);
		}
	}
	std::env::set_var("PATH", SAFE_PATH);
	std::env::set_var("HOME", "/root");
	std::env::set_var("USER", "root");
	std::env::set_var("LOGNAME", "root");
	std::env::set_var("SHELL", "/bin/bash");
	if pipe_mode {
		std::env::set_var("LC_ALL", "C.UTF-8");
		std::env::set_var("PAGER", "cat");
		std::env::set_var("SYSTEMD_PAGER", "");
		std::env::set_var("DEBIAN_FRONTEND", "noninteractive");
	}
}

fn emit(line: &str) {
	let mut out = std::io::stdout().lock();
	let _ = out.write_all(line.as_bytes());
	let _ = out.write_all(b"\n");
	let _ = out.flush();
}

fn escape(text: &str) -> String {
	let mut escaped = String::with_capacity(text.len() + 2);
	for character in text.chars() {
		match character {
			'"' => escaped.push_str("\\\""),
			'\\' => escaped.push_str("\\\\"),
			'\n' => escaped.push_str("\\n"),
			'\r' => escaped.push_str("\\r"),
			'\t' => escaped.push_str("\\t"),
			c if (c as u32) < 0x20 || c as u32 == 0x7f => escaped.push_str(&format!("\\u{:04x}", c as u32)),
			other => escaped.push(other),
		}
	}
	escaped
}

fn event_out(stream: &str, data: &str) {
	emit(&format!("{{\"kind\":\"out\",\"stream\":\"{stream}\",\"data\":\"{}\"}}", escape(data)));
}

fn fail(message: &str) -> ! {
	if PLAIN.load(Ordering::SeqCst) {
		let _ = writeln!(std::io::stderr(), "ash-agent-root: {message}");
	} else {
		emit(&format!("{{\"kind\":\"error\",\"message\":\"{}\"}}", escape(message)));
	}
	std::process::exit(126);
}

fn usage() -> String {
	format!(
		"ash-agent-root {VERSION}\n\
		 Runs a command with administrator rights for Ash Agent.\n\n\
		 Usage:\n\
		 \tash-agent-root [--cwd DIR] [--timeout SECONDS] -- PROGRAM [ARGS...]\n\
		 \tash-agent-root --exec [--cwd DIR] -- PROGRAM [ARGS...]\n\n\
		 Without --exec the program's output is reported as JSON lines, it is stopped\n\
		 after the timeout, and it is stopped when standard input is closed.\n\
		 With --exec the helper replaces itself with the program, which keeps the\n\
		 terminal it was started in.\n\n\
		 This program is not meant to be run directly. It is started through pkexec\n\
		 by Ash Agent when an action needs administrator rights.\n"
	)
}

struct Request {
	cwd: std::path::PathBuf,
	timeout: Option<Duration>,
	exec: bool,
	program: String,
	args: Vec<String>,
}

fn parse() -> Request {
	let mut raw: Vec<String> = std::env::args().skip(1).collect();
	match raw.first().map(String::as_str) {
		Some("--version") | Some("-V") => {
			println!("ash-agent-root {VERSION}");
			std::process::exit(0);
		}
		Some("--help") | Some("-h") => {
			print!("{}", usage());
			std::process::exit(0);
		}
		_ => {}
	}
	if raw.iter().take_while(|arg| arg.as_str() != "--").any(|arg| arg == "--exec") {
		PLAIN.store(true, Ordering::SeqCst);
	}
	let mut cwd = std::path::PathBuf::from("/root");
	let mut timeout = None;
	let mut exec = false;
	let mut index = 0;
	while index < raw.len() {
		match raw[index].as_str() {
			"--cwd" => {
				index += 1;
				let Some(value) = raw.get(index) else { fail("--cwd needs a directory") };
				cwd = std::path::PathBuf::from(value);
			}
			"--timeout" => {
				index += 1;
				let Some(value) = raw.get(index) else { fail("--timeout needs a number of seconds") };
				let Ok(seconds) = value.parse::<u64>() else { fail("--timeout needs a whole number of seconds") };
				timeout = (seconds > 0).then(|| Duration::from_secs(seconds));
			}
			"--exec" => exec = true,
			"--" => {
				index += 1;
				break;
			}
			other if other.starts_with('-') => fail(&format!("Unknown option {other}")),
			_ => break,
		}
		index += 1;
	}
	if index >= raw.len() {
		fail("No command was given");
	}
	let program = raw[index].clone();
	raw.drain(..=index);
	if program.trim().is_empty() {
		fail("No command was given");
	}
	Request { cwd, timeout, exec, program, args: raw }
}

fn main() {
	if unsafe { libc::geteuid() } != 0 {
		fail("This helper must run with administrator rights.");
	}
	let request = parse();
	strip_environment(!request.exec);
	let cwd = if request.cwd.is_absolute() && request.cwd.is_dir() { request.cwd.clone() } else { std::path::PathBuf::from("/root") };
	if let Err(error) = std::env::set_current_dir(&cwd) {
		fail(&format!("Could not enter {}: {error}", cwd.display()));
	}
	if request.exec {
		let error = Command::new(&request.program).args(&request.args).exec();
		fail(&format!("Could not start {}: {error}", request.program));
	}
	run_piped(&request);
}

fn signal_group(pid: i32, signal: i32) {
	if pid > 1 {
		unsafe { libc::kill(-pid, signal) };
	}
}

fn stop_group(pid: i32, finished: &AtomicBool) {
	signal_group(pid, libc::SIGTERM);
	let started = Instant::now();
	while started.elapsed() < GRACE {
		if finished.load(Ordering::SeqCst) {
			return;
		}
		std::thread::sleep(Duration::from_millis(50));
	}
	if !finished.load(Ordering::SeqCst) {
		signal_group(pid, libc::SIGKILL);
	}
}

fn run_piped(request: &Request) -> ! {
	let mut command = Command::new(&request.program);
	command.args(&request.args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
	let mut child = match command.spawn() {
		Ok(child) => child,
		Err(error) => fail(&format!("Could not start {}: {error}", request.program)),
	};
	let pid = child.id() as i32;
	emit(&format!("{{\"kind\":\"started\",\"pid\":{pid}}}"));
	let finished = Arc::new(AtomicBool::new(false));
	let stopping = Arc::new(AtomicI32::new(0));
	let out_handle = stream(child.stdout.take(), "stdout");
	let err_handle = stream(child.stderr.take(), "stderr");
	{
		let finished = Arc::clone(&finished);
		let stopping = Arc::clone(&stopping);
		std::thread::spawn(move || {
			let mut sink = [0u8; 256];
			let mut stdin = std::io::stdin().lock();
			loop {
				match stdin.read(&mut sink) {
					Ok(0) | Err(_) => break,
					Ok(_) => {}
				}
			}
			if !finished.load(Ordering::SeqCst) && stopping.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
				stop_group(pid, &finished);
			}
		});
	}
	let started = Instant::now();
	let mut timed_out = false;
	let status = loop {
		match child.try_wait() {
			Ok(Some(status)) => break Some(status),
			Ok(None) => {}
			Err(_) => break None,
		}
		if let Some(limit) = request.timeout {
			if !timed_out && started.elapsed() >= limit && stopping.compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
				timed_out = true;
				emit(&format!("{{\"kind\":\"timed_out\",\"seconds\":{}}}", limit.as_secs()));
				let finished = Arc::clone(&finished);
				std::thread::spawn(move || stop_group(pid, &finished));
			}
		}
		std::thread::sleep(Duration::from_millis(25));
	};
	finished.store(true, Ordering::SeqCst);
	let _ = out_handle.join();
	let _ = err_handle.join();
	let code = match status {
		Some(status) => status.code().unwrap_or_else(|| 128 + std::os::unix::process::ExitStatusExt::signal(&status).unwrap_or(0)),
		None => fail("The command could not be waited for."),
	};
	let code = if timed_out { 124 } else { code };
	emit(&format!("{{\"kind\":\"exit\",\"code\":{code}}}"));
	std::process::exit(code);
}

fn stream<R: std::io::Read + Send + 'static>(reader: Option<R>, name: &'static str) -> std::thread::JoinHandle<()> {
	std::thread::spawn(move || {
		let Some(reader) = reader else { return };
		let mut reader = BufReader::new(reader);
		let mut line = Vec::new();
		loop {
			line.clear();
			match reader.read_until(b'\n', &mut line) {
				Ok(0) | Err(_) => break,
				Ok(_) => event_out(name, &String::from_utf8_lossy(&line)),
			}
		}
	})
}
