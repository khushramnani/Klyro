//! Wire format and state files shared by the daemon and the CLI.
//!
//! Transport: one JSON line per request and per response over 127.0.0.1 TCP. The port and a
//! random token live in `daemon.json` in the user's state dir, readable only by that user;
//! the daemon rejects requests without the token.

use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub token: String,
    pub session: String,
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub json: bool,
    /// `open` only: start the session with a visible browser window.
    #[serde(default)]
    pub headed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    /// Output on success, error message on failure.
    pub out: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Info {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

/// `KLYRO_HOME`, else a per-user state dir.
pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("KLYRO_HOME") {
        return h.into();
    }
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
    };
    base.unwrap_or_else(std::env::temp_dir).join("klyro")
}

pub fn info_path() -> PathBuf {
    home().join("daemon.json")
}

pub fn log_path() -> PathBuf {
    home().join("daemon.log")
}

pub fn read_info() -> Option<Info> {
    serde_json::from_slice(&std::fs::read(info_path()).ok()?).ok()
}

/// Atomic write (temp file + rename), owner-only on Unix. On Windows the per-user
/// LOCALAPPDATA ACL already keeps other users out.
pub fn write_info(info: &Info) -> io::Result<()> {
    std::fs::create_dir_all(home())?;
    let tmp = home().join(format!("daemon.json.{}", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts.open(&tmp)?;
    f.write_all(serde_json::to_string(info)?.as_bytes())?;
    drop(f);
    std::fs::rename(tmp, info_path())
}

/// Removes daemon.json, but only if it still belongs to this process.
pub fn remove_info() {
    if read_info().is_some_and(|i| i.pid == std::process::id()) {
        let _ = std::fs::remove_file(info_path());
    }
}

/// 256-bit random hex token.
pub fn new_token() -> String {
    use std::hash::{BuildHasher, RandomState};
    // ponytail: SipHash keyed from OS randomness via RandomState; swap for getrandom if this ever leaves localhost.
    (0..4).map(|i| format!("{:016x}", RandomState::new().hash_one((i, std::time::SystemTime::now())))).collect()
}
