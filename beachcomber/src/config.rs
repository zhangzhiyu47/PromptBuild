//! Runtime configuration for the beachcomber daemon.
//!
//! This build of beachcomber is a minimal, workspace-embedded library used
//! exclusively by the `prompt` binary. It does not read any TOML config file,
//! env file, or conf.d drop-in: `Config::load()` always returns
//! `Config::default()`. The struct shape mirrors upstream beachcomber so the
//! scheduler and daemon code can keep their existing call sites, but every
//! per-source resolution method below simply returns the caller's declared
//! default (or the process-wide fallback) without consulting a file.

use std::time::Duration;

/// Top-level configuration.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub daemon: DaemonConfig,
    pub lifecycle: LifecycleConfig,
    pub failback: FailbackGlobalConfig,
}

/// Daemon-level knobs: socket path, logging, provider timeout
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Override for the Unix socket path. `None` falls through to the
    /// `BEACHCOMBER_SOCKET` env var, then to the per-user default.
    pub socket_path: Option<String>,
    /// Tracing log level for the daemon process.
    pub log_level: String,
    /// Maximum time a single provider execution may run before it is
    /// cancelled. `None` means "no timeout".
    pub provider_timeout_secs: Option<u64>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: None,
            log_level: "info".to_string(),
            provider_timeout_secs: Some(10),
        }
    }
}

/// Lifecycle defaults used by the scheduler when a source does not declare
/// its own poll interval / keep-alive count.
#[derive(Debug, Clone)]
pub struct LifecycleConfig {
    /// Idle-shutdown timeout. `None` (the default) keeps the daemon resident.
    pub idle_shutdown_secs: Option<u64>,
    /// Process-wide default poll interval, as a duration string.
    pub poll_interval: String,
    /// Process-wide default keep-alive poll count.
    pub poll_live_count: u32,
    /// Process-wide override for `fsevents_reinstate`. `None` means "let the
    /// source's declared default decide".
    pub fsevents_reinstate: Option<bool>,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            idle_shutdown_secs: Some(300),
            poll_interval: "60s".to_string(),
            poll_live_count: 12,
            fsevents_reinstate: None,
        }
    }
}

/// Global failure-backoff defaults.
#[derive(Debug, Clone)]
pub struct FailbackGlobalConfig {
    /// Consecutive failures before a source is suppressed.
    pub count: u32,
    /// Initial suppression duration, as a duration string.
    pub interval: String,
}

impl Default for FailbackGlobalConfig {
    fn default() -> Self {
        Self {
            count: 3,
            interval: "1s".to_string(),
        }
    }
}

impl FailbackGlobalConfig {
    /// Resolve the configured base backoff, falling back to one second if the
    /// interval string is unparseable.
    pub fn interval_duration(&self) -> Duration {
        parse_duration(&self.interval).unwrap_or(Duration::from_secs(1))
    }
}

/// Parse a duration string like `"30s"`, `"5m"`, `"1h"`, or a whole-second
/// `"...ms"` value (e.g. `"2000ms"`). Returns `None` for sub-second `ms`
/// values and malformed input.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(stripped) = s.strip_suffix("ms") {
        let n = stripped.trim().parse::<u64>().ok()?;
        if n < 1000 || n % 1000 != 0 {
            return None;
        }
        return Some(Duration::from_secs(n / 1000));
    }
    let (num_str, multiplier) = if let Some(stripped) = s.strip_suffix('s') {
        (stripped, 1u64)
    } else if let Some(stripped) = s.strip_suffix('m') {
        (stripped, 60)
    } else if let Some(stripped) = s.strip_suffix('h') {
        (stripped, 3600)
    } else {
        (s, 1)
    };
    num_str
        .trim()
        .parse::<u64>()
        .ok()
        .map(|n| Duration::from_secs(n * multiplier))
}

