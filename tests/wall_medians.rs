//! Measured wall medians against the `ref_median_ns` pins.
//!
//! `cargo bench --bench performance` leaves Criterion's estimates under
//! `target/criterion`. With `PERF_WALL_CHECK=1` the test below reads them and
//! fails a bench that runs slower, relative to the rest, than its pin says.
//!
//! The pins come from one reference machine and a CI runner is a different,
//! noisier one, so absolute times cannot be compared. Every bench's
//! measured/pinned ratio is instead divided by the median ratio across all
//! benches (how much slower or faster this machine is overall). A bench whose
//! normalised ratio passes `1 + WALL_TOLERANCE_PERCENT / 100` regressed against
//! its siblings. A pin lowered without a matching measurement fails the same
//! test, since its bench then measures slower than the pin allows. A uniform
//! slowdown moves every ratio and the median alike, so it is not caught here;
//! that is the cost of being independent of the machine.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// How far past the machine-normalised pin a bench may measure. Microbenchmarks
/// on a shared runner swing by tens of percent; a doubling is a regression.
const WALL_TOLERANCE_PERCENT: u64 = 100;

/// A bench of a few nanoseconds swings by more than its own size between CPU
/// generations, so a bench must also exceed its limit by this much to fail.
const ABSOLUTE_SLACK_NS: f64 = 10.0;

/// Fewest benches that make the median ratio a machine speed and not one bench.
const MIN_BENCHES: usize = 8;

/// Benches whose normalised ratio is out of tolerance, or a reason the check
/// cannot be made.
fn violations(pins_ns: &BTreeMap<String, u64>, measured_ns: &BTreeMap<String, f64>) -> Vec<String> {
    let mut errors = Vec::new();
    let mut ratios: Vec<(&str, f64, f64)> = Vec::new();
    for (id, &pin) in pins_ns {
        match measured_ns.get(id) {
            Some(&measured) if pin > 0 => ratios.push((id, measured / pin as f64, pin as f64)),
            Some(_) => errors.push(format!("bench `{id}` pins a zero median")),
            None => errors.push(format!("bench `{id}` has a pin but no measurement")),
        }
    }
    if ratios.len() < MIN_BENCHES {
        errors.push(format!("only {} benches measured; need {MIN_BENCHES}", ratios.len()));
        return errors;
    }
    let mut sorted: Vec<f64> = ratios.iter().map(|(_, ratio, _)| *ratio).collect();
    sorted.sort_by(f64::total_cmp);
    let speed = sorted[sorted.len() / 2];
    let limit = 1.0 + WALL_TOLERANCE_PERCENT as f64 / 100.0;
    for (id, ratio, pin) in ratios {
        let normalised = ratio / speed;
        let excess_ns = (ratio - limit * speed) * pin;
        if normalised > limit && excess_ns > ABSOLUTE_SLACK_NS {
            errors.push(format!(
                "bench `{id}` measures {normalised:.2}x its pin relative to this machine's {speed:.2}x (limit {limit:.2}x)"
            ));
        }
    }
    errors
}

/// Criterion median point estimates in nanoseconds, by full bench id.
fn measured_medians(criterion_dir: &Path) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    collect(criterion_dir, &mut out);
    out
}

fn collect(dir: &Path, out: &mut BTreeMap<String, f64>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.file_name().is_some_and(|name| name == "new") {
            if let Some((id, median)) = read_estimate(&path) {
                out.insert(id, median);
            }
            continue;
        }
        collect(&path, out);
    }
}

fn read_estimate(new_dir: &Path) -> Option<(String, f64)> {
    let benchmark: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(new_dir.join("benchmark.json")).ok()?).ok()?;
    let estimates: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(new_dir.join("estimates.json")).ok()?).ok()?;
    let id = benchmark.get("full_id")?.as_str()?.to_string();
    let median = estimates.get("median")?.get("point_estimate")?.as_f64()?;
    Some((id, median))
}

fn criterion_dir(root: &Path) -> PathBuf {
    if let Some(dir) = std::env::var_os("CRITERION_HOME") {
        return PathBuf::from(dir);
    }
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(target) => PathBuf::from(target).join("criterion"),
        None => root.join("target/criterion"),
    }
}

fn pins(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
    pairs.iter().map(|(id, ns)| ((*id).to_string(), *ns)).collect()
}

fn measured(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
    pairs.iter().map(|(id, ns)| ((*id).to_string(), *ns)).collect()
}

fn ids() -> Vec<String> {
    (0..MIN_BENCHES).map(|n| format!("b{n}")).collect()
}

fn uniform(speed: f64) -> (BTreeMap<String, u64>, BTreeMap<String, f64>) {
    let pin: BTreeMap<String, u64> = ids().into_iter().map(|id| (id, 1_000)).collect();
    let got = pin.iter().map(|(id, ns)| (id.clone(), *ns as f64 * speed)).collect();
    (pin, got)
}

#[test]
fn a_machine_uniformly_slower_or_faster_passes() {
    for speed in [0.3, 1.0, 4.0] {
        let (pin, got) = uniform(speed);
        assert_eq!(violations(&pin, &got), Vec::<String>::new(), "speed {speed}");
    }
}

