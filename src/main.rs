mod render;

use argh::FromArgs;
use nix::{
    sys::signal::Signal,
    unistd::{Uid, User, geteuid, gethostname},
};
use render::{Color, Frame, PathDisplay, Segment, render_prompt};
use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::fmt::Write;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// A collection of pipeline statuses.
#[derive(Debug)]
struct PipeStatus {
    /// Raw status codes, stored in order.
    codes: Vec<i32>,
}

impl PipeStatus {
    /// Parse a string like "0 1 130".
    fn parse(s: &str) -> Result<Self, String> {
        let mut codes = Vec::new();
        for token in s.split_whitespace() {
            match token.parse::<i32>() {
                Ok(n) => codes.push(n),
                Err(_) => return Err(format!("invalid status: {token}")),
            }
        }
        Ok(PipeStatus { codes })
    }

    /// Format into the error tail for display.
    /// Returns an empty string when every status is zero.
    fn error_tail(&self) -> String {
        if self.codes.iter().all(|&code| code == 0) {
            return String::new();
        }

        let mut err_tail = String::new();
        for (i, &status) in self.codes.iter().enumerate() {
            if i > 0 {
                err_tail.push('|');
            }

            match status
                .checked_sub(128)
                .filter(|n| *n > 0)
                .and_then(|n| Signal::try_from(n).ok())
            {
                Some(sig) => {
                    let _ = write!(err_tail, "{sig}");
                }
                None => {
                    let _ = write!(err_tail, "{status}");
                }
            }
        }
        err_tail
    }
}

#[derive(FromArgs)]
/// Render a shell prompt.
struct Args {
    /// pipeline exit statuses, space-separated, e.g. "0 1 130".
    #[argh(option, short = 'p', from_str_fn(PipeStatus::parse))]
    pipestatus: Option<PipeStatus>,

    /// force to show user name
    #[argh(switch, short = 'u')]
    show_user: Option<bool>,

    /// force to show host name
    #[argh(switch, short = 'h')]
    show_host: Option<bool>,
}

/// Resolve a `.git` entry into the repository's Git directory.
fn resolve_git_dir(dot_git: &Path) -> Option<PathBuf> {
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let mut cur = dot_git.to_path_buf();

    loop {
        let meta = fs::symlink_metadata(&cur).ok()?;
        if !seen.insert((meta.dev(), meta.ino())) {
            return None;
        }

        if meta.file_type().is_symlink() {
            let target = fs::read_link(&cur).ok()?;
            cur = if target.is_absolute() {
                target
            } else {
                cur.parent()?.join(target)
            };
        } else if meta.is_dir() {
            return cur.join("HEAD").is_file().then_some(cur);
        } else if meta.is_file() {
            let content = fs::read_to_string(&cur).ok()?;
            let target = content
                .lines()
                .find_map(|line| line.strip_prefix("gitdir:"))
                .map(str::trim)?;
            cur = if Path::new(target).is_absolute() {
                PathBuf::from(target)
            } else {
                cur.parent()?.join(target)
            };
        } else {
            return None;
        }
    }
}

/// Walk upwards from `start` until a Git directory is found.
fn find_git_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        if let Some(git_dir) = resolve_git_dir(&dir.join(".git")) {
            return Some(git_dir);
        }
        dir = dir.parent()?;
    }
}

/// Read `HEAD` and format it for display.
fn get_git_branch(physical_dir: &Path) -> Option<String> {
    let git_dir = find_git_dir(physical_dir)?;
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();

    if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
        return Some(branch.to_string());
    }
    Some(String::from("detected"))
}

/// Whether two paths refer to the same filesystem node.
fn same_node(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }

    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => ma.dev() == mb.dev() && ma.ino() == mb.ino(),
        _ => false,
    }
}

/// Return the logical working directory, following `pwd -L` semantics.
fn logical_pwd(physical: &Path) -> PathBuf {
    if let Ok(pwd) = env::var("PWD")
        && let path = Path::new(&pwd)
        && path.is_absolute()
        && !pwd.split('/').any(|seg| seg == "." || seg == "..")
        && same_node(path, physical)
    {
        return path.to_path_buf();
    }
    physical.to_path_buf()
}

/// Replace the home directory prefix with '~' if present.
fn replace_home(path: &str, home: &str) -> String {
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return path.to_string();
    }
    if path == home {
        return "~".to_string();
    }
    if let Some(rest) = path.strip_prefix(home)
        && rest.starts_with('/')
    {
        return format!("~{}", rest);
    }
    path.to_string()
}

fn is_in_ssh_session() -> bool {
    const SSH_ENV_KEYS: [&str; 3] = ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"];

    SSH_ENV_KEYS
        .iter()
        .any(|key| env::var_os(key).is_some_and(|v| !v.is_empty()))
}

/// Resolve the username to display.
fn resolve_username(euid: Uid) -> String {
    if euid.is_root() {
        return "root".to_string();
    }

    if let Some(name) = env::var("USER")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| env::var("LOGNAME").ok().filter(|s| !s.is_empty()))
    {
        return name;
    }

    if let Some(name) = User::from_uid(euid).ok().flatten().map(|u| u.name) {
        return name;
    }

    format!("#{euid}")
}

// ==============================
// Main program
// ==============================

fn main() {
    let args: Args = argh::from_env();

    // Read environment variables
    let cols: usize = terminal_size::terminal_size()
        .map(|(c, _)| c.0 as usize)
        .unwrap_or(80);

    let (current_dir, cwd_valid) = match env::current_dir() {
        Ok(dir) => (dir, true),
        Err(_) => (
            env::var("PWD")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/")),
            false,
        ),
    };

    let pwd = logical_pwd(&current_dir).to_string_lossy().into_owned();

    let euid = geteuid();
    let is_root = euid.is_root();
    let is_in_ssh = is_in_ssh_session();

    let mut user_and_host = String::new();

    if is_root || args.show_user.is_some() {
        user_and_host.push_str(&resolve_username(euid));
    }

    if is_in_ssh || args.show_host.is_some() {
        if !user_and_host.is_empty() {
            user_and_host.push_str("㉿");
        }

        let hostname = gethostname()
            .unwrap_or_else(|_| OsString::from("unknown"))
            .to_string_lossy()
            .into_owned();
        user_and_host.push_str(&hostname);
    }

    let home = env::home_dir()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    let mut segments = Vec::new();

    let mut badges = vec![
        get_git_branch(&current_dir),
        env::var("debian_chroot").ok(),
        env::var("VIRTUAL_ENV").ok().and_then(|v| {
            Path::new(&v)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        }),
    ];

    if is_in_ssh {
        badges.push(Some("ssh".to_string()));
    }

    for badge in badges.into_iter().flatten() {
        segments.push(Segment {
            frame: Frame::Paren,
            content: badge,
            color: Color::Border,
        });
    }

    if !user_and_host.is_empty() {
        segments.push(Segment {
            frame: Frame::Paren,
            content: user_and_host,
            color: if is_root {
                Color::BoldRed
            } else {
                Color::BoldBlue
            },
        });
    }

    if let Some(ps) = &args.pipestatus {
        let err = ps.error_tail();
        if !err.is_empty() {
            segments.push(Segment {
                frame: Frame::Bracket,
                content: err,
                color: Color::BoldRed,
            });
        }
    }

    let path = PathDisplay {
        text: replace_home(&pwd, &home),
        color: if cwd_valid {
            Color::Reset
        } else {
            Color::Yellow
        },
    };

    render_prompt(cols, &segments, is_root, &path);
}
