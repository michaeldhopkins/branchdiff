//! Performance budgets.
//!
//! `guard` is the project-agnostic checker. A later copy can live with the
//! shared templates. `surface` and `counts` are this program's adapter: the
//! command tree, the key map, the html page, and the process-count floors.

#[path = "perf_rules/counts.rs"]
mod counts;
#[path = "perf_rules/guard.rs"]
mod guard;
#[path = "perf_rules/surface.rs"]
mod surface;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

#[test]
fn the_manifest_matches_the_surface_and_its_budgets() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("tests/perf.toml")).expect("read tests/perf.toml");
    let surface = surface::operations();
    assert_eq!(surface::operations(), surface, "surface discovery is not stable");
    for prefix in ["command:", "key:", "frame:"] {
        assert!(
            surface.iter().any(|id| id.starts_with(prefix)),
            "surface must contain at least one `{prefix}` operation"
        );
    }
    let mut tests = BTreeSet::new();
    guard::collect_test_names(&root.join("tests"), &mut tests);
    guard::collect_test_names(&root.join("src"), &mut tests);
    assert!(!tests.is_empty(), "collected test names must not be empty");
    let bench_text =
        fs::read_to_string(root.join("benches/baselines.toml")).expect("read baselines");
    let benches = guard::must_recorded_benches(&bench_text);
    let mut errors = Vec::new();
    let baseline = surface::recorded_baseline(surface::committed_baseline(), &mut errors);
    let baseline_bench_text =
        surface::recorded_baseline(surface::committed_baseline_benches(), &mut errors);
    errors.extend(guard::check(&guard::Input {
        manifest: &manifest,
        surface: &surface,
        tests: &tests,
        baseline: baseline.as_deref(),
        benches: &benches,
    }));
    guard::check_baseline_medians(&bench_text, baseline_bench_text.as_deref(), &mut errors);
    assert!(
        errors.is_empty(),
        "{}\n\nsurface:\n{}",
        errors.join("\n"),
        surface.into_iter().collect::<Vec<_>>().join("\n")
    );
    for op in guard::parse_operations(&manifest).expect("manifest parses") {
        let prefix = match op.class.as_str() {
            "command" => "command:",
            "keystroke" => "key:",
            "page" => "page:",
            "frame" => "frame:",
            "mcp" => "mcp:",
            other => panic!("operation `{}` has unknown class {other}", op.id),
        };
        assert!(op.id.starts_with(prefix), "{} is class {}", op.id, op.class);
        if op.class == "command" && op.processes > 0 {
            assert!(
                !op.benches.is_empty() && !op.owed,
                "{} spawns {} processes and must have a measured bench floor",
                op.id,
                op.processes,
            );
        }
        if op.id == "command:branchdiff" {
            assert!(
                op.derivation.contains("--benchmark 1"),
                "command:branchdiff derivation must note --benchmark 1: {}",
                op.derivation
            );
        }
        if op.class == "page" {
            assert!(
                op.derivation.contains("CLI output"),
                "{} derivation must note CLI output: {}",
                op.id,
                op.derivation
            );
        }
    }
}

#[test]
fn fingerprint_speedup_to_40ms_without_proposal_passes_and_10_percent_above_bench_fails() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("tests/perf.toml")).expect("read tests/perf.toml");
    let surface = surface::operations();
    let mut tests = BTreeSet::new();
    guard::collect_test_names(&root.join("tests"), &mut tests);
    guard::collect_test_names(&root.join("src"), &mut tests);
    let bench_text =
        fs::read_to_string(root.join("benches/baselines.toml")).expect("read baselines");
    let baseline_benches = guard::must_recorded_benches(&bench_text);
    let mut benches = baseline_benches.clone();
    benches.insert("refresh/fingerprint/500f_25c".to_string(), 40_000);

    let sped_up: String = manifest
        .lines()
        .filter(|line| !line.starts_with("proposal = \"--fingerprint skips per-file blob reads"))
        .collect::<Vec<_>>()
        .join("\n")
        .replace(
            "wall_floor_us = 55805\ntarget_us = 50000",
            "wall_floor_us = 40000\ntarget_us = 40000",
        );

    let errors = guard::check(&guard::Input {
        manifest: &sped_up,
        surface: &surface,
        tests: &tests,
        baseline: Some(&manifest),
        benches: &benches,
    });
    assert!(errors.is_empty(), "40 ms fingerprint with no proposal must pass: {errors:?}");

    let floated = sped_up.replace(
        "wall_floor_us = 40000\ntarget_us = 40000",
        "wall_floor_us = 44000\ntarget_us = 40000",
    );
    let errors = guard::check(&guard::Input {
        manifest: &floated,
        surface: &surface,
        tests: &tests,
        baseline: Some(&manifest),
        benches: &benches,
    });
    assert!(
        errors.iter().any(|err| err.contains("command:fingerprint") && err.contains("44000")),
        "floor 10% above bench must fail: {errors:?}"
    );
}

