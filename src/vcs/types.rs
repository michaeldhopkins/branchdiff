use std::collections::HashMap;

use crate::diff::{DiffLine, FileDiff};
use crate::limits::DiffMetrics;

/// VCS-agnostic context describing what we're comparing.
///
/// For git: "feature vs main" with a merge-base SHA.
/// For jj: "@ vs @-" with a change ID.
#[derive(Debug, Clone)]
pub struct ComparisonContext {
    /// Label for what we're comparing against (e.g., "main", "@-")
    pub from_label: String,
    /// Label for what we're comparing to (e.g., "feature", "HEAD", "@")
    pub to_label: String,
    /// Resolved base reference for diff computation (e.g., merge-base SHA, change ID)
    pub base_identifier: String,
}

/// Result of a full refresh from a VCS backend.
#[derive(Debug)]
pub struct RefreshResult {
    pub files: Vec<FileDiff>,
    pub lines: Vec<DiffLine>,
    pub merge_base: String,
    pub current_branch: Option<String>,
    pub metrics: DiffMetrics,
    pub file_links: HashMap<String, String>,
}
