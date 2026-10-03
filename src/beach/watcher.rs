//! Watches each registered repo's `.git` directory; emits a `RepoChanged`
//! event through a tokio channel after a debounce window.

use notify_debouncer_mini::{
    new_debouncer,
    notify::{RecommendedWatcher, RecursiveMode},
    DebounceEventResult, Debouncer,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::sync::mpsc as std_mpsc;
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

    let mut debouncer = new_debouncer(Duration::from_millis(debounce_ms), std_tx)
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    std::thread::Builder::new()
        .name("debounce-fwd".into())
        .spawn(move || {
            while let Ok(res) = std_rx.recv() {
                match res {
                    Ok(events) => {
                        let mut seen: HashSet<PathBuf> = HashSet::new();
                        for ev in events {
                            if let Some(root) = repo_root_for(&ev.path)
                                && seen.insert(root.clone())
                            {
                                let _ = tx.try_send(RepoChanged { repo_root: root });
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

    /// Register a watch on `<repo_root>/.git`. Idempotent per repo.
    pub fn watch_repo(&mut self, repo_root: &Path) {
        if !self.watched.insert(repo_root.to_path_buf()) {
            return;
        }
        let git_dir = repo_root.join(".git");
        if !git_dir.exists() {
            return;
        }
        match self
            .debouncer
            .watcher()
            .watch(&git_dir, RecursiveMode::Recursive)
        {
            Ok(()) => debug!("watching {git_dir:?}"),
            Err(e) => warn!("failed to watch {git_dir:?}: {e}"),
        }
    }
}

/// Walk `path` and its ancestors looking for a directory with a `.git` child.
fn repo_root_for(path: &Path) -> Option<PathBuf> {
    let mut cur: Option<&Path> = Some(path);
    while let Some(p) = cur {
        if p.join(".git").exists() {
            return Some(p.to_path_buf());
        }
        cur = p.parent();
    }
    None
}
