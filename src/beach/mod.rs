//! Self-contained prompt-state daemon. Replaces beachcomber + libbeachcomber.

pub mod cache;
pub mod client;
pub mod daemon;
pub mod git;
pub mod watcher;

pub use client::Session;
