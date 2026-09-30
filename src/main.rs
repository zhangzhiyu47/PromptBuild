mod render;

use argh::FromArgs;
use nix::{
    sys::signal::Signal,
    unistd::{Uid, User, geteuid, gethostname},
};
use render::{Color, Frame, Prompt, Segment, render_prompt};
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

/// Parse a duration string into milliseconds.
/// A unit suffix is required: "500ms", "2s", "1.5s".
fn parse_duration_ms(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, scale) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1.0)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1000.0)
    } else {
        return Err(format!("missing unit suffix (ms/s): {s}"));
    };
    let v: f64 = num
        .trim()
        .parse()
        .map_err(|_| format!("invalid duration: {s}"))?;
    if v < 0.0 {
        return Err(format!("duration must be non-negative: {s}"));
    }
    Ok((v * scale).round() as u64)
}

/// Format a duration in milliseconds into a human-readable string.
fn format_duration(ms: u64) -> String {
    if ms < 1_000 {
        return format!("{ms}ms");
    }

    if ms < 60_000 {
        let s = (ms as f64 / 100.0).round() / 10.0;
        return if s.fract() == 0.0 {
            format!("{}s", s as u64)
        } else {
            format!("{s:.1}s")
        };
    }

    let total_secs = (ms + 500) / 1000;
    let (h, rem) = (total_secs / 3600, total_secs % 3600);
    let (m, s) = (rem / 60, rem % 60);

    let mut out = String::new();
    if h > 0 {
        let _ = write!(out, "{h}h");
    }
    if m > 0 {
        let _ = write!(out, "{m}m");
    }
    if s > 0 {
        let _ = write!(out, "{s}s");
    }
    out
}

fn parse_blank_lines(s: &str) -> Result<usize, String> {
    let n: usize = s.parse().map_err(|_| format!("invalid number: {s}"))?;
    if n > 10 {
        return Err(format!("blank lines must be 0-10, got {n}"));
    }
    Ok(n)
}

#[derive(FromArgs)]
#[argh(help_triggers("--help"))]
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

    /// number of blank lines before the prompt (max: 10)
    #[argh(option, short = 'b', from_str_fn(parse_blank_lines))]
    blank_lines: Option<usize>,

    /// last command duration in milliseconds, e.g. "1500ms", "1.5s"
    #[argh(option, short = 'd', from_str_fn(parse_duration_ms))]
    duration: Option<u64>,

    /// duration showing threshold
    /// (same format as -d)
    /// (default: 2s; use "0s" to always show)
    #[argh(option, short = 't', from_str_fn(parse_duration_ms))]
    duration_threshold: Option<u64>,

    /// force terminal width (columns)
    #[argh(option, short = 'c')]
    columns: Option<usize>,

    /// show version
    #[argh(switch, short = 'v')]
    version: Option<bool>,
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
    Some(String::from("detached"))
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

/// Read and validate the `PWD` environment variable.
///
/// A valid `PWD` is an absolute path without `.` or `..` components.
/// If `physical` is provided, the path must also refer to the same
/// filesystem node.
fn read_pwd(physical: Option<&Path>) -> Option<PathBuf> {
    let pwd = env::var("PWD").ok()?;
    let path = Path::new(&pwd);

    if !path.is_absolute() || pwd.split('/').any(|seg| seg == "." || seg == "..") {
        return None;
    }

    if let Some(physical) = physical
        && !same_node(path, physical)
    {
        return None;
    }

    Some(PathBuf::from(pwd))
}

/// Resolve the current working directory.
///
/// Returns `(logical, physical, cwd_valid)`:
/// - `logical`: `pwd -L` semantics, used for display.
/// - `physical`: `pwd -P` semantics, used for filesystem lookups.
/// - `cwd_valid`: whether the physical path came from the OS.
fn resolve_working_dir() -> (PathBuf, PathBuf, bool) {
    let (physical, cwd_valid) = match env::current_dir() {
        Ok(dir) => (dir, true),
        Err(_) => (read_pwd(None).unwrap_or_else(|| PathBuf::from("/")), false),
    };

    let logical = if cwd_valid {
        read_pwd(Some(&physical)).unwrap_or_else(|| physical.clone())
    } else {
        physical.clone()
    };

    (logical, physical, cwd_valid)
}

// ==============================
// Main program
// ==============================

fn main() {
    let args: Args = argh::from_env();

    if args.version.is_some() {
        println!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }

    // Read environment variables
    let cols: usize = args.columns.unwrap_or_else(|| {
        use terminal_size::{Width, terminal_size};
        match terminal_size() {
            Some((Width(cols), _)) => cols as usize,
            None => {
                eprintln!("cannot determine terminal width; please specify -c/--columns");
                std::process::exit(1);
            }
        }
    });

    // Get the physical path and logical path
    let (logical_path, physical_path, cwd_valid) = resolve_working_dir();
    let pwd = logical_path.to_string_lossy().into_owned();

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

    let badges = [
        get_git_branch(&physical_path),
        env::var("debian_chroot").ok(),
        env::var("VIRTUAL_ENV").ok().and_then(|v| {
            Path::new(&v)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        }),
        is_in_ssh.then(|| "ssh".to_string()),
    ];

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

    if let Some(dur) = args.duration {
        let threshold = args.duration_threshold.unwrap_or(2 * 1000);
        if dur >= threshold {
            segments.push(Segment {
                frame: Frame::Bracket,
                content: format_duration(dur),
                color: Color::Yellow,
            });
        }
    }

    let prompt = Prompt {
        cols,
        segments,
        is_root,
        path_text: replace_home(&pwd, &home),
        path_color: if cwd_valid {
            Color::Reset
        } else {
            Color::Yellow
        },
        blank_lines: args.blank_lines.unwrap_or(1),
    };

    render_prompt(prompt);
}
