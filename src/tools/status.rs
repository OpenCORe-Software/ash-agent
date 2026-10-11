use crate::paths;
use crate::util;
use std::path::Path;
use std::time::Duration;

const DRM: &str = "/sys/class/drm";
const SKIPPED_FILESYSTEMS: &[&str] = &[
	"tmpfs", "devtmpfs", "devpts", "proc", "sysfs", "cgroup", "cgroup2", "pstore", "bpf", "tracefs", "debugfs", "securityfs", "configfs", "fusectl", "mqueue",
	"hugetlbfs", "autofs", "efivarfs", "binfmt_misc", "overlay", "squashfs", "ramfs", "rpc_pipefs", "nsfs", "fuse.portal", "fuse.gvfsd-fuse", "fuse.snapfuse",
];

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
	std::fs::read_to_string(path).ok().map(|text| text.trim().to_string()).filter(|text| !text.is_empty())
}

fn binary_bytes(bytes: u64) -> String {
	const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
	let mut value = bytes as f64;
	let mut unit = 0;
	while value >= 1024.0 && unit < UNITS.len() - 1 {
		value /= 1024.0;
		unit += 1;
	}
	if unit == 0 { format!("{bytes} B") } else { format!("{value:.2} {}", UNITS[unit]) }
}

fn percent(part: u64, whole: u64) -> u64 {
	if whole == 0 { 0 } else { ((part as f64 / whole as f64) * 100.0).round() as u64 }
}

fn plural(count: u64, word: &str) -> String {
	format!("{count} {word}{}", if count == 1 { "" } else { "s" })
}

fn os_release() -> Vec<(String, String)> {
	let text = std::fs::read_to_string("/etc/os-release").or_else(|_| std::fs::read_to_string("/usr/lib/os-release")).unwrap_or_default();
	text.lines()
		.filter_map(|line| line.split_once('='))
		.map(|(key, value)| (key.trim().to_string(), value.trim().trim_matches('"').trim_matches('\'').to_string()))
		.collect()
}

fn utsname() -> Option<(String, String, String)> {
	let mut name: libc::utsname = unsafe { std::mem::zeroed() };
	if unsafe { libc::uname(&mut name) } != 0 {
		return None;
	}
	let field = |raw: &[libc::c_char]| unsafe { std::ffi::CStr::from_ptr(raw.as_ptr()) }.to_string_lossy().into_owned();
	Some((field(&name.sysname), field(&name.release), field(&name.machine)))
}

fn distribution() -> Option<String> {
	let release = os_release();
	let value = |key: &str| release.iter().find(|(name, _)| name == key).map(|(_, value)| value.clone()).filter(|value| !value.is_empty());
	let name = value("PRETTY_NAME").or_else(|| value("NAME"))?;
	Some(match utsname() {
		Some((_, _, machine)) => format!("{name} {machine}"),
		None => name,
	})
}

fn kernel() -> Option<String> {
	utsname().map(|(system, release, _)| format!("{system} {release}"))
}

fn uptime() -> Option<String> {
	let seconds = read_trimmed("/proc/uptime")?.split_whitespace().next()?.parse::<f64>().ok()? as u64;
	let days = seconds / 86_400;
	let hours = seconds % 86_400 / 3_600;
	let minutes = seconds % 3_600 / 60;
	let mut parts = Vec::new();
	if days > 0 {
		parts.push(plural(days, "day"));
	}
	if hours > 0 {
		parts.push(plural(hours, "hour"));
	}
	if minutes > 0 || parts.is_empty() {
		parts.push(plural(minutes, "min"));
	}
	Some(parts.join(", "))
}

fn meminfo() -> Option<Vec<(String, u64)>> {
	let text = std::fs::read_to_string("/proc/meminfo").ok()?;
	Some(
		text.lines()
			.filter_map(|line| {
				let (key, rest) = line.split_once(':')?;
				let kilobytes = rest.split_whitespace().next()?.parse::<u64>().ok()?;
				Some((key.to_string(), kilobytes * 1024))
			})
			.collect(),
	)
}

fn memory() -> Option<String> {
	let info = meminfo()?;
	let value = |key: &str| info.iter().find(|(name, _)| name == key).map(|(_, value)| *value);
	let total = value("MemTotal")?;
	let available = value("MemAvailable")?;
	let used = total.saturating_sub(available);
	Some(format!("{} used of {} ({}%), {} available", binary_bytes(used), binary_bytes(total), percent(used, total), binary_bytes(available)))
}

