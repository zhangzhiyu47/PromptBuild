//! Flat cache keyed by (provider, path, source).
//! moka's TTI drives eviction; a `get` refreshes the idle clock.

use moka::sync::Cache;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
    pub last_refreshed: Instant,
}

pub struct PromptCache {
    inner: Cache<CacheKey, Entry>,
}

impl PromptCache {
    pub fn new(tti_secs: u64) -> Self {
        let inner = Cache::builder()
            .time_to_idle(Duration::from_secs(tti_secs))
            .build();
        Self { inner }
    }

    pub fn put(
        &self,
        provider: &str,
        path: Option<&Path>,
        source: &str,
        fields: HashMap<String, Value>,
    ) {
        self.inner.insert(
            CacheKey::new(provider, path, source),
            Entry {
                fields,
                last_refreshed: Instant::now(),
            },
        );
    }

    pub fn get_source(
        &self,
        provider: &str,
        path: Option<&Path>,
        source: &str,
    ) -> Option<Entry> {
        self.inner.get(&CacheKey::new(provider, path, source))
    }

    pub fn invalidate_source(&self, provider: &str, path: Option<&Path>, source: &str) {
        self.inner
            .invalidate(&CacheKey::new(provider, path, source));
    }

    /// Drop every source under a (provider, path). Used when `.git` changes.
    pub fn invalidate_path(&self, provider: &str, path: Option<&Path>) {
        let provider = provider.to_string();
        let target = path.map(Path::to_path_buf);
        let _ = self
            .inner
            .invalidate_entries_if(move |k, _| k.provider == provider && k.path == target);
    }
}
