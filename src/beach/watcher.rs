//! Watches each registered repo's `.git` directory; emits a `RepoChanged`
//! event through a tokio channel after a debounce window.

use notify_debouncer_mini::{
    DebounceEventResult, Debouncer, new_debouncer,
    notify::{RecommendedWatcher, RecursiveMode},
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, warn};

#[derive(Debug, Clone)]
pub struct RepoChanged {
    pub repo_root: PathBuf,
}

pub struct RepoWatcher {
    debouncer: Debouncer<RecommendedWatcher>,
    watched: HashSet<PathBuf>,
}

impl RepoWatcher {
    pub fn new(debounce_ms: u64) -> std::io::Result<(Self, mpsc::Receiver<RepoChanged>)> {
        let (tx, rx) = mpsc::channel(64);
        let (std_tx, std_rx) = std_mpsc::channel::<DebounceEventResult>();

        let debouncer = new_debouncer(Duration::from_millis(debounce_ms), std_tx)
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        std::thread::Builder::new()
            .name("debounce-fwd".into())
            .spawn(move || {
                while let Ok(res) = std_rx.recv() {
                    match res {
                        Ok(events) => {
                            let mut seen: HashSet<PathBuf> = HashSet::new();
                            for ev in events {
                                if let Some(root) = super::git::find_repo_root(&ev.path)
                                    && seen.insert(root.clone())
                                {
                                    if tx.blocking_send(RepoChanged { repo_root: root }).is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        Err(e) => warn!("watch error: {e}"),
                    }
                }
            })
            .map_err(std::io::Error::other)?;

        Ok((
            Self {
                debouncer,
                watched: HashSet::new(),
            },
            rx,
        ))
    }

    /// Register watches on the authoritative git state files.
    ///
    /// Only files that can change what the prompt renders are watched.
    /// `objects/`, `hooks/`, `info/`, `config`, and the like are excluded
    /// on purpose: object churn during fetch/gc would otherwise flood the
    /// debouncer, and none of it affects the badge.
    pub fn watch_repo(&mut self, repo_root: &Path) {
        if self.watched.contains(repo_root) {
            return;
        }
        let Some(dirs) = super::git::resolve_git_dirs(repo_root) else {
            return;
        };

        // (path, recursive) pairs. Missing paths are skipped — e.g.
        // MERGE_HEAD only exists during a merge, rebase-* only during a
        // rebase. Their later appearance won't be caught by the watcher,
        // but the TTI (60s) is the safety net; and these appear via
        // operations that also touch HEAD/index, which we do watch.
        let candidates: &[(PathBuf, RecursiveMode)] = &[
            // Per-worktree state.
            (dirs.gitdir.join("HEAD"), RecursiveMode::NonRecursive),
            (dirs.gitdir.join("index"), RecursiveMode::NonRecursive),
            (dirs.gitdir.join("MERGE_HEAD"), RecursiveMode::NonRecursive),
            (
                dirs.gitdir.join("CHERRY_PICK_HEAD"),
                RecursiveMode::NonRecursive,
            ),
            (dirs.gitdir.join("REVERT_HEAD"), RecursiveMode::NonRecursive),
            (dirs.gitdir.join("BISECT_LOG"), RecursiveMode::NonRecursive),
            // Rebase progress dirs (msgnum/end, next/last).
            (
                dirs.gitdir.join("rebase-merge"),
                RecursiveMode::NonRecursive,
            ),
            (
                dirs.gitdir.join("rebase-apply"),
                RecursiveMode::NonRecursive,
            ),
            // Shared ref store.
            (dirs.commondir.join("refs"), RecursiveMode::Recursive),
            (
                dirs.commondir.join("packed-refs"),
                RecursiveMode::NonRecursive,
            ),
            (
                dirs.commondir.join("logs/refs/stash"),
                RecursiveMode::NonRecursive,
            ),
        ];

        let mut watched_any = false;
        let mut seen: HashSet<PathBuf> = HashSet::new();

        for (path, mode) in candidates {
            if !seen.insert(path.clone()) {
                continue;
            }
            if !path.exists() {
                continue;
            }
            match self.debouncer.watcher().watch(path, *mode) {
                Ok(()) => {
                    debug!("watching {path:?}");
                    watched_any = true;
                }
                Err(e) => warn!("failed to watch {path:?}: {e}"),
            }
        }

        if watched_any {
            self.watched.insert(repo_root.to_path_buf());
        }
        // If nothing could be watched, don't mark — a later call retries.
    }
}
