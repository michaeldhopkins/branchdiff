//! branchdiff library - exposes types for benchmarking and testing

pub mod app;
pub mod cli;
pub mod comments;
pub mod diff;
pub mod external;
pub mod fetch;
pub mod file_events;
pub mod file_links;
pub mod file_list;
pub mod fingerprint;
pub mod gitignore;
pub mod html;
pub mod html_page;
mod html_runs;
pub mod html_style;
pub mod image_diff;
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
pub mod startup_load;
pub mod syntax;
#[doc(hidden)]
pub mod terminal_query;
pub mod ui;
pub mod update;

#[cfg(test)]
pub mod test_support;

// Re-export commonly used types for benchmarks
pub use app::{App, ViewMode};
pub use diff::{DiffLine, LineSource};
