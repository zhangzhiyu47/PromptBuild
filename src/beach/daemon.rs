//! Minimal daemon: accepts `get` requests over a unix socket,
//! keeps a moka cache, and invalidates on `.git` changes.

use super::cache::PromptCache;
use super::git;
use super::watcher::{RepoChanged, RepoWatcher};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, info, warn};

const TTI_SECS: u64 = 300;
const DEBOUNCE_MS: u64 = 500;

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Request {
    Get {
        key: String,
        #[serde(default)]
        path: Option<String>,
    },
}

#[derive(Debug, Serialize)]
struct Response {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Response {
    fn ok(data: Value) -> Self {
        Self { ok: true, data: Some(data), error: None }
    }
    fn miss() -> Self {
        Self { ok: true, data: None, error: None }
    }
    fn error(msg: impl Into<String>) -> Self {
        Self { ok: false, data: None, error: Some(msg.into()) }
    }
}

pub async fn run(socket_path: PathBuf, tti_secs: u64) -> std::io::Result<()> {
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)?;
    info!("listening on {socket_path:?}");

    let cache = Arc::new(PromptCache::new(tti_secs));
    let (mut watcher, mut events) = RepoWatcher::new(DEBOUNCE_MS)?;
    let (register_tx, mut register_rx) = tokio::sync::mpsc::channel::<PathBuf>(64);

    loop {
        tokio::select! {
            // New connection
            Ok((stream, _)) = listener.accept() => {
                let cache = cache.clone();
                let reg = register_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_conn(stream, cache, reg).await {
                        debug!("conn error: {e}");
                    }
                });
            }

            // File change → invalidate cache for that repo.
            Some(changed) = events.recv() => {
                debug!("repo changed: {:?}", changed.repo_root);
                cache.invalidate_path("git", Some(&changed.repo_root));
            }

            // New repo to watch (from a first-time get).
            Some(repo) = register_rx.recv() => {
                watcher.watch_repo(&repo);
            }
        }
    }
}

async fn handle_conn(
    stream: UnixStream,
    cache: Arc<PromptCache>,
    register_tx: tokio::sync::mpsc::Sender<PathBuf>,
) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    while reader.read_line(&mut line).await? > 0 {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            line.clear();
            continue;
        }

        let resp = match serde_json::from_str::<Request>(trimmed) {
            Ok(Request::Get { key, path }) => {
                handle_get(&cache, &register_tx, &key, path.as_deref()).await
            }
            Err(e) => Response::error(format!("bad request: {e}")),
        };

        let mut out = serde_json::to_string(&resp).unwrap();
        out.push('\n');
        writer.write_all(out.as_bytes()).await?;
        line.clear();
    }
    Ok(())
}

async fn handle_get(
    cache: &PromptCache,
    register_tx: &tokio::sync::mpsc::Sender<PathBuf>,
    key: &str,
    path: Option<&str>,
) -> Response {
    let Some(path) = path else {
        return Response::error("missing path");
    };
    // key format: "<provider>.<source>", e.g. "git.refs".
    let Some((provider, source)) = key.split_once('.') else {
        return Response::error(format!("bad key: {key}"));
    };
    if provider != "git" {
        return Response::error(format!("unknown provider: {provider}"));
    }

    // Walk to repo root.
    let Some(repo_root) = git::find_repo_root(Path::new(path)) else {
        return Response::miss();
    };

    // Cache hit?
    if let Some(entry) = cache.get_source(provider, Some(&repo_root), source) {
        let json = serde_json::to_value(&entry.fields).unwrap_or(Value::Null);
        return Response::ok(json);
    }

    // Miss → execute in blocking pool.
    let repo = repo_root.clone();
    let source_owned = source.to_string();
    let fields = match tokio::task::spawn_blocking(move || {
        git::execute(&source_owned, &repo)
    })
    .await
    {
        Ok(Some(f)) => f,
        _ => return Response::miss(),
    };

    if fields.is_empty() {
        return Response::miss();
    }

    cache.put(provider, Some(&repo_root), source, fields.clone());
    // Register this repo for watching (idempotent on daemon side).
    let _ = register_tx.send(repo_root).await;

    Response::ok(serde_json::to_value(&fields).unwrap_or(Value::Null))
}
