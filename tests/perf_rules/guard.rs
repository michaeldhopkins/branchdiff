//! Fail a performance manifest that drifts from the surface, its tests, its
//! named benches, or the budgets already committed.
//!
//! Counts only tighten. A wall floor may rise only when both the old and new
//! values equal the bench-derived floor. A target may rise only with that
//! wall, and only by as much as the wall rose. A target may not pass its class
//! cap, may not exceed its wall floor, and has to carry a one-line derivation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

#[path = "../../benches/support/baselines.rs"]
pub mod baselines;

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

/// Fail when `benches/baselines.toml` fails validation or when any
/// `ref_median_ns`, `max_blocks`, `max_bytes`, or `max_bytes_x86_64` rises
/// above or disappears without `removed = "<reason>"` from the baseline
/// revision's copy.
pub fn check_baseline_medians(
    live_text: &str,
    baseline_text: Option<&str>,
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
    let live_by_id: BTreeMap<&str, &baselines::BenchRecord> =
        live_records.iter().map(|r| (r.id.as_str(), r)).collect();
    for before in prior_records.iter().filter(|r| r.removed.is_none()) {
        let bench = before.id.as_str();
        let Some(live) = live_by_id.get(bench) else {
            errors.push(format!("bench `{bench}` was in the baseline but is missing"));
            continue;
        };
        if live.removed.is_some() {
            continue;
        }
        check_pin_field(bench, "ref_median_ns", live.ref_median_ns, before.ref_median_ns, errors);
        check_pin_field(bench, "max_blocks", live.max_blocks, before.max_blocks, errors);
        check_pin_field(bench, "max_bytes", live.max_bytes, before.max_bytes, errors);
        check_pin_field(
            bench, "max_bytes_x86_64", live.max_bytes_x86_64, before.max_bytes_x86_64, errors,
        );
    }
    errors.sort();
    errors.dedup();
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
        let formula = accounted(op, benches) && accounted(before, benches);
        let wall_rose = op.wall_floor_us > before.wall_floor_us;
        if wall_rose && !formula {
            looser(&op.id, "wall_floor_us", op.wall_floor_us, before.wall_floor_us, errors);
        }
        if op.target_us > before.target_us {
            let wall_delta = op.wall_floor_us.saturating_sub(before.wall_floor_us);
            let target_delta = op.target_us - before.target_us;
            if !(formula && wall_rose && target_delta <= wall_delta) {
                looser(&op.id, "target_us", op.target_us, before.target_us, errors);
            }
        }
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
                    && formula
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

#[cfg(test)]
#[path = "guard_tests.rs"]
mod tests;
