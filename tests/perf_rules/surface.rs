//! This program's user-facing operations: the clap tree, the TUI key map,
//! and the live page's per-frame script and routes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};

use branchdiff::app::App;
use branchdiff::diff::{DiffLine, LineSource};

use super::guard::baselines::{self, BenchRecord};
use branchdiff::file_events::VcsLockState;
use branchdiff::input::{AppAction, handle_event};
use branchdiff::message::{Message, Repaint, UpdateResult};
use branchdiff::update::{RefreshState, Timers, UpdateConfig, update};
use branchdiff::vcs::{
    ComparisonContext, RefreshResult, Vcs, VcsBackend, VcsEventType, VcsWatchPaths,
};

#[path = "../support/frame_scan.rs"]
mod frame_scan;

use frame_scan::{per_frame_work, production};

pub(super) fn operations() -> BTreeSet<String> {
    let commands = command_ids();
    assert!(!commands.is_empty(), "command surface must not be empty");
    let keys = key_operations();
    assert!(!keys.is_empty(), "keystroke surface must not be empty");
    let frames = frame_and_route_ids();
    assert!(frames.iter().any(|id| id.starts_with("frame:")), "frame surface must not be empty");
    let mut ops = commands;
    ops.extend(keys);
    ops.extend(frames);
    ops
}

/// The manifest committed at `PERF_BASELINE_REV` (when set in CI) or on
/// `main@origin` / `main`. `Ok(None)` is a first landing: an all-zero revision
/// whose parent has no file, or a resolved revision whose history never had
/// the file. A revision that had the file and lost it is an error. An empty
/// or comment-only file that is present is not this case; the guard rejects it.
pub(super) fn committed_baseline() -> Result<Option<String>, String> {
    committed_baseline_file("tests/perf.toml")
}

pub(super) fn committed_baseline_benches() -> Result<Option<String>, String> {
    committed_baseline_file("benches/baselines.toml")
}

