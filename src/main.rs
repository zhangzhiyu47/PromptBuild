//! A custom prompt for a Unix-like shell, displaying user, host, path, Git branch,
//! virtual environment, chroot, and exit status of the last pipeline.
//!
//! The prompt consists of two lines, with colors indicating root/non-root and errors.

use nix::{
    sys::signal::Signal,
    unistd::{User, gethostname, getuid},
};
use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Color constants
const RESET: &str = "\x1b[0m";
const BOLD_RED: &str = "\x1b[1;31m";
const BORDER_ROOT: &str = "\x1b[34m"; // Blue
const BORDER_USER: &str = "\x1b[32m"; // Green
const USER_ROOT: &str = "\x1b[1;31m"; // Red
const USER_NORMAL: &str = "\x1b[1;34m"; // Blue

/// Warning colour for a working directory that could not be read.
const WARN: &str = "\x1b[33m"; // Yellow

const PROMPT_ROOT: &str = "#";
const PROMPT_USER: &str = "$";

/// Short hash length for a detached HEAD.
const SHORT_HASH: usize = 7;

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
    head.get(..SHORT_HASH).map(str::to_string)
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

/// Clip `s` to `budget` columns from the front.
fn clip_front(s: &str, budget: usize) -> &str {
    let mut width = 0;
    let mut end = 0;
    for (i, c) in s.char_indices() {
        let w = c.width().unwrap_or(0);
        if width + w > budget {
            break;
        }
        width += w;
        end = i + c.len_utf8();
    }
    &s[..end]
}

/// Clip `s` to `budget` columns from the front, backing up to the last '/'.
fn clip_head(s: &str, budget: usize) -> &str {
    let clipped = clip_front(s, budget);
    match clipped.rfind('/') {
        Some(p) if p > 0 => &clipped[..p],
        _ => clipped,
    }
}

/// Clip `s` to the last `budget` columns, keeping only complete
/// trailing segments (starting right after a '/').
fn clip_tail(s: &str, budget: usize) -> &str {
    let mut width = 0;
    let mut after_slash = None;
    for (i, c) in s.char_indices().rev() {
        let w = c.width().unwrap_or(0);
        if width + w > budget {
            break;
        }
        width += w;
        if c == '/' {
            after_slash = Some(i + 1);
        }
    }
    after_slash.map_or("", |i| &s[i..])
}

/// Clip `s` to the last `budget` columns.
fn clip_back(s: &str, budget: usize) -> &str {
    let mut width = 0;
    let mut start = s.len();
    for (i, c) in s.char_indices().rev() {
        let w = c.width().unwrap_or(0);
        if width + w > budget {
            break;
        }
        width += w;
        start = i;
    }
    &s[start..]
}

/// Turn a long path into an abbreviated form: keep as many trailing
/// complete segments as fit, then fill the remaining space with the head.
fn truncate_middle(path: &str, max: usize) -> String {
    if UnicodeWidthStr::width(path) <= max {
        return path.to_string();
    }

    const ELLIPSIS: &str = "…";
    let ell_width = UnicodeWidthStr::width(ELLIPSIS);
    if max <= ell_width {
        return ELLIPSIS.to_string();
    }

    // Trailing path segment, used as a fallback when no '/' fits.
    let last_start = path.rfind('/').map_or(0, |p| p + 1);
    let last = &path[last_start..];
    let last_width = UnicodeWidthStr::width(last);

    // Preferred shape: "<head>/…/<tail>", both sides aligned to '/'.
    let connector = ell_width + 2; // "/…/"
    if max >= connector {
        let available = max - connector;

        // Tail goes first: grab as many trailing complete segments as fit.
        let tail = clip_tail(path, available);
        let tail_width = UnicodeWidthStr::width(tail);

        if !tail.is_empty() && tail_width >= last_width {
            // Whatever is left goes to the head, still aligned to '/'.
            let head_budget = available - tail_width;
            let head = clip_head(path, head_budget);
            let head = if head.is_empty() || head == "/" { "" } else { head };

            return if head.is_empty() {
                format!("{ELLIPSIS}/{tail}")
            } else {
                format!("{head}/{ELLIPSIS}/{tail}")
            };
        }
    }

    // Fallbacks: "…/<last>", then "…<tail of last>".
    if ell_width + 1 + last_width <= max {
        return format!("{ELLIPSIS}/{last}");
    }
    format!("{ELLIPSIS}{}", clip_back(last, max - ell_width))
}

