// @template-sha256: 07c32bf99d52d176ba17a6b6da0c7b12f2570c8df8eed4461e82e6f5b2fb740f
//! Performance-budget guard. Copy this file unchanged.
//!
//! The adopter provides three things and does not edit this file:
//! - `tests/perf.toml`, one `[[op]]` per surface operation. Optional top-level
//!   `ci_pin_max_ratio` and `ci_pinned_benches`, before the first table, set the
//!   CI pin policy.
//! - `benches/baselines.toml`, the bench pins.
//! - a thin test that discovers the surface and calls `check` and
//!   `check_baseline_medians`. When the manifest omits the CI keys, the thin
//!   test passes the allowlist and the cap. Those values are the adopter's.
//!
//! Copy it to `tests/perf_rules/perf_budgets.rs` and load it with
//! `#[rustfmt::skip]` and `#[path = "perf_rules/perf_budgets.rs"]`. An inner
//! `#![rustfmt::skip]` does not compile in a submodule, so the attribute goes
//! on the module. Baseline lookup reads
//! `PERF_BASELINE_REV`, `GITHUB_REF`, and `PERF_BASELINE_REPO` (tests; the
//! default root is the crate manifest directory).
//!
//! Needs edition 2024 (it uses `let` chains). The manifest's CI policy keys
//! are not ratcheted against the baseline, so an adopter that wants the policy
//! to tighten only passes constants from its thin test and asserts the
//! manifest sets neither key.
//!
//! Counts only tighten. Each stage floor only tightens. A new stage is a new
//! floor, and the total is the sum of those floors, not a raised floor of its
//! own. A target may rise only when a new stage raised the total, and only by
//! as much as the total rose. A target may not pass its class cap, may not
//! exceed its wall floor, and has to carry a one-line derivation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub mod baselines {
    use std::collections::{BTreeMap, BTreeSet};

    /// One `[[bench]]` table in `benches/baselines.toml`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct BenchRecord {
        pub id: String,
        pub max_blocks: Option<u64>,
        pub max_bytes: Option<u64>,
        pub max_bytes_x86_64: Option<u64>,
        pub ref_median_ns: Option<u64>,
        pub ci_median_ns: Option<u64>,
        pub removed: Option<String>,
    }

    /// Parse and validate `benches/baselines.toml` with no serde dependency.
    /// Messages match the serde reader: unknown fields, duplicate ids, and the
    /// pin rules. An empty file, a comment-only file, and `bench = []` parse as
    /// no records.
    pub fn parse(text: &str) -> Result<Vec<BenchRecord>, Vec<String>> {
        let drafts = split_benches(text)?;
        let mut errors = Vec::new();
        let mut records = Vec::new();
        for fields in drafts {
            match build_record(fields) {
                Ok(record) => records.push(record),
                Err(message) => errors.push(message),
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        validate(records)
    }

    fn push_draft(drafts: &mut Vec<BTreeMap<String, super::Value>>, current: &mut Option<BTreeMap<String, super::Value>>) {
        if let Some(fields) = current.take() {
            drafts.push(fields);
        }
    }

    fn split_benches(text: &str) -> Result<Vec<BTreeMap<String, super::Value>>, Vec<String>> {
        let mut errors = Vec::new();
        let mut drafts = Vec::new();
        let mut current: Option<BTreeMap<String, super::Value>> = None;
        let mut saw_empty_array = false;
        for (index, raw) in text.lines().enumerate() {
            let line_no = index + 1;
            let line = super::strip_comment(raw.trim_end_matches('\r')).trim();
            if line.is_empty() {
                continue;
            }
            if line == "[[bench]]" {
                push_draft(&mut drafts, &mut current);
                current = Some(BTreeMap::new());
                continue;
            }
            if line.starts_with("[[") {
                errors.push(format!("baselines.toml: line {line_no}: unsupported table `{line}`"));
                continue;
            }
            match line.split_once('=') {
                Some((key, raw_value)) => assign_bench_line(
                    key.trim(),
                    raw_value.trim(),
                    line_no,
                    &mut current,
                    &mut saw_empty_array,
                    &mut errors,
                ),
                None => errors.push(format!("baselines.toml: line {line_no}: expected key = value")),
            }
        }
        push_draft(&mut drafts, &mut current);
        if saw_empty_array && !drafts.is_empty() {
            errors.push("baselines.toml: bench = [] cannot be combined with [[bench]]".to_string());
        }
        if errors.is_empty() { Ok(drafts) } else { Err(errors) }
    }

    fn assign_bench_line(
        key: &str,
        raw_value: &str,
        line_no: usize,
        current: &mut Option<BTreeMap<String, super::Value>>,
        saw_empty_array: &mut bool,
        errors: &mut Vec<String>,
    ) {
        let Some(fields) = current.as_mut() else {
            if key == "bench" && raw_value == "[]" {
                *saw_empty_array = true;
            } else {
                errors.push(format!("baselines.toml: unknown field `{key}`"));
            }
            return;
        };
        const KNOWN: &[&str] = &[
            "id",
            "max_blocks",
            "max_bytes",
            "max_bytes_x86_64",
            "ref_median_ns",
            "ci_median_ns",
            "removed",
        ];
        if !KNOWN.contains(&key) {
            errors.push(format!("baselines.toml: unknown field `{key}`"));
            return;
        }
        if fields.contains_key(key) {
            errors.push(format!("baselines.toml: duplicate key `{key}`"));
            return;
        }
        match super::parse_value(raw_value, line_no) {
            Ok(value) => {
                fields.insert(key.to_string(), value);
            }
            Err(message) => errors.push(format!("baselines.toml: {message}")),
        }
    }

    fn take_text(fields: &mut BTreeMap<String, super::Value>, key: &str) -> Result<Option<String>, String> {
        match fields.remove(key) {
            None => Ok(None),
            Some(super::Value::Text(text)) => Ok(Some(text)),
            Some(_) => Err(format!("baselines.toml: invalid type for `{key}`")),
        }
    }

    fn take_u64(fields: &mut BTreeMap<String, super::Value>, key: &str) -> Result<Option<u64>, String> {
        match fields.remove(key) {
            None => Ok(None),
            Some(super::Value::Number(number)) => Ok(Some(number)),
            Some(_) => Err(format!("baselines.toml: invalid type for `{key}`")),
        }
    }

    fn build_record(mut fields: BTreeMap<String, super::Value>) -> Result<BenchRecord, String> {
        let id = take_text(&mut fields, "id")?.ok_or_else(|| "baselines.toml: missing field `id`".to_string())?;
        let record = BenchRecord {
            id,
            max_blocks: take_u64(&mut fields, "max_blocks")?,
            max_bytes: take_u64(&mut fields, "max_bytes")?,
            max_bytes_x86_64: take_u64(&mut fields, "max_bytes_x86_64")?,
            ref_median_ns: take_u64(&mut fields, "ref_median_ns")?,
            ci_median_ns: take_u64(&mut fields, "ci_median_ns")?,
            removed: take_text(&mut fields, "removed")?,
        };
        if let Some(key) = fields.keys().next() {
            return Err(format!("baselines.toml: unknown field `{key}`"));
        }
        Ok(record)
    }

    fn validate(records: Vec<BenchRecord>) -> Result<Vec<BenchRecord>, Vec<String>> {
        let mut errors = Vec::new();
        let mut seen = BTreeSet::new();
        for record in &records {
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
        if errors.is_empty() { Ok(records) } else { Err(errors) }
    }
}

use baselines::BenchRecord;

/// One CLI command, in microseconds.
pub const COMMAND_US: u64 = 50_000;
/// Keystroke, to the next paint.
pub const KEYSTROKE_US: u64 = 8_000;
/// Page, to main content.
pub const PAGE_US: u64 = 100_000;
/// One MCP tool call.
pub const MCP_US: u64 = 200_000;
/// Frame, p95.
pub const FRAME_US: u64 = 4_000;

/// What the checker needs. The caller discovers the surface, the test names,
/// the bench medians, and the committed baseline. This module does not know
/// the project.
pub struct Input<'a> {
    pub manifest: &'a str,
    pub surface: &'a BTreeSet<String>,
    pub tests: &'a BTreeSet<String>,
    pub baseline: Option<&'a str>,
    pub benches: &'a BTreeMap<String, u64>,
}

/// One budgeted operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub id: String,
    pub class: String,
    pub fixture: String,
    pub test: String,
    pub processes: u64,
    pub owed: bool,
    pub floor: String,
    pub floor_reason: String,
    pub wall_floor_us: u64,
    pub target_us: u64,
    pub derivation: String,
    pub proposal: String,
    pub removed: String,
    pub benches: Vec<String>,
    pub alternatives: Vec<String>,
    /// `name=us` for each stage that runs, when there is more than one.
    /// A single running stage keeps its floor in `wall_floor_us`.
    pub stage_floors: Vec<String>,
}

#[derive(Debug)]
enum Value {
    Text(String),
    Number(u64),
    Bool(bool),
    List(Vec<String>),
}

/// Largest allowed `target_us` for `class`.
pub fn allowed_target_us(class: &str) -> Result<u64, String> {
    match class {
        "command" => Ok(COMMAND_US),
        "keystroke" => Ok(KEYSTROKE_US),
        "page" => Ok(PAGE_US),
        "mcp" => Ok(MCP_US),
        "frame" => Ok(FRAME_US),
        other => Err(format!("unknown class `{other}`")),
    }
}

/// Every problem in the manifest. Empty means the budgets hold.
/// A baseline that parses to zero operations fails when the live manifest has any:
/// a comment or a blank file would otherwise leave every live ceiling unchecked.
pub fn check(input: &Input<'_>) -> Vec<String> {
    let ops = match parse_operations(input.manifest) {
        Ok(ops) => ops,
        Err(errors) => return errors,
    };
    let mut errors = Vec::new();
    coverage(&ops, input.surface, &mut errors);
    for op in &ops {
        check_entry(op, input.tests, &mut errors);
    }
    check_benches(&ops, input.benches, &mut errors);
    if let Some(baseline) = input.baseline {
        match parse_operations(baseline) {
            Ok(previous) => {
                if previous.is_empty() && !ops.is_empty() {
                    errors.push("baseline manifest has no operations".to_string());
                }
                looseness(&ops, &previous, input.benches, &mut errors);
            }
            Err(parse_errors) => {
                errors.push(format!("baseline manifest: {}", parse_errors.join("; ")));
            }
        }
    }
    errors.sort();
    errors.dedup();
    errors
}

/// Active `[[bench]]` records mapped to `ref_median_ns` in nanoseconds.
/// Entries marked `removed = "<reason>"` are excluded. A file that does not
/// parse is an error: returning an empty map would drop every bench.
pub fn recorded_medians_ns(text: &str) -> Result<BTreeMap<String, u64>, Vec<String>> {
    let records = baselines::parse(text)?;
    Ok(records
        .into_iter()
        .filter(|record| record.removed.is_none())
        .filter_map(|record| record.ref_median_ns.map(|ns| (record.id, ns)))
        .collect())
}

/// `[[bench]]` records mapped to `ref_median_ns.div_ceil(1_000)` in microseconds.
pub fn recorded_benches(text: &str) -> Result<BTreeMap<String, u64>, Vec<String>> {
    Ok(recorded_medians_ns(text)?.into_iter().map(|(id, ns)| (id, ns.div_ceil(1_000))).collect())
}

/// Bench ceilings for a file that must parse. Callers that want the errors
/// as data use [`recorded_benches`].
pub fn must_recorded_benches(text: &str) -> BTreeMap<String, u64> {
    match recorded_benches(text) {
        Ok(benches) => benches,
        Err(errors) => panic!("{}", errors.join("\n")),
    }
}

/// The adopter's CI pin policy: which benches may carry `ci_median_ns`, and
/// the largest allowed `ci_median_ns / ref_median_ns`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiPolicy {
    pub pinned_benches: Vec<String>,
    pub max_ratio: u64,
}

/// Read `ci_pin_max_ratio` and `ci_pinned_benches` from the lines before the
/// first table. Both absent is `Ok(None)`: the thin test supplies the policy.
/// One without the other, an empty list, or a ratio of 0 is an error.
pub fn ci_policy_from_manifest(text: &str) -> Result<Option<CiPolicy>, String> {
    let mut ratio: Option<u64> = None;
    let mut benches: Option<Vec<String>> = None;
    for (index, raw) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = strip_comment(raw.trim_end_matches('\r')).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("[[") {
            break;
        }
        let Some((key, raw_value)) = line.split_once('=') else {
            return Err(format!("line {line_no}: text outside an operation"));
        };
        match key.trim() {
            "ci_pin_max_ratio" => {
                ratio = Some(parse_ci_ratio(raw_value.trim(), line_no, ratio.is_some())?);
            }
            "ci_pinned_benches" => {
                benches = Some(parse_ci_benches(raw_value.trim(), line_no, benches.is_some())?);
            }
            _ => return Err(format!("line {line_no}: text outside an operation")),
        }
    }
    match (ratio, benches) {
        (None, None) => Ok(None),
        (Some(max_ratio), Some(pinned_benches)) => Ok(Some(CiPolicy { pinned_benches, max_ratio })),
        _ => Err("ci pin policy needs both ci_pin_max_ratio and ci_pinned_benches".to_string()),
    }
}

fn parse_ci_ratio(raw: &str, line_no: usize, duplicate: bool) -> Result<u64, String> {
    if duplicate {
        return Err(format!("line {line_no}: duplicate key `ci_pin_max_ratio`"));
    }
    match parse_value(raw, line_no) {
        Ok(Value::Number(number)) if number > 0 => Ok(number),
        Ok(Value::Number(_)) => Err(format!("line {line_no}: ci_pin_max_ratio must be > 0")),
        Ok(_) => Err(format!("line {line_no}: ci_pin_max_ratio must be an integer")),
        Err(message) => Err(message),
    }
}

fn parse_ci_benches(raw: &str, line_no: usize, duplicate: bool) -> Result<Vec<String>, String> {
    if duplicate {
        return Err(format!("line {line_no}: duplicate key `ci_pinned_benches`"));
    }
    match parse_value(raw, line_no) {
        Ok(Value::List(items)) if !items.is_empty() => Ok(items),
        Ok(Value::List(_)) => Err(format!("line {line_no}: ci_pinned_benches is empty")),
        Ok(_) => Err(format!("line {line_no}: ci_pinned_benches must be a string array")),
        Err(message) => Err(message),
    }
}

/// Fail when `benches/baselines.toml` fails validation or when any
/// `ref_median_ns`, `max_blocks`, `max_bytes`, or `max_bytes_x86_64` rises
/// above or disappears without `removed = "<reason>"` from the baseline
/// revision's copy. A baseline that parses to zero records fails when the
/// live file has any: an empty or comment-only file would otherwise let
/// every pin rise. `pinned_benches` and `max_ratio` are the adopter's CI pin
/// allowlist and cap.
pub fn check_baseline_medians(
    live_text: &str,
    baseline_text: Option<&str>,
    pinned_benches: &[&str],
    max_ratio: u64,
    errors: &mut Vec<String>,
) {
    let live_records = match baselines::parse(live_text) {
        Ok(records) => records,
        Err(parse_errors) => {
            errors.extend(parse_errors);
            errors.sort();
            errors.dedup();
            return;
        }
    };
    check_ci_pins(&live_records, pinned_benches, max_ratio, errors);
    let Some(baseline_text) = baseline_text else {
        errors.sort();
        errors.dedup();
        return;
    };
    let prior_records = match baselines::parse(baseline_text) {
        Ok(records) => records,
        Err(parse_errors) => {
            errors.push(format!("baseline benches: {}", parse_errors.join("; ")));
            errors.sort();
            errors.dedup();
            return;
        }
    };
    if prior_records.is_empty() && !live_records.is_empty() {
        errors.push("baseline benches/baselines.toml has no records".to_string());
        errors.sort();
        errors.dedup();
        return;
    }
    let live_by_id: BTreeMap<&str, &baselines::BenchRecord> =
        live_records.iter().map(|record| (record.id.as_str(), record)).collect();
    for before in prior_records.iter().filter(|record| record.removed.is_none()) {
        let bench = before.id.as_str();
        let Some(live) = live_by_id.get(bench) else {
            errors.push(format!("bench `{bench}` was in the baseline but is missing"));
            continue;
        };
        if live.removed.is_some() {
            continue;
        }
        check_pin_field(bench, "ref_median_ns", live.ref_median_ns, before.ref_median_ns, errors);
        check_pin_field(bench, "ci_median_ns", live.ci_median_ns, before.ci_median_ns, errors);
        check_pin_field(bench, "max_blocks", live.max_blocks, before.max_blocks, errors);
        check_pin_field(bench, "max_bytes", live.max_bytes, before.max_bytes, errors);
        check_pin_field(
            bench, "max_bytes_x86_64", live.max_bytes_x86_64, before.max_bytes_x86_64, errors,
        );
    }
    errors.sort();
    errors.dedup();
}

fn check_ci_pins(
    records: &[baselines::BenchRecord],
    pinned_benches: &[&str],
    max_ratio: u64,
    errors: &mut Vec<String>,
) {
    for record in records.iter().filter(|record| record.removed.is_none()) {
        let Some(ci) = record.ci_median_ns else {
            continue;
        };
        let bench = record.id.as_str();
        if !pinned_benches.contains(&bench) {
            errors.push(format!(
                "bench `{bench}` has a ci_median_ns but is not in CI_PINNED_BENCHES"
            ));
        }
        if let Some(reference) = record.ref_median_ns
            && ci > reference.saturating_mul(max_ratio)
        {
            errors.push(format!(
                "bench `{bench}` ci_median_ns {ci} is over {max_ratio}x its ref_median_ns {reference}"
            ));
        }
    }
}

fn check_pin_field(
    bench: &str,
    key: &str,
    live: Option<u64>,
    before: Option<u64>,
    errors: &mut Vec<String>,
) {
    let Some(before_val) = before else {
        return;
    };
    match live {
        Some(live_val) if live_val > before_val => {
            errors.push(format!(
                "bench `{bench}` {key} {live_val} is looser than the baseline {before_val}"
            ));
        }
        None => {
            errors.push(format!("bench `{bench}` dropped {key} from the baseline"));
        }
        _ => {}
    }
}

/// Record every `#[test]` function under `dir` as `<file.rs>::<name>`.
pub fn collect_test_names(dir: &Path, out: &mut BTreeSet<String>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    collect_test_names_from(root, dir, out);
}

fn collect_test_names_from(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|err| panic!("read_dir {} failed: {err}", dir.display()));
    for entry in entries {
        let entry =
            entry.unwrap_or_else(|err| panic!("read_dir entry in {} failed: {err}", dir.display()));
        let path = entry.path();
        if path.is_dir() {
            collect_test_names_from(root, &path, out);
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "rs") {
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("read {} failed: {err}", path.display()));
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            push_test_names(&rel, &text, out);
        }
    }
}

/// Parse a manifest. The subset is `[[op]]` tables, `#` comments, one-line
/// strings, integers, and one-line string arrays.
pub fn parse_operations(text: &str) -> Result<Vec<Op>, Vec<String>> {
    let mut errors = Vec::new();
    let drafts = split_drafts(text, &mut errors);
    let mut ops = Vec::new();
    let mut seen = BTreeSet::new();
    for fields in drafts {
        let Some(op) = build_op(fields, &mut errors) else {
            continue;
        };
        if !seen.insert(op.id.clone()) {
            errors.push(format!("duplicate operation `{}`", op.id));
            continue;
        }
        ops.push(op);
    }
    if errors.is_empty() { Ok(ops) } else { Err(errors) }
}

fn split_drafts(text: &str, errors: &mut Vec<String>) -> Vec<BTreeMap<String, Value>> {
    let mut drafts = Vec::new();
    let mut current: Option<BTreeMap<String, Value>> = None;
    for (index, raw) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = strip_comment(raw.trim_end_matches('\r')).trim();
        if line.is_empty() {
            continue;
        }
        if line == "[[op]]" {
            if let Some(fields) = current.take() {
                drafts.push(fields);
            }
            current = Some(BTreeMap::new());
            continue;
        }
        if line.starts_with("[[") {
            errors.push(format!("line {line_no}: unsupported table `{line}`"));
            continue;
        }
        let Some(fields) = current.as_mut() else {
            if let Some((key, _)) = line.split_once('=') {
                let key = key.trim();
                if key == "ci_pin_max_ratio" || key == "ci_pinned_benches" {
                    continue;
                }
            }
            errors.push(format!("line {line_no}: text outside an operation"));
            continue;
        };
        assign_field(fields, line, line_no, errors);
    }
    if let Some(fields) = current {
        drafts.push(fields);
    }
    drafts
}

fn assign_field(
    fields: &mut BTreeMap<String, Value>,
    line: &str,
    line_no: usize,
    errors: &mut Vec<String>,
) {
    let Some((key, raw)) = line.split_once('=') else {
        errors.push(format!("line {line_no}: expected key = value"));
        return;
    };
    let key = key.trim();
    if key.is_empty() || !key.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        errors.push(format!("line {line_no}: invalid key"));
        return;
    }
    if fields.contains_key(key) {
        errors.push(format!("line {line_no}: duplicate key `{key}`"));
        return;
    }
    match parse_value(raw.trim(), line_no) {
        Ok(value) => {
            fields.insert(key.to_string(), value);
        }
        Err(message) => errors.push(message),
    }
}

fn parse_value(raw: &str, line_no: usize) -> Result<Value, String> {
    if raw.starts_with('"') {
        return Ok(Value::Text(parse_string(raw, line_no)?));
    }
    if raw.starts_with('[') {
        return Ok(Value::List(parse_list(raw, line_no)?));
    }
    if raw == "true" {
        return Ok(Value::Bool(true));
    }
    if raw == "false" {
        return Ok(Value::Bool(false));
    }
    raw.parse::<u64>().map(Value::Number).map_err(|_| {
        format!("line {line_no}: expected a string, integer, boolean, or string array")
    })
}