/// Record a baseline-resolution error and keep checking the rest of the manifest.
pub(super) fn recorded_baseline(
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
    let root = std::env::var_os("BRANCHDIFF_TEST_BASELINE_REPO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    let rev = std::env::var("PERF_BASELINE_REV").ok();
    let github_ref = std::env::var("GITHUB_REF").ok();
    committed_baseline_file_in(&root, rev.as_deref(), github_ref.as_deref(), rel_path)
}

#[cfg(test)]
pub(super) fn committed_baseline_in(
    root: &Path,
    rev_override: Option<&str>,
) -> Result<Option<String>, String> {
    committed_baseline_file_in(root, rev_override, None, "tests/perf.toml")
}

pub(super) fn committed_baseline_file_in(
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
        super::guard::parse_operations(text)
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
    let Ok(ops) = super::guard::parse_operations(text) else {
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
    let parsed: Vec<Vec<super::guard::Op>> = candidates
        .iter()
        .map(|(_, text)| super::guard::parse_operations(text).unwrap_or_default())
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
        let copies: Vec<&super::guard::Op> =
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
fn merged_stage_floors(copies: &[&super::guard::Op]) -> (Vec<String>, u64) {
    let mut order = Vec::new();
    let mut mins: BTreeMap<String, u64> = BTreeMap::new();
    let mut bounds = Vec::new();
    for op in copies {
        match super::guard::running_stage_floors(op) {
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
    op: &super::guard::Op,
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

const ALL_APP_ACTIONS: &[AppAction] = &[
    AppAction::Quit,
    AppAction::ScrollUp(1),
    AppAction::ScrollDown(1),
    AppAction::PageUp,
    AppAction::PageDown,
    AppAction::GoToTop,
    AppAction::GoToBottom,
    AppAction::NextFile,
    AppAction::PrevFile,
    AppAction::ToggleHelp,
    AppAction::CycleViewMode,
    AppAction::StartSelection(0, 0),
    AppAction::UpdateSelection(0, 0),
    AppAction::EndSelection,
    AppAction::CopyPath,
    AppAction::CopyDiff,
    AppAction::CopyPatch,
    AppAction::CopyOrQuit,
    AppAction::OpenSearch,
    AppAction::ToggleDiffBase,
    AppAction::ToggleReviewed,
    AppAction::ToggleAllReviewed,
    AppAction::OpenEditor,
    AppAction::OpenRepo,
    AppAction::RunRecovery,
    AppAction::Resize,
    AppAction::ForceRepaint,
    AppAction::None,
];

pub(super) fn action_name(action: AppAction) -> Option<&'static str> {
    Some(match action {
        AppAction::Quit => "key:quit",
        AppAction::ScrollUp(_) => "key:scroll-up",
        AppAction::ScrollDown(_) => "key:scroll-down",
        AppAction::PageUp => "key:page-up",
        AppAction::PageDown => "key:page-down",
        AppAction::GoToTop => "key:go-to-top",
        AppAction::GoToBottom => "key:go-to-bottom",
        AppAction::NextFile => "key:next-file",
        AppAction::PrevFile => "key:prev-file",
        AppAction::ToggleHelp => "key:toggle-help",
        AppAction::CycleViewMode => "key:cycle-view-mode",
        AppAction::StartSelection(_, _) => "key:start-selection",
        AppAction::UpdateSelection(_, _) => "key:update-selection",
        AppAction::EndSelection => "key:end-selection",
        AppAction::CopyPath => "key:copy-path",
        AppAction::CopyDiff => "key:copy-diff",
        AppAction::CopyPatch => "key:copy-patch",
        AppAction::CopyOrQuit => "key:copy-or-quit",
        AppAction::OpenSearch => "key:open-search",
        AppAction::ToggleDiffBase => "key:toggle-diff-base",
        AppAction::ToggleReviewed => "key:toggle-reviewed",
        AppAction::ToggleAllReviewed => "key:toggle-all-reviewed",
        AppAction::OpenEditor => "key:open-editor",
        AppAction::OpenRepo => "key:open-repo",
        AppAction::RunRecovery => "key:run-recovery",
        AppAction::Resize => "key:resize",
        AppAction::ForceRepaint => "key:force-repaint",
        AppAction::None => return None,
    })
}

/// Run one non-search action through the reducer. Clipboard arms stay with the
/// caller so unit tests never touch the system clipboard.
pub(super) fn apply_input(app: &mut App, action: AppAction) -> UpdateResult {
    let mut refresh = RefreshState::Idle;
    let mut lock = VcsLockState::default();
    let mut timers = Timers::default();
    let config = UpdateConfig::default();
    let vcs = IdleVcs;
    update(Message::Input(action), app, &mut refresh, &mut lock, &mut timers, &config, &vcs)
}

/// Apply one search-mode event and name the effect. An effect the buckets
/// do not recognise is `key:search-unclassified`, which has no budget.
pub(super) fn search_effect(app: &mut App, event: Event) -> Option<&'static str> {
    let before = snap(app);
    let result = apply_search(app, event);
    classify(&before, &snap(app), &result)
}

fn command_ids() -> BTreeSet<String> {
    use clap::CommandFactory;
    let mut cmd = branchdiff::cli::Cli::command();
    // Help is added while the command is built, not in the derive output.
    cmd.build();
    let mut ops = BTreeSet::new();
    walk_command(&cmd, &mut ops);
    ops
}

fn walk_command(cmd: &clap::Command, ops: &mut BTreeSet<String>) {
    ops.insert(format!("command:{}", normalize(cmd.get_name())));
    for arg in cmd.get_arguments() {
        if arg.is_positional() {
            continue;
        }
        let name = normalize(arg.get_long().unwrap_or(arg.get_id().as_str()));
        if name == "html" {
            push_html_pages(arg, ops);
            continue;
        }
        ops.insert(format!("command:{name}"));
    }
    for sub in cmd.get_subcommands() {
        walk_command(sub, ops);
    }
}

fn push_html_pages(arg: &clap::Arg, ops: &mut BTreeSet<String>) {
    let mut count = 0;
    for value in arg.get_possible_values() {
        count += 1;
        ops.insert(format!("page:html-{}", normalize(value.get_name())));
    }
    if count == 0 {
        ops.insert("page:html-unspecified".to_string());
    }
}

fn normalize(name: &str) -> String {
    name.replace('_', "-")
}

fn key_operations() -> BTreeSet<String> {
    let mut ops: BTreeSet<String> =
        ALL_APP_ACTIONS.iter().copied().filter_map(action_name).map(str::to_string).collect();
    let lines = probe_lines();
    for event in probe_events() {
        if let Some(id) = action_name(handle_event(event.clone())) {
            assert!(ops.contains(id), "ALL_APP_ACTIONS is missing variant for {id}");
        }
        let mut app = probe_app(&lines);
        if let Some(id) = search_effect(&mut app, event) {
            ops.insert(id.to_string());
        }
    }
    ops
}

fn frame_and_route_ids() -> BTreeSet<String> {
    let mut ops = BTreeSet::new();
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    walk_frames_and_routes(&src, &mut ops);
    ops
}

fn walk_frames_and_routes(dir: &Path, ops: &mut BTreeSet<String>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("read_dir {} failed: {err}", dir.display()));
    for entry in entries {
        let entry =
            entry.unwrap_or_else(|err| panic!("read_dir entry in {} failed: {err}", dir.display()));
        let path = entry.path();
        if path.is_dir() {
            walk_frames_and_routes(&path, ops);
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "rs") {
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("read {} failed: {err}", path.display()));
            scan_source_file(&path, &raw, ops);
        }
    }
}

fn scan_source_file(path: &Path, raw: &str, ops: &mut BTreeSet<String>) {
    if per_frame_work(raw).is_some() {
        ops.insert(frame_id(path));
    }
    let prod = production(raw);
    push_fetch_routes(&prod, ops);
}

fn frame_id(path: &Path) -> String {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let rel = path.strip_prefix(&src).unwrap_or(path).with_extension("");
    let parts: Vec<String> =
        rel.components().filter_map(|comp| comp.as_os_str().to_str()).map(normalize).collect();
    let slug = if parts.is_empty() { "page".to_string() } else { parts.join("/") };
    format!("frame:{slug}")
}

fn push_fetch_routes(text: &str, ops: &mut BTreeSet<String>) {
    for prefix in ["fetch('", "fetch(\""] {
        let quote = prefix.chars().last().expect("quote");
        let mut rest = text;
        while let Some(pos) = rest.find(prefix) {
            let after = &rest[pos + prefix.len()..];
            if let Some(end) = after.find(quote) {
                let route = &after[..end];
                if !route.is_empty() {
                    ops.insert(format!("page:route-{}", normalize(route.trim_start_matches('/'))));
                }
            }
            rest = after;
        }
    }
}

fn probe_lines() -> Vec<DiffLine> {
    let mut lines = vec![DiffLine::file_header("probe.txt")];
    for row in 0..210 {
        let text = if matches!(row, 10 | 80 | 150) {
            format!("value {row}")
        } else {
            format!("row {row}")
        };
        lines.push(DiffLine::new(LineSource::Base, text, ' ', Some(row + 1)));
    }
    lines
}

fn probe_app(lines: &[DiffLine]) -> App {
    let mut app = App::new_for_bench(lines.to_vec());
    app.set_viewport_height(50);
    app.open_search();
    app.search_insert_char('v');
    if let Some(search) = app.search.as_mut() {
        search.current = 1;
    }
    app.view.scroll_offset = 100;
    app.view.sub_row = 0;
    app
}

fn probe_events() -> Vec<Event> {
    let mut events = Vec::new();
    push_keys(&mut events);
    push_pointer(&mut events);
    events.push(Event::Resize(120, 50));
    events.push(Event::FocusGained);
    events.push(Event::FocusLost);
    events
}

fn push_keys(events: &mut Vec<Event>) {
    let modifiers = [
        KeyModifiers::NONE,
        KeyModifiers::SHIFT,
        KeyModifiers::CONTROL,
        KeyModifiers::ALT,
        KeyModifiers::SHIFT | KeyModifiers::CONTROL,
    ];
    for byte in 0u8..=127 {
        for modifiers in modifiers {
            events.push(key(KeyCode::Char(byte as char), modifiers));
        }
    }
    let mut specials = vec![
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Enter,
        KeyCode::Esc,
        KeyCode::Backspace,
        KeyCode::Tab,
        KeyCode::Delete,
        KeyCode::Insert,
    ];
    specials.extend((1..=12).map(KeyCode::F));
    for code in specials {
        for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT, KeyModifiers::CONTROL] {
            events.push(key(code, modifiers));
        }
    }
}

fn push_pointer(events: &mut Vec<Event>) {
    for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
        events.push(mouse(MouseEventKind::Down(button)));
        events.push(mouse(MouseEventKind::Drag(button)));
        events.push(mouse(MouseEventKind::Up(button)));
    }
    for kind in [
        MouseEventKind::Moved,
        MouseEventKind::ScrollUp,
        MouseEventKind::ScrollDown,
        MouseEventKind::ScrollLeft,
        MouseEventKind::ScrollRight,
    ] {
        events.push(mouse(kind));
    }
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent { code, modifiers, kind: KeyEventKind::Press, state: KeyEventState::NONE })
}