fn swap() -> Option<String> {
	let info = meminfo()?;
	let value = |key: &str| info.iter().find(|(name, _)| name == key).map(|(_, value)| *value);
	let total = value("SwapTotal")?;
	if total == 0 {
		return Some("none".to_string());
	}
	let used = total.saturating_sub(value("SwapFree").unwrap_or(total));
	Some(format!("{} used of {} ({}%)", binary_bytes(used), binary_bytes(total), percent(used, total)))
}

fn cpu() -> Option<String> {
	let text = std::fs::read_to_string("/proc/cpuinfo").ok()?;
	let model = text
		.lines()
		.filter_map(|line| line.split_once(':'))
		.find(|(key, _)| matches!(key.trim(), "model name" | "Model" | "Hardware" | "cpu model"))
		.map(|(_, value)| value.trim().to_string())?;
	let model = model.split(" @ ").next().unwrap_or(&model).replace(" CPU", "").replace("  ", " ").trim().to_string();
	let threads = std::thread::available_parallelism().map(|count| count.get()).unwrap_or(0);
	let max_khz = (0..threads.max(1))
		.filter_map(|index| read_trimmed(format!("/sys/devices/system/cpu/cpu{index}/cpufreq/cpuinfo_max_freq")))
		.filter_map(|value| value.parse::<u64>().ok())
		.max();
	let mut out = model;
	if threads > 0 {
		out.push_str(&format!(" ({threads} threads)"));
	}
	if let Some(khz) = max_khz {
		out.push_str(&format!(" up to {:.2} GHz", khz as f64 / 1_000_000.0));
	}
	Some(out)
}

fn unescape_mount(path: &str) -> String {
	let mut out = String::with_capacity(path.len());
	let mut chars = path.chars().peekable();
	while let Some(character) = chars.next() {
		if character == '\\' {
			let digits: String = chars.clone().take(3).collect();
			if digits.len() == 3 && digits.chars().all(|digit| ('0'..='7').contains(&digit)) {
				if let Ok(code) = u8::from_str_radix(&digits, 8) {
					out.push(code as char);
					chars.nth(2);
					continue;
				}
			}
		}
		out.push(character);
	}
	out
}

fn statvfs(path: &str) -> Option<(u64, u64, u64)> {
	let target = std::ffi::CString::new(path).ok()?;
	let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
	if unsafe { libc::statvfs(target.as_ptr(), &mut stats) } != 0 {
		return None;
	}
	let block = stats.f_frsize as u64;
	let total = stats.f_blocks as u64 * block;
	let free = stats.f_bfree as u64 * block;
	let available = stats.f_bavail as u64 * block;
	(total > 0).then_some((total, total.saturating_sub(free), available))
}

fn disks() -> Vec<String> {
	let text = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
	let mut seen = std::collections::HashSet::new();
	let mut rows: Vec<(String, String)> = Vec::new();
	for line in text.lines() {
		let fields: Vec<&str> = line.split_whitespace().collect();
		let [device, mount, kind, options, ..] = fields[..] else { continue };
		let mount = unescape_mount(mount);
		let real_device = device.starts_with('/') || matches!(kind, "zfs" | "btrfs" | "bcachefs");
		if !real_device || SKIPPED_FILESYSTEMS.contains(&kind) || (kind.starts_with("fuse.") && kind != "fuse.sshfs") {
			continue;
		}
		if mount.starts_with("/boot") || mount.starts_with("/snap/") || mount.starts_with("/proc") || mount.starts_with("/sys") || (mount.starts_with("/run/") && !mount.starts_with("/run/media/")) {
			continue;
		}
		if options.split(',').any(|option| option == "bind") || !seen.insert(device.to_string()) {
			continue;
		}
		let Some((total, used, available)) = statvfs(&mount) else { continue };
		rows.push((
			mount.clone(),
			format!("{mount}: {} used of {} ({}%), {} free, {kind}", binary_bytes(used), binary_bytes(total), percent(used, total), binary_bytes(available)),
		));
	}
	rows.sort_by(|left, right| (left.0 != "/").cmp(&(right.0 != "/")).then(left.0.cmp(&right.0)));
	rows.into_iter().map(|(_, text)| text).collect()
}