fn parse_list(raw: &str, line_no: usize) -> Result<Vec<String>, String> {
    let Some(inner) = raw.strip_prefix('[').and_then(|body| body.strip_suffix(']')) else {
        return Err(format!("line {line_no}: array must be one line"));
    };
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    for part in inner.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        items.push(parse_string(part, line_no)?);
    }
    Ok(items)
}

fn parse_string(raw: &str, line_no: usize) -> Result<String, String> {
    let mut chars = raw.char_indices();
    if chars.next().map(|(_, ch)| ch) != Some('"') {
        return Err(format!("line {line_no}: unclosed string"));
    }
    let mut escaped = false;
    let mut end = None;
    for (idx, ch) in chars {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            end = Some(idx);
            break;
        }
    }
    let Some(end) = end else {
        return Err(format!("line {line_no}: unclosed string"));
    };
    if !raw[end + '"'.len_utf8()..].trim().is_empty() {
        return Err(format!("line {line_no}: trailing junk after string"));
    }
    unescape(&raw[1..end], line_no)
}

/// A quoted string whose closing quote is the last character. Rejects a second
/// quote so `parse_string` does not accept `"a" leftover`.
fn unescape(inner: &str, line_no: usize) -> Result<String, String> {
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some(other) => {
                return Err(format!("line {line_no}: unknown escape \\{other}"));
            }
            None => return Err(format!("line {line_no}: trailing escape")),
        }
    }
    Ok(out)
}

fn build_op(mut fields: BTreeMap<String, Value>, errors: &mut Vec<String>) -> Option<Op> {
    let mut local = Vec::new();
    let id = require_one_line(&mut fields, "id", &mut local);
    let class = require_one_line(&mut fields, "class", &mut local);
    let fixture = require_one_line(&mut fields, "fixture", &mut local);
    let test = require_one_line(&mut fields, "test", &mut local);
    let processes = require_number(&mut fields, "processes", &mut local);
    let owed = optional_bool(&mut fields, "owed", &mut local);
    let floor = optional_one_line(&mut fields, "floor", &mut local);
    let floor_reason = optional_one_line(&mut fields, "floor_reason", &mut local);
    let wall_floor_us = require_number(&mut fields, "wall_floor_us", &mut local);
    let target_us = require_number(&mut fields, "target_us", &mut local);
    let derivation = require_one_line(&mut fields, "derivation", &mut local);
    let proposal = optional_one_line(&mut fields, "proposal", &mut local);
    let removed = optional_one_line(&mut fields, "removed", &mut local);
    let benches = optional_list(&mut fields, "benches", &mut local);
    let alternatives = optional_list(&mut fields, "alternatives", &mut local);
    let stage_floors = optional_list(&mut fields, "stage_floors", &mut local);
    let label = id.clone().unwrap_or_else(|| "?".to_string());
    for key in fields.keys() {
        local.push(format!("unknown key `{key}`"));
    }
    if !local.is_empty() {
        for message in local {
            errors.push(format!("operation `{label}`: {message}"));
        }
        return None;
    }
    Some(Op {
        id: id.unwrap_or_default(),
        class: class.unwrap_or_default(),
        fixture: fixture.unwrap_or_default(),
        test: test.unwrap_or_default(),
        processes: processes.unwrap_or_default(),
        owed: owed.unwrap_or_default(),
        floor: floor.unwrap_or_default(),
        floor_reason: floor_reason.unwrap_or_default(),
        wall_floor_us: wall_floor_us.unwrap_or_default(),
        target_us: target_us.unwrap_or_default(),
        derivation: derivation.unwrap_or_default(),
        proposal: proposal.unwrap_or_default(),
        removed: removed.unwrap_or_default(),
        benches: benches.unwrap_or_default(),
        alternatives: alternatives.unwrap_or_default(),
        stage_floors: stage_floors.unwrap_or_default(),
    })
}

fn require_one_line(
    fields: &mut BTreeMap<String, Value>,
    key: &str,
    errors: &mut Vec<String>,
) -> Option<String> {
    match fields.remove(key) {
        Some(Value::Text(text))
            if !text.trim().is_empty() && !text.contains('\n') && !text.contains('\r') =>
        {
            Some(text)
        }
        Some(Value::Text(_)) => {
            errors.push(format!("`{key}` must be one non-empty line"));
            None
        }
        Some(_) => {
            errors.push(format!("`{key}` must be a string"));
            None
        }
        None => {
            errors.push(format!("missing `{key}`"));
            None
        }
    }
}

fn require_number(
    fields: &mut BTreeMap<String, Value>,
    key: &str,
    errors: &mut Vec<String>,
) -> Option<u64> {
    match fields.remove(key) {
        Some(Value::Number(number)) => Some(number),
        Some(_) => {
            errors.push(format!("`{key}` must be an integer"));
            None
        }
        None => {
            errors.push(format!("missing `{key}`"));
            None
        }
    }
}

fn optional_one_line(
    fields: &mut BTreeMap<String, Value>,
    key: &str,
    errors: &mut Vec<String>,
) -> Option<String> {
    match fields.remove(key) {
        None => Some(String::new()),
        Some(Value::Text(text))
            if !text.trim().is_empty() && !text.contains('\n') && !text.contains('\r') =>
        {
            Some(text)
        }
        Some(Value::Text(_)) => {
            errors.push(format!("`{key}` must be one non-empty line"));
            None
        }
        Some(_) => {
            errors.push(format!("`{key}` must be a string"));
            None
        }
    }
}

fn optional_bool(
    fields: &mut BTreeMap<String, Value>,
    key: &str,
    errors: &mut Vec<String>,
) -> Option<bool> {
    match fields.remove(key) {
        None => Some(false),
        Some(Value::Bool(value)) => Some(value),
        Some(_) => {
            errors.push(format!("`{key}` must be a boolean"));
            None
        }
    }
}

fn optional_list(
    fields: &mut BTreeMap<String, Value>,
    key: &str,
    errors: &mut Vec<String>,
) -> Option<Vec<String>> {
    match fields.remove(key) {
        None => Some(Vec::new()),
        Some(Value::List(items)) => Some(items),
        Some(_) => {
            errors.push(format!("`{key}` must be a string array"));
            None
        }
    }
}

fn coverage(ops: &[Op], surface: &BTreeSet<String>, errors: &mut Vec<String>) {
    let present: BTreeSet<&str> = ops.iter().map(|op| op.id.as_str()).collect();
    for id in surface {
        if !present.contains(id.as_str()) {
            errors.push(format!("surface operation `{id}` has no budget"));
        }
    }
    for op in ops {
        if !surface.contains(&op.id) {
            errors.push(format!("operation `{}` is not on the surface", op.id));
        }
    }
}

fn check_entry(op: &Op, tests: &BTreeSet<String>, errors: &mut Vec<String>) {
    let valid_test_path = op
        .test
        .split_once("::")
        .is_some_and(|(file, name)| file.ends_with(".rs") && !name.is_empty());
    if !valid_test_path || !tests.contains(&op.test) {
        errors.push(format!("operation `{}` names missing test `{}`", op.id, op.test));
    }
    if op.wall_floor_us == 0 {
        errors.push(format!("operation `{}` wall_floor_us must be > 0", op.id));
    }
    if op.target_us == 0 {
        errors.push(format!("operation `{}` target_us must be > 0", op.id));
    }
    if op.target_us > op.wall_floor_us {
        errors.push(format!(
            "operation `{}` target_us {} exceeds wall_floor_us {}",
            op.id, op.target_us, op.wall_floor_us
        ));
    }
    if op.owed && !op.benches.is_empty() {
        errors.push(format!("operation `{}` is owed but names benches", op.id));
    }
    if op.owed && !op.floor.is_empty() {
        errors.push(format!("operation `{}` is owed but sets floor `{}`", op.id, op.floor));
    }
    if op.owed && !op.floor_reason.is_empty() {
        errors.push(format!("operation `{}` is owed but sets floor_reason", op.id));
    }
    if op.owed && !op.removed.is_empty() {
        errors.push(format!("operation `{}` is owed but sets removed", op.id));
    }
    if !op.owed && op.benches.is_empty() {
        errors.push(format!("operation `{}` is not owed and names no benches", op.id));
    }
    if !op.owed && !matches!(op.floor.as_str(), "sum" | "max") {
        errors.push(format!(
            "operation `{}` must set floor = \"sum\" or \"max\" (got `{}`)",
            op.id, op.floor
        ));
    }
    if !op.owed && op.floor_reason.is_empty() {
        errors.push(format!(
            "operation `{}` must set a one-line floor_reason for floor `{}`",
            op.id, op.floor
        ));
    }
    match allowed_target_us(&op.class) {
        Ok(cap) => {
            if op.target_us > cap {
                errors.push(format!(
                    "operation `{}` target_us {} exceeds the {} cap {cap}",
                    op.id, op.target_us, op.class
                ));
            }
            if !op.owed && op.wall_floor_us == cap {
                errors.push(format!(
                    "operation `{}` wall_floor_us {cap} equals the {} cap without owed = true",
                    op.id, op.class
                ));
            }
            if op.wall_floor_us > cap && op.proposal.is_empty() {
                errors.push(format!(
                    "operation `{}` wall_floor_us {} exceeds the {} cap {cap} without a proposal",
                    op.id, op.wall_floor_us, op.class
                ));
            }
        }
        Err(message) => errors.push(format!("operation `{}`: {message}", op.id)),
    }
}

fn check_benches(ops: &[Op], benches: &BTreeMap<String, u64>, errors: &mut Vec<String>) {
    let mut named = BTreeSet::new();
    for op in ops {
        let mut all_known = !op.benches.is_empty();
        let mut seen_in_op = BTreeSet::new();
        for bench in &op.benches {
            if !seen_in_op.insert(bench.as_str()) {
                errors.push(format!("operation `{}` lists bench `{bench}` more than once", op.id));
            }
            match benches.get(bench) {
                Some(&floor_us) => {
                    if floor_us == 0 {
                        errors.push(format!(
                            "operation `{}` bench `{bench}` ceiling must be > 0",
                            op.id
                        ));
                    }
                    if op.wall_floor_us < floor_us {
                        errors.push(format!(
                            "operation `{}` wall_floor_us {} is below bench `{bench}` ceiling {floor_us}",
                            op.id, op.wall_floor_us
                        ));
                    }
                }
                None => {
                    all_known = false;
                    errors.push(format!("operation `{}` names unknown bench `{bench}`", op.id));
                }
            }
            named.insert(bench.as_str());
        }
        if all_known && matches!(op.floor.as_str(), "sum" | "max") {
            let expected_floor = expected_floor(op, benches);
            if op.wall_floor_us != expected_floor {
                errors.push(format!(
                    "operation `{}` wall_floor_us {} does not match {} bench floor {expected_floor}",
                    op.id, op.wall_floor_us, op.floor
                ));
            }
        }
        check_stage_floor(op, benches, errors);
        require_stage_floors(op, benches, errors);
    }
    for bench in benches.keys() {
        if !named.contains(bench.as_str()) {
            errors.push(format!("bench `{bench}` is not named by any operation"));
        }
    }
}

fn is_alternative(op: &Op, bench: &str) -> bool {
    op.alternatives.iter().any(|alternative| alternative == bench)
}

/// Stages that run, in manifest order. An `alternatives` entry replaces one of
/// these and is not included.
fn stages(op: &Op) -> impl Iterator<Item = &String> {
    op.benches.iter().filter(|bench| !is_alternative(op, bench))
}

/// `sum` adds every stage that runs. `max` is the one-stage case: the floor is
/// the largest bench, and every other bench is an alternative.
fn expected_floor(op: &Op, benches: &BTreeMap<String, u64>) -> u64 {
    match op.floor.as_str() {
        "sum" => stages(op).filter_map(|bench| benches.get(bench)).copied().sum(),
        "max" => {
            op.benches.iter().filter_map(|bench| benches.get(bench)).copied().max().unwrap_or(0)
        }
        _ => 0,
    }
}

/// A floor sums every stage that runs in sequence. An alternative is another
/// fixture of a stage already counted: it has to be smaller than one of those
/// stages, and it cannot be the bench that sets a max floor.
fn check_stage_floor(op: &Op, benches: &BTreeMap<String, u64>, errors: &mut Vec<String>) {
    if !matches!(op.floor.as_str(), "sum" | "max") {
        if !op.alternatives.is_empty() {
            errors.push(format!(
                "operation `{}` lists alternatives without floor = \"sum\" or \"max\"",
                op.id
            ));
        }
        return;
    }
    let mut seen_alternatives = BTreeSet::new();
    for alternative in &op.alternatives {
        if !seen_alternatives.insert(alternative.as_str()) {
            errors.push(format!(
                "operation `{}` lists alternative `{alternative}` more than once",
                op.id
            ));
        }
        if !op.benches.contains(alternative) {
            errors.push(format!(
                "operation `{}` alternative `{alternative}` is not one of its benches",
                op.id
            ));
        }
    }
    let values: Vec<(&str, u64)> = op
        .benches
        .iter()
        .filter_map(|bench| benches.get(bench).map(|value| (bench.as_str(), *value)))
        .collect();
    if values.len() != op.benches.len() || values.is_empty() {
        return;
    }
    let max_us = values.iter().map(|(_, value)| *value).max().expect("at least one bench");
    let at_max: Vec<&str> =
        values.iter().filter(|(_, value)| *value == max_us).map(|(name, _)| *name).collect();
    for name in &at_max {
        if is_alternative(op, name) {
            errors.push(format!(
                "operation `{}` alternative `{name}` sets the max floor; an alternative cannot",
                op.id
            ));
        }
    }
    if op.floor == "max" {
        if at_max.len() != 1 {
            errors.push(format!(
                "operation `{}` floor = \"max\" ties at {max_us} us between {}; equal stages are summed",
                op.id,
                at_max.join(", ")
            ));
        }
        if let Some(dominant) = at_max.first().copied() {
            for (name, _) in &values {
                if *name != dominant && !is_alternative(op, name) {
                    errors.push(format!(
                        "operation `{}` bench `{name}` runs in sequence with the max floor; use floor = \"sum\"",
                        op.id
                    ));
                }
            }
        }
        return;
    }
    let stage_values: Vec<u64> = values
        .iter()
        .filter(|(name, _)| !is_alternative(op, name))
        .map(|(_, value)| *value)
        .collect();
    if stage_values.is_empty() {
        errors.push(format!("operation `{}` floor = \"sum\" lists only alternatives", op.id));
        return;
    }
    for (name, value) in &values {
        if is_alternative(op, name) && stage_values.iter().all(|stage| *value >= *stage) {
            errors.push(format!(
                "operation `{}` alternative `{name}` ({value} us) is not smaller than any stage that runs; a larger fixture belongs in the sum",
                op.id
            ));
        }
    }
}

/// `name=us`. The number is the stage's own ceiling, in microseconds.
fn parse_stage_floor(entry: &str) -> Option<(&str, u64)> {
    let (name, raw) = entry.rsplit_once('=')?;
    if name.is_empty() || name.contains('=') {
        return None;
    }
    let us: u64 = raw.parse().ok()?;
    if us == 0 { None } else { Some((name, us)) }
}

/// Stages a criss-cross head can be split into. An explicit `stage_floors`
/// list is used as written. One running stage uses `wall_floor_us`. Several
/// running stages and no list cannot be split.
pub fn running_stage_floors(op: &Op) -> Option<Vec<(String, u64)>> {
    if !op.stage_floors.is_empty() {
        let mut out = Vec::new();
        for entry in &op.stage_floors {
            let (name, us) = parse_stage_floor(entry)?;
            out.push((name.to_string(), us));
        }
        return Some(out);
    }
    let running: Vec<&String> = stages(op).collect();
    if running.len() == 1 && matches!(op.floor.as_str(), "sum" | "max") {
        return Some(vec![(running[0].clone(), op.wall_floor_us)]);
    }
    None
}

/// Per-stage floors for a looseness comparison.
///
/// An explicit list is that op's stages. One running stage keeps the whole
/// wall as its floor, which is how a motion measured on its own stays at that
/// number after a later stage is added. Several running stages with no list
/// split only when the wall is still the sum of the current bench ceilings.
fn split_stages(op: &Op, benches: &BTreeMap<String, u64>) -> Option<BTreeMap<String, u64>> {
    if !op.stage_floors.is_empty() {
        let mut map = BTreeMap::new();
        for entry in &op.stage_floors {
            let (name, us) = parse_stage_floor(entry)?;
            if map.insert(name.to_string(), us).is_some() {
                return None;
            }
        }
        return Some(map);
    }
    if !matches!(op.floor.as_str(), "sum" | "max") || op.benches.is_empty() {
        return None;
    }
    if !op.benches.iter().all(|bench| benches.contains_key(bench)) {
        return None;
    }
    // `floor = "max"` is one stage: the bench whose ceiling is the wall. Older
    // manifests named the other benches without an `alternatives` list.
    if op.floor == "max" {
        let matched: Vec<&String> =
            op.benches.iter().filter(|bench| benches[bench.as_str()] == op.wall_floor_us).collect();
        if matched.len() == 1 {
            return Some(BTreeMap::from([(matched[0].clone(), op.wall_floor_us)]));
        }
    }
    let running: Vec<&String> = stages(op).collect();
    if running.len() == 1 {
        return Some(BTreeMap::from([(running[0].clone(), op.wall_floor_us)]));
    }
    if running.len() > 1 {
        let sum: u64 = running.iter().map(|bench| benches[bench.as_str()]).sum();
        if op.wall_floor_us == sum {
            return Some(
                running.into_iter().map(|bench| (bench.clone(), benches[bench.as_str()])).collect(),
            );
        }
    }
    None
}

/// Two or more stages that run need `stage_floors`, one entry each, equal to
/// that bench's ceiling. The wall stays the sum. A single stage does not
/// repeat its floor in the list.
fn require_stage_floors(op: &Op, benches: &BTreeMap<String, u64>, errors: &mut Vec<String>) {
    if !matches!(op.floor.as_str(), "sum" | "max") {
        if !op.stage_floors.is_empty() {
            errors.push(format!(
                "operation `{}` lists stage_floors without floor = \"sum\" or \"max\"",
                op.id
            ));
        }
        return;
    }
    if op.benches.iter().any(|bench| !benches.contains_key(bench)) {
        return;
    }
    let running: Vec<&str> = stages(op).map(|bench| bench.as_str()).collect();
    if running.len() < 2 {
        if !op.stage_floors.is_empty() {
            errors.push(format!(
                "operation `{}` lists stage_floors with fewer than two running stages",
                op.id
            ));
        }
        return;
    }
    if op.stage_floors.is_empty() {
        errors.push(format!(
            "operation `{}` has {} running stages and no stage_floors",
            op.id,
            running.len()
        ));
        return;
    }
    let mut seen = BTreeSet::new();
    for entry in &op.stage_floors {
        let Some((name, us)) = parse_stage_floor(entry) else {
            errors.push(format!("operation `{}` stage floor `{entry}` must be name=us", op.id));
            continue;
        };
        if !seen.insert(name) {
            errors.push(format!("operation `{}` lists stage floor `{name}` more than once", op.id));
            continue;
        }
        if !running.contains(&name) {
            errors
                .push(format!("operation `{}` stage floor `{name}` is not a running stage", op.id));
            continue;
        }
        let ceiling = benches[name];
        if us != ceiling {
            errors.push(format!(
                "operation `{}` stage floor `{name}` {us} does not match bench ceiling {ceiling}",
                op.id
            ));
        }
    }
    for name in &running {
        if !seen.contains(name) {
            errors.push(format!("operation `{}` stage floor missing for `{name}`", op.id));
        }
    }
}

fn accounted(op: &Op, benches: &BTreeMap<String, u64>) -> bool {
    matches!(op.floor.as_str(), "sum" | "max")
        && !op.benches.is_empty()
        && op.benches.iter().all(|bench| benches.contains_key(bench))
        && op.wall_floor_us == expected_floor(op, benches)
}

fn looseness(
    ops: &[Op],
    previous: &[Op],
    benches: &BTreeMap<String, u64>,
    errors: &mut Vec<String>,
) {
    let prior: BTreeMap<&str, &Op> = previous.iter().map(|op| (op.id.as_str(), op)).collect();
    for op in ops {
        let Some(before) = prior.get(op.id.as_str()) else {
            if op.owed {
                errors.push(format!("operation `{}` is owed and absent from the baseline", op.id));
            }
            if !op.removed.is_empty() {
                errors.push(format!(
                    "operation `{}` sets removed without dropping a bench from the baseline",
                    op.id
                ));
            }
            continue;
        };
        looser(&op.id, "processes", op.processes, before.processes, errors);
        let live_stages = split_stages(op, benches);
        let base_stages = split_stages(before, benches);
        let mut shared_rose = false;
        match (&live_stages, &base_stages) {
            (Some(live), Some(base)) if live.len() == 1 && base.len() == 1 => {
                // One stage on each side: the wall is that stage's floor.
                if op.wall_floor_us > before.wall_floor_us {
                    shared_rose = true;
                }
                looser(&op.id, "wall_floor_us", op.wall_floor_us, before.wall_floor_us, errors);
            }
            (Some(live), Some(base)) => {
                for (name, base_us) in base {
                    if let Some(live_us) = live.get(name)
                        && *live_us > *base_us
                    {
                        shared_rose = true;
                        errors.push(format!(
                            "operation `{}` stage floor `{name}` {live_us} is looser than the baseline {base_us}",
                            op.id
                        ));
                    }
                }
            }
            _ => {
                looser(&op.id, "wall_floor_us", op.wall_floor_us, before.wall_floor_us, errors);
            }
        }
        let wall_rose = op.wall_floor_us > before.wall_floor_us;
        if op.target_us > before.target_us {
            let wall_delta = op.wall_floor_us.saturating_sub(before.wall_floor_us);
            let target_delta = op.target_us - before.target_us;
            let staged = live_stages.is_some()
                && base_stages.is_some()
                && !shared_rose
                && wall_rose
                && target_delta <= wall_delta;
            if !staged {
                looser(&op.id, "target_us", op.target_us, before.target_us, errors);
            }
        }
        let both_accounted = accounted(op, benches) && accounted(before, benches);
        if !before.owed && op.owed {
            errors.push(format!("operation `{}` is owed but was measured in the baseline", op.id));
        }
        if before.floor == "sum" && op.floor == "max" {
            errors.push(format!(
                "operation `{}` floor changed from `sum` to `max` against the baseline",
                op.id
            ));
        }
        // An owed baseline forbids alternatives, so paying it off is the first
        // time the measured entry may name the fixtures that replace each other.
        if !before.owed {
            for alternative in &op.alternatives {
                if before.alternatives.contains(alternative) {
                    continue;
                }
                let kept = before.benches.iter().any(|bench| bench == alternative)
                    && both_accounted
                    && expected_floor(op, benches) >= expected_floor(before, benches);
                if !kept {
                    errors.push(format!(
                        "operation `{}` adds alternative `{alternative}` not in the baseline",
                        op.id
                    ));
                }
            }
        }
        let mut dropped_any = false;
        for bench in &before.benches {
            if !op.benches.contains(bench) {
                dropped_any = true;
                if op.removed.is_empty() {
                    errors.push(format!(
                        "operation `{}` dropped bench `{bench}` from the baseline",
                        op.id
                    ));
                }
            }
        }
        if !dropped_any && !op.removed.is_empty() {
            errors.push(format!(
                "operation `{}` sets removed without dropping a bench from the baseline",
                op.id
            ));
        }
    }
}

