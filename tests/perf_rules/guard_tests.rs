use super::*;

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
    let missing = Path::new("/nonexistent/branchdiff-perf-rules-missing-dir");
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
    check_baseline_medians(raised_toml, Some(base_toml), &mut raised_errors);
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
    check_baseline_medians(missing_toml, Some(base_toml), &mut missing_errors);
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
    check_baseline_medians(documented_removal, Some(base_toml), &mut removed_errors);
    assert!(removed_errors.is_empty(), "documented removal must pass: {removed_errors:?}");
    let active = recorded_benches(documented_removal).expect("documented removal parses");
    assert!(!active.contains_key("refresh/old"));

    // Empty `removed = ""` or combining `removed` with `ref_median_ns` must fail.
    let empty_removed = concat!(
        "[[bench]]\nid = \"refresh/non_colocated/50f_10c\"\nref_median_ns = 73175330\n",
        "[[bench]]\nid = \"refresh/old\"\nremoved = \"\"\n",
    );
    let mut empty_errors = Vec::new();
    check_baseline_medians(empty_removed, Some(base_toml), &mut empty_errors);
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
    check_baseline_medians(both_set, Some(base_toml), &mut both_errors);
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
    check_baseline_medians(alloc_raised, Some(alloc_base), &mut alloc_raised_errors);
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
    check_baseline_medians(alloc_dropped, Some(alloc_base), &mut alloc_dropped_errors);
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
        check_baseline_medians(live, Some(baseline), &mut errors);
        assert_eq!(
            errors,
            vec!["baseline benches/baselines.toml has no records"],
            "{baseline:?} must not let ref_median_ns = 999999 through"
        );
    }

    let mut absent = Vec::new();
    check_baseline_medians(live, None, &mut absent);
    assert!(
        absent.is_empty(),
        "a missing baselines file is the caller's error, not an empty parse: {absent:?}"
    );

    let mut both_empty = Vec::new();
    check_baseline_medians("", Some("# comment\n"), &mut both_empty);
    assert!(both_empty.is_empty(), "an empty live file has no pins to protect: {both_empty:?}");

    let prior = "[[bench]]\nid = \"navigation/next_file\"\nref_median_ns = 10\n";
    let mut raised = Vec::new();
    check_baseline_medians(live, Some(prior), &mut raised);
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
            check_baseline_medians(&text, None, &mut errors);
            assert!(
                errors.iter().any(|error| error.contains("bench `x` sets both")),
                "`{pin}` beside removed must fail: {errors:?}"
            );
        }
    }
    let mut errors = Vec::new();
    check_baseline_medians("[[bench]]\nid = \"x\"\nremoved = \"reason\"\n", None, &mut errors);
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
        check_baseline_medians(live, Some(base), &mut errors);
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
    check_baseline_medians(&with_ci("ci_median_ns = 4000\n"), Some(first_record), &mut adopted);
    assert!(adopted.is_empty(), "a first ci pin has no earlier value to exceed: {adopted:?}");
    let ci_alone = "[[bench]]\nid = \"x\"\nci_median_ns = 5\n";
    assert!(recorded_benches(ci_alone).is_err());
}
