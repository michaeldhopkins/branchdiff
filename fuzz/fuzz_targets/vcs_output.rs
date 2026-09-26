#![no_main]

//! NEVER-PANICS target: every parser branchdiff runs over git's or jj's stdout.
//!
//! `git status`, `git diff --name-status` / `--numstat`, `git --version`, and jj's
//! `diff --summary` / `diff --stat` / rev-metadata template. None of these formats is
//! branchdiff's to version, and paths inside them are whatever the user named their
//! files. A panic here takes down the TUI on a refresh.
//!
//! Discards the results, so it finds availability bugs only.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // branchdiff reads every one of these through `stdout_lossy`, so lossy here too.
    let text = String::from_utf8_lossy(data);
    branchdiff::fuzz_api::parse_all_vcs_output(&text);
});