fn edid_name(edid: &[u8]) -> Option<String> {
	if edid.len() < 128 {
		return None;
	}
	(0..4).find_map(|slot| {
		let block = &edid[54 + slot * 18..72 + slot * 18];
		(block[0] == 0 && block[1] == 0 && block[3] == 0xfc).then(|| String::from_utf8_lossy(&block[5..]).split('\n').next().unwrap_or("").trim().to_string()).filter(|name| !name.is_empty())
	})
}

struct Output {
	connector: String,
	name: Option<String>,
	mode: Option<(u32, u32, f64)>,
	scale: Option<f64>,
	primary: bool,
}

fn drm_outputs() -> Vec<Output> {
	let Ok(entries) = std::fs::read_dir(DRM) else { return Vec::new() };
	let mut found: Vec<Output> = entries
		.flatten()
		.filter_map(|entry| {
			let file = entry.file_name().to_string_lossy().into_owned();
			let (_, connector) = file.split_once('-').filter(|(card, _)| card.starts_with("card"))?;
			let path = entry.path();
			if read_trimmed(path.join("status")).as_deref() != Some("connected") {
				return None;
			}
			let name = std::fs::read(path.join("edid")).ok().and_then(|edid| edid_name(&edid));
			let mode = read_trimmed(path.join("modes")).and_then(|modes| {
				let first = modes.lines().next()?.trim_end_matches('i');
				let (width, height) = first.split_once('x')?;
				Some((width.parse().ok()?, height.parse().ok()?, 0.0))
			});
			Some(Output { connector: connector.to_string(), name, mode, scale: None, primary: false })
		})
		.collect();
	found.sort_by(|left, right| left.connector.cmp(&right.connector));
	found
}

async fn kscreen_outputs() -> Option<Vec<Output>> {
	if !paths::has_program("kscreen-doctor") {
		return None;
	}
	let captured = util::capture("kscreen-doctor", &["-j"], Duration::from_secs(6)).await;
	if !captured.ok {
		return None;
	}
	let value: serde_json::Value = serde_json::from_str(captured.stdout.trim()).ok()?;
	let names: Vec<(String, Option<String>)> = drm_outputs().into_iter().map(|output| (output.connector, output.name)).collect();
	let mut outputs: Vec<(i64, i64, Output)> = value
		.get("outputs")?
		.as_array()?
		.iter()
		.filter(|output| output.get("enabled").and_then(|enabled| enabled.as_bool()).unwrap_or(false))
		.filter_map(|output| {
			let connector = output.get("name")?.as_str()?.to_string();
			let current = output.get("currentModeId").map(|id| id.as_str().map(str::to_string).unwrap_or_else(|| id.to_string()));
			let mode = output.get("modes").and_then(|modes| modes.as_array()).and_then(|modes| {
				let chosen = modes.iter().find(|mode| mode.get("id").map(|id| id.as_str().map(str::to_string).unwrap_or_else(|| id.to_string())) == current)?;
				let size = chosen.get("size")?;
				Some((size.get("width")?.as_u64()? as u32, size.get("height")?.as_u64()? as u32, chosen.get("refreshRate").and_then(|rate| rate.as_f64()).unwrap_or(0.0)))
			});
			let priority = output.get("priority").and_then(|priority| priority.as_i64()).unwrap_or(i64::MAX);
			let x = output.get("pos").and_then(|pos| pos.get("x")).and_then(|x| x.as_i64()).unwrap_or(0);
			let name = names.iter().find(|(drm, _)| *drm == connector).and_then(|(_, name)| name.clone());
			let scale = output.get("scale").and_then(|scale| scale.as_f64());
			Some((priority, x, Output { connector, name, mode, scale, primary: priority == 1 }))
		})
		.collect();
	outputs.sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(&right.0)));
	(!outputs.is_empty()).then(|| outputs.into_iter().map(|(_, _, output)| output).collect())
}

fn describe_output(output: &Output) -> String {
	let mut text = output.name.clone().map(|name| format!("{name} ({})", output.connector)).unwrap_or_else(|| output.connector.clone());
	if let Some((width, height, rate)) = output.mode {
		text.push_str(&format!(": {width}x{height}"));
		if rate > 0.0 {
			text.push_str(&format!(" @ {} Hz", rate.round() as u64));
		}
		if let Some(scale) = output.scale.filter(|scale| (scale - 1.0).abs() > 0.001) {
			text.push_str(&format!(", scaled to {}x{} ({}%)", (width as f64 / scale).round() as u64, (height as f64 / scale).round() as u64, (scale * 100.0).round() as u64));
		}
	}
	if output.primary {
		text.push_str(", primary");
	}
	text
}