fn looser(id: &str, field: &str, live: u64, before: u64, errors: &mut Vec<String>) {
    if live > before {
        errors
            .push(format!("operation `{id}` {field} {live} is looser than the baseline {before}"));
    }
}

fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    for (idx, ch) in line.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else if ch == '"' {
            in_string = true;
        } else if ch == '#' {
            return &line[..idx];
        }
    }
    line
}

fn push_test_names(rel_path: &str, source: &str, out: &mut BTreeSet<String>) {
    let scrubbed = scrub_comments_and_strings(source);
    let bytes = scrubbed.as_bytes();
    let mut index = 0;
    let mut has_test_attr = false;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"#[") {
            let Some(end) = bytes[index + 2..].iter().position(|&b| b == b']') else {
                break;
            };
            let attr = &scrubbed[index + 2..index + 2 + end];
            if is_test_attr(attr) {
                has_test_attr = true;
            }
            index += 2 + end + 1;
            continue;
        }
        if has_test_attr && matches!(bytes[index], b';' | b'{') {
            has_test_attr = false;
            index += 1;
            continue;
        }
        let at_fn = index + 3 < bytes.len()
            && matches!(&bytes[index..index + 3], b"fn " | b"fn\t" | b"fn\n");
        let boundary = index == 0 || !is_ident(bytes[index - 1]);
        if at_fn && boundary {
            let (name, next) = read_ident(bytes, index + 2);
            if has_test_attr && !name.is_empty() {
                out.insert(format!("{rel_path}::{name}"));
            }
            has_test_attr = false;
            index = next;
            continue;
        }
        index += 1;
    }
}

fn is_test_attr(attr: &str) -> bool {
    let path = attr.split('(').next().unwrap_or(attr).trim();
    path == "test" || path.ends_with("::test")
}

fn read_ident(bytes: &[u8], mut index: usize) -> (&str, usize) {
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    let start = index;
    while index < bytes.len() && is_ident(bytes[index]) {
        index += 1;
    }
    let name = std::str::from_utf8(&bytes[start..index]).unwrap_or("");
    (name, index)
}

fn scrub_comments_and_strings(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"//") {
            i = scrub_line_comment(bytes, i, &mut out);
        } else if bytes[i..].starts_with(b"/*") {
            i = scrub_block_comment(bytes, i, &mut out);
        } else if let Some(hashes) = raw_string_hashes(&bytes[i..]) {
            i = scrub_raw_string(bytes, i, hashes, &mut out);
        } else if bytes[i] == b'"' {
            i = scrub_quoted_string(bytes, i, &mut out);
        } else if bytes[i] == b'\'' {
            i = scrub_char_literal(bytes, i, &mut out);
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn scrub_line_comment(bytes: &[u8], mut i: usize, out: &mut String) -> usize {
    while i < bytes.len() && bytes[i] != b'\n' {
        out.push(' ');
        i += 1;
    }
    i
}

fn scrub_block_comment(bytes: &[u8], mut i: usize, out: &mut String) -> usize {
    let mut depth = 1_usize;
    out.push_str("  ");
    i += 2;
    while i < bytes.len() && depth > 0 {
        if bytes[i..].starts_with(b"/*") {
            depth += 1;
            out.push_str("  ");
            i += 2;
        } else if bytes[i..].starts_with(b"*/") {
            depth -= 1;
            out.push_str("  ");
            i += 2;
        } else {
            out.push(if bytes[i] == b'\n' { '\n' } else { ' ' });
            i += 1;
        }
    }
    i
}

fn raw_string_hashes(slice: &[u8]) -> Option<usize> {
    if slice.first().copied() != Some(b'r') {
        return None;
    }
    let hashes = slice[1..].iter().take_while(|&&b| b == b'#').count();
    (slice.get(1 + hashes).copied() == Some(b'"')).then_some(hashes)
}

fn scrub_raw_string(bytes: &[u8], mut i: usize, hashes: usize, out: &mut String) -> usize {
    for _ in 0..hashes + 2 {
        out.push(' ');
        i += 1;
    }
    while i < bytes.len() {
        if bytes[i] == b'"'
            && i + 1 + hashes <= bytes.len()
            && bytes[i + 1..i + 1 + hashes].iter().all(|&b| b == b'#')
        {
            for _ in 0..hashes + 1 {
                out.push(' ');
                i += 1;
            }
            break;
        }
        out.push(if bytes[i] == b'\n' { '\n' } else { ' ' });
        i += 1;
    }
    i
}

fn scrub_quoted_string(bytes: &[u8], mut i: usize, out: &mut String) -> usize {
    out.push(' ');
    i += 1;
    let mut escaped = false;
    while i < bytes.len() {
        let byte = bytes[i];
        out.push(if byte == b'\n' { '\n' } else { ' ' });
        i += 1;
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            break;
        }
    }
    i
}

fn scrub_char_literal(bytes: &[u8], i: usize, out: &mut String) -> usize {
    if i + 3 < bytes.len() && bytes[i + 1] == b'\\' && bytes[i + 3] == b'\'' {
        out.push_str("    ");
        return i + 4;
    }
    if i + 2 < bytes.len() && bytes[i + 2] == b'\'' {
        out.push_str("   ");
        return i + 3;
    }
    out.push('\'');
    i + 1
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The manifest committed at `PERF_BASELINE_REV` (when set in CI) or on
/// `main@origin` / `main`. `Ok(None)` is a first landing: an all-zero revision
/// whose parent has no file, or a resolved revision whose history never had
/// the file. A revision that had the file and lost it is an error. An empty
/// or comment-only file that is present is not this case; the guard rejects it.
pub fn committed_baseline() -> Result<Option<String>, String> {
    committed_baseline_file("tests/perf.toml")
}

pub fn committed_baseline_benches() -> Result<Option<String>, String> {
    committed_baseline_file("benches/baselines.toml")
}

/// Record a baseline-resolution error and keep checking the rest of the manifest.
pub fn recorded_baseline(
    result: Result<Option<String>, String>,
    errors: &mut Vec<String>,
) -> Option<String> {
    match result {
        Ok(text) => text,
        Err(message) => {
            errors.push(message);
            None
        }
    }
}

fn committed_baseline_file(rel_path: &str) -> Result<Option<String>, String> {
    let root = std::env::var_os("PERF_BASELINE_REPO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    let rev = std::env::var("PERF_BASELINE_REV").ok();
    let github_ref = std::env::var("GITHUB_REF").ok();
    committed_baseline_file_in(&root, rev.as_deref(), github_ref.as_deref(), rel_path)
}

#[cfg(test)]
pub fn committed_baseline_in(
    root: &Path,
    rev_override: Option<&str>,
) -> Result<Option<String>, String> {
    committed_baseline_file_in(root, rev_override, None, "tests/perf.toml")
}

pub fn committed_baseline_file_in(
    root: &Path,
    rev_override: Option<&str>,
    github_ref: Option<&str>,
    rel_path: &str,
) -> Result<Option<String>, String> {
    let rev_override = rev_override.map(str::trim).filter(|rev| !rev.is_empty());
    let github_ref = github_ref.map(str::trim).filter(|gref| !gref.is_empty());
    if root.join(".jj").exists() {
        committed_baseline_jj(root, rev_override, github_ref, rel_path)
    } else {
        committed_baseline_git(root, rev_override, github_ref, rel_path)
    }
}

fn is_all_zero_sha(rev: &str) -> bool {
    !rev.is_empty() && rev.bytes().all(|b| b == b'0')
}

const JJ_FORK_REVSETS: &[&str] =
    &["heads(::@ & ::main@origin)", "heads(::@ & ::main)", "main@origin", "main"];

fn missing_baseline(rel_path: &str, rev: &str) -> String {
    format!("baseline file `{rel_path}` is missing at `{rev}`")
}

enum BaselineFind {
    Text(String),
    Missing(String),
    Absent,
}

fn commit_baseline_jj(root: &Path, repo: &str, rev: &str, rel_path: &str) -> BaselineFind {
    match read_jj_file_at(root, repo, rev, rel_path) {
        Some(Some(text)) => BaselineFind::Text(text),
        Some(None) => BaselineFind::Missing(rev.to_string()),
        None => BaselineFind::Absent,
    }
}

fn search_jj(root: &Path, repo: &str, revsets: &[&str], rel_path: &str) -> BaselineFind {
    let mut missing = None;
    for revset in revsets {
        match commit_baseline_jj(root, repo, revset, rel_path) {
            BaselineFind::Text(text) => return BaselineFind::Text(text),
            BaselineFind::Missing(rev) => {
                missing.get_or_insert(rev);
            }
            BaselineFind::Absent => {}
        }
    }
    match missing {
        Some(rev) => BaselineFind::Missing(rev),
        None => BaselineFind::Absent,
    }
}

/// A missing file is a first landing when no ancestor ever changed the path,
/// and a deletion when one did.
fn file_was_removed(rel_path: &str, rev: &str, touched: bool) -> Result<Option<String>, String> {
    if touched { Err(missing_baseline(rel_path, rev)) } else { Ok(None) }
}

fn history_touches_jj(root: &Path, repo: &str, rev: &str, rel_path: &str) -> bool {
    let revset = format!("ancestors({rev}) & files(\"{rel_path}\")");
    let out = Command::new("jj")
        .current_dir(root)
        .args([
            "--ignore-working-copy", "--repository", repo, "log", "--no-graph", "-r", &revset,
            "-T", "commit_id",
        ])
        .output()
        .unwrap_or_else(|err| panic!("jj log failed to start: {err}"));
    assert!(
        out.status.success(),
        "jj log `{revset}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    !String::from_utf8_lossy(&out.stdout).trim().is_empty()
}

fn history_touches_git(repo: &str, rev: &str, rel_path: &str) -> bool {
    let out = Command::new("git")
        .args(["-C", repo, "rev-list", "-n", "1", rev, "--", rel_path])
        .output()
        .unwrap_or_else(|err| panic!("git rev-list failed to start: {err}"));
    assert!(
        out.status.success(),
        "git rev-list {rev} -- {rel_path} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    !String::from_utf8_lossy(&out.stdout).trim().is_empty()
}

fn baseline_from_find(
    found: BaselineFind,
    rel_path: &str,
    absent: &str,
    touched: impl FnOnce(&str) -> bool,
) -> Result<Option<String>, String> {
    match found {
        BaselineFind::Text(text) => Ok(Some(text)),
        BaselineFind::Missing(rev) => file_was_removed(rel_path, &rev, touched(&rev)),
        BaselineFind::Absent => panic!("{absent}"),
    }
}

fn finish_override(
    found: BaselineFind,
    rev: &str,
    rel_path: &str,
    parent_has_file: impl FnOnce() -> bool,
    read_at: impl FnOnce(&str) -> Option<Option<String>>,
    unresolved: impl FnOnce() -> String,
    touched: impl Fn(&str) -> bool,
) -> Result<Option<String>, String> {
    match found {
        BaselineFind::Text(text) => return Ok(Some(text)),
        BaselineFind::Missing(at) if !is_all_zero_sha(rev) => {
            return file_was_removed(rel_path, &at, touched(&at));
        }
        BaselineFind::Missing(_) | BaselineFind::Absent => {}
    }
    if is_all_zero_sha(rev) {
        assert!(
            !parent_has_file(),
            "PERF_BASELINE_REV is all-zero ({rev}) but {rel_path} already exists in the parent revision"
        );
        return Ok(None);
    }
    match read_at(rev) {
        Some(Some(text)) => Ok(Some(text)),
        Some(None) => file_was_removed(rel_path, rev, touched(rev)),
        None => panic!("{}", unresolved()),
    }
}

fn committed_baseline_jj(
    root: &Path,
    rev_override: Option<&str>,
    github_ref: Option<&str>,
    rel_path: &str,
) -> Result<Option<String>, String> {
    let repo = root.to_str().expect("repo path");
    let on_main = is_on_main_jj(root, repo, github_ref);
    if let Some(rev) = rev_override {
        let found = if on_main {
            BaselineFind::Absent
        } else {
            let is_pr = github_ref.is_some_and(|gref| gref.starts_with("refs/pull/"));
            if is_pr && !is_all_zero_sha(rev) {
                match commit_baseline_jj(root, repo, rev, rel_path) {
                    BaselineFind::Text(text) => return Ok(Some(text)),
                    BaselineFind::Missing(_) => {
                        return file_was_removed(
                            rel_path,
                            rev,
                            history_touches_jj(root, repo, rev, rel_path),
                        );
                    }
                    BaselineFind::Absent => {}
                }
            }
            search_jj(root, repo, JJ_FORK_REVSETS, rel_path)
        };
        let root_label = root.display().to_string();
        return finish_override(
            found,
            rev,
            rel_path,
            || read_jj_file_at(root, repo, "heads(::@ & ~empty())-", rel_path).flatten().is_some(),
            |pinned| read_jj_file_at(root, repo, pinned, rel_path),
            || format!("PERF_BASELINE_REV `{rev}` did not resolve in jj repository {root_label}"),
            |pinned| history_touches_jj(root, repo, pinned, rel_path),
        );
    }
    let revsets: &[&str] = if on_main { &["main@origin", "main"] } else { JJ_FORK_REVSETS };
    baseline_from_find(
        search_jj(root, repo, revsets, rel_path),
        rel_path,
        &format!("neither main@origin nor main resolved in jj repository {}", root.display()),
        |pinned| history_touches_jj(root, repo, pinned, rel_path),
    )
}

fn read_jj_file_at(root: &Path, repo: &str, rev: &str, rel_path: &str) -> Option<Option<String>> {
    let resolved = Command::new("jj")
        .current_dir(root)
        .args([
            "--ignore-working-copy", "--repository", repo, "log", "--no-graph", "-r", rev, "-T",
            "commit_id ++ \"\\n\"",
        ])
        .output()
        .unwrap_or_else(|err| panic!("jj log failed to start: {err}"));
    if !resolved.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&resolved.stdout);
    let mut commits: Vec<&str> = stdout.lines().map(str::trim).filter(|s| !s.is_empty()).collect();
    if commits.is_empty() {
        return None;
    }
    commits.sort_unstable();
    commits.dedup();
    let mut texts = Vec::new();
    for commit_id in commits {
        if let Some(text) = jj_file_at_commit(root, repo, commit_id, rel_path) {
            texts.push(text);
        }
    }
    if texts.is_empty() {
        return Some(None);
    }
    Some(Some(choose_jj_baseline(texts)))
}

/// Pick the baseline text from criss-cross heads, already ordered by commit id.
/// A head with no operations is skipped when another head has any, so a comment
/// or a blank file cannot hide a real manifest. A head that is lower on every
/// shared ceiling and higher on none wins as a whole file. Otherwise each
/// operation keeps the minimum `processes`, `wall_floor_us`, and `target_us`
/// across the heads that define it. When a head lists `stage_floors`, those
/// merge pointwise and the wall is the sum of the minimums; a head that cannot
/// be split contributes its wall as a bound, and a lower total does not win
/// the file whole. When that merge matches the earliest strictest head, that
/// head's original text is kept.
fn choose_jj_baseline(texts: Vec<String>) -> String {
    if let Some(merged) = merged_bench_baseline(&texts) {
        return merged;
    }
    let annotated: Vec<(BTreeMap<String, Ceilings>, String)> =
        texts.into_iter().map(|text| (operation_ceilings(&text), text)).collect();
    let any_ops = annotated.iter().any(|(ceilings, _)| !ceilings.is_empty());
    let candidates: Vec<(BTreeMap<String, Ceilings>, String)> =
        annotated.into_iter().filter(|(ceilings, _)| !(any_ops && ceilings.is_empty())).collect();
    assert!(!candidates.is_empty(), "a resolved file list has a baseline candidate");
    let best = strictest_index(&candidates);
    let staged = any_stage_floors(&candidates);
    if !staged && dominates(best, &candidates) {
        return candidates[best].1.clone();
    }
    let merged = render_merged(&candidates);
    if !staged && operation_ceilings(&merged) == candidates[best].0 {
        return candidates[best].1.clone();
    }
    merged
}

/// `benches/baselines.toml` heads. The file has no operations, so the manifest
/// rules above cannot rank them, and the lowest commit id would otherwise win
/// whole. Each pin keeps the minimum across the heads that record it, a record
/// pinned on any head is not removed, and a head with no records is skipped
/// when another has some. A head whose records already equal the merge keeps
/// its own text. `None` unless every head parses as bench records and one has any.
fn merged_bench_baseline(texts: &[String]) -> Option<String> {
    let parsed: Vec<(Vec<BenchRecord>, &String)> = texts
        .iter()
        .filter_map(|text| baselines::parse(text).ok().map(|records| (records, text)))
        .collect();
    if parsed.len() != texts.len() || parsed.iter().all(|(records, _)| records.is_empty()) {
        return None;
    }
    let heads: Vec<&(Vec<BenchRecord>, &String)> =
        parsed.iter().filter(|(records, _)| !records.is_empty()).collect();
    let mut order: Vec<String> = Vec::new();
    let mut merged: BTreeMap<String, BenchRecord> = BTreeMap::new();
    for (records, _) in &heads {
        for record in records {
            match merged.get_mut(&record.id) {
                Some(existing) => merge_bench_record(existing, record),
                None => {
                    order.push(record.id.clone());
                    merged.insert(record.id.clone(), record.clone());
                }
            }
        }
    }
    for (records, text) in &heads {
        let by_id: BTreeMap<&str, &BenchRecord> =
            records.iter().map(|record| (record.id.as_str(), record)).collect();
        let same = by_id.len() == merged.len()
            && merged.iter().all(|(id, record)| by_id.get(id.as_str()) == Some(&record));
        if same {
            return Some((*text).clone());
        }
    }
    Some(order.iter().map(|id| render_bench(&merged[id])).collect())
}

/// A pin on either head stands; both pinned keeps the lower. A record removed
/// on one head and pinned on the other stays pinned.
fn merge_bench_record(into: &mut BenchRecord, other: &BenchRecord) {
    if other.removed.is_some() {
        return;
    }
    if into.removed.is_some() {
        *into = other.clone();
        return;
    }
    into.max_blocks = min_pin(into.max_blocks, other.max_blocks);
    into.max_bytes = min_pin(into.max_bytes, other.max_bytes);
    into.max_bytes_x86_64 = min_pin(into.max_bytes_x86_64, other.max_bytes_x86_64);
    into.ref_median_ns = min_pin(into.ref_median_ns, other.ref_median_ns);
}

fn min_pin(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(pin), None) | (None, Some(pin)) => Some(pin),
        (None, None) => None,
    }
}

fn render_bench(record: &BenchRecord) -> String {
    let mut lines = vec!["[[bench]]".to_string(), format!("id = \"{}\"", toml_basic(&record.id))];
    if let Some(reason) = &record.removed {
        lines.push(format!("removed = \"{}\"", toml_basic(reason)));
    }
    for (key, value) in [
        ("max_blocks", record.max_blocks),
        ("max_bytes", record.max_bytes),
        ("max_bytes_x86_64", record.max_bytes_x86_64),
        ("ref_median_ns", record.ref_median_ns),
    ] {
        if let Some(value) = value {
            lines.push(format!("{key} = {value}"));
        }
    }
    lines.push(String::new());
    lines.push(String::new());
    lines.join("\n")
}

fn any_stage_floors(candidates: &[(BTreeMap<String, Ceilings>, String)]) -> bool {
    candidates.iter().any(|(_, text)| {
        parse_operations(text)
            .unwrap_or_default()
            .iter()
            .any(|op| !op.stage_floors.is_empty())
    })
}

fn strictest_index(candidates: &[(BTreeMap<String, Ceilings>, String)]) -> usize {
    let mut best = 0;
    for index in 1..candidates.len() {
        if is_stricter(&candidates[index].0, &candidates[best].0) {
            best = index;
        }
    }
    best
}

/// True when `best` is lower on a shared ceiling than every other head and
/// higher on none. A single head qualifies. A head that only adds ids does
/// not: the merge keeps those ids, then the original text when the ceilings match.
fn dominates(best: usize, candidates: &[(BTreeMap<String, Ceilings>, String)]) -> bool {
    candidates
        .iter()
        .enumerate()
        .all(|(index, (ceilings, _))| index == best || is_stricter(&candidates[best].0, ceilings))
}

/// Ceilings of one operation. `is_stricter` compares shared ids only: an id
/// missing on either side is not tighter, and any higher shared ceiling
/// rejects the candidate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Ceilings {
    processes: u64,
    wall_floor_us: u64,
    target_us: u64,
}

