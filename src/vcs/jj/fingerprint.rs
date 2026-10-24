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
            return Ok(FingerprintInputs { base, head, state: String::new(), changed });
        }
        let mut head = id_of("@")?;
        let mut changed: Vec<String> = self
            .discover_working_changes(effective_from, &cancel)?
            .into_iter()
            .map(|f| f.path)
            .collect();

        // A tip above `@` holds committed changes the working copy does not show.
        if let Some(tip) = resolve_stack_tip(&self.repo_path, &self.from_rev, &cancel) {
            head = format!("{head},{}", id_of(&tip.change_id)?);
            let out = self.run_jj(&no_snapshot(&[
                "diff", "--from", effective_from, "--to", &tip.change_id, "--name-only",
            ]))?;
            changed.extend(out.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from));
        }
        Ok(FingerprintInputs { base, head, state: String::new(), changed })
    }
}
