//! The outputs that skip rendering a diff: `--files` and `--fingerprint`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Result;

use crate::cli::OutputMode;
use crate::file_list::{FileList, entries, render_json, render_text};
use crate::fingerprint::fingerprint;
use crate::vcs::Vcs;

/// The text for `--files` or `--fingerprint`, or `None` for every other mode.
pub fn render(vcs: &dyn Vcs, mode: OutputMode, json: bool) -> Result<Option<String>> {
    match mode {
        OutputMode::Fingerprint => {
            let inputs = vcs.fingerprint_inputs()?;
            let hash = fingerprint(&inputs, |path| vcs.working_file_bytes(path).ok().flatten());
            Ok(Some(format!("{hash}\n")))
        }
        OutputMode::Files => {
            let refresh = vcs.refresh(&Arc::new(AtomicBool::new(false)))?;
            let list = FileList {
                base: refresh.base_identifier.clone(),
                head: vcs.current_revision_id()?,
                files: entries(&refresh.files),
            };
            Ok(Some(if json { render_json(&list) } else { render_text(&list) }))
        }
        _ => Ok(None),
    }
}

/// Print the listing for `mode` if it is one; whether it was.
pub fn print(vcs: &dyn Vcs, mode: OutputMode, json: bool) -> Result<bool> {
    let Some(text) = render(vcs, mode, json)? else { return Ok(false) };
    print!("{text}");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn run(dir: &Path, program: &str, args: &[&str]) {
        let out = Command::new(program).args(args).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "{program} {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn available(program: &str) -> bool {
        Command::new(program).arg("--version").output().is_ok_and(|o| o.status.success())
    }

    /// A git repo on `main` with one commit, then a feature branch with another.
    fn git_repo() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        run(d, "git", &["init", "-q", "-b", "main"]);
        run(d, "git", &["config", "user.email", "t@example.com"]);
        run(d, "git", &["config", "user.name", "t"]);
        std::fs::write(d.join("f.txt"), "a\n").unwrap();
        run(d, "git", &["add", "."]);
        run(d, "git", &["commit", "-q", "-m", "base"]);
        run(d, "git", &["checkout", "-q", "-b", "feature"]);
        std::fs::write(d.join("g.txt"), "b\nc\n").unwrap();
        run(d, "git", &["add", "."]);
        run(d, "git", &["commit", "-q", "-m", "feature"]);
        t
    }

    fn jj_repo() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        run(d, "jj", &["git", "init"]);
        std::fs::write(d.join("f.txt"), "a\n").unwrap();
        run(d, "jj", &["commit", "-m", "base"]);
        run(d, "jj", &["bookmark", "create", "main", "-r", "@-"]);
        std::fs::write(d.join("g.txt"), "b\nc\n").unwrap();
        run(d, "jj", &["commit", "-m", "feature"]);
        run(d, "jj", &["bookmark", "create", "feature", "-r", "@-"]);
        t
    }

    fn text(vcs: &dyn Vcs, mode: OutputMode, json: bool) -> String {
        render(vcs, mode, json).unwrap().expect("a listing mode")
    }

    #[test]
    fn other_modes_are_not_listings() {
        if !available("git") { return; }
        let t = git_repo();
        let vcs = crate::vcs::detect(t.path()).unwrap();
        for mode in [OutputMode::Tui, OutputMode::Print, OutputMode::Diff, OutputMode::Html] {
            assert!(render(vcs.as_ref(), mode, false).unwrap().is_none(), "{mode:?}");
        }
    }

    #[test]
    fn git_files_lists_committed_and_uncommitted_with_counts() {
        if !available("git") { return; }
        let t = git_repo();
        std::fs::write(t.path().join("h.txt"), "new\n").unwrap();
        let vcs = crate::vcs::detect(t.path()).unwrap();
        let out = text(vcs.as_ref(), OutputMode::Files, false);
        assert!(out.starts_with("# base "), "{out}");
        assert!(out.contains("added\t+2\t-0\tg.txt\n"), "{out}");
        assert!(out.contains("added\t+1\t-0\th.txt\n"), "{out}");
    }

    #[test]
    fn git_files_json_names_both_ids() {
        if !available("git") { return; }
        let t = git_repo();
        let vcs = crate::vcs::detect(t.path()).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&text(vcs.as_ref(), OutputMode::Files, true)).unwrap();
        assert!(!v["base"].as_str().unwrap().is_empty());
        assert!(!v["head"].as_str().unwrap().is_empty());
        assert_eq!(v["files"][0]["path"], "g.txt");
        assert_eq!(v["files"][0]["status"], "added");
    }

    #[test]
    fn git_fingerprint_moves_with_edits_staging_and_commits_and_otherwise_holds() {
        if !available("git") { return; }
        let t = git_repo();
        let d = t.path();
        let vcs = crate::vcs::detect(d).unwrap();
        let fp = || text(vcs.as_ref(), OutputMode::Fingerprint, false);

        let clean = fp();
        assert_eq!(clean, fp(), "polling twice must agree");
        assert_eq!(clean.trim().len(), 16);

        std::fs::write(d.join("g.txt"), "b\nCHANGED\n").unwrap();
        let edited = fp();
        assert_ne!(clean, edited, "a content edit");

        run(d, "git", &["add", "g.txt"]);
        let staged = fp();
        assert_ne!(edited, staged, "staging redraws the diff");

        run(d, "git", &["commit", "-q", "-m", "more"]);
        assert_ne!(staged, fp(), "a new commit");
    }

    #[test]
    fn jj_files_includes_the_feature_commit_and_uncommitted_edits() {
        if !available("jj") { return; }
        let t = jj_repo();
        std::fs::write(t.path().join("h.txt"), "dirty\n").unwrap();
        let vcs = crate::vcs::detect(t.path()).unwrap();
        let out = text(vcs.as_ref(), OutputMode::Files, false);
        assert!(out.contains("g.txt"), "{out}");
        assert!(out.contains("h.txt"), "{out}");
    }

    fn operation_count(repo: &Path) -> usize {
        let out = Command::new("jj")
            .args(["--ignore-working-copy", "op", "log", "--no-graph", "-T", "id.short() ++ \"\\n\""])
            .current_dir(repo).output().unwrap();
        String::from_utf8_lossy(&out.stdout).lines().count()
    }

    #[test]
    fn jj_fingerprint_tracks_edits_without_recording_an_operation() {
        if !available("jj") { return; }
        let t = jj_repo();
        let d = t.path();
        std::fs::write(d.join("h.txt"), "dirty\n").unwrap();
        let vcs = crate::vcs::detect(d).unwrap();
        let fp = || text(vcs.as_ref(), OutputMode::Fingerprint, false);
        let before = operation_count(d);

        let first = fp();
        assert_eq!(first, fp());
        std::fs::write(d.join("h.txt"), "dirtier\n").unwrap();
        let second = fp();
        std::fs::write(d.join("new.txt"), "x\n").unwrap();
        let third = fp();

        assert_ne!(first, second, "an edit to an uncommitted file");
        assert_ne!(second, third, "a new untracked file");
        assert_eq!(operation_count(d), before, "polling the fingerprint recorded a jj operation");
    }


    #[test]
    fn jj_fingerprint_in_a_secondary_workspace_tracks_edits_without_an_operation() {
        if !available("jj") { return; }
        let t = jj_repo();
        run(t.path(), "jj", &["edit", "feature"]);
        let parent = tempfile::tempdir().unwrap();
        let ws = parent.path().join("ws");
        run(t.path(), "jj", &["workspace", "add", ws.to_str().unwrap(), "-r", "feature"]);
        std::fs::write(ws.join("h.txt"), "one\n").unwrap();
        let vcs = crate::vcs::detect(&ws).unwrap();
        let before = operation_count(&ws);

        let a = text(vcs.as_ref(), OutputMode::Fingerprint, false);
        std::fs::write(ws.join("h.txt"), "two\n").unwrap();
        let b = text(vcs.as_ref(), OutputMode::Fingerprint, false);

        assert_ne!(a, b);
        assert_eq!(operation_count(&ws), before);
    }
    #[test]
    fn jj_fingerprint_moves_when_the_commit_does() {
        if !available("jj") { return; }
        let t = jj_repo();
        let d = t.path();
        let vcs = crate::vcs::detect(d).unwrap();
        let before = text(vcs.as_ref(), OutputMode::Fingerprint, false);
        std::fs::write(d.join("k.txt"), "k\n").unwrap();
        run(d, "jj", &["commit", "-m", "more"]);
        let vcs = crate::vcs::detect(d).unwrap();
        assert_ne!(before, text(vcs.as_ref(), OutputMode::Fingerprint, false));
    }
}
