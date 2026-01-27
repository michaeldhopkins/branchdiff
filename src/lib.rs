//! branchdiff library - exposes types for benchmarking and testing

pub mod app;
pub mod diff;
pub mod git;
pub mod gitignore;
pub mod input;
pub mod limits;
pub mod message;
pub mod syntax;
pub mod update;
pub mod ui;

#[cfg(test)]
pub mod test_support;

// Re-export commonly used types for benchmarks
pub use app::{App, ViewMode};
pub use diff::{DiffLine, LineSource};
