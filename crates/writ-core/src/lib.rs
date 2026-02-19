//! writ-core — Core library for the AI-native version control system.
//!
//! Writ is a VCS designed for LLMs and multi-agent development fleets.
//! Its core primitives are **specs** (not branches), **seals** (not commits),
//! and **convergence** (not merging).

#[cfg(feature = "bridge")]
pub mod bridge;
pub mod context;
pub mod convergence;
pub mod diff;
pub mod error;
pub mod fsutil;
pub mod hash;
pub mod ignore;
pub mod index;
pub mod lock;
pub mod object;
pub mod remote;
pub mod repo;
pub mod seal;
pub mod spec;
pub mod state;

pub use error::{WritError, WritResult};
pub use repo::Repository;
