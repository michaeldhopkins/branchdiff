//! Inputs for `--fingerprint`, read without snapshotting the working copy.

use super::*;
use crate::fingerprint::FingerprintInputs;

impl JjVcs {
    pub(super) fn gather_fingerprint_inputs(&self) -> Result<FingerprintInputs> {
        let cancel = Arc::new(AtomicBool::new(false));
        let target_rev = self.target_rev();
        let base_rev = self.base_rev();
        let fork_point =
            stack::resolve_fork_point_for(&self.repo_path, &self.from_rev, target_rev, &cancel);
        let effective_from = match self.load_diff_base() {
            DiffBase::ForkPoint => fork_point.as_deref().unwrap_or(base_rev.as_ref()),
            DiffBase::TrunkTip => base_rev.as_ref(),
        };
        let id_of = |rev: &str| {
            self.commit_id_of(rev).ok_or_else(|| anyhow::anyhow!("could not resolve {rev:?}"))
        };
        let base = id_of(effective_from)?;
        if let Some(head_rev) = &self.head_rev {
            let head = id_of(head_rev)?;
            let changed = self
                .get_changed_files_no_snapshot(effective_from, head_rev, &cancel)?
                .into_iter()
                .map(|f| f.path)
                .collect();
            let state = self
                .highlight_revset
                .as_deref()
                .map(|hl| {
                    highlight_fingerprint_state(
                        &self.repo_path, hl, effective_from, head_rev, &cancel,
                    )
                })
                .unwrap_or_default();
            return Ok(FingerprintInputs { base, head, state, changed });
        }
        let mut head = id_of("@")?;
        let mut changed: Vec<String> = self
            .discover_working_changes(effective_from, &cancel)?
            .into_iter()
            .map(|f| f.path)
            .collect();

        // A tip above `@` holds committed changes the working copy does not show.
        let mut effective_to = "@".to_string();
        if let Some(tip) = resolve_stack_tip(&self.repo_path, &self.from_rev, &cancel) {
            head = format!("{head},{}", id_of(&tip.change_id)?);
            let out = self.run_jj(&no_snapshot(&[
                "diff", "--from", effective_from, "--to", &tip.change_id, "--name-only",
            ]))?;
            changed.extend(out.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from));
            effective_to = tip.change_id;
        }
        let state = self
            .highlight_revset
            .as_deref()
            .map(|hl| {
                highlight_fingerprint_state(
                    &self.repo_path, hl, effective_from, &effective_to, &cancel,
                )
            })
            .unwrap_or_default();
        Ok(FingerprintInputs { base, head, state, changed })
    }
}

pub(super) fn highlight_fingerprint_state(
    repo_path: &Path,
    revset: &str,
    effective_from: &str,
    effective_to: &str,
    cancel: &Arc<AtomicBool>,
) -> String {
    let scoped = format!("({revset}) & ({effective_from})..({effective_to})");
    let mut state = String::from("\n#highlight\n");
    for entry in highlight::query_highlight_log(repo_path, &scoped, cancel).unwrap_or_default() {
        state.push_str(&entry.commit_id);
        state.push(':');
        state.push_str(&entry.parents.join(","));
        state.push('\n');
    }
    state
}

fn working_copy_digest(repo_path: &Path, branch_changed_files: &[ChangedFile]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let feed = |h: &mut u64, bytes: &[u8]| {
        for &b in bytes {
            *h = (*h ^ u64::from(b)).wrapping_mul(0x100000001b3);
        }
        *h = (*h ^ 0xff).wrapping_mul(0x100000001b3);
    };
    for f in branch_changed_files {
        feed(&mut h, f.path.as_bytes());
        feed(&mut h, f.old_path.as_deref().unwrap_or("").as_bytes());
        match std::fs::read(repo_path.join(&f.path)) {
            Ok(bytes) => {
                feed(&mut h, &[1]);
                feed(&mut h, &bytes);
            }
            Err(_) => feed(&mut h, &[0]),
        }
    }
    h
}

pub(super) fn op_head_key(
    op_store_dir: &Path,
    repo_path: &Path,
    effective_from: &str,
    effective_to: &str,
    read_from_disk: bool,
    branch_changed_files: &[ChangedFile],
) -> Option<String> {
    let op_heads_dir = op_store_dir.parent()?.join("op_heads").join("heads");
    let mut heads: Vec<String> = std::fs::read_dir(op_heads_dir)
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    if heads.is_empty() {
        return None;
    }
    heads.sort_unstable();
    let wc = if read_from_disk { working_copy_digest(repo_path, branch_changed_files) } else { 0 };
    Some(format!("{}:{effective_from}:{effective_to}:{read_from_disk}:{wc:016x}", heads.join(",")))
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::super::JjVcs;
    use super::super::parse::ChangedFile;
    use super::super::tests::{jj_available, setup_repo_with_remote};
    use super::*;
    use crate::vcs::Vcs;

    fn jj(repo: &std::path::Path, args: &[&str]) {
        let output = Command::new("jj").args(args).current_dir(repo).output().unwrap();
        assert!(
            output.status.success(),
            "jj {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn working_copy_digest_matches_fnv1a() {
        // Independent of `working_copy_digest`: FNV-1a over
        // path, old path, the presence byte, then the file bytes,
        // each feed ending in a mix with 0xff. Offset 0xcbf29ce484222325,
        // prime 0x100000001b3. The three `^` mutants hash to
        // 0xb604c8a959880106, 0x000071000000c003 and 0x4a14f17277e80d4d.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hi\n").unwrap();
        let files =
            [ChangedFile { path: "a.txt".to_string(), old_path: Some("prev.txt".to_string()) }];
        assert_eq!(working_copy_digest(dir.path(), &files), 0xabc0_75c2_afb7_9e6d);
    }

    #[test]
    fn tip_above_the_working_copy_adds_its_files() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        std::fs::write(repo.join("c1.txt"), "c1\n").unwrap();
        jj(repo, &["commit", "-m", "c1"]);
        std::fs::write(repo.join("c2.txt"), "c2\n").unwrap();
        jj(repo, &["commit", "-m", "c2"]);
        std::fs::write(repo.join("tip_only.txt"), "tip\n").unwrap();
        jj(repo, &["commit", "-m", "c3"]);
        // `@` is the empty working copy above c3, so `@---` is c1.
        // `tip_only.txt` exists only in c3, above that checkout.
        jj(repo, &["edit", "@---"]);

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let changed = vcs.fingerprint_inputs().unwrap().changed;
        assert!(
            changed.iter().any(|p| p == "tip_only.txt"),
            "the stack tip's file must be in the fingerprint inputs, got {changed:?}"
        );
    }
}
