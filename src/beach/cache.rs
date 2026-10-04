//! Flat cache keyed by (provider, path, source).
//! moka's TTI drives eviction; a `get` refreshes the idle clock.

use moka::sync::Cache;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub provider: String,
    pub path: Option<PathBuf>,
    pub source: String,
}

impl CacheKey {
    pub fn new(provider: &str, path: Option<&Path>, source: &str) -> Self {
        Self {
            provider: provider.to_string(),
            path: path.map(Path::to_path_buf),
            source: source.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub fields: HashMap<String, Value>,
}

pub struct PromptCache {
    inner: Cache<CacheKey, Entry>,

    /// Monotonic generation counter. Bumped on every invalidation.
    /// A single global counter is enough: false positives only cost
    /// a skipped write, and repos change at similar rates.
    generation: AtomicU64,
}

impl PromptCache {
    pub fn new(tti_secs: u64, ttl_secs: u64) -> Self {
        let inner = Cache::builder()
            .time_to_idle(Duration::from_secs(tti_secs))
            .time_to_live(Duration::from_secs(ttl_secs))
            .support_invalidation_closures()
            .build();

        Self {
            inner,
            generation: AtomicU64::new(0),
        }
    }

    /// Snapshot the current generation. Call before a computation
    /// whose result may be cached.
    pub fn snapshot(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Insert only if the generation still matches `snapshot`.
    /// Returns false when a concurrent invalidation happened.
    pub fn put_if_fresh(
        &self,
        provider: &str,
        path: Option<&Path>,
        source: &str,
        fields: HashMap<String, Value>,
        snapshot: u64,
    ) -> bool {
        if self.generation.load(Ordering::Acquire) != snapshot {
            return false;
        }
        self.inner
            .insert(CacheKey::new(provider, path, source), Entry { fields });
        true
    }

    pub fn get_source(&self, provider: &str, path: Option<&Path>, source: &str) -> Option<Entry> {
        self.inner.get(&CacheKey::new(provider, path, source))
    }

    /// Drop every source under a (provider, path). Used when `.git` changes.
    pub fn invalidate_path(&self, provider: &str, path: Option<&Path>) {
        // Bump before invalidating so an in-flight miss-path write
        // holding an older snapshot is refused.
        self.generation.fetch_add(1, Ordering::AcqRel);

        let provider = provider.to_string();
        let target = path.map(Path::to_path_buf);
        let _ = self
            .inner
            .invalidate_entries_if(move |k, _| k.provider == provider && k.path == target);
        self.inner.run_pending_tasks();
    }
}
