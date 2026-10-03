//! Git source execution. Each source returns a field map.

use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct GitDirs {
    pub gitdir: PathBuf,
    pub commondir: PathBuf,
}

/// Walk up from `start` to find a directory containing `.git`.
pub fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

fn resolve_git_dirs(repo_root: &Path) -> Option<GitDirs> {
    let dot_git = repo_root.join(".git");
    let meta = std::fs::metadata(&dot_git).ok()?;
    if meta.is_dir() {
        return Some(GitDirs {
            gitdir: dot_git.clone(),
            commondir: dot_git,
        });
    }
    let contents = std::fs::read_to_string(&dot_git).ok()?;
    let rel = contents.lines().next()?.strip_prefix("gitdir:")?.trim();
    let gitdir = resolve_against(repo_root, rel);
    let commondir = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(s) => resolve_against(&gitdir, s.trim()),
        Err(_) => gitdir.clone(),
    };
    Some(GitDirs { gitdir, commondir })
}

fn resolve_against(base: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    };
    std::fs::canonicalize(&joined).unwrap_or(joined)
}

/// Execute a source by name. `None` for unknown source.
pub fn execute(source: &str, repo_root: &Path) -> Option<HashMap<String, Value>> {
    match source {
        "head" => Some(execute_head(repo_root)),
        "refs" => Some(execute_refs(repo_root)),
        "status" => Some(execute_status(repo_root)),
        "diff" => Some(execute_diff(repo_root)),
        _ => None,
    }
}

/// `head`: parse `<gitdir>/HEAD` directly. No subprocess.
fn execute_head(repo_root: &Path) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    let Some(dirs) = resolve_git_dirs(repo_root) else {
        return out;
    };
    let Ok(contents) = std::fs::read_to_string(dirs.gitdir.join("HEAD")) else {
        return out;
    };
    let line = contents.trim();
    let (branch, detached) = if let Some(b) = line.strip_prefix("ref: refs/heads/") {
        (b.to_string(), false)
    } else if (line.len() == 40 || line.len() == 64)
        && line.chars().all(|c| c.is_ascii_hexdigit())
    {
        (String::new(), true)
    } else {
        (String::new(), false)
    };
    out.insert("branch".into(), Value::String(branch));
    out.insert("detached".into(), Value::Bool(detached));
    out
}

/// `refs`: repo state + upstream divergence + stash count.
fn execute_refs(repo_root: &Path) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    let Some(s) = parse_git_status(repo_root) else {
        return out;
    };
    let dirs = resolve_git_dirs(repo_root);
    let stash = dirs.as_ref().map(count_stashes).unwrap_or(0);
    let (state, step, total) = dirs
        .as_ref()
        .map(detect_repo_state)
        .unwrap_or_else(|| ("clean".to_string(), 0, 0));

    out.insert("state".into(), Value::String(state));
    out.insert("state_step".into(), Value::Number(step.into()));
    out.insert("state_total".into(), Value::Number(total.into()));
    out.insert("stash".into(), Value::Number(stash.into()));
    out.insert("ahead".into(), Value::Number(s.ahead.into()));
    out.insert("behind".into(), Value::Number(s.behind.into()));
    out
}

/// `status`: staged/unstaged/untracked/conflicted counts.
fn execute_status(repo_root: &Path) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    let Some(s) = parse_git_status(repo_root) else {
        return out;
    };
    let dirty = s.staged > 0 || s.unstaged > 0 || s.untracked > 0 || s.conflicted > 0;
    out.insert("staged".into(), Value::Number(s.staged.into()));
    out.insert("unstaged".into(), Value::Number(s.unstaged.into()));
    out.insert("untracked".into(), Value::Number(s.untracked.into()));
    out.insert("conflicted".into(), Value::Number(s.conflicted.into()));
    out.insert("dirty".into(), Value::Bool(dirty));
    out
}

