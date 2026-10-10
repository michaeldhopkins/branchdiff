//! Inputs for `--fingerprint`, read without snapshotting the working copy.

use super::*;
use crate::fingerprint::FingerprintInputs;

impl JjVcs {
    pub(super) fn gather_fingerprint_inputs(&self) -> Result<FingerprintInputs> {
        let cancel = Arc::new(AtomicBool::new(false));
        let fork_point = resolve_fork_point(&self.repo_path, &self.from_rev, &cancel);
        let effective_from = match self.load_diff_base() {
            DiffBase::ForkPoint => fork_point.as_deref().unwrap_or(&self.from_rev),
            DiffBase::TrunkTip => &self.from_rev,
        };
        let id_of = |rev: &str| {
            self.commit_id_of(rev)
                .ok_or_else(|| anyhow::anyhow!("could not resolve {rev:?}"))
        };
        let base = id_of(effective_from)?;
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
