//! Test harness for TUI integration tests.
//!
//! Provides helpers for setting up git repos and controlling the branchdiff TUI.

mod repo;
mod session;

pub use repo::TestRepo;
pub use session::TuiSession;
