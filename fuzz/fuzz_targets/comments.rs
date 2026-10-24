#![no_main]

//! Roundtrip + structural invariant + never-panics target for all review comment formats:
//! branchdiff JSONL, revdiff plain text, reviewdog rdjson/rdjsonl, and GitHub PR review
//! comments JSON, plus anchoring against a diff and HTML escaping.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    branchdiff::fuzz_api::fuzz_comments(data);
});