/// Turn whitespace-separated pipeline statuses into a display string.
/// Empty when every status is zero, so the caller renders no error tail.
fn error_string(pipestatus: &str) -> String {
    use std::fmt::Write;

    let mut err_tail = String::new();
    let mut has_nonzero = false;

    for token in pipestatus.split_whitespace() {
        let Ok(status) = token.parse::<i32>() else { continue };

        if !err_tail.is_empty() {
            err_tail.push('|');
        }

        if status != 0 {
            has_nonzero = true;
        }

        match status
            .checked_sub(128)
            .filter(|n| *n > 0)
            .and_then(|n| Signal::try_from(n).ok())
        {
            Some(sig) => { let _ = write!(err_tail, "{sig}"); }
            None => { let _ = write!(err_tail, "{status}"); }
        }
    }

    if has_nonzero { err_tail } else { String::new() }
}

/// Render both prompt lines, truncating the path to fit `cols`.
fn render_prompt(
    cols: usize,
    badges: &[&str],
    username: &str,
    hostname: &str,
    err: &str,
    work_dir: &str,
    is_root: bool,
    cwd_valid: bool,
) {
    let (border, user_color, sym) = if is_root {
        (BORDER_ROOT, USER_ROOT, PROMPT_ROOT)
    } else {
        (BORDER_USER, USER_NORMAL, PROMPT_USER)
    };

    // Path turns yellow when the working directory had to be guessed
    // from `$PWD`
    let path_color = if cwd_valid { RESET } else { WARN };

    let wrap_width = UnicodeWidthStr::width("─");

    // Each badge renders as "(label)─": two parentheses plus one dash.
    let badge_width: usize = badges
        .iter()
        .map(|b| UnicodeWidthStr::width(*b) + 2 + wrap_width)
        .sum();

    // Error tail contributes "─[" + err + "]" when present.
    let err_width = if err.is_empty() {
        0
    } else {
        UnicodeWidthStr::width("─[") + UnicodeWidthStr::width(err) + 1
    };

    // Width of everything except the path itself. All plain text here; no
    // escape sequences are counted.
    let fixed_width: usize = ["┌──(", username, "㉿", hostname, ")-[]"]
        .iter()
        .map(|s| UnicodeWidthStr::width(*s))
        .sum::<usize>()
        + badge_width
        + err_width;

    let path_len = cols.saturating_sub(fixed_width);
    let path = truncate_middle(work_dir, path_len);

    let rendered: String = badges.iter().map(|b| format!("({})─", b)).collect();

    let err_tail = if err.is_empty() {
        String::new()
    } else {
        format!("─[{BOLD_RED}{err}{border}]")
    };

    println!();
    println!(
        "{border}┌──{rendered}({user_color}{username}㉿{hostname}{RESET}{border})-[{path_color}{path}{border}]{err_tail}{RESET}"
    );
    println!("{border}└─{user_color}{sym}{RESET} ");
}

// ==============================
// Main program
// ==============================
fn main() {
    // Read command line arguments (pipestatus)
    let pipestatus_str = env::args().nth(1).unwrap_or_default();

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

    let pwd = logical_pwd(&current_dir);
    let pwd = pwd.to_string_lossy();

    let home = env::home_dir()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let uid = getuid();
    let username = User::from_uid(uid)
        .ok()
        .flatten()
        .map(|user| user.name)
        .unwrap_or_else(|| "unknown".to_string());

    let hostname = gethostname()
        .unwrap_or_else(|_| OsString::from("unknown"))
        .to_string_lossy()
        .into_owned();

    // Bare labels for the parenthesised indicators before user@host.
    // Extend this array to add a new badge; nothing else changes.
    let git = get_git_branch(&current_dir);
    let chroot = env::var("debian_chroot").ok();
    let venv = env::var("VIRTUAL_ENV").ok().and_then(|v| {
        Path::new(&v)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    });

    let badges: Vec<&str> = [git.as_deref(), chroot.as_deref(), venv.as_deref()]
        .into_iter()
        .flatten()
        .collect();

    let err_str = error_string(&pipestatus_str);

    // Truncate the current path
    let path_with_home = replace_home(&pwd, &home);

    // Output
    render_prompt(
        cols,
        &badges,
        &username,
        &hostname,
        &err_str,
        &path_with_home,
        uid.is_root(),
        cwd_valid,
    );
}
