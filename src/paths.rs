use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub fn home() -> PathBuf {
	dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

pub fn config_dir() -> PathBuf {
	dirs::config_dir().unwrap_or_else(|| home().join(".config")).join("ash-agent")
}

pub fn data_dir() -> PathBuf {
	dirs::data_dir().unwrap_or_else(|| home().join(".local/share")).join("ash-agent")
}

pub fn runtime_dir() -> PathBuf {
	dirs::runtime_dir().unwrap_or_else(std::env::temp_dir).join("ash-agent")
}

pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
	fs::create_dir_all(dir)?;
	fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
	if let Some(parent) = path.parent() {
		ensure_private_dir(parent)?;
	}
	let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
	{
		let mut file = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
		file.write_all(bytes)?;
		file.sync_all()?;
	}
	fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
	fs::rename(&tmp, path)
}

pub fn root_helper() -> PathBuf {
	let installed = PathBuf::from("/usr/libexec/ash-agent/ash-agent-root");
	if installed.exists() {
		return installed;
	}
	std::env::current_exe()
		.ok()
		.and_then(|exe| exe.parent().map(|dir| dir.join("ash-agent-root")))
		.filter(|p| p.exists())
		.unwrap_or(installed)
}

pub fn find_program(name: &str) -> Option<PathBuf> {
	if name.contains('/') {
		let p = PathBuf::from(name);
		return p.is_file().then_some(p);
	}
	let path = std::env::var_os("PATH").unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".into());
	std::env::split_paths(&path)
		.chain(["/usr/sbin", "/sbin", "/usr/lib/cups/backend"].iter().map(PathBuf::from))
		.map(|dir| dir.join(name))
		.find(|candidate| {
			candidate
				.metadata()
				.map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
				.unwrap_or(false)
		})
}

pub fn has_program(name: &str) -> bool {
	find_program(name).is_some()
}