fn operation_ceilings(text: &str) -> BTreeMap<String, Ceilings> {
    let Ok(ops) = parse_operations(text) else {
        return BTreeMap::new();
    };
    ops.into_iter()
        .map(|op| {
            (
                op.id,
                Ceilings {
                    processes: op.processes,
                    wall_floor_us: op.wall_floor_us,
                    target_us: op.target_us,
                },
            )
        })
        .collect()
}

fn is_stricter(
    candidate: &BTreeMap<String, Ceilings>,
    current: &BTreeMap<String, Ceilings>,
) -> bool {
    let mut tighter = false;
    for (id, cand) in candidate {
        let Some(known) = current.get(id) else {
            continue;
        };
        if cand.processes > known.processes
            || cand.wall_floor_us > known.wall_floor_us
            || cand.target_us > known.target_us
        {
            return false;
        }
        if cand.processes < known.processes
            || cand.wall_floor_us < known.wall_floor_us
            || cand.target_us < known.target_us
        {
            tighter = true;
        }
    }
    tighter
}

fn render_merged(candidates: &[(BTreeMap<String, Ceilings>, String)]) -> String {
    let parsed: Vec<Vec<Op>> = candidates
        .iter()
        .map(|(_, text)| parse_operations(text).unwrap_or_default())
        .collect();
    let mut ids = Vec::new();
    let mut seen = BTreeSet::new();
    for ops in &parsed {
        for op in ops {
            if seen.insert(op.id.clone()) {
                ids.push(op.id.clone());
            }
        }
    }
    let mut out = String::new();
    for id in ids {
        let copies: Vec<&Op> =
            parsed.iter().filter_map(|ops| ops.iter().find(|op| op.id == id)).collect();
        let Some(first) = copies.first().copied() else {
            continue;
        };
        let processes = copies.iter().map(|op| op.processes).min().expect("at least one copy");
        let target = copies.iter().map(|op| op.target_us).min().expect("at least one copy");
        let explicit = copies.iter().any(|op| !op.stage_floors.is_empty());
        if !explicit {
            let wall = copies.iter().map(|op| op.wall_floor_us).min().expect("at least one copy");
            out.push_str(&render_op(first, processes, wall, target, None));
            continue;
        }
        let (stage_floors, wall) = merged_stage_floors(&copies);
        out.push_str(&render_op(first, processes, wall, target, Some(&stage_floors)));
    }
    out
}

/// Pointwise minimum of the stages each copy can name. A copy that cannot be
/// split does not invent stages; its wall is an upper bound on the sum.
fn merged_stage_floors(copies: &[&Op]) -> (Vec<String>, u64) {
    let mut order = Vec::new();
    let mut mins: BTreeMap<String, u64> = BTreeMap::new();
    let mut bounds = Vec::new();
    for op in copies {
        match running_stage_floors(op) {
            Some(stages) => {
                for (name, us) in stages {
                    if let Some(existing) = mins.get_mut(&name) {
                        *existing = (*existing).min(us);
                    } else {
                        order.push(name.clone());
                        mins.insert(name, us);
                    }
                }
            }
            None => bounds.push(op.wall_floor_us),
        }
    }
    let mut wall: u64 = mins.values().sum();
    if let Some(bound) = bounds.iter().copied().min() {
        wall = wall.min(bound);
    }
    let stage_floors = order.into_iter().map(|name| format!("{name}={}", mins[&name])).collect();
    (stage_floors, wall)
}

fn render_op(
    op: &Op,
    processes: u64,
    wall: u64,
    target: u64,
    stage_floors: Option<&[String]>,
) -> String {
    let mut lines = vec![
        "[[op]]".to_string(),
        format!("id = \"{}\"", toml_basic(&op.id)),
        format!("class = \"{}\"", toml_basic(&op.class)),
        format!("fixture = \"{}\"", toml_basic(&op.fixture)),
        format!("test = \"{}\"", toml_basic(&op.test)),
        format!("processes = {processes}"),
    ];
    if op.owed {
        lines.push("owed = true".to_string());
    }
    push_text(&mut lines, "floor", &op.floor);
    push_text(&mut lines, "floor_reason", &op.floor_reason);
    let floors = stage_floors.unwrap_or(op.stage_floors.as_slice());
    if !floors.is_empty() {
        lines.push(format!("stage_floors = [{}]", toml_list(floors)));
    }
    lines.push(format!("wall_floor_us = {wall}"));
    lines.push(format!("target_us = {target}"));
    lines.push(format!("derivation = \"{}\"", toml_basic(&op.derivation)));
    push_text(&mut lines, "proposal", &op.proposal);
    push_text(&mut lines, "removed", &op.removed);
    if !op.benches.is_empty() {
        lines.push(format!("benches = [{}]", toml_list(&op.benches)));
    }
    if !op.alternatives.is_empty() {
        lines.push(format!("alternatives = [{}]", toml_list(&op.alternatives)));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn push_text(lines: &mut Vec<String>, key: &str, value: &str) {
    if !value.is_empty() {
        lines.push(format!("{key} = \"{}\"", toml_basic(value)));
    }
}

fn toml_list(items: &[String]) -> String {
    items.iter().map(|item| format!("\"{}\"", toml_basic(item))).collect::<Vec<_>>().join(", ")
}

fn toml_basic(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

/// On main when CI names `main`, or, with `GITHUB_REF` unset, when the
/// working-copy commit is the `main` bookmark. A descendant of `main` is not
/// on main: it baselines at the fork, the way `is_on_main_git` reads `HEAD`.
fn is_on_main_jj(root: &Path, repo: &str, github_ref: Option<&str>) -> bool {
    if let Some(gref) = github_ref {
        return gref == "refs/heads/main" || gref == "main";
    }
    let out = Command::new("jj")
        .current_dir(root)
        .args([
            "--ignore-working-copy", "--repository", repo, "log", "--no-graph", "-r", "@ & main",
            "-T", "commit_id",
        ])
        .output()
        .unwrap_or_else(|err| panic!("jj log failed to start: {err}"));
    out.status.success() && !String::from_utf8_lossy(&out.stdout).trim().is_empty()
}

/// The file at one commit, or `None` when the commit does not hold it.
fn jj_file_at_commit(root: &Path, repo: &str, commit_id: &str, rel_path: &str) -> Option<String> {
    let root_pattern = format!("root:\"{rel_path}\"");
    let listed = Command::new("jj")
        .current_dir(root)
        .args([
            "--ignore-working-copy", "--repository", repo, "file", "list", "-r", commit_id,
            &root_pattern,
        ])
        .output()
        .unwrap_or_else(|err| panic!("jj file list failed to start: {err}"));
    assert!(
        listed.status.success(),
        "jj file list -r {commit_id} {rel_path} failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    if String::from_utf8_lossy(&listed.stdout).trim().is_empty() {
        return None;
    }
    let shown = Command::new("jj")
        .current_dir(root)
        .args([
            "--ignore-working-copy", "--repository", repo, "file", "show", "-r", commit_id,
            &root_pattern,
        ])
        .output()
        .unwrap_or_else(|err| panic!("jj file show failed to start: {err}"));
    assert!(
        shown.status.success(),
        "jj file show -r {commit_id} {rel_path} failed: {}",
        String::from_utf8_lossy(&shown.stderr)
    );
    Some(String::from_utf8(shown.stdout).expect("baseline is utf-8"))
}

/// On main when CI names `main`, when `HEAD` is the `main` branch, or when a
/// detached `HEAD` is `main`'s commit. Any other detached commit baselines at
/// the merge base, the same as a feature branch.
fn is_on_main_git(repo: &str, github_ref: Option<&str>) -> bool {
    if let Some(gref) = github_ref {
        return gref == "refs/heads/main" || gref == "main";
    }
    let out = Command::new("git")
        .args(["-C", repo, "symbolic-ref", "--short", "-q", "HEAD"])
        .output()
        .unwrap_or_else(|err| panic!("git symbolic-ref failed to start: {err}"));
    if out.status.success() {
        let branch = String::from_utf8_lossy(&out.stdout);
        return branch.trim() == "main";
    }
    match (git_commit(repo, "HEAD"), git_commit(repo, "main")) {
        (Some(head), Some(main)) => head == main,
        _ => false,
    }
}

fn git_commit(repo: &str, rev: &str) -> Option<String> {
    let spec = format!("{rev}^{{commit}}");
    let out = Command::new("git")
        .args(["-C", repo, "rev-parse", "--verify", "--quiet", &spec])
        .output()
        .unwrap_or_else(|err| panic!("git rev-parse failed to start: {err}"));
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8(out.stdout).expect("rev-parse utf-8");
    let sha = sha.trim();
    if sha.is_empty() { None } else { Some(sha.to_string()) }
}

fn git_merge_base(repo: &str, left: &str, right: &str) -> String {
    let out = Command::new("git")
        .args(["-C", repo, "merge-base", left, right])
        .output()
        .unwrap_or_else(|err| panic!("git merge-base failed to start: {err}"));
    assert!(
        out.status.success(),
        "git merge-base {left} {right} failed in {repo}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("merge-base utf-8").trim().to_string()
}

fn origin_main_exists(repo: &str) -> bool {
    Command::new("git")
        .args(["-C", repo, "rev-parse", "--verify", "--quiet", "origin/main^{commit}"])
        .status()
        .is_ok_and(|status| status.success())
}

fn search_git(repo: &str, root: &Path, on_main: bool, rel_path: &str) -> BaselineFind {
    let mut missing = None;
    if !on_main && origin_main_exists(repo) {
        let mb = git_merge_base(repo, "HEAD", "origin/main");
        match read_git_file_at(repo, &mb, rel_path) {
            Some(Some(text)) => return BaselineFind::Text(text),
            Some(None) => missing = Some(mb),
            None => panic!(
                "merge-base `{mb}` with origin/main did not resolve in git repository {}",
                root.display()
            ),
        }
    }
    for rev in ["origin/main", "main"] {
        match read_git_file_at(repo, rev, rel_path) {
            Some(Some(text)) => return BaselineFind::Text(text),
            Some(None) => {
                missing.get_or_insert_with(|| rev.to_string());
            }
            None => {}
        }
    }
    match missing {
        Some(rev) => BaselineFind::Missing(rev),
        None => BaselineFind::Absent,
    }
}

fn committed_baseline_git(
    root: &Path,
    rev_override: Option<&str>,
    github_ref: Option<&str>,
    rel_path: &str,
) -> Result<Option<String>, String> {
    let repo = root.to_str().expect("repo path");
    let on_main = is_on_main_git(repo, github_ref);
    if let Some(rev) = rev_override {
        let found = if on_main {
            BaselineFind::Absent
        } else {
            let is_pr = github_ref.is_some_and(|gref| gref.starts_with("refs/pull/"));
            if is_pr && !is_all_zero_sha(rev) {
                match read_git_file_at(repo, rev, rel_path) {
                    Some(Some(text)) => return Ok(Some(text)),
                    Some(None) => {
                        return file_was_removed(
                            rel_path,
                            rev,
                            history_touches_git(repo, rev, rel_path),
                        );
                    }
                    None => {}
                }
            }
            if !origin_main_exists(repo) {
                let _ = git_merge_base(repo, "HEAD", "origin/main");
            }
            search_git(repo, root, false, rel_path)
        };
        let root_label = root.display().to_string();
        return finish_override(
            found,
            rev,
            rel_path,
            || read_git_file_at(repo, "HEAD~1", rel_path).flatten().is_some(),
            |pinned| read_git_file_at(repo, pinned, rel_path),
            || format!("PERF_BASELINE_REV `{rev}` did not resolve in git repository {root_label}"),
            |pinned| history_touches_git(repo, pinned, rel_path),
        );
    }
    baseline_from_find(
        search_git(repo, root, on_main, rel_path),
        rel_path,
        &format!("neither origin/main nor main resolved in git repository {}", root.display()),
        |pinned| history_touches_git(repo, pinned, rel_path),
    )
}

fn read_git_file_at(repo: &str, rev: &str, rel_path: &str) -> Option<Option<String>> {
    let verify_arg = format!("{rev}^{{commit}}");
    let resolved = Command::new("git")
        .args(["-C", repo, "rev-parse", "--verify", "--quiet", &verify_arg])
        .output()
        .unwrap_or_else(|err| panic!("git rev-parse failed to start: {err}"));
    if !resolved.status.success() {
        return None;
    }
    let listed = Command::new("git")
        .args(["-C", repo, "ls-tree", "--name-only", rev, "--", rel_path])
        .output()
        .unwrap_or_else(|err| panic!("git ls-tree failed to start: {err}"));
    assert!(
        listed.status.success(),
        "git ls-tree {rev} {rel_path} failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    if String::from_utf8_lossy(&listed.stdout).trim().is_empty() {
        return Some(None);
    }
    let spec = format!("{rev}:{rel_path}");
    let shown = Command::new("git")
        .args(["-C", repo, "show", &spec])
        .output()
        .unwrap_or_else(|err| panic!("git show failed to start: {err}"));
    assert!(
        shown.status.success(),
        "git show {spec} failed: {}",
        String::from_utf8_lossy(&shown.stderr)
    );
    Some(Some(String::from_utf8(shown.stdout).expect("baseline is utf-8")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        fn new() -> std::io::Result<Self> {
            static N: AtomicU64 = AtomicU64::new(0);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "perf-budgets-{}-{}-{}",
                std::process::id(),
                nanos,
                N.fetch_add(1, Ordering::Relaxed),
            ));
            std::fs::create_dir(&path)?;
            Ok(Self { path })
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    const TEST_PINNED: &[&str] = &[
        "html/render/50_lines",
        "html/render/200_lines",
        "html/render/500_lines",
    ];
    const TEST_RATIO: u64 = 6;

    fn check_live_medians(live: &str, baseline: Option<&str>, errors: &mut Vec<String>) {
        super::check_baseline_medians(live, baseline, TEST_PINNED, TEST_RATIO, errors);
    }

    #[test]
    fn ci_policy_is_optional_and_a_complete_pair_parses_with_the_operations() {
        assert!(super::ci_policy_from_manifest("[[op]]\n").unwrap().is_none());
        assert!(super::ci_policy_from_manifest("# note\n").unwrap().is_none());
        let text = "\
ci_pin_max_ratio = 4
ci_pinned_benches = [\"one\", \"two\"]
[[op]]
id = \"command:print\"
class = \"command\"
fixture = \"one file\"
test = \"tests/counts.rs::counts\"
processes = 2
floor = \"max\"
floor_reason = \"Single benchmark bounds the operation.\"
wall_floor_us = 40000
target_us = 40000
derivation = \"One external call plus the class margin.\"
benches = [\"bench-a\"]
";
        let policy = super::ci_policy_from_manifest(text).unwrap().unwrap();
        assert_eq!(policy.max_ratio, 4);
        assert_eq!(policy.pinned_benches, vec!["one".to_string(), "two".to_string()]);
        let ops = super::parse_operations(text).expect("policy keys are not operation text");
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].id, "command:print");
        assert!(super::ci_policy_from_manifest("ci_pin_max_ratio = 4\n").is_err());
        assert!(super::ci_policy_from_manifest("ci_pinned_benches = [\"one\"]\n").is_err());
        assert!(super::ci_policy_from_manifest(
            "ci_pin_max_ratio = 0\nci_pinned_benches = [\"one\"]\n"
        ).is_err());
        assert!(super::ci_policy_from_manifest(
            "ci_pin_max_ratio = 4\nci_pinned_benches = []\n"
        ).is_err());
        assert!(super::ci_policy_from_manifest(
            "ci_pin_max_ratio = 4\nci_pin_max_ratio = 5\nci_pinned_benches = [\"one\"]\n"
        ).is_err());
        let after = "[[op]]\nid = \"command:print\"\nci_pin_max_ratio = 4\n";
        assert!(super::ci_policy_from_manifest(after).unwrap().is_none());
        let errors = super::parse_operations(after).expect_err("a policy key inside an op");
        assert!(
            errors.iter().any(|error| error.contains("unknown key `ci_pin_max_ratio`")),
            "{errors:?}"
        );
    }


fn names(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn bench_map(items: &[(&str, u64)]) -> BTreeMap<String, u64> {
    items.iter().map(|(id, floor)| ((*id).to_string(), *floor)).collect()
}

struct Case<'a> {
    manifest: &'a str,
    surface: &'a [&'a str],
    tests: &'a [&'a str],
    baseline: Option<&'a str>,
    benches: &'a [(&'a str, u64)],
}

fn run(case: Case<'_>) -> Vec<String> {
    let surface = names(case.surface);
    let tests = names(case.tests);
    let benches = bench_map(case.benches);
    check(&Input {
        manifest: case.manifest,
        surface: &surface,
        tests: &tests,
        baseline: case.baseline,
        benches: &benches,
    })
}

const HAPPY: &str = r#"
# a comment with an = sign is not a field
[[op]]
id = "command:print"
class = "command"
fixture = "one file"
test = "tests/counts.rs::counts"
processes = 2
floor = "max"
floor_reason = "Single benchmark bounds the operation."
wall_floor_us = 40000
target_us = 40000
derivation = "One external call plus the class margin."
benches = ["bench-a"]
"#;

fn happy<'a>(baseline: Option<&'a str>) -> Case<'a> {
    Case {
        manifest: HAPPY,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline,
        benches: &[("bench-a", 40_000)],
    }
}

fn manifest(class: &str, wall_floor: u64, target: u64, proposal: &str) -> String {
    let proposal_line =
        if proposal.is_empty() { String::new() } else { format!("proposal = \"{proposal}\"\n") };
    format!(
        r#"
[[op]]
id = "item"
class = "{class}"
fixture = "n"
test = "tests/counts.rs::counts"
processes = 1
owed = true
wall_floor_us = {wall_floor}
target_us = {target}
derivation = "Within the class."
{proposal_line}"#
    )
}

fn class_case(manifest: &str) -> Vec<String> {
    run(Case {
        manifest,
        surface: &["item"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[],
    })
}

#[test]
fn class_caps_match_the_ratchet() {
    assert_eq!(COMMAND_US, 50_000);
    assert_eq!(allowed_target_us("command").unwrap(), 50_000);
    assert_eq!(allowed_target_us("keystroke").unwrap(), KEYSTROKE_US);
    assert_eq!(KEYSTROKE_US, 8_000);
    assert_eq!(allowed_target_us("page").unwrap(), 100_000);
    assert_eq!(allowed_target_us("mcp").unwrap(), 200_000);
    assert_eq!(allowed_target_us("frame").unwrap(), 4_000);
    assert!(allowed_target_us("nope").is_err());
}

#[test]
fn a_complete_manifest_passes() {
    assert!(run(happy(None)).is_empty());
    assert!(run(happy(Some(HAPPY))).is_empty());
}

#[test]
fn an_empty_baseline_manifest_fails_when_the_live_manifest_has_operations() {
    let loose = HAPPY.replace("processes = 2", "processes = 999999");
    for baseline in ["", "# placeholder\n", "\n\n# comment only\n"] {
        let errors = run(Case {
            manifest: &loose,
            surface: &["command:print"],
            tests: &["tests/counts.rs::counts"],
            baseline: Some(baseline),
            benches: &[("bench-a", 40_000)],
        });
        assert_eq!(
            errors,
            vec!["baseline manifest has no operations"],
            "{baseline:?} must not let processes = 999999 through"
        );
    }
    let both_empty = run(Case {
        manifest: "",
        surface: &[],
        tests: &[],
        baseline: Some("# placeholder\n"),
        benches: &[],
    });
    assert!(
        !both_empty.iter().any(|error| error.contains("no operations")),
        "an empty live manifest has nothing to protect: {both_empty:?}"
    );
}

#[test]
fn a_surface_operation_without_an_entry_fails() {
    let errors = run(Case {
        manifest: HAPPY,
        surface: &["command:print", "command:help"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[("bench-a", 100)],
    });
    assert!(
        errors.iter().any(|error| error.contains("command:help") && error.contains("no budget"))
    );
}

#[test]
fn an_entry_for_a_missing_operation_fails() {
    let errors = run(Case {
        manifest: HAPPY,
        surface: &[],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[("bench-a", 100)],
    });
    assert!(
        errors
            .iter()
            .any(|error| error.contains("command:print") && error.contains("not on the surface"))
    );
}

#[test]
fn an_entry_that_names_a_missing_or_bare_test_fails() {
    let errors = run(Case {
        manifest: HAPPY,
        surface: &["command:print"],
        tests: &[],
        baseline: None,
        benches: &[("bench-a", 100)],
    });
    assert!(errors.iter().any(|error| error.contains("missing test")));

    let bare_manifest = HAPPY.replace("tests/counts.rs::counts", "counts");
    let errors = run(Case {
        manifest: &bare_manifest,
        surface: &["command:print"],
        tests: &["counts", "tests/counts.rs::counts"],
        baseline: None,
        benches: &[("bench-a", 100)],
    });
    assert!(
        errors.iter().any(|error| error.contains("missing test `counts`")),
        "bare test name must fail even if present in set: {errors:?}"
    );

    let wrong_file = HAPPY.replace("tests/counts.rs::counts", "tests/other.rs::counts");
    let errors = run(Case {
        manifest: &wrong_file,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[("bench-a", 100)],
    });
    assert!(
        errors.iter().any(|error| error.contains("missing test `tests/other.rs::counts`")),
        "test name from wrong file must fail: {errors:?}"
    );
}

#[test]
#[should_panic(expected = "read_dir")]
fn collect_test_names_panics_when_read_dir_fails() {
    let missing = Path::new("/nonexistent/perf-rules-missing-dir");
    let mut out = BTreeSet::new();
    collect_test_names(missing, &mut out);
}

#[test]
fn a_looser_floor_or_target_fails_and_a_tighter_one_passes() {
    let looser = HAPPY
        .replace("processes = 2", "processes = 3")
        .replace("wall_floor_us = 40000", "wall_floor_us = 45000")
        .replace("target_us = 40000", "target_us = 45000");
    let tighter = HAPPY
        .replace("processes = 2", "processes = 1")
        .replace("wall_floor_us = 40000", "wall_floor_us = 30000")
        .replace("target_us = 40000", "target_us = 30000");
    let errors = run(Case {
        manifest: &tighter,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: Some(HAPPY),
        benches: &[("bench-a", 30_000)],
    });
    assert!(errors.is_empty(), "{errors:?}");

    let errors = run(Case {
        manifest: &looser,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: Some(HAPPY),
        benches: &[("bench-a", 45_000)],
    });
    for field in ["processes", "wall_floor_us", "target_us"] {
        assert!(
            errors.iter().any(|error| error.contains(field) && error.contains("looser")),
            "{field}: {errors:?}"
        );
    }
}

fn scroll_case<'a>(
    manifest: &'a str,
    baseline: Option<&'a str>,
    benches: &'a [(&str, u64)],
) -> Case<'a> {
    Case {
        manifest,
        surface: &["key:scroll-down"],
        tests: &["tests/counts.rs::counts"],
        baseline,
        benches,
    }
}

