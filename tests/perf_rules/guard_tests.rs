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
    let empty = HAPPY
        .replace("derivation = \"One external call plus the class margin.\"", "derivation = \"\"");
    let errors = run(Case {
        manifest: &empty,
        surface: &["command:print"],
        tests: &["tests/counts.rs::counts"],
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
        tests: &["tests/counts.rs::counts"],
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
    assert!(!recorded_benches(documented_removal).contains_key("refresh/old"));

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
