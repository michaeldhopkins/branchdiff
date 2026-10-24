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

use std::collections::BTreeSet;
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
    let benches = guard::recorded_benches(&bench_text);
    let baseline = surface::committed_baseline();
    let errors = guard::check(&guard::Input {
        manifest: &manifest,
        surface: &surface,
        tests: &tests,
        baseline: baseline.as_deref(),
        benches: &benches,
    });
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
    let mut benches = guard::recorded_benches(&bench_text);
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
