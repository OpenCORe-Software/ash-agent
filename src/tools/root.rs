use crate::paths;
use crate::util;
use serde::Deserialize;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

pub const HELPER_ACTION: &str = "one.opencore.ashagent.run";
pub const POLICY_PATH: &str = "/usr/share/polkit-1/actions/one.opencore.ashagent.policy";
const AUTH_ALLOWANCE: Duration = Duration::from_secs(300);
const STOP_GRACE: Duration = Duration::from_secs(10);

pub type LogSink = Arc<dyn Fn(&str, &str) + Send + Sync>;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum HelperEvent {
	Started {
		#[allow(dead_code)]
		pid: i32,
	},
	Out { stream: String, data: String },
	TimedOut { seconds: u64 },
	Exit { code: i32 },
	Error { message: String },
}

pub fn helper_path() -> std::path::PathBuf {
	paths::root_helper()
}

pub fn helper_installed() -> bool {
	helper_path().is_file()
}

pub fn policy_installed() -> bool {
	std::path::Path::new(POLICY_PATH).is_file()
}

pub fn available() -> bool {
	helper_installed() && paths::has_program("pkexec")
}

pub fn unavailable_reason() -> Option<String> {
	if !helper_installed() {
		return Some("The administrator helper is not installed, so this cannot run with administrator rights.".to_string());
	}
	if !paths::has_program("pkexec") {
		return Some("pkexec is not installed on this system, so administrator actions are unavailable.".to_string());
	}
	None
}

fn denial_text(text: &str) -> bool {
	let lowered = text.to_lowercase();
	lowered.contains("not authorized")
		|| lowered.contains("dismissed")
		|| lowered.contains("authentication failed")
		|| lowered.contains("no authentication agent")
		|| lowered.contains("error executing command as another user")
}

pub fn denied_message() -> String {
	"Administrator rights were not granted, so nothing was changed.".to_string()
}

fn failure(message: String) -> util::Captured {
	util::Captured { ok: false, code: None, stdout: String::new(), stderr: message }
}

pub async fn run_root(command: &str, cwd: &std::path::Path, timeout: Duration, log: Option<LogSink>) -> util::Captured {
	if let Some(reason) = unavailable_reason() {
		return failure(reason);
	}
	let helper = helper_path();
	let seconds = timeout.as_secs().max(1).to_string();
	let mut child = match tokio::process::Command::new("pkexec")
		.arg("--disable-internal-agent")
		.arg(&helper)
		.arg("--cwd")
		.arg(cwd)
		.arg("--timeout")
		.arg(&seconds)
		.arg("--")
		.arg("/bin/bash")
		.arg("-lc")
		.arg(command)
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.kill_on_drop(true)
		.spawn()
	{
		Ok(child) => child,
		Err(error) => return failure(format!("Could not ask for administrator rights: {error}")),
	};
	let keep_alive = child.stdin.take();
	let Some(stdout) = child.stdout.take() else { return failure("The administrator helper produced no output channel.".to_string()) };
	let stderr = child.stderr.take();
	let stderr_task = tokio::spawn(async move {
		let mut text = String::new();
		if let Some(mut stderr) = stderr {
			let mut bytes = Vec::new();
			let _ = stderr.read_to_end(&mut bytes).await;
			text = String::from_utf8_lossy(&bytes).into_owned();
		}
		text
	});
	let mut lines = BufReader::new(stdout).lines();
	let mut output = String::new();
	let mut started = false;
	let mut exit_code: Option<i32> = None;
	let mut helper_error: Option<String> = None;
	let mut timed_out: Option<u64> = None;
	let deadline = tokio::time::Instant::now() + timeout + AUTH_ALLOWANCE + STOP_GRACE;
	let mut abandoned = false;
	loop {
		let line = match tokio::time::timeout_at(deadline, lines.next_line()).await {
			Ok(Ok(Some(line))) => line,
			Ok(Ok(None)) | Ok(Err(_)) => break,
			Err(_) => {
				abandoned = true;
				break;
			}
		};
		match serde_json::from_str::<HelperEvent>(&line) {
			Ok(HelperEvent::Started { .. }) => started = true,
			Ok(HelperEvent::Out { stream, data }) => {
				if let Some(sink) = &log {
					sink(&stream, &data);
				}
				if output.len() < 4 * 1024 * 1024 {
					output.push_str(&data);
				}
			}
			Ok(HelperEvent::TimedOut { seconds }) => timed_out = Some(seconds),
			Ok(HelperEvent::Exit { code }) => exit_code = Some(code),
			Ok(HelperEvent::Error { message }) => helper_error = Some(message),
			Err(_) => {
				output.push_str(&line);
				output.push('\n');
			}
		}
	}
	drop(keep_alive);
	let status = match tokio::time::timeout(STOP_GRACE, child.wait()).await {
		Ok(Ok(status)) => status.code(),
		_ => None,
	};
	let pkexec_text = tokio::time::timeout(Duration::from_secs(2), stderr_task).await.ok().and_then(Result::ok).unwrap_or_default();
	if abandoned {
		let mut message = output;
		message.push_str(&format!(
			"\nThe administrator command did not finish within {} seconds and was stopped.",
			timeout.as_secs()
		));
		return util::Captured { ok: false, code: None, stdout: String::new(), stderr: message };
	}
	if !started {
		if let Some(message) = helper_error {
			return failure(format!("The administrator helper refused the command: {message}"));
		}
		if matches!(status, Some(126) | Some(127)) || denial_text(&pkexec_text) {
			return failure(denied_message());
		}
		let detail = pkexec_text.trim();
		return failure(if detail.is_empty() {
			"Administrator rights could not be obtained.".to_string()
		} else {
			format!("Administrator rights could not be obtained: {}", util::truncate_end(detail, 400))
		});
	}
	let code = exit_code.or(status);
	let mut stderr = String::new();
	if let Some(seconds) = timed_out {
		stderr = format!("The command did not finish within {seconds} seconds and was stopped.");
	} else if let Some(message) = helper_error {
		stderr = message;
	}
	util::Captured { ok: timed_out.is_none() && code == Some(0), code, stdout: output, stderr }
}

pub fn policy_xml() -> String {
	format!(
		r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE policyconfig PUBLIC "-//freedesktop//DTD PolicyKit Policy Configuration 1.0//EN" "http://www.freedesktop.org/standards/PolicyKit/1/policyconfig.dtd">
<policyconfig>
	<vendor>Ember</vendor>
	<vendor_url>https://opencore.one</vendor_url>
	<icon_name>ash-agent</icon_name>
	<action id="{HELPER_ACTION}">
		<description>Run a command as an administrator for Ash Agent</description>
		<message>Ash Agent wants to run a command with administrator rights. Enter your password to allow it.</message>
		<defaults>
			<allow_any>auth_admin</allow_any>
			<allow_inactive>auth_admin</allow_inactive>
			<allow_active>auth_admin</allow_active>
		</defaults>
		<annotate key="org.freedesktop.policykit.exec.path">{}</annotate>
	</action>
</policyconfig>
"#,
		helper_path().to_string_lossy()
	)
}