fn mouse(kind: MouseEventKind) -> Event {
    Event::Mouse(MouseEvent { kind, column: 1, row: 1, modifiers: KeyModifiers::NONE })
}

struct Snap {
    query: Option<String>,
    current: usize,
    scroll: usize,
}

fn snap(app: &App) -> Snap {
    Snap {
        query: app.search.as_ref().map(|search| search.query.clone()),
        current: app.search.as_ref().map(|search| search.current).unwrap_or(0),
        scroll: app.view.scroll_offset,
    }
}

fn classify(before: &Snap, after: &Snap, result: &UpdateResult) -> Option<&'static str> {
    match (&before.query, &after.query) {
        (Some(_), None) => return Some("key:search-close"),
        (Some(old), Some(new)) if new.chars().count() > old.chars().count() => {
            return Some("key:search-type");
        }
        (Some(old), Some(new)) if new.chars().count() < old.chars().count() => {
            return Some("key:search-backspace");
        }
        _ => {}
    }
    if before.query.is_some() && after.current != before.current {
        return Some(if after.current > before.current {
            "key:search-next"
        } else {
            "key:search-prev"
        });
    }
    let delta = after.scroll as i64 - before.scroll as i64;
    if (-5..=-1).contains(&delta) {
        return Some("key:scroll-up");
    }
    if (1..=5).contains(&delta) {
        return Some("key:scroll-down");
    }
    if delta <= -6 {
        return Some("key:page-up");
    }
    if delta >= 6 {
        return Some("key:page-down");
    }
    let unchanged = before.query == after.query && before.current == after.current && delta == 0;
    if unchanged && result.repaint == Repaint::Full {
        return Some("key:force-repaint");
    }
    if unchanged && !result.needs_redraw {
        return None;
    }
    Some("key:search-unclassified")
}

