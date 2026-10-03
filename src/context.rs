//! Environment context badges: chroot, Python venv, and the active
//! Kubernetes context. All three are cheap local reads — env vars or
//! one small kubeconfig file — so they never touch the daemon.

use crate::render::{Color, Frame, Segment};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Append a badge for each environment context currently in effect.
/// A context that isn't set contributes nothing.
pub fn push_context_segments(segments: &mut Vec<Segment>) {
    push_chroot(segments);
    push_venv(segments);
    push_kube(segments);
}

/// `debian_chroot` is set by Debian's bashrc on chroot entry.
fn push_chroot(segments: &mut Vec<Segment>) {
    if let Some(name) = env::var("debian_chroot").ok().filter(|s| !s.is_empty()) {
        segments.push(Segment {
            frame: Frame::Paren,
            content: name,
            color: Color::Border,
        });
    }
}

/// Show the basename of `$VIRTUAL_ENV`. The venv is expected to have
/// been activated by the shell; this only surfaces which one.
fn push_venv(segments: &mut Vec<Segment>) {
    let Some(path) = env::var("VIRTUAL_ENV").ok().filter(|s| !s.is_empty()) else {
        return;
    };
    let Some(name) = Path::new(&path).file_name() else {
        return;
    };
    segments.push(Segment {
        frame: Frame::Paren,
        content: name.to_string_lossy().into_owned(),
        color: Color::Border,
    });
}

/// Show `k8s:<context>`, or `k8s:<context>/<namespace>` when the
/// namespace isn't `default`. An absent or unreadable kubeconfig is a
/// silent no-op.
fn push_kube(segments: &mut Vec<Segment>) {
    let paths = resolve_kubeconfig_paths();
    if paths.is_empty() {
        return;
    }
    let Some((ctx, ns)) = read_kubeconfig(&paths) else {
        return;
    };

    let content = if ns == "default" {
        format!("k8s:{ctx}")
    } else {
        format!("k8s:{ctx}/{ns}")
    };

    segments.push(Segment {
        frame: Frame::Paren,
        content,
        color: Color::Border,
    });
}

/// Candidate kubeconfig files: `$KUBECONFIG` (colon-separated) when
/// set, otherwise `~/.kube/config`. Mirrors kubectl's resolution.
fn resolve_kubeconfig_paths() -> Vec<PathBuf> {
    if let Some(paths) = env::var("KUBECONFIG").ok().filter(|s| !s.is_empty()) {
        return paths
            .split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
    }
    if let Some(home) = env::var("HOME").ok().filter(|s| !s.is_empty()) {
        return vec![PathBuf::from(home).join(".kube").join("config")];
    }
    Vec::new()
}

/// Read `current-context` and that context's namespace across all
/// candidate files.
///
/// Returns `Some((context, namespace))`, with later files winning on
/// conflict (matching kubectl's merge semantics). `None` when no file
/// declares a usable `current-context`.
fn read_kubeconfig(paths: &[PathBuf]) -> Option<(String, String)> {
    let mut current: Option<String> = None;
    let mut namespaces: HashMap<String, String> = HashMap::new();

    for path in paths {
        let Ok(contents) = fs::read_to_string(path) else {
            continue;
        };
        if let Some(name) = find_current_context(&contents) {
            current = Some(name);
        }
        for (name, ns) in parse_context_namespaces(&contents) {
            namespaces.insert(name, ns);
        }
    }

    let ctx = current?;
    let ns = namespaces
        .get(&ctx)
        .cloned()
        .unwrap_or_else(|| "default".to_string());
    Some((ctx, ns))
}

/// First non-empty `current-context:` value in the file.
fn find_current_context(contents: &str) -> Option<String> {
    for line in contents.lines() {
        if let Some(v) = line.strip_prefix("current-context:") {
            let v = v.trim().trim_matches('"');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Scan the `contexts:` block for (name, namespace) pairs. Each list
/// item (`- ` at any indent) starts a new entry; a `name:` within an
/// entry is the key, `namespace:` the value. Missing namespace
/// defaults to `default`.
fn parse_context_namespaces(contents: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut in_contexts = false;
    let mut block = String::new();

    for line in contents.lines() {
        // A top-level non-list, non-empty line ends the block.
        if in_contexts && !line.starts_with(' ') && !line.starts_with('-') && !line.is_empty() {
            if let Some(pair) = parse_context_block(&block) {
                out.push(pair);
            }
            block.clear();
            in_contexts = false;
        }

        if !in_contexts {
            if line.starts_with("contexts:") {
                in_contexts = true;
            }
            continue;
        }

        // A new list item flushes the previous one.
        if line.trim_start().starts_with("- ") && !block.is_empty() {
            if let Some(pair) = parse_context_block(&block) {
                out.push(pair);
            }
            block.clear();
        }
        block.push_str(line);
        block.push('\n');
    }

    if let Some(pair) = parse_context_block(&block) {
        out.push(pair);
    }
    out
}

fn parse_context_block(block: &str) -> Option<(String, String)> {
    let mut name = None;
    let mut ns = None;
    for line in block.lines() {
        let trimmed = line.trim();
        if let Some(v) = trimmed.strip_prefix("name:") {
            let v = v.trim().trim_matches('"');
            if !v.is_empty() {
                name = Some(v.to_string());
            }
        } else if let Some(v) = trimmed.strip_prefix("namespace:") {
            let v = v.trim().trim_matches('"');
            if !v.is_empty() {
                ns = Some(v.to_string());
            }
        }
    }
    name.map(|n| (n, ns.unwrap_or_else(|| "default".to_string())))
}