/// A motion keeps the floor it had when it ran alone. A later stage is its own
/// floor. The total is the sum of those floors, and a shared stage may not rise.
#[test]
fn a_new_stage_keeps_the_old_floor_and_a_shared_stage_may_not_rise() {
    let before = r#"
[[op]]
id = "key:scroll-down"
class = "keystroke"
fixture = "n"
test = "tests/counts.rs::counts"
processes = 0
benches = ["scroll"]
floor = "max"
floor_reason = "The scroll bench measured the motion on its own."
wall_floor_us = 4
target_us = 4
derivation = "Ceiling of the scroll bench."
"#;
    let summed_without_stages = r#"
[[op]]
id = "key:scroll-down"
class = "keystroke"
fixture = "n"
test = "tests/counts.rs::counts"
processes = 0
benches = ["scroll", "span"]
floor = "sum"
floor_reason = "Scroll runs, then the span pass."
wall_floor_us = 54
target_us = 54
derivation = "Scroll bench plus the span bench."
"#;
    let benches = [("scroll", 4_u64), ("span", 50)];
    let errors = run(scroll_case(summed_without_stages, Some(before), &benches));
    assert!(
        errors.iter().any(|error| error.contains("stage_floors")),
        "two running stages with no stage floors must fail: {errors:?}"
    );

    let after = r#"
[[op]]
id = "key:scroll-down"
class = "keystroke"
fixture = "n"
test = "tests/counts.rs::counts"
processes = 0
benches = ["scroll", "span"]
floor = "sum"
floor_reason = "Scroll runs, then the span pass."
stage_floors = ["scroll=4", "span=50"]
wall_floor_us = 54
target_us = 54
derivation = "Scroll bench plus the span bench."
"#;
    let errors = run(scroll_case(after, Some(before), &benches));
    assert!(errors.is_empty(), "{errors:?}");

    let raised_benches = [("scroll", 84_u64), ("span", 50)];
    let raised_stage = after
        .replace(
            "stage_floors = [\"scroll=4\", \"span=50\"]",
            "stage_floors = [\"scroll=84\", \"span=50\"]",
        )
        .replace("wall_floor_us = 54", "wall_floor_us = 134")
        .replace("target_us = 54", "target_us = 134");
    let errors = run(scroll_case(&raised_stage, Some(before), &raised_benches));
    assert!(
        errors.iter().any(|error| error.contains("stage floor")
            && error.contains("scroll")
            && error.contains("looser")),
        "a shared stage raised from 4 to 84 must fail: {errors:?}"
    );

    let hand = after
        .replace("wall_floor_us = 54", "wall_floor_us = 200")
        .replace("target_us = 54", "target_us = 200");
    let errors = run(scroll_case(&hand, Some(before), &benches));
    assert!(
        errors.iter().any(|error| error.contains("wall_floor_us") && error.contains("54")),
        "a total above the sum of the stage floors must fail: {errors:?}"
    );

    let target_jump = after.replace("target_us = 54", "target_us = 300");
    let errors = run(scroll_case(&target_jump, Some(before), &benches));
    assert!(
        errors.iter().any(|error| error.contains("target_us") && error.contains("looser")),
        "{errors:?}"
    );
}

/// A baseline that already summed its stages, with no `stage_floors` field,
/// still ratchets each stage at that stage's bench ceiling.
#[test]
fn an_already_summed_baseline_splits_into_its_bench_ceilings() {
    let before = r#"
[[op]]
id = "key:scroll-down"
class = "keystroke"
fixture = "n"
test = "tests/counts.rs::counts"
processes = 0
benches = ["scroll", "span"]
floor = "sum"
floor_reason = "Scroll runs, then the span pass."
wall_floor_us = 54
target_us = 54
derivation = "Scroll bench plus the span bench."
"#;
    let after = r#"
[[op]]
id = "key:scroll-down"
class = "keystroke"
fixture = "n"
test = "tests/counts.rs::counts"
processes = 0
benches = ["scroll", "span"]
floor = "sum"
floor_reason = "Scroll runs, then the span pass."
stage_floors = ["scroll=4", "span=50"]
wall_floor_us = 54
target_us = 54
derivation = "Scroll bench plus the span bench."
"#;
    let benches = [("scroll", 4_u64), ("span", 50)];
    let errors = run(scroll_case(after, Some(before), &benches));
    assert!(errors.is_empty(), "{errors:?}");

    let explicit = after;
    let raised_benches = [("scroll", 9_u64), ("span", 50)];
    let raised = after
        .replace(
            "stage_floors = [\"scroll=4\", \"span=50\"]",
            "stage_floors = [\"scroll=9\", \"span=50\"]",
        )
        .replace("wall_floor_us = 54", "wall_floor_us = 59")
        .replace("target_us = 54", "target_us = 59");
    let errors = run(scroll_case(&raised, Some(explicit), &raised_benches));
    assert!(
        errors.iter().any(|error| error.contains("stage floor") && error.contains("looser")),
        "raising a recorded stage floor must fail: {errors:?}"
    );
}

#[test]
fn a_target_over_its_class_cap_always_fails() {
    let cases = [
        ("command", 50_001_u64),
        ("keystroke", 8_001),
        ("page", 100_001),
        ("frame", 4_001),
        ("mcp", 200_001),
    ];
    for (class, over_target) in cases {
        let over = manifest(class, over_target, over_target, "");
        let errors = class_case(&over);
        assert!(
            errors.iter().any(|error| error.contains("exceeds") && error.contains(class)),
            "{class}: {errors:?}"
        );
        let with_proposal = manifest(class, over_target, over_target, "split off the hot path");
        let errors = class_case(&with_proposal);
        assert!(
            errors.iter().any(|error| error.contains("exceeds") && error.contains(class)),
            "{class} with proposal must still fail over cap: {errors:?}"
        );
        let at_cap = manifest(class, over_target - 1, over_target - 1, "");
        assert!(class_case(&at_cap).is_empty(), "{class} at cap: {:?}", class_case(&at_cap));
    }
}

#[test]
fn a_wall_floor_over_its_class_cap_requires_a_proposal() {
    let cases = [
        ("command", 50_000_u64),
        ("keystroke", 8_000),
        ("page", 100_000),
        ("frame", 4_000),
        ("mcp", 200_000),
    ];
    for (class, cap) in cases {
        let missing = manifest(class, cap + 1, cap, "");
        let errors = class_case(&missing);
        assert!(
            errors.iter().any(|error| error.contains("wall_floor_us")
                && error.contains("proposal")
                && error.contains(class)),
            "{class} without proposal must fail: {errors:?}"
        );

        let present =
            manifest(class, cap + 1, cap, "Batch per-file subprocesses into one streaming call.");
        let errors = class_case(&present);
        assert!(errors.is_empty(), "{class} with proposal must pass: {errors:?}");
    }
}

#[test]
fn target_above_wall_floor_or_zero_budget_fails() {
    let over_floor = manifest("keystroke", 500, 1_000, "");
    let errors = class_case(&over_floor);
    assert!(errors.iter().any(|error| error.contains("exceeds wall_floor_us")), "{errors:?}");

    let zero_target = manifest("keystroke", 1_000, 0, "");
    let errors = class_case(&zero_target);
    assert!(errors.iter().any(|error| error.contains("target_us must be > 0")), "{errors:?}");
}

#[test]
fn a_derivation_must_be_one_non_empty_line() {
    for (field, current) in [
        ("id", "id = \"command:print\""),
        ("class", "class = \"command\""),
        ("fixture", "fixture = \"one file\""),
        ("test", "test = \"tests/counts.rs::counts\""),
        ("floor", "floor = \"max\""),
        ("floor_reason", "floor_reason = \"Single benchmark bounds the operation.\""),
        ("derivation", "derivation = \"One external call plus the class margin.\""),
    ] {
        for bad in ["\"\"", "\"first\\nsecond\"", "\"first\\rsecond\""] {
            let edited = HAPPY.replace(current, &format!("{field} = {bad}"));
            let errors = run(happy_manifest(&edited));
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains(field) && error.contains("one non-empty line")),
                "{field} = {bad}: {errors:?}"
            );
        }
    }
    for optional in ["proposal", "removed"] {
        for bad in ["\"\"", "\"first\\nsecond\"", "\"first\\rsecond\""] {
            let edited = format!("{HAPPY}{optional} = {bad}\n");
            let errors = run(happy_manifest(&edited));
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains(optional) && error.contains("one non-empty line")),
                "{optional} = {bad}: {errors:?}"
            );
        }
    }
}

#[test]
fn a_new_operation_absent_from_the_baseline_is_allowed_when_measured_and_refused_when_owed() {
    let live_measured = format!(
        "{HAPPY}\n[[op]]\nid = \"command:help\"\nclass = \"command\"\nfixture = \"none\"\ntest = \"tests/counts.rs::counts\"\nprocesses = 0\nfloor = \"max\"\nfloor_reason = \"Single benchmark bounds help.\"\nwall_floor_us = 40000\ntarget_us = 40000\nderivation = \"Measured against bench-a.\"\nbenches = [\"bench-a\"]\n"
    );
    let errors = run(Case {
        manifest: &live_measured,
        surface: &["command:print", "command:help"],
        tests: &["tests/counts.rs::counts"],
        baseline: Some(HAPPY),
        benches: &[("bench-a", 40_000)],
    });
    assert!(errors.is_empty(), "{errors:?}");

    let live_owed = format!(
        "{HAPPY}\n[[op]]\nid = \"command:help\"\nclass = \"command\"\nfixture = \"none\"\ntest = \"tests/counts.rs::counts\"\nprocesses = 0\nowed = true\nwall_floor_us = 1\ntarget_us = 1\nderivation = \"No external call.\"\n"
    );
    let owed_errors = run(Case {
        manifest: &live_owed,
        surface: &["command:print", "command:help"],
        tests: &["tests/counts.rs::counts"],
        baseline: Some(HAPPY),
        benches: &[("bench-a", 40_000)],
    });
    assert!(
        owed_errors.iter().any(|error| error.contains("command:help")
            && error.contains("owed")
            && error.contains("baseline")),
        "growing the owed set against the baseline must fail: {owed_errors:?}"
    );
}

#[test]
fn every_named_bench_has_to_exist_and_wall_floor_must_equal_its_bench_or_derived_sum() {
    let unknown = HAPPY.replace("bench-a", "bench-missing");
    let errors = run(Case {
        manifest: &unknown,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[("bench-a", 40_000)],
    });
    assert!(
        errors.iter().any(|error| error.contains("unknown bench `bench-missing`")),
        "{errors:?}"
    );
    assert!(
        errors.iter().any(|error| error.contains("bench `bench-a` is not named")),
        "{errors:?}"
    );

    let below_bench = run(Case {
        manifest: HAPPY,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[("bench-a", 60_000)],
    });
    assert!(
        below_bench.iter().any(|error| error.contains("below bench `bench-a` ceiling 60000")),
        "{below_bench:?}"
    );
}