fn apply_search(app: &mut App, event: Event) -> UpdateResult {
    let mut refresh = RefreshState::Idle;
    let mut lock = VcsLockState::default();
    let mut timers = Timers::default();
    let config = UpdateConfig::default();
    let vcs = IdleVcs;
    update(Message::SearchInput(event), app, &mut refresh, &mut lock, &mut timers, &config, &vcs)
}

struct IdleVcs;

impl Vcs for IdleVcs {
    fn repo_path(&self) -> &Path {
        Path::new("/bench")
    }

    fn comparison_context(&self) -> anyhow::Result<ComparisonContext> {
        unimplemented!("a keystroke must not query the vcs")
    }

    fn refresh(&self, _cancel: &Arc<AtomicBool>) -> anyhow::Result<RefreshResult> {
        unimplemented!("a keystroke must not refresh")
    }

    fn single_file_diff(&self, _file_path: &str) -> Option<branchdiff::diff::FileDiff> {
        None
    }

    fn base_identifier(&self) -> anyhow::Result<String> {
        unimplemented!("a keystroke must not query the vcs")
    }

    fn base_file_bytes(&self, _file_path: &str) -> anyhow::Result<Option<Vec<u8>>> {
        unimplemented!("a keystroke must not read file bytes")
    }

    fn working_file_bytes(&self, _file_path: &str) -> anyhow::Result<Option<Vec<u8>>> {
        unimplemented!("a keystroke must not read file bytes")
    }