#[test]
fn raising_non_colocated_bench_median_or_deleting_files_bench_from_derivation_fails() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("tests/perf.toml")).expect("read tests/perf.toml");
    let surface = surface::operations();
    let mut tests = BTreeSet::new();
    guard::collect_test_names(&root.join("tests"), &mut tests);
    guard::collect_test_names(&root.join("src"), &mut tests);
    let bench_text =
        fs::read_to_string(root.join("benches/baselines.toml")).expect("read baselines");
    let benches = guard::must_recorded_benches(&bench_text);

    // 1. Raising refresh/non_colocated/50f_10c ref_median_ns 73175330 -> 130000000 must fail.
    let raised_bench_text =
        bench_text.replace("ref_median_ns = 73175330", "ref_median_ns = 130000000");
    assert_ne!(raised_bench_text, bench_text, "refresh/non_colocated/50f_10c median must exist");
    let raised_benches = guard::must_recorded_benches(&raised_bench_text);
    let mut raised_errors = guard::check(&guard::Input {
        manifest: &manifest,
        surface: &surface,
        tests: &tests,
        baseline: Some(&manifest),
        benches: &raised_benches,
    });
    guard::check_baseline_medians(&raised_bench_text, Some(&bench_text), &mut raised_errors);
    assert!(
        raised_errors
            .iter()
            .any(|err| err.contains("refresh/non_colocated/50f_10c") && err.contains("looser")),
        "raising refresh/non_colocated/50f_10c ref_median_ns 73175330 -> 130000000 must fail: {raised_errors:?}"
    );

    // 2. Deleting a bench from command:files' derivation while lowering its floor must fail.
    let stripped_derivation = manifest
        .replace(
            "Floor is refresh/colocated/500f_25c (131542 us) plus refresh/files_head/500f_25c (10499 us).",
            "Floor is refresh/colocated/500f_25c (131542 us).",
        )
        .replace("wall_floor_us = 142041", "wall_floor_us = 131542");
    assert_ne!(stripped_derivation, manifest);
    let files_errors = guard::check(&guard::Input {
        manifest: &stripped_derivation,
        surface: &surface,
        tests: &tests,
        baseline: Some(&manifest),
        benches: &benches,
    });
    assert!(
        files_errors.iter().any(|err| err.contains("command:files")
            && err.contains("131542")
            && err.contains("142041")),
        "deleting a bench from command:files' derivation while lowering its floor must fail: {files_errors:?}"
    );
}

/// A keystroke that has a bench of its own must name that bench, not borrow a
/// sibling's median. `key:go-to-top` once bound `navigation/go_to_bottom` on an
/// unmeasured claim of symmetry.
#[test]
fn keystrokes_with_their_own_bench_are_bound_to_it() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("tests/perf.toml")).expect("read tests/perf.toml");
    let ops = guard::parse_operations(&manifest).expect("manifest parses");
    for (id, bench) in [
        ("key:go-to-top", "navigation/go_to_top"),
        ("key:go-to-bottom", "navigation/go_to_bottom"),
        ("key:next-file", "navigation/next_file"),
    ] {
        let op = ops.iter().find(|op| op.id == id).unwrap_or_else(|| panic!("{id} is budgeted"));
        assert!(!op.owed, "{id} must be measured");
        assert!(op.benches.iter().any(|name| name == bench), "{id} must name {bench}");
        assert!(
            !op.alternatives.iter().any(|name| name == bench),
            "{id} must not list {bench} as an alternative"
        );
    }
}

/// Page, file, go-to, and view-mode motions invalidate inline spans. The span
/// bench is a later stage of the same draw, added to the motion bench.
#[test]
fn navigation_floors_include_the_span_stage() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("tests/perf.toml")).expect("read tests/perf.toml");
    let ops = guard::parse_operations(&manifest).expect("manifest parses");
    let bench_text =
        fs::read_to_string(root.join("benches/baselines.toml")).expect("read baselines");
    let benches = guard::must_recorded_benches(&bench_text);
    let span = "inline_spans/ensure_visible_1200_candidates";
    let span_us = benches[span];
    for (id, motion) in [
        ("key:scroll-up", "navigation/scroll_down_10"),
        ("key:scroll-down", "navigation/scroll_down_10"),
        ("key:page-up", "navigation/page_down"),
        ("key:page-down", "navigation/page_down"),
        ("key:go-to-top", "navigation/go_to_top"),
        ("key:go-to-bottom", "navigation/go_to_bottom"),
        ("key:next-file", "navigation/next_file"),
        ("key:prev-file", "navigation/next_file"),
        ("key:cycle-view-mode", "view_mode/cycle_view_mode"),
    ] {
        let op = ops.iter().find(|op| op.id == id).unwrap_or_else(|| panic!("{id} is budgeted"));
        assert_eq!(op.floor, "sum", "{id}");
        assert!(op.benches.iter().any(|name| name == motion), "{id} must name {motion}");
        assert!(op.benches.iter().any(|name| name == span), "{id} must name {span}");
        assert!(
            !op.alternatives.iter().any(|name| name == motion || name == span),
            "{id} must count {motion} and {span}"
        );
        assert_eq!(op.wall_floor_us, benches[motion] + span_us, "{id}");
        let floors: BTreeMap<_, _> = op
            .stage_floors
            .iter()
            .map(|entry| {
                let (name, us) =
                    entry.split_once('=').unwrap_or_else(|| panic!("{id} stage floor {entry}"));
                (name, us.parse::<u64>().unwrap_or_else(|_| panic!("{id} stage floor {entry}")))
            })
            .collect();
        assert_eq!(
            floors.get(motion).copied(),
            Some(benches[motion]),
            "{id} motion stage: {floors:?}"
        );
        assert_eq!(floors.get(span).copied(), Some(span_us), "{id} span stage: {floors:?}");
        assert_eq!(floors.len(), 2, "{id} stages: {floors:?}");
    }
}
