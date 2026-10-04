//! Process mode dispatch and path resolution.
//!
//! Nothing here does I/O beyond probing candidate runtime directories.
//! The socket bind happens in the daemon.

use nix::unistd::{AccessFlags, access, getuid};
use std::env;
use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "prompt";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Set by `spawn_self_as_daemon` on the daemon child. Value = the socket
/// path the child should bind. Only the child reads it; the parent writes
/// it before spawn so the child never re-derives the path.
pub const SOCKET_ENV: &str = "PROMPT_SOCKET";

/// Daemon name.
pub const DAEMON_NAME: &str = "promptd";

/// Filename appended to each candidate runtime directory.
const SOCKET: &str = "promptd.sock";

/// Log filename.
const LOG: &str = "promptd.log";

pub fn is_daemon() -> bool {
    let arg0 = env::args_os().next();

    let is_arg0_matched = arg0
        .as_deref()
        .and_then(|s| Path::new(s).file_name())
        .is_some_and(|name| name == DAEMON_NAME);

    let is_env_matched = env::var_os(SOCKET_ENV).filter(|v| !v.is_empty()).is_some();

    if is_arg0_matched && is_env_matched {
        true
    } else {
        false
    }
}

/// Resolve the socket path.
pub fn socket_path() -> PathBuf {
    if let Some(path) = env::var_os(SOCKET_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(path);
    }

    let uid = getuid().as_raw();

    let mut candidates: Vec<PathBuf> = Vec::with_capacity(3);

    candidates.push(PathBuf::from(format!("/var/run/user/{uid}")).join(SOCKET));

    candidates.push(PathBuf::from("/data/data/com.termux/files/usr/var").join(SOCKET));

    if let Some(xdg) = env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        candidates.push(PathBuf::from(xdg).join(SOCKET));
    }

    for candidate in &candidates {
        let Some(parent) = candidate.parent() else {
            continue;
        };

        if parent.is_dir() && access(parent, AccessFlags::W_OK).is_ok() {
            return candidate.clone();
        }
    }

    eprintln!(
        "prompt: no writable runtime directory for the daemon socket; tried: {}",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    std::process::exit(1);
}

/// Resolve the daemon log file path.
pub fn log_path() -> PathBuf {
    let base = if let Some(dir) = env::var_os("XDG_STATE_HOME").filter(|s| !s.is_empty()) {
        PathBuf::from(dir)
    } else {
        env::home_dir()
            .unwrap_or_default()
            .join(".local")
            .join("state")
    };
    PathBuf::from(base).join(APP_NAME).join(LOG)
}
