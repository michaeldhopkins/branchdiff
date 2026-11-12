//! Shared `benches/baselines.toml` reader.
//!
//! Deserializes `[[bench]]` tables through `toml` and `serde` with
//! `#[serde(deny_unknown_fields)]` so no unknown key, duplicate key, or
//! malformed value can slip past any consumer (`tests/wall_medians.rs`,
//! `tests/perf_rules/guard.rs`, `benches/alloc.rs`).

use std::collections::BTreeSet;

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct BaselinesFile {
    #[serde(default)]
    bench: Vec<BenchRecord>,
}

/// One `[[bench]]` table in `benches/baselines.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchRecord {
    pub id: String,
    pub max_blocks: Option<u64>,
    pub max_bytes: Option<u64>,
    pub max_bytes_x86_64: Option<u64>,
    pub ref_median_ns: Option<u64>,
    pub ci_median_ns: Option<u64>,
    pub removed: Option<String>,
}

/// Parse and validate `benches/baselines.toml`.
pub fn parse(text: &str) -> Result<Vec<BenchRecord>, Vec<String>> {
    let file: BaselinesFile =
        toml::from_str(text).map_err(|err| vec![format!("baselines.toml: {err}")])?;
    let mut errors = Vec::new();
    let mut seen = BTreeSet::new();
    for record in &file.bench {
        let id = &record.id;
        if id.trim().is_empty() || id.contains('\n') || id.contains('\r') {
            errors.push("baselines.toml: bench `id` must be one non-empty line".to_string());
        }
        if !seen.insert(id.as_str()) {
            errors.push(format!("bench `{id}` appears more than once in baselines.toml"));
        }
        let has_pin = record.ref_median_ns.is_some()
            || record.max_blocks.is_some()
            || record.max_bytes.is_some()
            || record.max_bytes_x86_64.is_some();
        if let Some(reason) = &record.removed {
            if reason.trim().is_empty() || reason.contains('\n') || reason.contains('\r') {
                errors.push(format!("bench `{id}` removed must be one non-empty line"));
            }
            if has_pin {
                errors.push(format!("bench `{id}` sets both ref_median_ns and removed"));
            }
            continue;
        }
        if record.ref_median_ns == Some(0) {
            errors.push(format!("bench `{id}` ref_median_ns must be > 0"));
        }
        if record.ci_median_ns == Some(0) {
            errors.push(format!("bench `{id}` ci_median_ns must be > 0"));
        }
        if record.ci_median_ns.is_some() && record.ref_median_ns.is_none() {
            errors.push(format!("bench `{id}` sets ci_median_ns without ref_median_ns"));
        }
        if record.max_blocks.is_some() != record.max_bytes.is_some() {
            errors.push(format!("bench `{id}` sets only one of max_blocks and max_bytes"));
        }
        if record.max_bytes_x86_64.is_some() && record.max_bytes.is_none() {
            errors.push(format!("bench `{id}` sets max_bytes_x86_64 without max_bytes"));
        }
        if !has_pin {
            errors.push(format!("bench `{id}` has no pin fields"));
        }
    }
    if errors.is_empty() { Ok(file.bench) } else { Err(errors) }
}
