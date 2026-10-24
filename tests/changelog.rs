use std::path::Path;
use std::process::Command;

fn released_sections(changelog: &str) -> Vec<(&str, Vec<&str>)> {
    let mut sections: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in changelog.lines() {
        if let Some(rest) = line.strip_prefix("## [") {
            if !rest.starts_with("unreleased]") {
                sections.push((line, Vec::new()));
            }
        } else if let Some((_, body)) = sections.last_mut() {
            body.push(line);
        }
    }
    while let Some((_, body)) = sections.last_mut() {
        if body.last().is_some_and(|l| l.is_empty()) {
            body.pop();
        } else {
            break;
        }
    }
    sections
}

fn verify_changelog(base_changelog: Option<&str>, current_changelog: &str) -> Result<(), String> {
    let lines: Vec<&str> = current_changelog.lines().collect();
    if lines.first().copied() != Some("## [unreleased]") {
        return Err("CHANGELOG.md must start with `## [unreleased]`".to_string());
    }

    // Ensure the first released `## [x.y.z]` header after `## [unreleased]` is preceded by a blank line.
    if let Some(first_released_idx) =
        lines.iter().enumerate().skip(1).find_map(|(i, l)| l.starts_with("## [").then_some(i))
        && !lines[first_released_idx - 1].is_empty()
    {
        return Err(format!(
            "missing blank line before `{}` on line {}",
            lines[first_released_idx],
            first_released_idx + 1
        ));
    }

    let current_sections = released_sections(current_changelog);
    if !current_sections.iter().any(|(hdr, _)| hdr.starts_with("## [0.1.0]")) {
        return Err("CHANGELOG.md is missing initial release section `## [0.1.0]`".to_string());
    }

    if let Some(base) = base_changelog {
        let base_sections = released_sections(base);
        for (base_hdr, base_body) in base_sections {
            let Some((_, cur_body)) = current_sections.iter().find(|(hdr, _)| *hdr == base_hdr)
            else {
                return Err(format!(
                    "CHANGELOG.md lost released section `{base_hdr}` present on main@origin"
                ));
            };
            let trimmed_cur: &[&str] = match cur_body.iter().rposition(|l| !l.is_empty()) {
                Some(last) => &cur_body[..=last],
                None => &[],
            };
            let trimmed_base: &[&str] = match base_body.iter().rposition(|l| !l.is_empty()) {
                Some(last) => &base_body[..=last],
                None => &[],
            };
            if trimmed_cur != trimmed_base {
                return Err(format!(
                    "CHANGELOG.md modified or truncated released section `{base_hdr}` from main@origin"
                ));
            }
        }
    }

    Ok(())
}

fn load_main_origin_changelog(repo_root: &Path) -> Option<String> {
    for rev in ["main@origin", "main"] {
        if let Ok(out) = Command::new("jj")
            .args(["--ignore-working-copy", "file", "show", "-r", rev, "CHANGELOG.md"])
            .current_dir(repo_root)
            .output()
            && out.status.success()
            && let Ok(text) = String::from_utf8(out.stdout)
            && !text.is_empty()
        {
            return Some(text);
        }
    }
    for spec in ["origin/main:CHANGELOG.md", "main:CHANGELOG.md"] {
        if let Ok(out) = Command::new("git").args(["show", spec]).current_dir(repo_root).output()
            && out.status.success()
            && let Ok(text) = String::from_utf8(out.stdout)
            && !text.is_empty()
        {
            return Some(text);
        }
    }
    None
}

#[test]
fn changelog_retains_all_released_sections_from_main_origin() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let current = std::fs::read_to_string(repo_root.join("CHANGELOG.md")).expect("CHANGELOG.md");
    let base = load_main_origin_changelog(repo_root);
    if let Err(msg) = verify_changelog(base.as_deref(), &current) {
        panic!("{msg}");
    }
}

#[test]
fn verify_changelog_catches_deleted_section_and_missing_blank_line() {
    let base = "\
## [unreleased]

- next change

## [0.78.0] - 2026-10-24

- feature a
## [0.1.0] - 2026-05-19

- initial
";
    let valid = "\
## [unreleased]

- brand new

## [0.78.1] - 2026-10-24

- next change
## [0.78.0] - 2026-10-24

- feature a
## [0.1.0] - 2026-05-19

- initial
";
    assert!(verify_changelog(Some(base), valid).is_ok());

    let missing_section = "\
## [unreleased]

- brand new

## [0.78.1] - 2026-10-24

- next change
## [0.1.0] - 2026-05-19

- initial
";
    let err = verify_changelog(Some(base), missing_section).unwrap_err();
    assert!(err.contains("## [0.78.0] - 2026-10-24"), "got: {err}");

    let no_blank_line = "\
## [unreleased]

- brand new
## [0.78.0] - 2026-10-24

- feature a
## [0.1.0] - 2026-05-19

- initial
";
    let err = verify_changelog(Some(base), no_blank_line).unwrap_err();
    assert!(err.contains("missing blank line"), "got: {err}");
}
