//! An exclusion in `.cargo/mutants.toml` (`exclude_re`, `skip_calls`) or a
//! `#[mutants::skip]` needs a `tests/mutants_triage.toml` entry whose `pattern`
//! is an id from the rust-mutation-testing catalog. `untriaged_exclude_re` in
//! `tests/ratchets.toml` is how many config exclusions still have no entry; it
//! only falls.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde::Deserialize;

/// Ids of the mutation-pattern catalog. A new id is
/// added to the catalog first; this list is the catalog the test accepts.
const CATALOG: &[&str] = &[
    "tolerance-band", "derived-expectation", "sentinel-collision", "upper-bound-only",
    "redundant-guard", "log-in-guard", "hint-arg", "stdout-only", "cfg-twin", "copy-twin",
    "library-default", "hang-guard", "is-terminal", "unreachable",
];

#[derive(Debug, Deserialize)]
struct MutantsConfig {
    #[serde(default)]
    exclude_re: Vec<String>,
    #[serde(default)]
    skip_calls: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TriageFile {
    #[serde(default)]
    equivalent: Vec<TriageEntry>,
}

#[derive(Debug, Deserialize)]
struct TriageEntry {
    pattern: String,
    exclude: String,
    reason: String,
}

fn catalog_has(id: &str) -> bool {
    CATALOG.contains(&id)
}

/// Config exclusions that no triage `exclude` string matches exactly.
fn untriaged<'a>(exclusions: &'a [String], triaged: &BTreeSet<&str>) -> Vec<&'a str> {
    exclusions.iter().filter(|item| !triaged.contains(item.as_str())).map(String::as_str).collect()
}

fn entry_problems(entries: &[TriageEntry]) -> Vec<String> {
    let mut problems = Vec::new();
    for entry in entries {
        if !catalog_has(&entry.pattern) {
            problems.push(format!("pattern id {:?} is not in the catalog", entry.pattern));
        }
        if entry.reason.trim().is_empty() {
            problems.push(format!("empty reason for {}", entry.exclude));
        }
        if entry.exclude.trim().is_empty() {
            problems.push("empty exclude string".to_string());
        }
    }
    problems
}

fn pin(root: &Path) -> usize {
    let raw = fs::read_to_string(root.join("tests/ratchets.toml")).unwrap();
    for line in raw.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(rest) = line.strip_prefix("untriaged_exclude_re") {
            let value = rest.trim().trim_start_matches('=').trim();
            return value.parse().expect("untriaged_exclude_re is a number");
        }
    }
    panic!("tests/ratchets.toml is missing untriaged_exclude_re");
}

fn skip_attributes(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    collect_skips(&root.join("src"), root, &mut found);
    found
}

fn collect_skips(dir: &Path, root: &Path, found: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            collect_skips(&path, root, found);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        if text.lines().any(|line| {
            let code = line.split("//").next().unwrap_or("");
            code.contains("#[mutants::skip")
        }) {
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            found.push(rel);
        }
    }
}

#[test]
fn an_exclusion_without_an_entry_is_untriaged() {
    let exclusions = vec!["^src/new.rs:.*replace < with <=$".to_string()];
    let gaps = untriaged(&exclusions, &BTreeSet::new());
    assert_eq!(gaps, vec!["^src/new.rs:.*replace < with <=$"]);
    let triaged = BTreeSet::from([exclusions[0].as_str()]);
    assert!(untriaged(&exclusions, &triaged).is_empty());
}

#[test]
fn an_unknown_pattern_id_is_rejected() {
    assert!(catalog_has("unreachable"));
    assert!(!catalog_has("impossible-tie"));
    let problems = entry_problems(&[TriageEntry {
        pattern: "impossible-tie".into(),
        exclude: "^src/x.rs:.*$".into(),
        reason: "not a catalog id".into(),
    }]);
    assert!(problems.iter().any(|p| p.contains("impossible-tie")), "{problems:?}");
    let ok = entry_problems(&[TriageEntry {
        pattern: "unreachable".into(),
        exclude: "^src/x.rs:.*$".into(),
        reason: "the tie cannot happen".into(),
    }]);
    assert!(ok.is_empty(), "{ok:?}");
}

#[test]
fn configured_exclusions_are_triaged_or_pinned() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mutants: MutantsConfig =
        toml::from_str(&fs::read_to_string(root.join(".cargo/mutants.toml")).unwrap()).unwrap();
    let triage: TriageFile =
        toml::from_str(&fs::read_to_string(root.join("tests/mutants_triage.toml")).unwrap())
            .unwrap();
    let problems = entry_problems(&triage.equivalent);
    assert!(problems.is_empty(), "{problems:?}");

    let triaged: BTreeSet<&str> = triage.equivalent.iter().map(|e| e.exclude.as_str()).collect();
    for entry in &triage.equivalent {
        let configured = mutants.exclude_re.iter().any(|item| item == &entry.exclude)
            || mutants.skip_calls.iter().any(|item| item == &entry.exclude);
        assert!(configured, "triage exclude is not an exclusion: {}", entry.exclude);
    }

    let mut exclusions = mutants.exclude_re.clone();
    exclusions.extend(mutants.skip_calls.iter().cloned());
    let gaps = untriaged(&exclusions, &triaged);
    let allowed = pin(root);
    assert_eq!(
        gaps.len(),
        allowed,
        "untriaged exclusions are {gaps:?}; pin is {allowed}. Triage a new one in tests/mutants_triage.toml, or lower untriaged_exclude_re when one gains an entry"
    );

    let skips = skip_attributes(root);
    for file in &skips {
        assert!(
            triage.equivalent.iter().any(|e| e.exclude.contains(file.as_str())),
            "{file} has #[mutants::skip] and no triage entry names it"
        );
    }
}
