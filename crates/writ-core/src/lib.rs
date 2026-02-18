//! writ-core — Core library for the AI-native version control system.
//!
//! Writ is a VCS designed for LLMs and multi-agent development fleets.
//! Its core primitives are **specs** (not branches), **seals** (not commits),
//! and **convergence** (not merging).

pub mod context;
pub mod diff;
pub mod error;
pub mod hash;
pub mod ignore;
pub mod index;
pub mod object;
pub mod repo;
pub mod seal;
pub mod spec;
pub mod state;

pub use error::{WritError, WritResult};
pub use repo::Repository;