fn execute_diff(repo_root: &Path) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    let (a, d) = diff_numstat(repo_root, false);
    let (sa, sd) = diff_numstat(repo_root, true);
    out.insert("lines_added".into(), Value::Number(a.into()));
    out.insert("lines_removed".into(), Value::Number(d.into()));
    out.insert("lines_staged_added".into(), Value::Number(sa.into()));
    out.insert("lines_staged_removed".into(), Value::Number(sd.into()));
    out
}

// ── helpers ─────────────────────────────────────────────────

struct ParsedStatus {
    ahead: i64,
    behind: i64,
    staged: i64,
    unstaged: i64,
    untracked: i64,
    conflicted: i64,
}

fn parse_git_status(repo_root: &Path) -> Option<ParsedStatus> {
    let output = git(repo_root, &["status", "--porcelain=v2", "--branch"]).ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut s = ParsedStatus {
        ahead: 0,
        behind: 0,
        staged: 0,
        unstaged: 0,
        untracked: 0,
        conflicted: 0,
    };
    for line in stdout.lines() {
        if line.starts_with("# branch.ab ") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                s.ahead = parts[2].trim_start_matches('+').parse().unwrap_or(0);
                s.behind = parts[3].trim_start_matches('-').parse().unwrap_or(0);
            }
        } else if line.starts_with("1 ") || line.starts_with("2 ") {
            let chars: Vec<char> = line.chars().collect();
            if chars.len() >= 4 {
                if chars[2] != '.' {
                    s.staged += 1;
                }
                if chars[3] != '.' {
                    s.unstaged += 1;
                }
            }
        } else if line.starts_with("u ") {
            s.conflicted += 1;
        } else if line.starts_with("? ") {
            s.untracked += 1;
        }
    }
    Some(s)
}

fn count_stashes(dirs: &GitDirs) -> i64 {
    std::fs::read_to_string(dirs.commondir.join("logs").join("refs").join("stash"))
        .map(|s| s.lines().count() as i64)
        .unwrap_or(0)
}

fn detect_repo_state(dirs: &GitDirs) -> (String, i64, i64) {
    let g = &dirs.gitdir;
    if g.join("MERGE_HEAD").exists() {
        return ("merge".into(), 0, 0);
    }
    if g.join("rebase-merge").exists() {
        return (
            "rebase".into(),
            read_int(&g.join("rebase-merge/msgnum")),
            read_int(&g.join("rebase-merge/end")),
        );
    }
    if g.join("rebase-apply").exists() {
        return (
            "rebase".into(),
            read_int(&g.join("rebase-apply/next")),
            read_int(&g.join("rebase-apply/last")),
        );
    }
    if g.join("CHERRY_PICK_HEAD").exists() {
        return ("cherry-pick".into(), 0, 0);
    }
    if g.join("BISECT_LOG").exists() {
        return ("bisect".into(), 0, 0);
    }
    if g.join("REVERT_HEAD").exists() {
        return ("revert".into(), 0, 0);
    }
    ("clean".into(), 0, 0)
}

fn read_int(p: &Path) -> i64 {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn diff_numstat(repo_root: &Path, staged: bool) -> (i64, i64) {
    let args: &[&str] = if staged {
        &["diff", "--cached", "--numstat"]
    } else {
        &["diff", "--numstat"]
    };
    let Ok(out) = git(repo_root, args) else {
        return (0, 0);
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (mut a, mut d) = (0i64, 0i64);
    for line in stdout.lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() >= 2 {
            a += parts[0].parse::<i64>().unwrap_or(0);
            d += parts[1].parse::<i64>().unwrap_or(0);
        }
    }
    (a, d)
}

fn git(dir: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "true")
        .env("SSH_ASKPASS", "true")
        .env("GCM_INTERACTIVE", "Never")
        .env_remove("GIT_DIR")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_WORK_TREE")
        .current_dir(dir)
        .output()
}
