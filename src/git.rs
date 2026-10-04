//! Git repo detection and prompt badge construction.

use crate::beach::DaemonConn;
use crate::render::{Color, Frame, Segment};
use serde_json::Value;
use std::path::Path;

/// Push git badge segments. Lazily opens the daemon session — nothing
/// is queried (and no daemon is spawned) outside a git repo.
pub fn push_git_segments(daemon: &mut DaemonConn, path: &Path, segments: &mut Vec<Segment>) {
    if !crate::beach::git::is_in_git_repo(path) {
        return;
    }
    let Some(session) = daemon.session() else {
        return;
    };
    let path_str = path.to_string_lossy();

    let Some(snap) = session
        .get("git.snapshot", Some(path_str.as_ref()))
        .ok()
        .flatten()
    else {
        return;
    };

    // ---- head segment: branch + repo state ----
    let mut head: Vec<String> = Vec::new();

    match snap.get("branch").and_then(Value::as_str) {
        Some("") => head.push("detached".into()),
        Some(s) => head.push(s.into()),
        None => {}
    }

    match snap.get("state").and_then(Value::as_str).unwrap_or("clean") {
        "bisect" => head.push("Bisect".into()),
        "merge" => head.push("Merge".into()),
        "rebase" => {
            let step = snap.get("state_step").and_then(Value::as_i64).unwrap_or(0);
            let total = snap.get("state_total").and_then(Value::as_i64).unwrap_or(0);
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

    if !head.is_empty() {
        segments.push(Segment {
            frame: Frame::Paren,
            content: head.join("|"),
            color: Color::Border,
        });
    }

    // ---- tail segment: working-tree counts + divergence ----
    let mut tail: Vec<String> = Vec::new();

    let has_conflict = snap.get("conflicted").and_then(Value::as_i64).unwrap_or(0) > 0;

    for (field, sym) in [
        ("conflicted", '×'),
        ("staged", '+'),
        ("unstaged", '~'),
        ("untracked", '?'),
    ] {
        let n = snap.get(field).and_then(Value::as_i64).unwrap_or(0);
        if n > 0 {
            tail.push(format!("{sym}{n}"));
        }
    }

    let stash = snap.get("stash").and_then(Value::as_i64).unwrap_or(0);
    if stash > 0 {
        tail.push(format!("≡{stash}"));
    }

    let ahead = snap.get("ahead").and_then(Value::as_i64).unwrap_or(0);
    let behind = snap.get("behind").and_then(Value::as_i64).unwrap_or(0);
    if ahead > 0 {
        tail.push(format!("↑{ahead}"));
    }
    if behind > 0 {
        tail.push(format!("↓{behind}"));
    }

    if !tail.is_empty() {
        segments.push(Segment {
            frame: Frame::Paren,
            content: tail.join("|"),
            color: if has_conflict {
                Color::Orange
            } else {
                Color::Gray
            },
        });
    }
}
