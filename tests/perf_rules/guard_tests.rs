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
