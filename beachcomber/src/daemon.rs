use crate::cache::Cache;
use crate::config::Config;
use crate::provider::registry::ProviderRegistry;
use crate::scheduler::{Scheduler, SchedulerMessage};
use crate::server::Server;
use crate::watcher_registry::WatcherRegistry;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::info;

// ---------------------------------------------------------------------------
// OS-bound: in-process daemon startup (used by tests and embedded mode)
// ---------------------------------------------------------------------------

pub fn start_in_process_with_cancel(
    socket_path: PathBuf,
    config: Config,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        run_daemon_with_cancel(socket_path, config, cancel).await;
    })
}

async fn run_daemon_with_cancel(socket_path: PathBuf, config: Config, cancel: CancellationToken) {
    let watchers = Arc::new(WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_builtin());

    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry.clone(),
        config.clone(),
        watchers.clone(),
    );
    // Daemon path: probe fs-event delivery before trusting the native backend
    // (canon provider_source.md §"Watch backend health").
    scheduler.self_test_watch_backend();

    let scheduler_handle = handle.clone();
    let scheduler_task = tokio::spawn(async move { scheduler.run().await });

    let server = Server::new(socket_path, cache, registry, Some(handle), watchers);

    tokio::select! {
        result = server.run() => {
            if let Err(e) = result {
                tracing::error!("Server error: {}", e);
            }
            // Server returned (bind failure or accept-loop error): shut the
            // scheduler down too, or `scheduler_task.await` below blocks
            // forever and the process lingers, unkillable by SIGTERM (the
            // cancel token has no remaining observer once this select ends).
            scheduler_handle.send(SchedulerMessage::Shutdown).await;
        }
        _ = cancel.cancelled() => {
            info!("Shutdown signal received");
            scheduler_handle.send(SchedulerMessage::Shutdown).await;
        }
    }

    let _ = scheduler_task.await;
    info!("Daemon shut down cleanly");
}