async fn displays() -> Vec<String> {
	let outputs = match kscreen_outputs().await {
		Some(outputs) => outputs,
		None => drm_outputs(),
	};
	outputs.iter().map(describe_output).collect()
}

async fn desktop() -> String {
	let desktop = std::env::var("XDG_CURRENT_DESKTOP").ok().filter(|value| !value.is_empty()).unwrap_or_else(|| "unknown desktop".to_string());
	let session = std::env::var("XDG_SESSION_TYPE").ok().filter(|value| !value.is_empty()).unwrap_or_else(|| "unknown".to_string());
	let pretty_session = match session.as_str() {
		"wayland" => "Wayland".to_string(),
		"x11" => "X11".to_string(),
		other => other.to_string(),
	};
	let kde = desktop.split(':').any(|part| part.eq_ignore_ascii_case("KDE"));
	let version = if kde {
		let captured = util::capture("plasmashell", &["--version"], Duration::from_secs(5)).await;
		captured.stdout.lines().find_map(|line| line.trim().strip_prefix("plasmashell ").map(str::to_string))
	} else {
		None
	};
	let mut text = match (kde, version) {
		(true, Some(version)) => format!("KDE Plasma {version}"),
		(true, None) => "KDE Plasma".to_string(),
		_ => desktop.replace(':', " / "),
	};
	text.push_str(&format!(" on {pretty_session}"));
	if let Some((width, height)) = crate::tools::screen::workspace_size().await {
		text.push_str(&format!(", whole desktop {width}x{height}"));
	}
	text
}

async fn network() -> Option<String> {
	if paths::has_program("nmcli") {
		let captured = util::capture("nmcli", &["-t", "-f", "DEVICE,TYPE,STATE,CONNECTION", "device"], Duration::from_secs(6)).await;
		if captured.ok {
			let split = |line: &str| -> Vec<String> {
				let mut fields = Vec::new();
				let mut current = String::new();
				let mut chars = line.chars();
				while let Some(character) = chars.next() {
					match character {
						'\\' => current.extend(chars.next()),
						':' => fields.push(std::mem::take(&mut current)),
						other => current.push(other),
					}
				}
				fields.push(current);
				fields
			};
			let active: Vec<String> = captured
				.stdout
				.lines()
				.map(split)
				.filter(|fields| fields.len() >= 4 && fields[2] == "connected" && fields[1] != "loopback")
				.map(|fields| {
					let kind = match fields[1].as_str() {
						"ethernet" => "wired",
						"wifi" => "Wi-Fi",
						"bt" => "Bluetooth",
						other => other,
					};
					let address = local_address(&fields[0]);
					match address {
						Some(address) => format!("{} ({kind}, {}, {address})", fields[3], fields[0]),
						None => format!("{} ({kind}, {})", fields[3], fields[0]),
					}
				})
				.collect();
			if !active.is_empty() {
				return Some(format!("connected through {}", active.join("; ")));
			}
			let state = util::capture("nmcli", &["-t", "-f", "STATE", "general"], Duration::from_secs(6)).await;
			let state = state.stdout.trim().to_lowercase();
			return Some(if state.is_empty() { "not connected".to_string() } else { state });
		}
	}
	let route = std::fs::read_to_string("/proc/net/route").ok()?;
	let device = route.lines().skip(1).find_map(|line| {
		let fields: Vec<&str> = line.split_whitespace().collect();
		(fields.get(1) == Some(&"00000000")).then(|| fields[0].to_string())
	});
	Some(match device {
		Some(device) => match local_address(&device) {
			Some(address) => format!("connected through {device} ({address})"),
			None => format!("connected through {device}"),
		},
		None => "not connected".to_string(),
	})
}

