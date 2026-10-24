//! Fail a performance manifest that drifts from the surface, its tests, its
//! named benches, or the budgets already committed.
//!
//! Counts and wall floors only tighten. A target may not rise, may not pass
//! its class cap, may not exceed its wall floor, and has to carry a one-line
//! derivation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

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
    pub wall_floor_us: u64,
    pub target_us: u64,
    pub derivation: String,
    pub proposal: String,
    pub alloc: Vec<String>,
}

#[derive(Debug)]
enum Value {
    Text(String),
    Number(u64),
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
            Ok(previous) => looseness(&ops, &previous, &mut errors),
            Err(parse_errors) => {
                errors.push(format!("baseline manifest: {}", parse_errors.join("; ")));
            }
        }
    }
    errors.sort();
    errors.dedup();
    errors
}

/// `[[bench]]` records mapped to `ref_median_ns.div_ceil(1_000)` in microseconds.
pub fn recorded_benches(text: &str) -> BTreeMap<String, u64> {
    let mut benches = BTreeMap::new();
    let mut current_id: Option<String> = None;
    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line == "[[bench]]" {
            current_id = None;
            continue;
        }
        if let Some(id) = id_assignment(line) {
            benches.entry(id.clone()).or_insert(0);
            current_id = Some(id);
            continue;
        }
        if let Some(ns) = number_assignment(line, "ref_median_ns")
            && let Some(id) = current_id.as_ref()
        {
            benches.insert(id.clone(), ns.div_ceil(1_000));
        }
    }
    benches
}

/// Record every `#[test]` function name under `dir`.
pub fn collect_test_names(dir: &Path, out: &mut BTreeSet<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_test_names(&path, out);
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "rs")
            && let Ok(text) = fs::read_to_string(&path)
        {
            push_test_names(&text, out);
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
    raw.parse::<u64>()
        .map(Value::Number)
        .map_err(|_| format!("line {line_no}: expected a string, integer, or string array"))
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
    let id = require_text(&mut fields, "id", &mut local);
    let class = require_text(&mut fields, "class", &mut local);
    let fixture = require_one_line(&mut fields, "fixture", &mut local);
    let test = require_one_line(&mut fields, "test", &mut local);
    let processes = require_number(&mut fields, "processes", &mut local);
    let wall_floor_us = require_number(&mut fields, "wall_floor_us", &mut local);
    let target_us = require_number(&mut fields, "target_us", &mut local);
    let derivation = require_one_line(&mut fields, "derivation", &mut local);
    let proposal = optional_one_line(&mut fields, "proposal", &mut local);
    let alloc = optional_list(&mut fields, "alloc", &mut local);
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
        wall_floor_us: wall_floor_us.unwrap_or_default(),
        target_us: target_us.unwrap_or_default(),
        derivation: derivation.unwrap_or_default(),
        proposal: proposal.unwrap_or_default(),
        alloc: alloc.unwrap_or_default(),
    })
}