    fn fetch(&self) -> anyhow::Result<()> {
        unimplemented!("a keystroke must not fetch")
    }

    fn has_conflicts(&self) -> anyhow::Result<bool> {
        Ok(false)
    }

    fn is_locked(&self) -> bool {
        false
    }

    fn watch_paths(&self) -> VcsWatchPaths {
        VcsWatchPaths { files: Vec::new(), recursive_dirs: Vec::new() }
    }

    fn classify_event(&self, _path: &Path) -> VcsEventType {
        VcsEventType::Internal
    }

    fn backend(&self) -> VcsBackend {
        VcsBackend::Git
    }

    fn current_revision_id(&self) -> anyhow::Result<String> {
        Ok("bench".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

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
    fn surface_includes_resize_keystroke_and_path_based_frame_ids() {
        let ops = operations();
        assert!(ops.contains("key:resize"), "probe_events must surface key:resize");
        assert!(ops.contains("frame:html-page"), "src/html_page.rs must map to frame:html-page");

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut frame_ops = BTreeSet::new();
        let snippet = "const RAF: &str = \"requestAnimationFrame(step)\";";
        scan_source_file(&root.join("src/ui/mod.rs"), snippet, &mut frame_ops);
        scan_source_file(&root.join("src/vcs/mod.rs"), snippet, &mut frame_ops);
        assert!(frame_ops.contains("frame:ui/mod"), "{frame_ops:?}");
        assert!(frame_ops.contains("frame:vcs/mod"), "{frame_ops:?}");
        assert_eq!(frame_ops.len(), 2, "two mod.rs files must not collide: {frame_ops:?}");
    }

    fn baseline_child(repo_os: std::ffi::OsString) {
        let repo = PathBuf::from(repo_os);
        let mode = std::env::var("BRANCHDIFF_TEST_BASELINE_MODE").expect("mode");
        let baseline = committed_baseline();
        match mode.as_str() {
            "tighter" => {
                let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
                let manifest =
                    fs::read_to_string(repo.join("tests/perf.toml")).expect("read manifest");
                let surface = operations();
                let mut tests = BTreeSet::new();
                super::super::guard::collect_test_names(&root.join("tests"), &mut tests);
                super::super::guard::collect_test_names(&root.join("src"), &mut tests);
                let bench_text = fs::read_to_string(root.join("benches/baselines.toml"))
                    .expect("read baselines");
                let benches = super::super::guard::must_recorded_benches(&bench_text);
                let baseline = baseline.expect("baseline");
                let errors = super::super::guard::check(&super::super::guard::Input {
                    manifest: &manifest,
                    surface: &surface,
                    tests: &tests,
                    baseline: baseline.as_deref(),
                    benches: &benches,
                });
                assert!(errors.is_empty(), "manifest looser than baseline:\n{}", errors.join("\n"));
            }
            "new_zero" => {
                assert_eq!(baseline, Ok(None), "all-zero SHA on a new manifest must return None");
            }
            _ => unreachable!("unknown mode {mode}"),
        }
    }

    fn spawn_baseline_child(repo: &Path, mode: &str, rev: &str) -> std::process::Output {
        Command::new(std::env::current_exe().expect("current_exe"))
            .args([
                "--exact",
                "surface::tests::perf_baseline_rev_env_var_tighter_commit_fails_and_all_zero_checks_manifest_history",
                "--nocapture",
            ])
            .env("BRANCHDIFF_TEST_BASELINE_REPO", repo)
            .env("BRANCHDIFF_TEST_BASELINE_MODE", mode)
            .env("PERF_BASELINE_REV", rev)
            .output()
            .expect("spawn baseline child")
    }

    #[test]
    fn perf_baseline_rev_env_var_tighter_commit_fails_and_all_zero_checks_manifest_history() {
        if let Some(repo_os) = std::env::var_os("BRANCHDIFF_TEST_BASELINE_REPO") {
            baseline_child(repo_os);
            return;
        }

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let live_manifest =
            fs::read_to_string(root.join("tests/perf.toml")).expect("read live manifest");
        let tighter_manifest = live_manifest.replacen("processes = 61", "processes = 1", 1);

        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "git", &["init", "-q", "-b", "main"]);
        fs::write(tmp.path().join("README.md"), "init\n").expect("write readme");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "init without manifest"]);

        fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        fs::write(tmp.path().join("tests/perf.toml"), &tighter_manifest)
            .expect("write tighter perf.toml");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "add tighter perf.toml"]);
        let tighter_sha = git_head(tmp.path());

        let zero = "0000000000000000000000000000000000000000";
        let new_zero = spawn_baseline_child(tmp.path(), "new_zero", zero);
        assert!(
            new_zero.status.success(),
            "all-zero SHA when manifest is new must pass:\n{}",
            String::from_utf8_lossy(&new_zero.stderr)
        );

        fs::write(tmp.path().join("tests/perf.toml"), &live_manifest)
            .expect("write live perf.toml");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "loosen perf.toml"]);

        let tighter_out = spawn_baseline_child(tmp.path(), "tighter", &tighter_sha);
        assert!(!tighter_out.status.success(), "tighter PERF_BASELINE_REV must fail the child");
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&tighter_out.stdout),
            String::from_utf8_lossy(&tighter_out.stderr)
        );
        assert!(
            combined.contains("processes 61 is looser than the baseline 1"),
            "expected looser-than-baseline error, got:\n{combined}"
        );

        let existing_zero = spawn_baseline_child(tmp.path(), "new_zero", zero);
        assert!(
            !existing_zero.status.success(),
            "all-zero SHA when manifest already existed in parent must fail loudly"
        );
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

    #[test]
    #[should_panic(expected = "read_dir")]
    fn walk_frames_and_routes_panics_when_read_dir_fails() {
        let mut ops = BTreeSet::new();
        walk_frames_and_routes(
            Path::new("/nonexistent/branchdiff-perf-surface-missing-dir"),
            &mut ops,
        );
    }

    #[test]
    fn raf_or_short_timer_added_to_src_html_rs_fails_the_guard() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let manifest = fs::read_to_string(root.join("tests/perf.toml")).expect("read perf.toml");
        let html_rs = fs::read_to_string(root.join("src/html.rs")).expect("read src/html.rs");
        let mut tests = BTreeSet::new();
        super::super::guard::collect_test_names(&root.join("tests"), &mut tests);
        super::super::guard::collect_test_names(&root.join("src"), &mut tests);
        let bench_text =
            fs::read_to_string(root.join("benches/baselines.toml")).expect("read baselines");
        let benches = super::super::guard::must_recorded_benches(&bench_text);

        for snippet in [
            "const RAF: &str = \"requestAnimationFrame(step)\";",
            "const TIMER: &str = \"setInterval(step, 16)\";",
        ] {
            let mut surface = operations();
            let modified = format!("{html_rs}\n{snippet}\n");
            scan_source_file(&root.join("src/html.rs"), &modified, &mut surface);
            let errors = super::super::guard::check(&super::super::guard::Input {
                manifest: &manifest,
                surface: &surface,
                tests: &tests,
                baseline: None,
                benches: &benches,
            });
            assert!(
                errors.iter().any(|err| err.contains("frame:html") && err.contains("no budget")),
                "snippet `{snippet}` in src/html.rs must fail the guard: {errors:?}"
            );
        }
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

    fn bench_records(text: &str) -> BTreeMap<String, crate::guard::baselines::BenchRecord> {
        crate::guard::baselines::parse(text)
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
        crate::guard::check_baseline_medians(LOOSE_BENCHES, Some(text), &mut errors);
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

    fn parsed_op(text: &str, id: &str) -> crate::guard::Op {
        let ops = crate::guard::parse_operations(text)
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
            let errors = crate::guard::check(&crate::guard::Input {
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