impl Config {
    /// Return the default configuration. No file is read and no env file is
    /// loaded. This is the only constructor the daemon uses in this build.
    pub fn load() -> Self {
        Self::default()
    }

    /// Resolve failure reattempts for a source. Config-file overrides are not
    /// consulted; the source's declared default wins, falling back to the
    /// process-wide failback count.
    pub fn resolve_failure_reattempts_for_source(
        &self,
        _provider_name: &str,
        _source_name: Option<&str>,
        source_default: Option<u32>,
    ) -> u32 {
        source_default.unwrap_or(self.failback.count)
    }

    /// Resolve failure backoff interval for a source. Same precedence as
    /// `resolve_failure_reattempts_for_source`.
    pub fn resolve_failure_backoff_for_source(
        &self,
        _provider_name: &str,
        _source_name: Option<&str>,
        source_default: Option<Duration>,
    ) -> Duration {
        source_default.unwrap_or_else(|| self.failback.interval_duration())
    }

    /// Resolve poll interval for a source. Config-file overrides are not
    /// consulted; the source's declared default wins, falling back to a
    /// 60-second process-wide default.
    pub fn resolve_poll_interval_for_source(
        &self,
        _provider_name: &str,
        _source_name: Option<&str>,
        source_default: Option<Duration>,
    ) -> Duration {
        source_default.unwrap_or_else(|| {
            parse_duration(&self.lifecycle.poll_interval).unwrap_or(Duration::from_secs(60))
        })
    }

    /// Resolve poll keep-alive count for a source.
    pub fn resolve_poll_live_count_for_source(
        &self,
        _provider_name: &str,
        _source_name: Option<&str>,
        source_default: Option<u32>,
    ) -> u32 {
        source_default.unwrap_or(self.lifecycle.poll_live_count)
    }

    /// Resolve `fsevents_reinstate` for a source. The per-source file override
    /// is not read; the process-wide `[lifecycle]` value (when set) wins over
    /// the source's declared default.
    pub fn resolve_fsevents_reinstate_for_source(
        &self,
        _provider_name: &str,
        _source_name: Option<&str>,
        source_default: bool,
    ) -> bool {
        self.lifecycle.fsevents_reinstate.unwrap_or(source_default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_returns_defaults_without_reading_a_file() {
        let cfg = Config::load();
        assert_eq!(cfg.daemon.log_level, "info");
        assert_eq!(cfg.daemon.provider_timeout_secs, Some(10));
        assert_eq!(cfg.lifecycle.poll_live_count, 12);
        assert_eq!(cfg.failback.count, 3);
    }

    #[test]
    fn parse_duration_accepts_seconds_minutes_hours_and_whole_second_ms() {
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration("2000ms"), Some(Duration::from_secs(2)));
        assert_eq!(parse_duration("500ms"), None);
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("nonsense"), None);
    }

    #[test]
    fn source_default_wins_over_global() {
        let cfg = Config::default();
        assert_eq!(
            cfg.resolve_poll_interval_for_source(
                "git",
                Some("head"),
                Some(Duration::from_secs(15)),
            ),
            Duration::from_secs(15)
        );
        assert_eq!(
            cfg.resolve_poll_interval_for_source("git", Some("head"), None),
            Duration::from_secs(60)
        );
        assert_eq!(
            cfg.resolve_failure_reattempts_for_source("git", None, Some(7)),
            7
        );
        assert_eq!(
            cfg.resolve_failure_reattempts_for_source("git", None, None),
            3
        );
    }

    #[test]
    fn lifecycle_global_overrides_fsevents_reinstate_when_set() {
        let mut cfg = Config::default();
        assert!(!cfg.resolve_fsevents_reinstate_for_source("git", None, false));
        assert!(cfg.resolve_fsevents_reinstate_for_source("git", None, true));
        cfg.lifecycle.fsevents_reinstate = Some(false);
        assert!(!cfg.resolve_fsevents_reinstate_for_source("git", None, true));
    }
}