fn require_text(
    fields: &mut BTreeMap<String, Value>,
    key: &str,
    errors: &mut Vec<String>,
) -> Option<String> {
    match fields.remove(key) {
        Some(Value::Text(text)) if !text.trim().is_empty() => Some(text),
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

fn require_one_line(
    fields: &mut BTreeMap<String, Value>,
    key: &str,
    errors: &mut Vec<String>,
) -> Option<String> {
    let text = require_text(fields, key, errors)?;
    if text.contains('\n') || text.contains('\r') {
        errors.push(format!("`{key}` must be one non-empty line"));
        None
    } else {
        Some(text)
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
    if !tests.contains(&op.test) {
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
    match allowed_target_us(&op.class) {
        Ok(cap) if op.target_us > cap => {
            errors.push(format!(
                "operation `{}` target_us {} exceeds the {} cap {cap}",
                op.id, op.target_us, op.class
            ));
        }
        Ok(_) => {}
        Err(message) => errors.push(format!("operation `{}`: {message}", op.id)),
    }
}

fn check_benches(ops: &[Op], benches: &BTreeMap<String, u64>, errors: &mut Vec<String>) {
    let mut named = BTreeSet::new();
    for op in ops {
        for bench in &op.alloc {
            match benches.get(bench) {
                Some(&floor_us) => {
                    if op.wall_floor_us < floor_us {
                        errors.push(format!(
                            "operation `{}` wall_floor_us {} is below bench `{bench}` ceiling {floor_us}",
                            op.id, op.wall_floor_us
                        ));
                    }
                }
                None => {
                    errors.push(format!("operation `{}` names unknown bench `{bench}`", op.id));
                }
            }
            named.insert(bench.as_str());
        }
    }
    for bench in benches.keys() {
        if !named.contains(bench.as_str()) {
            errors.push(format!("bench `{bench}` is not named by any operation"));
        }
    }
}

fn looseness(ops: &[Op], previous: &[Op], errors: &mut Vec<String>) {
    let prior: BTreeMap<&str, &Op> = previous.iter().map(|op| (op.id.as_str(), op)).collect();
    for op in ops {
        let Some(before) = prior.get(op.id.as_str()) else {
            continue;
        };
        looser(&op.id, "processes", op.processes, before.processes, errors);
        looser(&op.id, "wall_floor_us", op.wall_floor_us, before.wall_floor_us, errors);
        looser(&op.id, "target_us", op.target_us, before.target_us, errors);
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

fn id_assignment(line: &str) -> Option<String> {
    let rest = line.strip_prefix("id")?;
    if !matches!(rest.chars().next(), Some(' ' | '\t' | '=')) {
        return None;
    }
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    parse_string(rest, 0).ok()
}

fn number_assignment(line: &str, key: &str) -> Option<u64> {
    let rest = line.strip_prefix(key)?;
    if !matches!(rest.chars().next(), Some(' ' | '\t' | '=')) {
        return None;
    }
    let rest = rest.trim_start().strip_prefix('=')?.trim();
    rest.parse::<u64>().ok()
}

fn push_test_names(source: &str, out: &mut BTreeSet<String>) {
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
                out.insert(name.to_string());
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
test = "counts"
processes = 2
wall_floor_us = 50000
target_us = 50000
derivation = "One external call plus the class margin."
alloc = ["bench-a"]
"#;

fn happy<'a>(baseline: Option<&'a str>) -> Case<'a> {
    Case {
        manifest: HAPPY,
        surface: &["command:print"],
        tests: &["counts"],
        baseline,
        benches: &[("bench-a", 100)],
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
test = "counts"
processes = 1
wall_floor_us = {wall_floor}
target_us = {target}
derivation = "Within the class."
{proposal_line}"#
    )
}

fn class_case(manifest: &str) -> Vec<String> {
    run(Case { manifest, surface: &["item"], tests: &["counts"], baseline: None, benches: &[] })
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
fn a_surface_operation_without_an_entry_fails() {
    let errors = run(Case {
        manifest: HAPPY,
        surface: &["command:print", "command:help"],
        tests: &["counts"],
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
        tests: &["counts"],
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
fn an_entry_that_names_a_missing_test_fails() {
    let errors = run(Case {
        manifest: HAPPY,
        surface: &["command:print"],
        tests: &[],
        baseline: None,
        benches: &[("bench-a", 100)],
    });
    assert!(errors.iter().any(|error| error.contains("missing test `counts`")));
}

#[test]
fn a_looser_floor_or_target_fails_and_a_tighter_one_passes() {
    let looser = HAPPY
        .replace("processes = 2", "processes = 3")
        .replace("wall_floor_us = 50000", "wall_floor_us = 50001")
        .replace("target_us = 50000", "target_us = 50001");
    let tighter = HAPPY
        .replace("processes = 2", "processes = 1")
        .replace("wall_floor_us = 50000", "wall_floor_us = 40000")
        .replace("target_us = 50000", "target_us = 40000");
    let errors = run(Case {
        manifest: &tighter,
        surface: &["command:print"],
        tests: &["counts"],
        baseline: Some(HAPPY),
        benches: &[("bench-a", 100)],
    });
    assert!(errors.is_empty(), "{errors:?}");

    let errors = run(Case {
        manifest: &looser,
        surface: &["command:print"],
        tests: &["counts"],
        baseline: Some(HAPPY),
        benches: &[("bench-a", 100)],
    });
    for field in ["processes", "wall_floor_us", "target_us"] {
        assert!(
            errors.iter().any(|error| error.contains(field) && error.contains("looser")),
            "{field}: {errors:?}"
        );
    }
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
    let empty = HAPPY
        .replace("derivation = \"One external call plus the class margin.\"", "derivation = \"\"");
    let errors = run(Case {
        manifest: &empty,
        surface: &["command:print"],
        tests: &["counts"],
        baseline: None,
        benches: &[("bench-a", 100)],
    });
    assert!(
        errors
            .iter()
            .any(|error| error.contains("derivation") && error.contains("one non-empty line"))
    );

    let split = HAPPY.replace(
        "derivation = \"One external call plus the class margin.\"",
        "derivation = \"first\\nsecond\"",
    );
    let errors = run(Case {
        manifest: &split,
        surface: &["command:print"],
        tests: &["counts"],
        baseline: None,
        benches: &[("bench-a", 100)],
    });
    assert!(
        errors
            .iter()
            .any(|error| error.contains("derivation") && error.contains("one non-empty line")),
        "{errors:?}"
    );
}

#[test]
fn a_new_operation_absent_from_the_baseline_is_allowed() {
    let live = format!(
        "{HAPPY}\n[[op]]\nid = \"command:help\"\nclass = \"command\"\nfixture = \"none\"\ntest = \"counts\"\nprocesses = 0\nwall_floor_us = 1\ntarget_us = 1\nderivation = \"No external call.\"\n"
    );
    let errors = run(Case {
        manifest: &live,
        surface: &["command:print", "command:help"],
        tests: &["counts"],
        baseline: Some(HAPPY),
        benches: &[("bench-a", 100)],
    });
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn every_named_bench_has_to_exist_and_wall_floor_must_cover_its_median() {
    let unknown = HAPPY.replace("bench-a", "bench-missing");
    let errors = run(Case {
        manifest: &unknown,
        surface: &["command:print"],
        tests: &["counts"],
        baseline: None,
        benches: &[("bench-a", 100)],
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
        tests: &["counts"],
        baseline: None,
        benches: &[("bench-a", 60_000)],
    });
    assert!(
        below_bench.iter().any(|error| error.contains("below bench `bench-a` ceiling 60000")),
        "{below_bench:?}"
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
        tests: &["counts"],
        baseline: None,
        benches: &[("bench-a", 100)],
    }
}

#[test]
fn recorded_benches_parse_ids_and_ceiling_medians() {
    let text =
        "# id = \"nope\"\n[[bench]]\nidentity = \"nope\"\nid = \"real\"\nref_median_ns = 3513\n";
    let benches = recorded_benches(text);
    assert_eq!(benches.get("real"), Some(&4));
    assert_eq!(benches.len(), 1);
}

#[test]
fn test_names_come_only_from_test_attributed_functions() {
    let mut found = BTreeSet::new();
    push_test_names(
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
    assert_eq!(found.iter().map(String::as_str).collect::<Vec<_>>(), vec!["alpha", "beta"]);
}
