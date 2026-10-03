//! Git repo detection and prompt badge construction.

use crate::daemon::DaemonConn;
use crate::render::{Color, Frame, Segment};
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Search up the git repository level by level starting from `path`.
/// Return true if found, otherwise false
fn is_in_git_repo(path: &Path) -> bool {
    let looks_like_git = |dir: &Path| {
        dir.join("HEAD").is_file()
            && (dir.join("objects").is_dir()
                || dir.join("refs").is_dir()
                || dir.join("commondir").is_file())
    };

    let mut dir = path;
    loop {
        if looks_like_git(dir) {
            return true;
        }

        let dot_git = dir.join(".git");

        if dot_git.is_dir() && looks_like_git(&dot_git) {
            return true;
        }

        if dot_git.is_file() {
            if let Some(line) = fs::File::open(&dot_git)
                .ok()
                .and_then(|f| BufReader::new(f).lines().next())
                .and_then(Result::ok)
            {
                if let Some(target) = line.trim().strip_prefix("gitdir:").map(str::trim) {
                    if !target.is_empty() {
                        let p = Path::new(target);
                        let git_dir = if p.is_absolute() {
                            p.to_path_buf()
                        } else {
                            match dot_git.parent() {
                                Some(parent) => parent.join(p),
                                None => continue,
                            }
                        };
                        let git_dir = fs::canonicalize(&git_dir).unwrap_or(git_dir);
                        if looks_like_git(&git_dir) {
                            return true;
                        }
                    }
                }
            }
        }

        match dir.parent() {
            Some(parent) => dir = parent,
            None => return false,
        }
    }
}

/// Push git badge segments. Lazily opens the daemon session — nothing
/// is queried (and no daemon is spawned) outside a git repo.
pub fn push_git_segments(daemon: &mut DaemonConn, path: &Path, segments: &mut Vec<Segment>) {
    if !is_in_git_repo(path) {
        return;
    }
    let Some(session) = daemon.session() else {
        return;
    };
    let path_str = path.to_string_lossy();

    let refs = session
        .get("git.refs", Some(path_str.as_ref()))
        .ok()
        .flatten();

    let head_info = session
        .get("git.head", Some(path_str.as_ref()))
        .ok()
        .flatten();

    let mut head: Vec<String> = Vec::new();

    if let Some(h) = &head_info {
        match h.get("branch").and_then(Value::as_str) {
            Some("") => head.push("detached".into()),
            Some(s) => head.push(s.into()),
            None => {}
        }
    }

    if let Some(r) = &refs {
        match r.get("state").and_then(Value::as_str).unwrap_or("clean") {
            "bisect" => head.push("Bisect".into()),
            "merge" => head.push("Merge".into()),
            "rebase" => {
                let step = r.get("state_step").and_then(Value::as_i64).unwrap_or(0);
                let total = r.get("state_total").and_then(Value::as_i64).unwrap_or(0);
                head.push(if total > 0 {
                    format!("Rebase:{step}/{total}")
                } else {
                    "Rebase".into()
                });
            }
            "cherry-pick" => head.push("Cherry-pick".into()),
            "revert" => head.push("Revert".into()),
            _ => {}
        }
    }

    if !head.is_empty() {
        segments.push(Segment {
            frame: Frame::Paren,
            content: head.join("|"),
            color: Color::Border,
        });
    }

    let mut tail: Vec<String> = Vec::new();
    let mut has_conflict = false;

    if let Some(status) = session
        .get("git.status", Some(path_str.as_ref()))
        .ok()
        .flatten()
    {
        has_conflict = status.get("conflicted").and_then(Value::as_i64).unwrap_or(0) > 0;
        for (field, sym) in [
            ("conflicted", '×'),
            ("staged", '+'),
            ("unstaged", '~'),
            ("untracked", '?'),
        ] {
            let n = status.get(field).and_then(Value::as_i64).unwrap_or(0);
            if n > 0 {
                tail.push(format!("{sym}{n}"));
            }
        }
    }

    if let Some(r) = &refs {
        let stash = r.get("stash").and_then(Value::as_i64).unwrap_or(0);
        if stash > 0 {
            tail.push(format!("≡{stash}"));
        }
        let ahead = r.get("ahead").and_then(Value::as_i64).unwrap_or(0);
        let behind = r.get("behind").and_then(Value::as_i64).unwrap_or(0);
        if ahead > 0 {
            tail.push(format!("↑{ahead}"));
        }
        if behind > 0 {
            tail.push(format!("↓{behind}"));
        }
    }

    if !tail.is_empty() {
        segments.push(Segment {
            frame: Frame::Paren,
            content: tail.join("|"),
            color: if has_conflict { Color::Orange } else { Color::Gray },
        });
    }
}