fn local_address(device: &str) -> Option<String> {
	let mut addresses: *mut libc::ifaddrs = std::ptr::null_mut();
	if unsafe { libc::getifaddrs(&mut addresses) } != 0 {
		return None;
	}
	let mut found = None;
	let mut cursor = addresses;
	while !cursor.is_null() {
		let entry = unsafe { &*cursor };
		cursor = entry.ifa_next;
		if entry.ifa_addr.is_null() || entry.ifa_name.is_null() {
			continue;
		}
		let name = unsafe { std::ffi::CStr::from_ptr(entry.ifa_name) }.to_string_lossy();
		if name != device || unsafe { (*entry.ifa_addr).sa_family } as i32 != libc::AF_INET {
			continue;
		}
		let address = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in) };
		found = Some(std::net::Ipv4Addr::from(u32::from_be(address.sin_addr.s_addr)).to_string());
		break;
	}
	unsafe { libc::freeifaddrs(addresses) };
	found
}

async fn audio() -> Option<String> {
	if !paths::has_program("pactl") {
		return None;
	}
	let (sink, listed) = tokio::join!(
		util::capture("pactl", &["get-default-sink"], Duration::from_secs(5)),
		util::capture("pactl", &["-f", "json", "list", "sinks"], Duration::from_secs(5))
	);
	let sink = sink.stdout.trim().to_string();
	if !sink.is_empty() {
		let found = serde_json::from_str::<serde_json::Value>(listed.stdout.trim())
			.ok()
			.and_then(|value| value.as_array().and_then(|sinks| sinks.iter().find(|item| item.get("name").and_then(|name| name.as_str()) == Some(sink.as_str())).cloned()));
		if let Some(found) = found {
			let description = found.get("description").and_then(|value| value.as_str()).filter(|value| !value.is_empty()).unwrap_or(&sink).to_string();
			let volume = found
				.get("volume")
				.and_then(|volume| volume.as_object())
				.and_then(|channels| channels.values().filter_map(|channel| channel.get("value_percent").and_then(|value| value.as_str())).filter_map(|value| value.trim_end_matches('%').parse::<u64>().ok()).max());
			let muted = found.get("mute").and_then(|mute| mute.as_bool()).unwrap_or(false);
			let mut text = description;
			match (muted, volume) {
				(true, _) => text.push_str(", muted"),
				(false, Some(volume)) => text.push_str(&format!(", volume {volume}%")),
				_ => {}
			}
			return Some(text);
		}
		return Some(sink);
	}
	None
}

fn amdgpu_name(device: &str, revision: &str) -> Option<String> {
	let table = ["/usr/share/libdrm/amdgpu.ids", "/usr/local/share/libdrm/amdgpu.ids"].iter().find_map(|path| std::fs::read_to_string(path).ok())?;
	let device = device.trim_start_matches("0x").to_uppercase();
	let revision = revision.trim_start_matches("0x").to_uppercase();
	table.lines().filter(|line| !line.starts_with('#')).find_map(|line| {
		let mut fields = line.split(',').map(str::trim);
		let (id, rev, name) = (fields.next()?, fields.next()?, fields.next()?);
		(id.eq_ignore_ascii_case(&device) && rev.eq_ignore_ascii_case(&revision)).then(|| name.to_string())
	})
}

fn shorten_vendor(name: &str) -> String {
	name.replace("Advanced Micro Devices, Inc. [AMD/ATI]", "AMD")
		.replace("NVIDIA Corporation", "NVIDIA")
		.replace("Intel Corporation", "Intel")
}

