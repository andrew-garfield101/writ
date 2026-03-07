//! writ-core — Core library for the AI-native version control system.
//!
//! Writ is a VCS designed for LLMs and multi-agent development fleets.
//! Its core primitives are **specs** (not branches), **seals** (not commits),
//! and **convergence** (not merging).

pub mod agent;
#[cfg(feature = "bridge")]
pub mod bridge;
pub mod config;
pub mod context;
pub mod convergence;
pub mod crypto;
pub mod diff;
pub mod env_scan;
pub mod error;
pub mod format;
pub mod fsutil;
pub mod gc;
#[cfg(feature = "bridge")]
pub mod git_ops;
pub mod hash;
pub mod hooks;
pub mod ignore;
pub mod index;
pub mod keystore;
pub mod lock;
pub mod migrate;
pub mod object;
pub mod proposal;
pub mod remote;
pub mod repo;
pub mod seal;
pub mod security;
pub mod settings;
pub mod spec;
pub mod state;
pub mod status;

pub use error::{WritError, WritResult};
pub use repo::Repository;