#[test]
fn fingerprint_floor_must_equal_its_single_bench() {
    // A 40 ms fingerprint bench with floor 40000 and no proposal passes.
    let fp_40ms = r#"
[[op]]
id = "command:fingerprint"
class = "command"
fixture = "500 files, 25 changed"
test = "tests/counts.rs::counts"
processes = 11
benches = ["refresh/fingerprint/500f_25c"]
floor = "max"
floor_reason = "Single benchmark bounds fingerprint."
wall_floor_us = 40000
target_us = 40000
derivation = "Floor is refresh/fingerprint/500f_25c (40000 us)."
"#;
    let ok = run(Case {
        manifest: fp_40ms,
        surface: &["command:fingerprint"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[("refresh/fingerprint/500f_25c", 40_000)],
    });
    assert!(ok.is_empty(), "40 ms fingerprint bench with floor 40000 must pass: {ok:?}");

    // A floor 10% above its benches fails.
    let fp_10pct_above = fp_40ms.replace("wall_floor_us = 40000", "wall_floor_us = 44000");
    let floated = run(Case {
        manifest: &fp_10pct_above,
        surface: &["command:fingerprint"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[("refresh/fingerprint/500f_25c", 40_000)],
    });
    assert!(
        floated
            .iter()
            .any(|error| error.contains("wall_floor_us 44000") && error.contains("40000")),
        "floor 10% above bench must fail: {floated:?}"
    );
}

#[test]
fn sum_floor_must_equal_the_sum_of_all_named_benches() {
    // When floor = "sum", wall_floor_us must equal the sum of all named benches,
    // regardless of what the derivation prose says.
    let summed = r#"
[[op]]
id = "command:files"
class = "command"
fixture = "500 files"
test = "tests/counts.rs::counts"
processes = 5
benches = ["bench-a", "bench-b"]
floor = "sum"
floor_reason = "bench-a and bench-b run sequentially."
stage_floors = ["bench-a=30000", "bench-b=10000"]
wall_floor_us = 40000
target_us = 40000
derivation = "Floor is bench-a (30000 us) plus bench-b (10000 us)."
"#;
    let sum_ok = run(Case {
        manifest: summed,
        surface: &["command:files"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[("bench-a", 30_000), ("bench-b", 10_000)],
    });
    assert!(sum_ok.is_empty(), "derived sum must pass: {sum_ok:?}");

    // Deleting a bench from derivation while lowering wall_floor_us must fail.
    let stripped_derivation = summed
        .replace(
            "Floor is bench-a (30000 us) plus bench-b (10000 us).",
            "Floor is bench-a (30000 us).",
        )
        .replace("wall_floor_us = 40000", "wall_floor_us = 30000");
    let stripped_err = run(Case {
        manifest: &stripped_derivation,
        surface: &["command:files"],
        tests: &["tests/counts.rs::counts"],
        baseline: Some(summed),
        benches: &[("bench-a", 30_000), ("bench-b", 10_000)],
    });
    assert!(
        stripped_err
            .iter()
            .any(|error| error.contains("wall_floor_us 30000") && error.contains("40000")),
        "deleting a bench from derivation while lowering floor must fail: {stripped_err:?}"
    );

    let sum_floated = summed.replace("wall_floor_us = 40000", "wall_floor_us = 44000");
    let sum_err = run(Case {
        manifest: &sum_floated,
        surface: &["command:files"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[("bench-a", 30_000), ("bench-b", 10_000)],
    });
    assert!(
        sum_err
            .iter()
            .any(|error| error.contains("wall_floor_us 44000") && error.contains("40000")),
        "floor 10% above derived sum must fail: {sum_err:?}"
    );
}

#[test]
fn baseline_medians_ratchet_rejects_raised_or_missing_benches_and_allows_documented_removal() {
    let base_toml = concat!(
        "[[bench]]\nid = \"refresh/non_colocated/50f_10c\"\nref_median_ns = 73175330\n",
        "[[bench]]\nid = \"refresh/old\"\nref_median_ns = 1000000\n",
    );
    let raised_toml = concat!(
        "[[bench]]\nid = \"refresh/non_colocated/50f_10c\"\nref_median_ns = 130000000\n",
        "[[bench]]\nid = \"refresh/old\"\nref_median_ns = 1000000\n",
    );
    let mut raised_errors = Vec::new();
    check_live_medians(raised_toml, Some(base_toml), &mut raised_errors);
    assert!(
        raised_errors.iter().any(|err| err.contains("refresh/non_colocated/50f_10c")
            && err.contains("130000000")
            && err.contains("73175330")),
        "raised ref_median_ns must fail: {raised_errors:?}"
    );

    // Undocumented removal from baselines.toml must fail with `was in the baseline but is missing`.
    let missing_toml =
        "[[bench]]\nid = \"refresh/non_colocated/50f_10c\"\nref_median_ns = 73175330\n";
    let mut missing_errors = Vec::new();
    check_live_medians(missing_toml, Some(base_toml), &mut missing_errors);
    assert_eq!(
        missing_errors,
        vec!["bench `refresh/old` was in the baseline but is missing"],
        "missing baseline bench must fail: {missing_errors:?}"
    );

    // Documented removal with `removed = "<reason>"` is allowed and excluded from active benches.
    let documented_removal = concat!(
        "[[bench]]\nid = \"refresh/non_colocated/50f_10c\"\nref_median_ns = 73175330\n",
        "[[bench]]\nid = \"refresh/old\"\nremoved = \"Superseded by refresh/non_colocated/50f_10c.\"\n",
    );
    let mut removed_errors = Vec::new();
    check_live_medians(documented_removal, Some(base_toml), &mut removed_errors);
    assert!(removed_errors.is_empty(), "documented removal must pass: {removed_errors:?}");
    let active = recorded_benches(documented_removal).expect("documented removal parses");
    assert!(!active.contains_key("refresh/old"));

    // Empty `removed = ""` or combining `removed` with `ref_median_ns` must fail.
    let empty_removed = concat!(
        "[[bench]]\nid = \"refresh/non_colocated/50f_10c\"\nref_median_ns = 73175330\n",
        "[[bench]]\nid = \"refresh/old\"\nremoved = \"\"\n",
    );
    let mut empty_errors = Vec::new();
    check_live_medians(empty_removed, Some(base_toml), &mut empty_errors);
    assert!(
        empty_errors
            .iter()
            .any(|err| err.contains("refresh/old") && err.contains("one non-empty line")),
        "empty removed reason must fail: {empty_errors:?}"
    );

    let both_set = concat!(
        "[[bench]]\nid = \"refresh/non_colocated/50f_10c\"\nref_median_ns = 73175330\n",
        "[[bench]]\nid = \"refresh/old\"\nref_median_ns = 1000000\nremoved = \"reason\"\n",
    );
    let mut both_errors = Vec::new();
    check_live_medians(both_set, Some(base_toml), &mut both_errors);
    assert!(
        both_errors.iter().any(
            |err| err.contains("refresh/old") && err.contains("both ref_median_ns and removed")
        ),
        "setting both ref_median_ns and removed must fail: {both_errors:?}"
    );

    let removed_first = concat!(
        "[[bench]]\nid = \"refresh/non_colocated/50f_10c\"\nref_median_ns = 73175330\n",
        "[[bench]]\nid = \"refresh/old\"\nremoved = \"reason\"\nref_median_ns = 1000000\n",
    );
    let removed_first_errors =
        recorded_benches(removed_first).expect_err("a pin beside removed is an error");
    assert!(
        removed_first_errors
            .iter()
            .any(|err| err.contains("refresh/old") && err.contains("sets both")),
        "a pin after removed must fail closed, not drop the bench: {removed_first_errors:?}"
    );

    // Raising or dropping max_blocks, max_bytes, or max_bytes_x86_64 against the baseline must fail.
    let alloc_base = "[[bench]]\nid = \"html/render/50_lines\"\nmax_blocks = 10\nmax_bytes = 200\nmax_bytes_x86_64 = 220\nref_median_ns = 1000\n";
    let alloc_raised = "[[bench]]\nid = \"html/render/50_lines\"\nmax_blocks = 11\nmax_bytes = 201\nmax_bytes_x86_64 = 225\nref_median_ns = 1000\n";
    let mut alloc_raised_errors = Vec::new();
    check_live_medians(alloc_raised, Some(alloc_base), &mut alloc_raised_errors);
    assert_eq!(
        alloc_raised_errors,
        vec![
            "bench `html/render/50_lines` max_blocks 11 is looser than the baseline 10",
            "bench `html/render/50_lines` max_bytes 201 is looser than the baseline 200",
            "bench `html/render/50_lines` max_bytes_x86_64 225 is looser than the baseline 220",
        ]
    );

    let alloc_dropped = "[[bench]]\nid = \"html/render/50_lines\"\nref_median_ns = 1000\n";
    let mut alloc_dropped_errors = Vec::new();
    check_live_medians(alloc_dropped, Some(alloc_base), &mut alloc_dropped_errors);
    assert_eq!(
        alloc_dropped_errors,
        vec![
            "bench `html/render/50_lines` dropped max_blocks from the baseline",
            "bench `html/render/50_lines` dropped max_bytes from the baseline",
            "bench `html/render/50_lines` dropped max_bytes_x86_64 from the baseline",
        ]
    );
}

#[test]
fn an_empty_baselines_file_fails_when_the_live_file_has_records() {
    let live = "[[bench]]\nid = \"navigation/next_file\"\nref_median_ns = 999999\n";
    for baseline in ["", "# comment\n", "\n\n# comment only\n", "bench = []\n"] {
        let mut errors = Vec::new();
        check_live_medians(live, Some(baseline), &mut errors);
        assert_eq!(
            errors,
            vec!["baseline benches/baselines.toml has no records"],
            "{baseline:?} must not let ref_median_ns = 999999 through"
        );
    }

    let mut absent = Vec::new();
    check_live_medians(live, None, &mut absent);
    assert!(
        absent.is_empty(),
        "a missing baselines file is the caller's error, not an empty parse: {absent:?}"
    );

    let mut both_empty = Vec::new();
    check_live_medians("", Some("# comment\n"), &mut both_empty);
    assert!(both_empty.is_empty(), "an empty live file has no pins to protect: {both_empty:?}");

    let prior = "[[bench]]\nid = \"navigation/next_file\"\nref_median_ns = 10\n";
    let mut raised = Vec::new();
    check_live_medians(live, Some(prior), &mut raised);
    assert_eq!(
        raised,
        vec!["bench `navigation/next_file` ref_median_ns 999999 is looser than the baseline 10",],
        "a real prior record still ratchets: {raised:?}"
    );
}

#[test]
fn changing_floor_from_sum_to_max_or_dropping_a_bench_from_baseline_fails() {
    let baseline_sum = HAPPY.replace("floor = \"max\"", "floor = \"sum\"");
    let sum_to_max_errors = run(Case {
        manifest: HAPPY,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: Some(&baseline_sum),
        benches: &[("bench-a", 40_000)],
    });
    assert_eq!(
        sum_to_max_errors,
        vec!["operation `command:print` floor changed from `sum` to `max` against the baseline"],
        "changing floor from sum to max must fail: {sum_to_max_errors:?}"
    );

    let baseline_two_benches =
        HAPPY.replace("benches = [\"bench-a\"]", "benches = [\"bench-a\", \"bench-b\"]");
    let dropped_errors = run(Case {
        manifest: HAPPY,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: Some(&baseline_two_benches),
        benches: &[("bench-a", 40_000)],
    });
    assert_eq!(
        dropped_errors,
        vec!["operation `command:print` dropped bench `bench-b` from the baseline"],
        "dropping a bench from baseline without removed reason must fail: {dropped_errors:?}"
    );

    let documented_drop = format!("{HAPPY}removed = \"bench-b was folded into bench-a.\"\n");
    let documented_ok = run(Case {
        manifest: &documented_drop,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: Some(&baseline_two_benches),
        benches: &[("bench-a", 40_000)],
    });
    assert!(
        documented_ok.is_empty(),
        "dropping a bench with a one-line removed reason must pass: {documented_ok:?}"
    );

    let unused_removed = run(Case {
        manifest: &documented_drop,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: Some(HAPPY),
        benches: &[("bench-a", 40_000)],
    });
    assert_eq!(
        unused_removed,
        vec!["operation `command:print` sets removed without dropping a bench from the baseline"],
        "setting removed without dropping a bench from the baseline must fail: {unused_removed:?}"
    );
}

#[test]
fn every_measured_entry_requires_a_one_line_floor_reason_and_owed_entries_forbid_it() {
    let missing_reason =
        HAPPY.replace("floor_reason = \"Single benchmark bounds the operation.\"\n", "");
    let errors = run(happy_manifest(&missing_reason));
    assert_eq!(
        errors,
        vec!["operation `command:print` must set a one-line floor_reason for floor `max`"],
        "measured entry without floor_reason must fail: {errors:?}"
    );

    let owed_with_reason = format!(
        "{}\nowed = true\nfloor_reason = \"Not allowed on owed.\"\n",
        HAPPY
            .replace("benches = [\"bench-a\"]\n", "")
            .replace("floor = \"max\"\n", "")
            .replace("floor_reason = \"Single benchmark bounds the operation.\"\n", "")
    );
    let owed_errors = run(Case {
        manifest: &owed_with_reason,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[],
    });
    assert_eq!(
        owed_errors,
        vec!["operation `command:print` is owed but sets floor_reason"],
        "owed entry with floor_reason must fail: {owed_errors:?}"
    );
}

#[test]
fn a_non_owed_floor_at_its_class_cap_fails_while_an_owed_placeholder_passes() {
    let non_owed_at_cap = HAPPY
        .replace("wall_floor_us = 40000", "wall_floor_us = 50000")
        .replace("target_us = 40000", "target_us = 50000");
    let errors = run(Case {
        manifest: &non_owed_at_cap,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[("bench-a", 50_000)],
    });
    assert!(
        errors.iter().any(|error| error.contains("owed") && error.contains("50000")),
        "non-owed floor at class cap must fail: {errors:?}"
    );

    let non_owed_no_benches = HAPPY.replace("benches = [\"bench-a\"]\n", "");
    let errors = run(Case {
        manifest: &non_owed_no_benches,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[],
    });
    assert!(
        errors.iter().any(|error| error.contains("not owed") && error.contains("benches")),
        "non-owed operation without benches must fail: {errors:?}"
    );

    let owed_with_benches = format!("{HAPPY}owed = true\n");
    let errors = run(happy_manifest(&owed_with_benches));
    assert!(
        errors.iter().any(|error| error.contains("owed") && error.contains("benches")),
        "owed operation with benches must fail: {errors:?}"
    );

    let became_owed = format!("{}\nowed = true\n", HAPPY.replace("benches = [\"bench-a\"]\n", ""));
    let errors = run(Case {
        manifest: &became_owed,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: Some(HAPPY),
        benches: &[],
    });
    assert!(
        errors.iter().any(|error| error.contains("owed") && error.contains("baseline")),
        "measured operation becoming owed must fail: {errors:?}"
    );
}

#[test]
fn an_unknown_key_or_class_or_duplicate_id_fails() {
    let extra = format!("{HAPPY}note = \"no\"\n");
    let errors = run(happy_manifest(&extra));
    assert!(errors.iter().any(|error| error.contains("unknown key `note`")), "{errors:?}");

    let bad_class = HAPPY.replace("class = \"command\"", "class = \"widget\"");
    let errors = run(happy_manifest(&bad_class));
    assert!(errors.iter().any(|error| error.contains("unknown class `widget`")), "{errors:?}");

    let doubled = format!("{HAPPY}\n{HAPPY}");
    let errors = run(happy_manifest(&doubled));
    assert!(errors.iter().any(|error| error.contains("duplicate operation")), "{errors:?}");

    let doubled_key = HAPPY.replace("processes = 2\n", "processes = 2\nprocesses = 1\n");
    let errors = run(happy_manifest(&doubled_key));
    assert!(errors.iter().any(|error| error.contains("duplicate key `processes`")), "{errors:?}");
}

fn happy_manifest<'a>(manifest: &'a str) -> Case<'a> {
    Case {
        manifest,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[("bench-a", 40_000)],
    }
}

#[test]
fn recorded_benches_parse_ids_and_ceiling_medians() {
    let late = "[[bench]]\nref_median_ns = 5000\nid = \"late\"\n";
    let benches = recorded_benches(late).expect("field order must not matter");
    assert_eq!(benches.get("late"), Some(&5));

    let tight = "[[bench]]\nid=\"spaced\"\nref_median_ns=1500\n";
    let benches = recorded_benches(tight).expect("spaces around = are optional");
    assert_eq!(benches.get("spaced"), Some(&2));

    let commented = "[[bench]]\nid = \"c\" # name\nref_median_ns = 1000 # ns\n";
    let benches = recorded_benches(commented).expect("inline comments parse");
    assert_eq!(benches.get("c"), Some(&1));

    let unknown =
        "# id = \"nope\"\n[[bench]]\nidentity = \"nope\"\nid = \"real\"\nref_median_ns = 3513\n";
    let errors = recorded_benches(unknown).expect_err("unknown field must not be dropped");
    assert!(errors.iter().any(|err| err.contains("unknown field")), "{errors:?}");

    let duplicate = concat!(
        "[[bench]]\nid = \"a\"\nref_median_ns = 1000\n",
        "[[bench]]\nid = \"a\"\nref_median_ns = 2000\n",
    );
    let errors = recorded_benches(duplicate).expect_err("duplicate id");
    assert!(errors.iter().any(|err| err.contains("more than once")), "{errors:?}");
}

#[test]
fn test_names_come_only_from_test_attributed_functions() {
    let mut found = BTreeSet::new();
    push_test_names(
        "src/lib.rs",
        r##"
        fn helper() {}
        // #[test]
        // fn commented_test() {}
        /* #[test] fn block_commented() {} */
        let literal = "#[test] fn string_test() {}";
        #[cfg(test)]
        mod tests {
            fn inner_helper() {}
            #[test]
            fn alpha() {}
            #[tokio::test]
            #[ignore]
            async fn beta() {}
        }
        "##,
        &mut found,
    );
    assert_eq!(
        found.iter().map(String::as_str).collect::<Vec<_>>(),
        vec!["src/lib.rs::alpha", "src/lib.rs::beta"]
    );
}

fn owed_case(extra: &str) -> Vec<String> {
    let manifest = format!(
        "[[op]]\nid = \"command:print\"\nclass = \"command\"\nfixture = \"n\"\ntest = \"tests/counts.rs::counts\"\nprocesses = 1\nowed = true\nwall_floor_us = 1000\ntarget_us = 1000\nderivation = \"Owed.\"\n{extra}"
    );
    run(Case {
        manifest: &manifest,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[],
    })
}

#[test]
fn an_owed_entry_may_not_set_a_floor_and_a_measured_one_must_set_sum_or_max() {
    assert!(owed_case("").is_empty());
    assert_eq!(
        owed_case("floor = \"max\"\n"),
        vec!["operation `command:print` is owed but sets floor `max`"]
    );
    let no_floor = HAPPY.replace("floor = \"max\"\n", "");
    assert_eq!(
        run(happy_manifest(&no_floor)),
        vec!["operation `command:print` must set floor = \"sum\" or \"max\" (got ``)"]
    );
    let odd_floor = HAPPY.replace("floor = \"max\"", "floor = \"min\"");
    assert!(
        run(happy_manifest(&odd_floor))
            .iter()
            .any(|error| error.contains("must set floor = \"sum\" or \"max\" (got `min`)"))
    );
}

#[test]
fn a_bench_that_is_removed_may_not_keep_any_pin() {
    for pin in ["ref_median_ns = 5", "max_blocks = 5", "max_bytes = 5", "max_bytes_x86_64 = 5"] {
        for text in [
            format!("[[bench]]\nid = \"x\"\n{pin}\nremoved = \"reason\"\n"),
            format!("[[bench]]\nid = \"x\"\nremoved = \"reason\"\n{pin}\n"),
        ] {
            let mut errors = Vec::new();
            check_live_medians(&text, None, &mut errors);
            assert!(
                errors.iter().any(|error| error.contains("bench `x` sets both")),
                "`{pin}` beside removed must fail: {errors:?}"
            );
        }
    }
    let mut errors = Vec::new();
    check_live_medians("[[bench]]\nid = \"x\"\nremoved = \"reason\"\n", None, &mut errors);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn an_operation_absent_from_the_baseline_may_not_set_removed() {
    let live = format!("{HAPPY}removed = \"dropped it\"\n");
    let baseline = "";
    let errors = run(Case {
        manifest: &live,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: Some(baseline),
        benches: &[("bench-a", 40_000)],
    });
    assert!(
        errors.iter().any(|error| error.contains("command:print")
            && error.contains("sets removed without dropping a bench")),
        "{errors:?}"
    );
}

fn max_case(benches: &[(&str, u64)], alternatives: &str, floor: u64) -> Vec<String> {
    let names: Vec<String> = benches.iter().map(|(id, _)| format!("\"{id}\"")).collect();
    let manifest = HAPPY
        .replace("benches = [\"bench-a\"]", &format!("benches = [{}]", names.join(", ")))
        .replace("wall_floor_us = 40000", &format!("wall_floor_us = {floor}"))
        .replace("target_us = 40000", &format!("target_us = {floor}"))
        + alternatives;
    run(Case {
        manifest: &manifest,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches,
    })
}

#[test]
fn a_max_floor_keeps_every_sequential_stage() {
    // A smaller bench still runs. No percentage drops it.
    let dropped = max_case(&[("bench-a", 40_000), ("bench-b", 4_000)], "", 40_000);
    assert!(
        dropped
            .iter()
            .any(|error| error.contains("bench-b") && error.contains("use floor = \"sum\"")),
        "{dropped:?}"
    );
    let also_dropped = max_case(&[("bench-a", 40_000), ("bench-b", 4_001)], "", 40_000);
    assert!(
        also_dropped.iter().any(|error| error.contains("use floor = \"sum\"")),
        "{also_dropped:?}"
    );
    let both = [("bench-a", 40_000), ("bench-b", 30_000)];
    assert!(max_case(&both, "alternatives = [\"bench-b\"]\n", 40_000).is_empty());
    let stray = max_case(&both, "alternatives = [\"bench-z\"]\n", 40_000);
    assert!(stray.iter().any(|error| error.contains("`bench-z` is not one of its benches")));
    let dominant = max_case(&both, "alternatives = [\"bench-a\"]\n", 40_000);
    assert!(dominant.iter().any(|error| error.contains("cannot")), "{dominant:?}");
}

#[test]
fn alternatives_replace_a_stage_and_cannot_grow_against_a_measured_baseline() {
    let odd =
        HAPPY.replace("floor = \"max\"", "floor = \"min\"") + "alternatives = [\"bench-a\"]\n";
    let errors = run(happy_manifest(&odd));
    assert!(errors.iter().any(|error| error.contains("alternatives without floor")), "{errors:?}");

    let only =
        HAPPY.replace("floor = \"max\"", "floor = \"sum\"") + "alternatives = [\"bench-a\"]\n";
    let errors = run(happy_manifest(&only));
    assert!(
        errors.iter().any(|error| error.contains("only alternatives") || error.contains("cannot")),
        "{errors:?}"
    );

    let two = HAPPY.replace("benches = [\"bench-a\"]", "benches = [\"bench-a\", \"bench-b\"]");
    let live = format!("{two}alternatives = [\"bench-b\"]\n");
    let errors = run(Case {
        manifest: &live,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: Some(&two),
        benches: &[("bench-a", 40_000), ("bench-b", 30_000)],
    });
    assert!(
        errors.is_empty(),
        "a bench the baseline already measured can be named as an alternative when the floor stays the formula: {errors:?}"
    );
    let errors = run(Case {
        manifest: &live,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: Some(&live),
        benches: &[("bench-a", 40_000), ("bench-b", 30_000)],
    });
    assert!(errors.is_empty(), "{errors:?}");

    let summed = two
        .replace("floor = \"max\"", "floor = \"sum\"")
        .replace("wall_floor_us = 40000", "wall_floor_us = 70000")
        .replace("target_us = 40000", "target_us = 70000");
    let hidden = format!(
        "{}alternatives = [\"bench-b\"]\n",
        summed
            .replace("wall_floor_us = 70000", "wall_floor_us = 40000")
            .replace("target_us = 70000", "target_us = 40000")
    );
    let errors = run(Case {
        manifest: &hidden,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: Some(&summed),
        benches: &[("bench-a", 40_000), ("bench-b", 30_000)],
    });
    assert!(
        errors.iter().any(|error| error.contains("adds alternative `bench-b`")),
        "hiding a stage that shrinks the floor must fail: {errors:?}"
    );
}

#[test]
fn paying_off_an_owed_op_may_introduce_alternatives() {
    let owed = r#"
[[op]]
id = "command:print"
class = "command"
fixture = "one file"
test = "tests/counts.rs::counts"
processes = 2
owed = true
wall_floor_us = 40000
target_us = 40000
derivation = "Owed until measured."
"#;
    let paid = r#"
[[op]]
id = "command:print"
class = "command"
fixture = "one file"
test = "tests/counts.rs::counts"
processes = 2
benches = ["op/1k", "op/10k"]
floor = "max"
alternatives = ["op/1k"]
floor_reason = "The 1k fixture replaces the 10k fixture."
wall_floor_us = 40000
target_us = 40000
derivation = "Measured."
"#;
    let errors = run(Case {
        manifest: paid,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: Some(owed),
        benches: &[("op/1k", 10_000), ("op/10k", 40_000)],
    });
    assert!(errors.is_empty(), "{errors:?}");

    let measured = paid
        .replace("benches = [\"op/1k\", \"op/10k\"]\n", "benches = [\"op/10k\"]\n")
        .replace("alternatives = [\"op/1k\"]\n", "");
    let errors = run(Case {
        manifest: paid,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
        baseline: Some(&measured),
        benches: &[("op/1k", 10_000), ("op/10k", 40_000)],
    });
    assert!(
        errors.iter().any(|error| error.contains("adds alternative `op/1k`")),
        "a measured baseline that did not name the bench must still reject the alternative: {errors:?}"
    );
}

#[test]
fn a_single_bench_cannot_be_its_own_alternative() {
    let errors = max_case(&[("bench-a", 40_000)], "alternatives = [\"bench-a\"]\n", 40_000);
    assert!(errors.iter().any(|error| error.contains("cannot")), "{errors:?}");
}

#[test]
fn tied_dominant_benches_fail_whichever_side_is_the_alternative() {
    let tied = [("bench-a", 40_000), ("bench-b", 40_000)];
    for alternative in ["bench-a", "bench-b"] {
        let errors = max_case(&tied, &format!("alternatives = [\"{alternative}\"]\n"), 40_000);
        assert!(
            errors.iter().any(|error| error.contains("ties")),
            "alternative `{alternative}` must fail: {errors:?}"
        );
    }
}

#[test]
fn duplicate_alternatives_are_rejected() {
    let errors = max_case(
        &[("bench-a", 40_000), ("bench-b", 30_000)],
        "alternatives = [\"bench-b\", \"bench-b\"]\n",
        40_000,
    );
    assert!(errors.iter().any(|error| error.contains("more than once")), "{errors:?}");
}

#[test]
fn a_sum_floor_adds_stages_and_rejects_a_larger_hidden_fixture() {
    let summed = r#"
[[op]]
id = "command:files"
class = "command"
fixture = "500 files"
test = "tests/counts.rs::counts"
processes = 5
benches = ["bench-a", "bench-b", "bench-c"]
floor = "sum"
alternatives = ["bench-c"]
floor_reason = "bench-a and bench-b run sequentially; bench-c replaces bench-b."
stage_floors = ["bench-a=30000", "bench-b=10000"]
wall_floor_us = 40000
target_us = 40000
derivation = "Floor is bench-a (30000 us) plus bench-b (10000 us)."
"#;
    let ok = run(Case {
        manifest: summed,
        surface: &["command:files"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[("bench-a", 30_000), ("bench-b", 10_000), ("bench-c", 1_000)],
    });
    assert!(ok.is_empty(), "{ok:?}");

    let dropped_small = summed.replace("wall_floor_us = 40000", "wall_floor_us = 30000");
    let errors = run(Case {
        manifest: &dropped_small,
        surface: &["command:files"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[("bench-a", 30_000), ("bench-b", 10_000), ("bench-c", 1_000)],
    });
    assert!(
        errors.iter().any(|error| error.contains("does not match") && error.contains("40000")),
        "{errors:?}"
    );

    let smaller_than_one = run(Case {
        manifest: summed,
        surface: &["command:files"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[("bench-a", 30_000), ("bench-b", 10_000), ("bench-c", 20_000)],
    });
    assert!(
        smaller_than_one.is_empty(),
        "a fixture smaller than one stage replaces that stage: {smaller_than_one:?}"
    );

    let tied_to_smallest = run(Case {
        manifest: summed,
        surface: &["command:files"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[("bench-a", 30_000), ("bench-b", 10_000), ("bench-c", 10_000)],
    });
    assert!(
        tied_to_smallest.is_empty(),
        "equal to the smallest stage and smaller than another is still an alternative: {tied_to_smallest:?}"
    );

    let covers_every_stage = run(Case {
        manifest: summed,
        surface: &["command:files"],
        tests: &["tests/counts.rs::counts"],
        baseline: None,
        benches: &[("bench-a", 30_000), ("bench-b", 10_000), ("bench-c", 30_000)],
    });
    assert!(
        covers_every_stage
            .iter()
            .any(|error| error.contains("bench-c") && error.contains("not smaller")),
        "{covers_every_stage:?}"
    );
}

#[test]
fn a_ci_median_pin_ratchets_down_only_and_cannot_be_dropped() {
    let base =
        "[[bench]]\nid = \"html/render/50_lines\"\nref_median_ns = 1000\nci_median_ns = 4000\n";
    let with_ci =
        |ci: &str| format!("[[bench]]\nid = \"html/render/50_lines\"\nref_median_ns = 1000\n{ci}");
    let check = |live: &str| {
        let mut errors = Vec::new();
        check_live_medians(live, Some(base), &mut errors);
        errors
    };
    assert_eq!(
        check(&with_ci("ci_median_ns = 4001\n")),
        vec!["bench `html/render/50_lines` ci_median_ns 4001 is looser than the baseline 4000"]
    );
    assert_eq!(
        check(&with_ci("")),
        vec!["bench `html/render/50_lines` dropped ci_median_ns from the baseline"]
    );
    assert!(check(&with_ci("ci_median_ns = 3000\n")).is_empty());
    assert!(check(&with_ci("ci_median_ns = 4000\n")).is_empty());
    let first_record = "[[bench]]\nid = \"html/render/50_lines\"\nref_median_ns = 1000\n";
    let mut adopted = Vec::new();
    check_live_medians(&with_ci("ci_median_ns = 4000\n"), Some(first_record), &mut adopted);
    assert!(adopted.is_empty(), "a first ci pin has no earlier value to exceed: {adopted:?}");
    let ci_alone = "[[bench]]\nid = \"x\"\nci_median_ns = 5\n";
    assert!(recorded_benches(ci_alone).is_err());
}

fn first_ci_pin(id: &str, ref_ns: u64, ci_ns: u64) -> Vec<String> {
    let before = format!("[[bench]]\nid = \"{id}\"\nref_median_ns = {ref_ns}\n");
    let live = format!("{before}ci_median_ns = {ci_ns}\n");
    let mut errors = Vec::new();
    check_live_medians(&live, Some(&before), &mut errors);
    errors
}

#[test]
fn a_first_ci_pin_needs_an_allowlisted_bench_within_the_cap() {
    assert!(first_ci_pin("html/render/200_lines", 1000, 6000).is_empty());
    assert_eq!(
        first_ci_pin("html/render/200_lines", 1000, 6001),
        vec!["bench `html/render/200_lines` ci_median_ns 6001 is over 6x its ref_median_ns 1000"]
    );
    assert_eq!(
        first_ci_pin("html/render/200_lines", 1000, 100_000_000_000),
        vec![
            "bench `html/render/200_lines` ci_median_ns 100000000000 is over 6x its ref_median_ns 1000"
        ]
    );
    assert_eq!(
        first_ci_pin("refresh/other/500f_25c", 1000, 2000),
        vec!["bench `refresh/other/500f_25c` has a ci_median_ns but is not in CI_PINNED_BENCHES"]
    );
}

#[test]
fn the_cap_and_allowlist_hold_without_a_baseline_revision() {
    let live = "[[bench]]\nid = \"nope\"\nref_median_ns = 10\nci_median_ns = 1000\n";
    let mut errors = Vec::new();
    check_live_medians(live, None, &mut errors);
    assert_eq!(errors.len(), 2, "{errors:?}");
}

    fn run_cmd(dir: &Path, program: &str, args: &[&str]) {
        let out = Command::new(program)
            .args(args)
            .current_dir(dir)
            .env("JJ_USER", "Test")
            .env("JJ_EMAIL", "test@example.com")
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("spawn command");
        assert!(
            out.status.success(),
            "{program} {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn git_head(dir: &Path) -> String {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .expect("git rev-parse HEAD");
        assert!(out.status.success());
        String::from_utf8(out.stdout).expect("utf8").trim().to_string()
    }

    #[test]
    #[should_panic(expected = "resolved")]
    fn committed_baseline_panics_when_revision_does_not_resolve() {
        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "jj", &["git", "init", "--colocate"]);
        let _ = committed_baseline_in(tmp.path(), None);
    }

    #[test]
    #[should_panic(expected = "all-zero")]
    fn committed_baseline_jj_panics_on_all_zero_sha_when_manifest_exists_in_parent() {
        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "jj", &["git", "init", "--colocate"]);
        fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        fs::write(tmp.path().join("tests/perf.toml"), "# first\n").expect("write perf.toml");
        run_cmd(tmp.path(), "jj", &["commit", "-m", "add perf"]);
        let zero = "0000000000000000000000000000000000000000";
        assert_eq!(committed_baseline_in(tmp.path(), Some(zero)), Ok(None));

        fs::write(tmp.path().join("tests/perf.toml"), "# second\n").expect("write second");
        run_cmd(tmp.path(), "jj", &["commit", "-m", "update perf"]);
        let _ = committed_baseline_in(tmp.path(), Some(zero));
    }

    #[test]
    fn committed_baseline_fails_when_a_resolved_revision_lacks_the_file_and_reads_git_checkout() {
        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "git", &["init", "-q", "-b", "main"]);
        fs::write(tmp.path().join("README.md"), "hello\n").expect("write readme");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "init"]);
        assert_eq!(
            committed_baseline_in(tmp.path(), None),
            Ok(None),
            "a revision that predates the manifest bootstraps"
        );

        fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        fs::write(tmp.path().join("tests/perf.toml"), "# added\n").expect("write perf.toml");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "add perf"]);
        fs::remove_file(tmp.path().join("tests/perf.toml")).expect("delete perf.toml");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "delete perf"]);
        let deleted = committed_baseline_in(tmp.path(), None);
        let mut errors = Vec::new();
        assert!(recorded_baseline(deleted, &mut errors).is_none());
        assert!(
            errors.iter().any(|err| err.contains("missing") && err.contains("tests/perf.toml")),
            "deleting the manifest must still fail: {errors:?}"
        );

        fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        fs::write(tmp.path().join("tests/perf.toml"), "# baseline\n").expect("write perf.toml");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "add perf"]);
        assert_eq!(
            committed_baseline_in(tmp.path(), None).expect("baseline").as_deref(),
            Some("# baseline\n")
        );
    }

    #[test]
    fn a_detached_head_off_main_baselines_at_the_merge_base_not_at_main_tip() {
        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "git", &["init", "-q", "-b", "main"]);
        fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        fs::write(tmp.path().join("tests/perf.toml"), "# tight\n").expect("write tight");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "tight"]);
        let tight = git_head(tmp.path());

        // The feature commit changes the file, so HEAD, the merge base, and
        // main's tip are three different texts. Only the merge base is `# tight`.
        run_cmd(tmp.path(), "git", &["checkout", "-q", "-b", "feature"]);
        fs::write(tmp.path().join("tests/perf.toml"), "# feature\n").expect("write feature");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "feature"]);
        let feature = git_head(tmp.path());

        run_cmd(tmp.path(), "git", &["checkout", "-q", "main"]);
        fs::write(tmp.path().join("tests/perf.toml"), "# loose\n").expect("write loose");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "loose"]);
        let loose = git_head(tmp.path());
        run_cmd(tmp.path(), "git", &["update-ref", "refs/remotes/origin/main", &loose]);
        assert_ne!(tight, loose);
        assert_ne!(feature, loose);

        run_cmd(tmp.path(), "git", &["checkout", "-q", "--detach", &feature]);
        let symbolic = Command::new("git")
            .args(["symbolic-ref", "--short", "-q", "HEAD"])
            .current_dir(tmp.path())
            .output()
            .expect("git symbolic-ref");
        assert!(!symbolic.status.success(), "the checkout must be detached");
        assert_eq!(
            committed_baseline_in(tmp.path(), None).expect("baseline").as_deref(),
            Some("# tight\n"),
            "a detached feature commit baselines at the merge base, not main's tip"
        );
        assert_eq!(
            committed_baseline_file_in(
                tmp.path(),
                None,
                Some("refs/heads/main"),
                "tests/perf.toml",
            )
            .expect("baseline")
            .as_deref(),
            Some("# loose\n"),
            "GITHUB_REF names main even when HEAD is detached on a feature commit"
        );

        run_cmd(tmp.path(), "git", &["checkout", "-q", "--detach", &loose]);
        assert_eq!(
            committed_baseline_in(tmp.path(), None).expect("baseline").as_deref(),
            Some("# loose\n"),
            "detached at main's own commit still baselines at main's tip"
        );
    }

    #[test]
    fn non_main_branch_push_and_force_push_resolve_baseline_via_origin_main_merge_base() {
        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "git", &["init", "-q", "-b", "main"]);
        fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        fs::write(tmp.path().join("tests/perf.toml"), "# origin-main-baseline\n")
            .expect("write main perf.toml");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "main baseline"]);
        let main_sha = git_head(tmp.path());
        run_cmd(tmp.path(), "git", &["update-ref", "refs/remotes/origin/main", &main_sha]);

        run_cmd(tmp.path(), "git", &["checkout", "-q", "-b", "feature"]);
        fs::write(tmp.path().join("tests/perf.toml"), "# feature-commit-1\n")
            .expect("write feature 1");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "feature commit 1"]);
        let feature_1_sha = git_head(tmp.path());

        fs::write(tmp.path().join("tests/perf.toml"), "# feature-commit-2\n")
            .expect("write feature 2");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "feature commit 2"]);

        let zero = "0000000000000000000000000000000000000000";
        let unreachable_sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

        // New-branch push: before is all-zeros while HEAD~1 already has tests/perf.toml.
        assert_eq!(
            committed_baseline_file_in(
                tmp.path(),
                Some(zero),
                Some("refs/heads/feature"),
                "tests/perf.toml"
            )
            .expect("baseline")
            .as_deref(),
            Some("# origin-main-baseline\n"),
            "new-branch push must resolve baseline via merge-base with origin/main"
        );

        // Force-push: old before SHA is unreachable.
        assert_eq!(
            committed_baseline_file_in(
                tmp.path(),
                Some(unreachable_sha),
                Some("refs/heads/feature"),
                "tests/perf.toml"
            )
            .expect("baseline")
            .as_deref(),
            Some("# origin-main-baseline\n"),
            "force-push with unreachable before SHA must resolve baseline via merge-base with origin/main"
        );

        // Incremental push to feature branch: still baselines against merge-base with origin/main.
        assert_eq!(
            committed_baseline_file_in(
                tmp.path(),
                Some(&feature_1_sha),
                Some("refs/heads/feature"),
                "tests/perf.toml"
            )
            .expect("baseline")
            .as_deref(),
            Some("# origin-main-baseline\n"),
            "feature branch push must baseline against merge-base with origin/main"
        );

        // PR with reachable base.sha uses base.sha directly; unreachable base.sha falls back to merge-base.
        assert_eq!(
            committed_baseline_file_in(
                tmp.path(),
                Some(&feature_1_sha),
                Some("refs/pull/1/merge"),
                "tests/perf.toml"
            )
            .expect("baseline")
            .as_deref(),
            Some("# feature-commit-1\n"),
            "PR with reachable base.sha must use that revision"
        );
        assert_eq!(
            committed_baseline_file_in(
                tmp.path(),
                Some(unreachable_sha),
                Some("refs/pull/1/merge"),
                "tests/perf.toml"
            )
            .expect("baseline")
            .as_deref(),
            Some("# origin-main-baseline\n"),
            "PR with unreachable base.sha must fall back to merge-base with origin/main"
        );
    }

    #[test]
    fn jj_criss_cross_merge_with_multiple_merge_base_heads_picks_one_deterministically() {
        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "jj", &["git", "init", "--colocate"]);
        fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        fs::write(tmp.path().join("tests/perf.toml"), "# root\n").expect("write root");
        run_cmd(tmp.path(), "jj", &["commit", "-m", "root"]);

        // Two diverging commits A and B above root, both with the same baseline file.
        run_cmd(tmp.path(), "jj", &["new", "@-", "-m", "A"]);
        fs::write(tmp.path().join("a.txt"), "a\n").expect("write a");
        fs::write(tmp.path().join("tests/perf.toml"), "# criss-cross-base\n")
            .expect("write perf A");
        run_cmd(tmp.path(), "jj", &["bookmark", "create", "branch-a", "-r", "@"]);

        run_cmd(tmp.path(), "jj", &["new", "description(substring:\"root\")", "-m", "B"]);
        fs::write(tmp.path().join("b.txt"), "b\n").expect("write b");
        fs::write(tmp.path().join("tests/perf.toml"), "# criss-cross-base\n")
            .expect("write perf B");
        run_cmd(tmp.path(), "jj", &["bookmark", "create", "branch-b", "-r", "@"]);

        // Merge 1 (`main` bookmark): parents branch-a and branch-b.
        run_cmd(tmp.path(), "jj", &["new", "branch-a", "branch-b", "-m", "main merge"]);
        fs::write(tmp.path().join("m1.txt"), "m1\n").expect("write m1");
        run_cmd(tmp.path(), "jj", &["bookmark", "create", "main", "-r", "@"]);

        // Merge 2 (`@` feature): also parents branch-a and branch-b -> criss-cross!
        run_cmd(tmp.path(), "jj", &["new", "branch-a", "branch-b", "-m", "feature merge"]);
        fs::write(tmp.path().join("m2.txt"), "m2\n").expect("write m2");

        // Verify `heads(::@ & ::main)` returns 2 revisions (branch-a and branch-b).
        let heads_out = Command::new("jj")
            .current_dir(tmp.path())
            .args([
                "--ignore-working-copy", "log", "--no-graph", "-r", "heads(::@ & ::main)", "-T",
                "commit_id ++ \"\\n\"",
            ])
            .output()
            .expect("jj log heads");
        let heads_stdout = String::from_utf8_lossy(&heads_out.stdout);
        let heads: Vec<_> = heads_stdout.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(heads.len(), 2, "criss-cross merge must produce 2 heads: {heads:?}");

        // committed_baseline_file_in must deterministically resolve one head without failing `jj file list`.
        let resolved = committed_baseline_file_in(
            tmp.path(),
            Some("0000000000000000000000000000000000000000"),
            Some("refs/heads/feature"),
            "tests/perf.toml",
        );
        assert_eq!(resolved.expect("baseline").as_deref(), Some("# criss-cross-base\n"));
    }

    #[test]
    #[should_panic(expected = "did not resolve")]
    fn main_branch_push_with_unreachable_before_sha_panics_loudly() {
        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "git", &["init", "-q", "-b", "main"]);
        fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        fs::write(tmp.path().join("tests/perf.toml"), "# main\n").expect("write perf.toml");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "main"]);
        let _ = committed_baseline_file_in(
            tmp.path(),
            Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
            Some("refs/heads/main"),
            "tests/perf.toml",
        );
    }

    fn jj_commit_id(dir: &Path, rev: &str) -> String {
        let out = Command::new("jj")
            .current_dir(dir)
            .args(["--ignore-working-copy", "log", "--no-graph", "-r", rev, "-T", "commit_id"])
            .output()
            .expect("jj log");
        assert!(out.status.success(), "jj log {rev}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).expect("utf8").trim().to_string()
    }

    /// A jj repo where `main` has moved on since a feature branch forked:
    /// fork point R ("# fork"), `main` at M ("# main-tip"), feature F
    /// ("# feature") and `@` above F. Returns F's commit id.
    fn jj_repo_with_diverged_main(dir: &Path) -> String {
        run_cmd(dir, "jj", &["git", "init", "--colocate"]);
        fs::create_dir_all(dir.join("tests")).expect("mkdir tests");
        fs::write(dir.join("tests/perf.toml"), "# fork\n").expect("write fork");
        run_cmd(dir, "jj", &["commit", "-m", "fork"]);
        run_cmd(dir, "jj", &["new", "@-", "-m", "main tip"]);
        fs::write(dir.join("tests/perf.toml"), "# main-tip\n").expect("write main tip");
        run_cmd(dir, "jj", &["bookmark", "create", "main", "-r", "@"]);
        run_cmd(dir, "jj", &["new", "description(exact:\"fork\n\")", "-m", "feature"]);
        fs::write(dir.join("tests/perf.toml"), "# feature\n").expect("write feature");
        run_cmd(dir, "jj", &["bookmark", "create", "feature", "-r", "@"]);
        let feature = jj_commit_id(dir, "feature");
        run_cmd(dir, "jj", &["new", "feature", "-m", "work"]);
        feature
    }

    #[test]
    fn jj_non_main_ref_without_an_override_baselines_against_the_fork_point() {
        let tmp = TempDir::new().expect("tempdir");
        jj_repo_with_diverged_main(tmp.path());
        let resolved = committed_baseline_file_in(
            tmp.path(),
            None,
            Some("refs/heads/feature"),
            "tests/perf.toml",
        );
        assert_eq!(resolved.expect("baseline").as_deref(), Some("# fork\n"), "not main's tip");
        let on_main = committed_baseline_file_in(
            tmp.path(),
            None,
            Some("refs/heads/main"),
            "tests/perf.toml",
        );
        assert_eq!(on_main.expect("baseline").as_deref(), Some("# main-tip\n"));
        assert_eq!(
            committed_baseline_in(tmp.path(), None).expect("baseline").as_deref(),
            Some("# fork\n"),
            "GITHUB_REF unset and @ off main baselines at the fork, not main's tip"
        );

        // Moving local `main` forward to `@` before pushing still baselines against `main@origin`.
        let remote = TempDir::new().expect("remote");
        run_cmd(remote.path(), "git", &["init", "-q", "--bare"]);
        let remote_url = remote.path().to_str().expect("remote path");
        run_cmd(tmp.path(), "jj", &["git", "remote", "add", "origin", remote_url]);
        run_cmd(tmp.path(), "jj", &["git", "push", "--bookmark", "main"]);
        run_cmd(tmp.path(), "jj", &["new", "main", "-m", "next on main"]);
        fs::write(tmp.path().join("tests/perf.toml"), "# moved-local-main\n")
            .expect("write moved main");
        run_cmd(tmp.path(), "jj", &["bookmark", "set", "main", "-r", "@"]);
        assert_eq!(
            committed_baseline_in(tmp.path(), None).expect("baseline").as_deref(),
            Some("# main-tip\n"),
            "moving local main bookmark must still baseline against main@origin"
        );
    }

    #[test]
    fn jj_pull_request_baselines_against_its_base_sha_and_a_push_against_the_fork_point() {
        let tmp = TempDir::new().expect("tempdir");
        let feature = jj_repo_with_diverged_main(tmp.path());
        let pull = committed_baseline_file_in(
            tmp.path(),
            Some(&feature),
            Some("refs/pull/7/merge"),
            "tests/perf.toml",
        );
        assert_eq!(
            pull.expect("baseline").as_deref(),
            Some("# feature\n"),
            "a PR uses its base.sha"
        );
        let zero = "0000000000000000000000000000000000000000";
        let new_pull = committed_baseline_file_in(
            tmp.path(),
            Some(zero),
            Some("refs/pull/7/merge"),
            "tests/perf.toml",
        );
        assert_eq!(
            new_pull.expect("baseline").as_deref(),
            Some("# fork\n"),
            "an all-zero base is not a revision"
        );
        let push = committed_baseline_file_in(
            tmp.path(),
            Some(&feature),
            Some("refs/heads/feature"),
            "tests/perf.toml",
        );
        assert_eq!(push.expect("baseline").as_deref(), Some("# fork\n"), "a push is not a PR");
    }

    fn budget_op(id: &str, processes: u64, wall: u64, target: u64) -> String {
        format!(
            r#"
[[op]]
id = "{id}"
class = "command"
fixture = "one file"
test = "tests/counts.rs::counts"
processes = {processes}
wall_floor_us = {wall}
target_us = {target}
derivation = "Pinned."
"#
        )
    }

    #[test]
    fn strictness_compares_shared_ceilings_and_ignores_unquoted_comments() {
        let plain = budget_op("command:print", 1, 500_000, 500_000);
        let commented = format!("{plain}# wall_floor_us = 1\n");
        assert_eq!(operation_ceilings(&plain), operation_ceilings(&commented));
        assert!(operation_ceilings("wall = 10\n").is_empty());
        assert!(operation_ceilings("# nothing\n").is_empty());
        let quoted = r#"
[[op]]
id = "command:print"
class = "command"
fixture = "one file"
test = "tests/counts.rs::counts"
processes = 1
wall_floor_us = 3
target_us = 3
derivation = "hash # stays"
"#;
        assert_eq!(operation_ceilings(quoted)["command:print"].wall_floor_us, 3);

        let low_wall = operation_ceilings(&budget_op("command:print", 1, 1, 1));
        let high_wall_and_extra = operation_ceilings(&format!(
            "{}{}",
            budget_op("command:print", 1, 500_000, 500_000),
            budget_op("command:extra", 2, 1, 1)
        ));
        assert!(is_stricter(&low_wall, &high_wall_and_extra));
        assert!(!is_stricter(&high_wall_and_extra, &low_wall));

        let same = operation_ceilings(&budget_op("command:print", 1, 10, 10));
        let same_plus_extra = operation_ceilings(&format!(
            "{}{}",
            budget_op("command:print", 1, 10, 10),
            budget_op("command:extra", 1, 1, 1)
        ));
        assert!(!is_stricter(&same_plus_extra, &same));
        assert!(!is_stricter(&same, &same_plus_extra));

        let missing = operation_ceilings(&budget_op("command:other", 1, 1, 1));
        assert!(!is_stricter(&missing, &low_wall));
        assert!(!is_stricter(&low_wall, &missing));

        let low_processes = operation_ceilings(&budget_op("command:print", 1, 500_000, 1));
        let low_wall_high_processes = operation_ceilings(&budget_op("command:print", 9, 1, 1));
        assert!(!is_stricter(&low_processes, &low_wall_high_processes));
        assert!(!is_stricter(&low_wall_high_processes, &low_processes));
        assert!(!is_stricter(&low_wall, &low_wall));
    }

    /// Two merges over the same pair of branches (a criss-cross) leave two
    /// merge-base heads holding different baselines. Commit ids are random, so
    /// the tight copy is put on each side in turn: one of the two orders has the
    /// loose head sorting first.
    fn resolved_criss_cross(a: &str, b: &str) -> Result<Option<String>, String> {
        draw_criss_cross(a, b).baseline
    }

    struct DrawnCrissCross {
        baseline: Result<Option<String>, String>,
        first_commit: String,
        second_commit: String,
    }

    fn draw_criss_cross(a: &str, b: &str) -> DrawnCrissCross {
        let root = budget_op("command:root", 1, 50, 50);
        draw_criss_cross_at("tests/perf.toml", &root, a, b)
    }

    /// Both merges resolve `rel_path` back to `root`, so the file differs only
    /// on the two merge-base heads.
    fn draw_criss_cross_at(rel_path: &str, root: &str, a: &str, b: &str) -> DrawnCrissCross {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path();
        let file = dir.join(rel_path);
        run_cmd(dir, "jj", &["git", "init", "--colocate"]);
        fs::create_dir_all(file.parent().expect("file has a parent")).expect("mkdir parent");
        fs::write(&file, root).expect("write root");
        run_cmd(dir, "jj", &["commit", "-m", "root"]);
        run_cmd(dir, "jj", &["new", "@-", "-m", "A"]);
        fs::write(&file, a).expect("write a");
        run_cmd(dir, "jj", &["bookmark", "create", "branch-a", "-r", "@"]);
        let first_commit = jj_commit_id(dir, "branch-a");
        run_cmd(dir, "jj", &["new", "description(substring:\"root\")", "-m", "B"]);
        fs::write(&file, b).expect("write b");
        run_cmd(dir, "jj", &["bookmark", "create", "branch-b", "-r", "@"]);
        let second_commit = jj_commit_id(dir, "branch-b");
        run_cmd(dir, "jj", &["new", "branch-a", "branch-b", "-m", "main merge"]);
        fs::write(&file, root).expect("resolve main");
        run_cmd(dir, "jj", &["bookmark", "create", "main", "-r", "@"]);
        run_cmd(dir, "jj", &["new", "branch-a", "branch-b", "-m", "feature merge"]);
        fs::write(&file, root).expect("resolve feature");
        let baseline = committed_baseline_file_in(
            dir,
            Some("0000000000000000000000000000000000000000"),
            Some("refs/heads/feature"),
            rel_path,
        );
        DrawnCrissCross { baseline, first_commit, second_commit }
    }

    const LOOSE_BENCHES: &str = "[[bench]]\nid = \"a\"\nmax_blocks = 10\nmax_bytes = 1000\nref_median_ns = 100\n\n[[bench]]\nid = \"b\"\nref_median_ns = 5\n\n[[bench]]\nid = \"c\"\nremoved = \"gone on this head\"\n";
    const TIGHT_BENCHES: &str = "[[bench]]\nid = \"a\"\nmax_blocks = 20\nmax_bytes = 500\nref_median_ns = 50\n\n[[bench]]\nid = \"c\"\nref_median_ns = 7\n";

    fn bench_records(text: &str) -> BTreeMap<String, baselines::BenchRecord> {
        baselines::parse(text)
            .unwrap_or_else(|errors| panic!("{}: {text}", errors.join("; ")))
            .into_iter()
            .map(|record| (record.id.clone(), record))
            .collect()
    }

    fn assert_minimum_pins(text: &str) {
        let records = bench_records(text);
        let a = &records["a"];
        assert_eq!(
            (a.max_blocks, a.max_bytes, a.ref_median_ns),
            (Some(10), Some(500), Some(50)),
            "{text}"
        );
        assert_eq!(records["b"].ref_median_ns, Some(5), "{text}");
        assert_eq!(records["c"].ref_median_ns, Some(7), "{text}");
        assert!(records["c"].removed.is_none(), "a pinned head beats a removed one: {text}");
        let mut errors = Vec::new();
        check_live_medians(LOOSE_BENCHES, Some(text), &mut errors);
        assert!(
            errors.iter().any(|error| error.contains("`a`")
                && error.contains("ref_median_ns")
                && error.contains("looser")),
            "the loose head's pin must fail against the merge: {errors:?}"
        );
    }

    /// `benches/baselines.toml` has no operations, so the manifest's strictness
    /// rules cannot rank its heads. Each pin keeps the minimum across the heads
    /// that record it, whichever commit id sorts first.
    #[test]
    fn a_baselines_criss_cross_keeps_the_minimum_of_each_pin() {
        for (left, right) in [(LOOSE_BENCHES, TIGHT_BENCHES), (TIGHT_BENCHES, LOOSE_BENCHES)] {
            assert_minimum_pins(&choose_jj_baseline(vec![left.to_string(), right.to_string()]));
        }
        assert_eq!(choose_jj_baseline(vec![LOOSE_BENCHES.to_string()]), LOOSE_BENCHES);
        assert_eq!(
            choose_jj_baseline(vec![TIGHT_BENCHES.to_string(), TIGHT_BENCHES.to_string()]),
            TIGHT_BENCHES
        );
        assert_eq!(
            choose_jj_baseline(vec!["# only\n".to_string(), TIGHT_BENCHES.to_string()]),
            TIGHT_BENCHES
        );
        assert_eq!(
            choose_jj_baseline(vec![String::new(), TIGHT_BENCHES.to_string()]),
            TIGHT_BENCHES
        );
    }

    #[test]
    fn jj_criss_cross_of_baselines_keeps_the_minimum_of_each_pin() {
        let root = "[[bench]]\nid = \"a\"\nref_median_ns = 100\n";
        for (left, right) in [(LOOSE_BENCHES, TIGHT_BENCHES), (TIGHT_BENCHES, LOOSE_BENCHES)] {
            let drawn = draw_criss_cross_at("benches/baselines.toml", root, left, right);
            let text = drawn.baseline.expect("baseline").expect("text");
            assert_minimum_pins(&text);
        }
    }

    #[test]
    fn a_comment_only_manifest_loses_to_a_manifest_with_operations() {
        let real = budget_op("command:print", 1, 10, 10);
        let placeholder = "# placeholder\n".to_string();
        let blank = String::new();
        assert_eq!(choose_jj_baseline(vec![placeholder.clone(), real.clone()]), real);
        assert_eq!(choose_jj_baseline(vec![blank, real.clone()]), real);
        assert_eq!(choose_jj_baseline(vec![real.clone(), placeholder]), real);
        assert_eq!(choose_jj_baseline(vec!["# only\n".to_string()]), "# only\n");
    }

    fn parsed_op(text: &str, id: &str) -> Op {
        let ops = parse_operations(text)
            .unwrap_or_else(|errors| panic!("{}", errors.join("; ")));
        ops.into_iter().find(|op| op.id == id).unwrap_or_else(|| panic!("missing `{id}` in {text}"))
    }

    #[test]
    fn a_criss_cross_trade_off_keeps_the_minimum_of_each_ceiling() {
        let low_processes = budget_op("command:print", 1, 100, 100);
        let low_wall = budget_op("command:print", 9, 50, 40);
        for (left, right) in [(&low_processes, &low_wall), (&low_wall, &low_processes)] {
            let merged = choose_jj_baseline(vec![left.clone(), right.clone()]);
            let op = parsed_op(&merged, "command:print");
            assert_eq!(op.processes, 1, "{merged}");
            assert_eq!(op.wall_floor_us, 50, "{merged}");
            assert_eq!(op.target_us, 40, "{merged}");
        }
    }

    #[test]
    fn a_criss_cross_trade_off_keeps_an_operation_that_only_one_head_has() {
        let low_processes = budget_op("command:print", 1, 100, 100);
        let low_wall = format!(
            "{}{}",
            budget_op("command:print", 9, 50, 40),
            budget_op("command:extra", 4, 8, 8)
        );
        for (left, right) in [(&low_processes, &low_wall), (&low_wall, &low_processes)] {
            let merged = choose_jj_baseline(vec![left.clone(), right.clone()]);
            let print = parsed_op(&merged, "command:print");
            assert_eq!((print.processes, print.wall_floor_us, print.target_us), (1, 50, 40));
            let extra = parsed_op(&merged, "command:extra");
            assert_eq!((extra.processes, extra.wall_floor_us, extra.target_us), (4, 8, 8));
        }
    }

    #[test]
    fn equal_shared_ceilings_keep_an_operation_only_one_head_has() {
        let shared = budget_op("command:print", 1, 10, 10);
        let with_extra = format!("{}{}", shared, budget_op("command:extra", 4, 8, 8));
        for (left, right) in [(&shared, &with_extra), (&with_extra, &shared)] {
            let merged = choose_jj_baseline(vec![left.clone(), right.clone()]);
            let print = parsed_op(&merged, "command:print");
            assert_eq!((print.processes, print.wall_floor_us, print.target_us), (1, 10, 10));
            let extra = parsed_op(&merged, "command:extra");
            assert_eq!((extra.processes, extra.wall_floor_us, extra.target_us), (4, 8, 8));
        }
    }

    #[test]
    fn a_lower_shared_ceiling_keeps_that_file_whole() {
        let few = budget_op("command:print", 1, 1, 1);
        let many = format!(
            "{}{}",
            budget_op("command:print", 1, 500_000, 500_000),
            budget_op("command:extra", 2, 1, 1)
        );
        assert_eq!(choose_jj_baseline(vec![few.clone(), many.clone()]), few);
        assert_eq!(choose_jj_baseline(vec![many, few.clone()]), few);
    }

    #[test]
    fn equal_ceilings_keep_the_earlier_heads_text() {
        let earlier = format!("{}# from-a\n", budget_op("command:print", 1, 10, 10));
        let later = format!("{}# from-b\n", budget_op("command:print", 1, 10, 10));
        assert_eq!(choose_jj_baseline(vec![earlier.clone(), later.clone()]), earlier);
        assert_eq!(choose_jj_baseline(vec![later.clone(), earlier]), later);
    }

    #[test]
    fn a_merged_baseline_keeps_escaped_text_from_the_first_head() {
        let quoted = r#"
[[op]]
id = "command:print"
class = "command"
fixture = "one file"
test = "tests/counts.rs::counts"
processes = 1
wall_floor_us = 100
target_us = 100
derivation = "say \"hi\"\twide\\path"
"#;
        let other = budget_op("command:print", 9, 50, 40);
        let merged = choose_jj_baseline(vec![quoted.to_string(), other.clone()]);
        let op = parsed_op(&merged, "command:print");
        assert_eq!(op.derivation, "say \"hi\"\twide\\path");
        assert_eq!((op.processes, op.wall_floor_us, op.target_us), (1, 50, 40));

        let other_first = choose_jj_baseline(vec![other, quoted.to_string()]);
        let op = parsed_op(&other_first, "command:print");
        assert_eq!(op.derivation, "Pinned.");
        assert_eq!((op.processes, op.wall_floor_us, op.target_us), (1, 50, 40));
    }

    #[test]
    fn jj_criss_cross_skips_a_comment_only_head_when_the_other_has_operations() {
        let real = budget_op("command:print", 1, 10, 10);
        let placeholder = "# placeholder\n";
        let mut saw_placeholder_sort_first = false;
        for _ in 0..16 {
            let drawn = draw_criss_cross(placeholder, &real);
            if drawn.first_commit > drawn.second_commit {
                continue;
            }
            saw_placeholder_sort_first = true;
            assert_eq!(drawn.baseline.expect("baseline").as_deref(), Some(real.as_str()));
            break;
        }
        assert!(saw_placeholder_sort_first, "commit ids never put the comment-only head first");
    }

    #[test]
    fn jj_criss_cross_picks_the_strictest_head_whichever_id_sorts_first() {
        let tight = budget_op("command:print", 1, 10, 10);
        let loose = budget_op("command:print", 1, 90, 90);
        assert_eq!(
            resolved_criss_cross(&tight, &loose).expect("baseline").as_deref(),
            Some(tight.as_str())
        );
        assert_eq!(
            resolved_criss_cross(&loose, &tight).expect("baseline").as_deref(),
            Some(tight.as_str())
        );
    }

    #[test]
    fn jj_criss_cross_prefers_a_lower_shared_ceiling_over_more_entries() {
        let few = budget_op("command:print", 1, 1, 1);
        let many = format!(
            "{}{}",
            budget_op("command:print", 1, 500_000, 500_000),
            budget_op("command:extra", 2, 1, 1)
        );
        assert_eq!(
            resolved_criss_cross(&many, &few).expect("baseline").as_deref(),
            Some(few.as_str())
        );
        assert_eq!(
            resolved_criss_cross(&few, &many).expect("baseline").as_deref(),
            Some(few.as_str())
        );
    }

    fn stage_budget(motion: u64, span: u64, target: u64) -> String {
        format!(
            r#"
[[op]]
id = "key:next-file"
class = "keystroke"
fixture = "n"
test = "tests/counts.rs::counts"
processes = 0
benches = ["motion", "span"]
floor = "sum"
floor_reason = "The motion runs, then the span pass."
stage_floors = ["motion={motion}", "span={span}"]
wall_floor_us = {}
target_us = {target}
derivation = "Motion plus the span stage."
"#,
            motion + span
        )
    }

    /// Stage floors merge pointwise. A lower total on one head must not hide a
    /// tighter stage on the other, and the merged wall is the sum of the mins.
    #[test]
    fn jj_criss_cross_stage_floors_take_the_pointwise_minimum() {
        let low_motion = stage_budget(4, 80, 84);
        let low_span = stage_budget(10, 50, 60);
        for (left, right) in [(&low_motion, &low_span), (&low_span, &low_motion)] {
            let text = resolved_criss_cross(left, right).expect("baseline").expect("text");
            let op = parsed_op(&text, "key:next-file");
            assert_eq!(
                op.stage_floors,
                vec!["motion=4".to_string(), "span=50".to_string()],
                "{text}"
            );
            assert_eq!(op.wall_floor_us, 54, "{text}");
            assert_eq!(op.target_us, 60, "{text}");
        }

        let legacy = r#"
[[op]]
id = "key:next-file"
class = "keystroke"
fixture = "n"
test = "tests/counts.rs::counts"
processes = 0
benches = ["motion"]
floor = "max"
floor_reason = "The motion bench on its own."
wall_floor_us = 4
target_us = 4
derivation = "Ceiling of the motion bench."
"#;
        let with_span = stage_budget(4, 50, 54);
        let text = resolved_criss_cross(legacy, &with_span).expect("baseline").expect("text");
        let op = parsed_op(&text, "key:next-file");
        assert_eq!(op.stage_floors, vec!["motion=4".to_string(), "span=50".to_string()], "{text}");
        assert_eq!(op.wall_floor_us, 54, "{text}");
    }

    #[test]
    fn jj_criss_cross_trade_off_keeps_the_minimum_of_each_ceiling() {
        let low_processes = budget_op("command:print", 1, 100, 100);
        let low_wall = budget_op("command:print", 9, 50, 40);
        for (left, right) in [(&low_processes, &low_wall), (&low_wall, &low_processes)] {
            let text = resolved_criss_cross(left, right).expect("baseline").expect("text");
            let op = parsed_op(&text, "command:print");
            assert_eq!(op.processes, 1, "{text}");
            assert_eq!(op.wall_floor_us, 50, "{text}");
            assert_eq!(op.target_us, 40, "{text}");
            let live = budget_op("command:print", 1, 100, 100);
            let surface = BTreeSet::from(["command:print".to_string()]);
            let tests = BTreeSet::from(["tests/counts.rs::counts".to_string()]);
            let benches = BTreeMap::new();
            let errors = check(&Input {
                manifest: &live,
                surface: &surface,
                tests: &tests,
                baseline: Some(&text),
                benches: &benches,
            });
            assert!(
                errors.iter().any(|error| {
                    error.contains("wall_floor_us 100") && error.contains("baseline 50")
                }),
                "a live wall of 100 must fail against the merged ceiling: {errors:?}"
            );
        }
    }

    #[test]
    fn a_jj_parent_that_predates_the_manifest_bootstraps_and_a_deletion_does_not() {
        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "jj", &["git", "init", "--colocate"]);
        fs::write(tmp.path().join("README.md"), "x\n").expect("write readme");
        run_cmd(tmp.path(), "jj", &["commit", "-m", "no manifest"]);
        run_cmd(tmp.path(), "jj", &["bookmark", "create", "main", "-r", "@-"]);
        let predating = jj_commit_id(tmp.path(), "description(exact:\"no manifest\\n\")");
        assert_eq!(
            committed_baseline_file_in(
                tmp.path(),
                Some(&predating),
                Some("refs/pull/1/merge"),
                "tests/perf.toml",
            ),
            Ok(None),
            "a jj PR base that predates the manifest bootstraps"
        );
        assert_eq!(
            committed_baseline_file_in(
                tmp.path(),
                Some(&predating),
                Some("refs/heads/main"),
                "tests/perf.toml",
            ),
            Ok(None),
            "a jj push-to-main parent that predates the manifest bootstraps"
        );

        fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        fs::write(tmp.path().join("tests/perf.toml"), "# added\n").expect("write perf");
        run_cmd(tmp.path(), "jj", &["commit", "-m", "add perf"]);
        fs::remove_file(tmp.path().join("tests/perf.toml")).expect("delete perf");
        run_cmd(tmp.path(), "jj", &["commit", "-m", "delete perf"]);
        let deleted_id = jj_commit_id(tmp.path(), "description(exact:\"delete perf\\n\")");
        let deleted_pr = committed_baseline_file_in(
            tmp.path(),
            Some(&deleted_id),
            Some("refs/pull/2/merge"),
            "tests/perf.toml",
        );
        let pr_err = deleted_pr.expect_err("a jj base that deleted the manifest");
        assert!(pr_err.contains("missing") && pr_err.contains(&deleted_id), "{pr_err}");
        let deleted_push = committed_baseline_file_in(
            tmp.path(),
            Some(&deleted_id),
            Some("refs/heads/main"),
            "tests/perf.toml",
        );
        let push_err = deleted_push.expect_err("a jj push parent that deleted the manifest");
        assert!(push_err.contains("missing") && push_err.contains(&deleted_id), "{push_err}");
    }

    #[test]
    fn a_missing_baseline_file_fails_the_check_and_a_later_revision_is_still_read() {
        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "jj", &["git", "init", "--colocate"]);
        fs::write(tmp.path().join("README.md"), "hello\n").expect("write readme");
        run_cmd(tmp.path(), "jj", &["commit", "-m", "origin"]);
        run_cmd(tmp.path(), "jj", &["bookmark", "create", "main", "-r", "@-"]);
        let remote = TempDir::new().expect("remote");
        run_cmd(remote.path(), "git", &["init", "-q", "--bare"]);
        let url = remote.path().to_str().expect("remote path");
        run_cmd(tmp.path(), "jj", &["git", "remote", "add", "origin", url]);
        run_cmd(tmp.path(), "jj", &["git", "push", "--bookmark", "main"]);
        run_cmd(tmp.path(), "jj", &["new", "main", "-m", "local main"]);
        fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        fs::write(tmp.path().join("tests/perf.toml"), "# from-main\n").expect("write perf");
        run_cmd(tmp.path(), "jj", &["bookmark", "set", "main", "-r", "@"]);

        let on_main = committed_baseline_file_in(
            tmp.path(),
            None,
            Some("refs/heads/main"),
            "tests/perf.toml",
        );
        assert_eq!(on_main.expect("baseline").as_deref(), Some("# from-main\n"));

        run_cmd(tmp.path(), "jj", &["new", "-m", "work"]);
        assert_eq!(
            committed_baseline_in(tmp.path(), None).expect("baseline").as_deref(),
            Some("# from-main\n")
        );

        let empty = TempDir::new().expect("tempdir");
        run_cmd(empty.path(), "jj", &["git", "init", "--colocate"]);
        fs::write(empty.path().join("README.md"), "x\n").expect("write readme");
        run_cmd(empty.path(), "jj", &["commit", "-m", "no manifest"]);
        run_cmd(empty.path(), "jj", &["bookmark", "create", "main", "-r", "@-"]);
        assert_eq!(
            committed_baseline_in(empty.path(), None),
            Ok(None),
            "jj main that never had the manifest bootstraps"
        );
        fs::create_dir_all(empty.path().join("tests")).expect("mkdir tests");
        fs::write(empty.path().join("tests/perf.toml"), "# added\n").expect("write perf");
        run_cmd(empty.path(), "jj", &["commit", "-m", "add perf"]);
        fs::remove_file(empty.path().join("tests/perf.toml")).expect("delete perf");
        run_cmd(empty.path(), "jj", &["commit", "-m", "delete perf"]);
        run_cmd(empty.path(), "jj", &["bookmark", "set", "main", "-r", "@-"]);
        let mut errors = Vec::new();
        assert!(
            recorded_baseline(committed_baseline_in(empty.path(), None), &mut errors).is_none()
        );
        assert!(
            errors.iter().any(|err| err.contains("missing") && err.contains("tests/perf.toml")),
            "deleting the manifest on jj main must still fail: {errors:?}"
        );

        let git_repo = TempDir::new().expect("tempdir");
        run_cmd(git_repo.path(), "git", &["init", "-q", "-b", "main"]);
        fs::write(git_repo.path().join("README.md"), "hello\n").expect("write readme");
        run_cmd(git_repo.path(), "git", &["add", "."]);
        run_cmd(git_repo.path(), "git", &["commit", "-q", "-m", "init"]);
        let root_sha = git_head(git_repo.path());
        run_cmd(git_repo.path(), "git", &["checkout", "-q", "-b", "feature"]);
        run_cmd(git_repo.path(), "git", &["checkout", "-q", "main"]);
        fs::create_dir_all(git_repo.path().join("tests")).expect("mkdir tests");
        fs::write(git_repo.path().join("tests/perf.toml"), "# on-main\n").expect("write perf");
        run_cmd(git_repo.path(), "git", &["add", "."]);
        run_cmd(git_repo.path(), "git", &["commit", "-q", "-m", "add perf"]);
        let main_sha = git_head(git_repo.path());
        run_cmd(git_repo.path(), "git", &["update-ref", "refs/remotes/origin/main", &main_sha]);
        run_cmd(git_repo.path(), "git", &["checkout", "-q", "feature"]);
        assert_eq!(
            committed_baseline_in(git_repo.path(), None).expect("baseline").as_deref(),
            Some("# on-main\n")
        );
        let pinned = committed_baseline_file_in(
            git_repo.path(),
            Some(&root_sha),
            Some("refs/pull/1/merge"),
            "tests/perf.toml",
        );
        assert_eq!(pinned, Ok(None), "a PR base that predates the manifest bootstraps");

        let pushed = committed_baseline_file_in(
            git_repo.path(),
            Some(&root_sha),
            Some("refs/heads/main"),
            "tests/perf.toml",
        );
        assert_eq!(pushed, Ok(None), "a push-to-main parent that predates the manifest bootstraps");

        run_cmd(git_repo.path(), "git", &["checkout", "-q", "main"]);
        fs::remove_file(git_repo.path().join("tests/perf.toml")).expect("delete perf");
        run_cmd(git_repo.path(), "git", &["add", "."]);
        run_cmd(git_repo.path(), "git", &["commit", "-q", "-m", "delete perf"]);
        let deleted_sha = git_head(git_repo.path());
        let deleted = committed_baseline_file_in(
            git_repo.path(),
            Some(&deleted_sha),
            Some("refs/pull/2/merge"),
            "tests/perf.toml",
        );
        let err = deleted.expect_err("a base that deleted the manifest");
        assert!(err.contains("missing") && err.contains(&deleted_sha), "{err}");
    }
}