async fn graphics() -> Option<String> {
	let Ok(entries) = std::fs::read_dir(DRM) else { return None };
	let mut cards: Vec<String> = entries
		.flatten()
		.map(|entry| entry.file_name().to_string_lossy().into_owned())
		.filter(|name| name.starts_with("card") && !name.contains('-'))
		.collect();
	cards.sort();
	let listed = if paths::has_program("lspci") { util::capture("lspci", &["-vmm", "-k"], Duration::from_secs(8)).await.stdout } else { String::new() };
	let mut described = Vec::new();
	let mut seen = std::collections::HashSet::new();
	for card in cards {
		let device_dir = Path::new(DRM).join(&card).join("device");
		let Some(slot) = std::fs::canonicalize(&device_dir).ok().and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned())) else { continue };
		if !seen.insert(slot.clone()) {
			continue;
		}
		let vendor = read_trimmed(device_dir.join("vendor")).unwrap_or_default();
		let device = read_trimmed(device_dir.join("device")).unwrap_or_default();
		let revision = read_trimmed(device_dir.join("revision")).unwrap_or_default();
		let driver = std::fs::read_link(device_dir.join("driver")).ok().and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()));
		let short_slot = slot.split_once(':').map(|(_, rest)| rest.to_string()).unwrap_or_else(|| slot.clone());
		let block = listed.split("\n\n").find(|block| block.lines().any(|line| line.strip_prefix("Slot:").map(str::trim) == Some(short_slot.as_str()) || line.strip_prefix("Slot:").map(str::trim) == Some(slot.as_str())));
		let field = |key: &str| block.and_then(|block| block.lines().find_map(|line| line.strip_prefix(key).map(|value| value.trim().to_string())));
		let pci_name = match (field("Vendor:"), field("Device:")) {
			(Some(vendor), Some(device)) => Some(shorten_vendor(&format!("{vendor} {device}"))),
			_ => None,
		};
		let marketed = (vendor == "0x1002").then(|| amdgpu_name(&device, &revision)).flatten();
		let mut text = marketed.or(pci_name).unwrap_or_else(|| format!("PCI device {vendor}:{device}"));
		if let Some(vram) = read_trimmed(device_dir.join("mem_info_vram_total")).and_then(|value| value.parse::<u64>().ok()).filter(|value| *value > 0) {
			text.push_str(&format!(", {} video memory", binary_bytes(vram)));
		}
		match driver {
			Some(driver) => text.push_str(&format!(" (driver {driver})")),
			None => text.push_str(" (no driver loaded)"),
		}
		described.push(text);
	}
	(!described.is_empty()).then(|| described.join("; "))
}

async fn packages() -> Option<String> {
	let mut counts: Vec<String> = Vec::new();
	let dpkg = std::fs::read_to_string("/var/lib/dpkg/status").ok().map(|text| {
		text.split("\n\n").filter(|block| block.lines().any(|line| line.starts_with("Status:") && line.ends_with(" installed"))).count()
	});
	if let Some(count) = dpkg.filter(|count| *count > 0) {
		counts.push(format!("{count} (dpkg)"));
	}
	if paths::has_program("rpm") && dpkg.is_none() {
		let captured = util::capture("rpm", &["-qa"], Duration::from_secs(15)).await;
		let count = captured.stdout.lines().filter(|line| !line.trim().is_empty()).count();
		if captured.ok && count > 0 {
			counts.push(format!("{count} (rpm)"));
		}
	}
	if let Ok(entries) = std::fs::read_dir("/var/lib/pacman/local") {
		let count = entries.flatten().filter(|entry| entry.path().is_dir()).count();
		if count > 0 {
			counts.push(format!("{count} (pacman)"));
		}
	}
	let children = |path: &Path| -> Vec<std::path::PathBuf> { std::fs::read_dir(path).map(|entries| entries.flatten().map(|entry| entry.path()).collect()).unwrap_or_default() };
	let count_dirs = |path: &Path| -> usize {
		children(path)
			.iter()
			.flat_map(|reference| children(reference))
			.flat_map(|arch| children(&arch))
			.filter(|branch| branch.join("active").exists())
			.filter(|branch| {
				let reference = branch.parent().and_then(Path::parent).and_then(Path::file_name).map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
				!reference.ends_with(".Locale") && !reference.ends_with(".Debug") && !reference.ends_with(".Sources")
			})
			.count()
	};
	let flatpak_system = count_dirs(Path::new("/var/lib/flatpak/app")) + count_dirs(Path::new("/var/lib/flatpak/runtime"));
	if flatpak_system > 0 {
		counts.push(format!("{flatpak_system} (flatpak system)"));
	}
	if let Some(data) = dirs::data_dir() {
		let flatpak_user = count_dirs(&data.join("flatpak/app")) + count_dirs(&data.join("flatpak/runtime"));
		if flatpak_user > 0 {
			counts.push(format!("{flatpak_user} (flatpak user)"));
		}
	}
	if let Ok(entries) = std::fs::read_dir("/var/lib/snapd/snaps") {
		let count = entries.flatten().filter(|entry| entry.path().extension().is_some_and(|extension| extension == "snap")).count();
		if count > 0 {
			counts.push(format!("{count} (snap)"));
		}
	}
	if paths::has_program("brew") {
		let prefix = util::capture("brew", &["--prefix"], Duration::from_secs(5)).await;
		let prefix = prefix.stdout.trim();
		if !prefix.is_empty() {
			let count = ["Cellar", "Caskroom"].iter().map(|dir| std::fs::read_dir(Path::new(prefix).join(dir)).map(|entries| entries.flatten().count()).unwrap_or(0)).sum::<usize>();
			if count > 0 {
				counts.push(format!("{count} (brew)"));
			}
		}
	}
	(!counts.is_empty()).then(|| counts.join(", "))
}

