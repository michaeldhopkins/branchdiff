//! branchdiff library - exposes types for benchmarking and testing

pub mod app;
pub mod cli;
pub mod diff;
pub mod external;
pub mod fetch;
pub mod file_events;
pub mod file_links;
pub mod file_list;
pub mod fingerprint;
pub mod image_diff;
pub mod gitignore;
pub mod listing;
pub mod vcs;

#[cfg(fuzzing)]
#[doc(hidden)]
pub mod fuzz_api;
pub mod input;
pub mod limits;
pub mod message;
pub mod output;
pub mod patch;
pub mod startup;
pub mod syntax;
pub mod update;
pub mod ui;

#[cfg(test)]
pub mod test_support;

// Re-export commonly used types for benchmarks
pub use app::{App, ViewMode};
pub use diff::{DiffLine, LineSource};