#[test]
fn one_bench_past_the_tolerance_fails_and_one_inside_passes() {
    let (pin, mut got) = uniform(1.0);
    got.insert("b3".to_string(), 1_900.0);
    assert!(violations(&pin, &got).is_empty());
    got.insert("b3".to_string(), 2_100.0);
    let errors = violations(&pin, &got);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("`b3`"), "{errors:?}");
}

#[test]
fn a_pin_lowered_without_a_measurement_fails() {
    let (mut pin, got) = uniform(1.0);
    pin.insert("b5".to_string(), 400);
    let errors = violations(&pin, &got);
    assert!(errors.iter().any(|error| error.contains("`b5`")), "{errors:?}");
}

#[test]
fn a_pin_with_no_measurement_or_too_few_benches_fails() {
    let (pin, mut got) = uniform(1.0);
    got.remove("b2");
    assert!(
        violations(&pin, &got).iter().any(|e| e.contains("`b2`") && e.contains("no measurement"))
    );
    let few = violations(&pins(&[("a", 10)]), &measured(&[("a", 10.0)]));
    assert!(few.iter().any(|error| error.contains("need")), "{few:?}");
}

#[test]
fn a_bench_of_a_few_nanoseconds_may_swing_within_the_absolute_slack() {
    let (mut pin, mut got) = uniform(1.0);
    pin.insert("b1".to_string(), 2);
    got.insert("b1".to_string(), 7.0);
    assert!(violations(&pin, &got).is_empty());
    got.insert("b1".to_string(), 40.0);
    assert!(violations(&pin, &got).iter().any(|error| error.contains("`b1`")));
}

#[test]
fn a_zero_pin_is_refused() {
    let (mut pin, got) = uniform(1.0);
    pin.insert("b0".to_string(), 0);
    assert!(violations(&pin, &got).iter().any(|error| error.contains("zero")));
}

#[test]
fn criterion_estimates_are_read_by_full_id() {
    let temp = tempfile_dir();
    let new = temp.join("navigation/go_to_top/new");
    fs::create_dir_all(&new).unwrap();
    fs::write(new.join("benchmark.json"), r#"{"full_id":"navigation/go_to_top"}"#).unwrap();
    fs::write(new.join("estimates.json"), r#"{"median":{"point_estimate":123.5}}"#).unwrap();
    let found = measured_medians(&temp);
    assert_eq!(found.get("navigation/go_to_top"), Some(&123.5));
    fs::remove_dir_all(&temp).unwrap();
}

fn tempfile_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wall-medians-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The check CI runs after `cargo bench --bench performance`. Without
/// `PERF_WALL_CHECK=1` there are no fresh estimates to read and it does nothing.
#[test]
fn measured_wall_medians_hold_the_pins() {
    if std::env::var_os("PERF_WALL_CHECK").is_none_or(|value| value != "1") {
        return;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let text = fs::read_to_string(root.join("benches/baselines.toml")).expect("read baselines");
    let pins = recorded_pins(&text);
    let measured = measured_medians(&criterion_dir(&root));
    let errors = violations(&pins, &measured);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// `ref_median_ns` per bench id, leaving out records marked `removed`.
fn recorded_pins(text: &str) -> BTreeMap<String, u64> {
    let mut pins = BTreeMap::new();
    let mut id: Option<String> = None;
    let mut removed = false;
    for line in text.lines().map(str::trim) {
        if line == "[[bench]]" {
            id = None;
            removed = false;
        } else if let Some(value) = line.strip_prefix("id = ") {
            id = Some(value.trim_matches('"').to_string());
        } else if line.starts_with("removed =") {
            removed = true;
            if let Some(id) = &id {
                pins.remove(id);
            }
        } else if let Some(value) = line.strip_prefix("ref_median_ns = ")
            && !removed
            && let (Some(id), Ok(ns)) = (&id, value.trim().parse::<u64>())
        {
            pins.insert(id.clone(), ns);
        }
    }
    pins
}

#[test]
fn ci_runs_the_wall_bench_and_its_comparison_as_blocking_steps() {
    let ci = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/.github/workflows/ci.yml"))
        .expect("read ci.yml");
    assert!(ci.contains("--bench performance"), "no wall bench step");
    assert!(ci.contains("PERF_WALL_CHECK: \"1\""), "no wall comparison step");
    assert!(ci.contains("--test wall_medians"), "no wall comparison step");
    assert!(!ci.contains("continue-on-error"), "no CI step may be advisory");
}

#[test]
fn pins_are_read_from_baselines_and_removed_records_are_left_out() {
    let text = "[[bench]]\nid = \"a/b\"\nref_median_ns = 5\n\n[[bench]]\nid = \"gone\"\nref_median_ns = 9\nremoved = \"why\"\n\n[[bench]]\nid = \"alloc/only\"\nmax_blocks = 1\n";
    assert_eq!(recorded_pins(text), pins(&[("a/b", 5)]));
    let real =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/benches/baselines.toml")).unwrap();
    assert!(recorded_pins(&real).len() >= MIN_BENCHES);
}