async fn engine_line() -> String {
	let state = crate::ml5::state(false).await;
	if state.running {
		format!(
			"Local engine: ML5 {} is running with {} model{} loaded",
			state.version.unwrap_or_else(|| "unknown version".to_string()),
			state.loaded.len(),
			if state.loaded.len() == 1 { "" } else { "s" }
		)
	} else {
		"Local engine: ML5 is not running".to_string()
	}
}

fn admin_line() -> String {
	if !crate::tools::root::helper_installed() {
		"not installed, so actions that need administrator rights cannot run".to_string()
	} else if !crate::tools::root::policy_installed() {
		"installed, but its policy file is missing".to_string()
	} else if !paths::has_program("pkexec") {
		"installed, but pkexec is missing".to_string()
	} else {
		"available".to_string()
	}
}

pub async fn snapshot() -> serde_json::Value {
	let (desktop, displays, network, graphics, audio, packages, engine, screen, windows, clipboard, input) = tokio::join!(
		desktop(),
		displays(),
		network(),
		graphics(),
		audio(),
		packages(),
		engine_line(),
		crate::tools::screen::state(),
		crate::tools::window::scripting_available(),
		crate::tools::clipboard::available(),
		crate::tools::input::available()
	);
	let accessibility = crate::tools::access::state().await;
	let text = |value: Option<String>| value.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null);
	let list = |items: Vec<String>| match items.len() {
		0 => serde_json::Value::Null,
		1 => serde_json::Value::String(items.into_iter().next().unwrap_or_default()),
		_ => serde_json::Value::String(items.join("; ")),
	};
	let mut fields = serde_json::Map::new();
	fields.insert("distribution".into(), text(distribution()));
	fields.insert("kernel".into(), text(kernel()));
	fields.insert("uptime".into(), text(uptime()));
	fields.insert("desktop".into(), serde_json::Value::String(desktop));
	fields.insert("displays".into(), list(displays));
	fields.insert("processor".into(), text(cpu()));
	fields.insert("graphics".into(), text(graphics));
	fields.insert("memory".into(), text(memory()));
	fields.insert("swap".into(), text(swap()));
	fields.insert("disks".into(), list(disks()));
	fields.insert("network".into(), text(network));
	fields.insert("audioOutput".into(), text(audio));
	fields.insert("packages".into(), text(packages));
	fields.insert("localEngine".into(), serde_json::Value::String(engine.split_once(": ").map(|(_, rest)| rest.to_string()).unwrap_or(engine)));
	fields.insert("screenshots".into(), serde_json::Value::String(screen.message));
	fields.insert("windowControl".into(), serde_json::Value::String(if windows { "available".into() } else { "unavailable: KWin scripting could not be reached".into() }));
	fields.insert("mouseAndKeyboard".into(), serde_json::Value::String(match input {
		Ok(()) => "available".to_string(),
		Err(reason) => format!("unavailable: {reason}"),
	}));
	fields.insert("accessibility".into(), serde_json::Value::String(accessibility.message));
	fields.insert("clipboard".into(), serde_json::Value::String(if clipboard { "available".into() } else { "unavailable".into() }));
	fields.insert("administrator".into(), serde_json::Value::String(admin_line()));
	fields.retain(|_, value| !value.is_null());
	serde_json::Value::Object(fields)
}

fn label_for(key: &str) -> String {
	let mut out = String::new();
	for (index, character) in key.chars().enumerate() {
		if character.is_uppercase() {
			out.push(' ');
			out.extend(character.to_lowercase());
		} else if index == 0 {
			out.extend(character.to_uppercase());
		} else {
			out.push(character);
		}
	}
	out
}

pub async fn report() -> String {
	let snapshot = snapshot().await;
	let Some(fields) = snapshot.as_object() else { return String::new() };
	fields
		.iter()
		.map(|(key, value)| format!("{}: {}", label_for(key), value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string())))
		.collect::<Vec<_>>()
		.join("\n")
}
