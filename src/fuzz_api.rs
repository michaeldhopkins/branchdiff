//! Entry points for the fuzz targets in `fuzz/`, compiled only under `cargo fuzz`
//! (which passes `--cfg fuzzing`). The parsers they wrap are crate-private; this
//! module reaches them without making them part of the published library API.

use std::collections::HashMap;

use crate::vcs::git::{changed_files, commands, parse};
use crate::vcs::jj;

/// Every parser branchdiff runs over git's or jj's stdout, with results discarded.
pub fn parse_all_vcs_output(text: &str) {
    let _ = commands::parse_git_version(text);
    for line in text.lines() {
        let _ = changed_files::parse_diff_line(line);
    }
    let _ = parse::parse_status_porcelain(text);
    let _ = parse::parse_numstat_binaries(text);

    let _ = jj::parse_rev_metadata(text);
    let _ = jj::parse_jj_summary(text);
    let _ = jj::parse_rename(text);
    let _ = jj::parse_binary_from_stat(text);
}

pub fn read_cat_file_batch(stream: &[u8], file_paths: &[&str]) -> HashMap<String, String> {
    parse::read_cat_file_batch(&mut &stream[..], file_paths)
}
