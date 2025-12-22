//! branchdiff library - exposes types for benchmarking and testing

pub mod app;
pub mod diff;
pub mod git;
pub mod input;
pub mod message;
pub mod update;
pub mod ui;

// Re-export commonly used types for benchmarks
pub use app::{App, ViewMode};
pub use diff::{DiffLine, LineSource};
