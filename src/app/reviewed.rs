//! Reviewed-file state: mark a file reviewed, drop stale reviews, toggle all.

use super::App;

impl App {
    /// Toggle reviewed state for a file. Returns true if now reviewed, false if un-reviewed.
    pub fn toggle_reviewed(&mut self, path: &str) -> bool {
        if self.view.reviewed_files.contains_key(path) {
            // Un-review: remove from reviewed, uncollapse, unlock from manual toggle
            self.view.reviewed_files.remove(path);
            self.view.collapsed_files.remove(path);
            self.view.manually_toggled.remove(path);
            self.view.reviewed_flash = Some((path.to_string(), std::time::Instant::now()));
            self.view.needs_inline_spans = true;
            false
        } else {
            // Review: record hash, collapse, lock
            let hash = self
                .files
                .iter()
                .find(|f| f.lines.first().and_then(|l| l.file_path.as_deref()) == Some(path))
                .map(|f| f.content_hash)
                .unwrap_or(0);
            self.view.reviewed_files.insert(path.to_string(), hash);
            self.view.collapsed_files.insert(path.to_string());
            self.view.manually_toggled.insert(path.to_string());
            self.view.reviewed_flash = Some((path.to_string(), std::time::Instant::now()));
            self.view.needs_inline_spans = true;
            true
        }
    }

    /// Check reviewed files against current content hashes.
    /// Un-reviews any file whose content changed.
    pub fn check_reviewed_staleness(&mut self) {
        let stale: Vec<String> = self
            .view
            .reviewed_files
            .iter()
            .filter(|(path, old_hash)| {
                let current_hash = self
                    .files
                    .iter()
                    .find(|f| {
                        f.lines.first().and_then(|l| l.file_path.as_deref()) == Some(path.as_str())
                    })
                    .map(|f| f.content_hash);
                match current_hash {
                    Some(h) => h != **old_hash,
                    None => true, // file gone
                }
            })
            .map(|(path, _)| path.clone())
            .collect();

        for path in &stale {
            self.view.reviewed_files.remove(path);
            self.view.collapsed_files.remove(path);
            self.view.manually_toggled.remove(path);
            self.view.reviewed_flash = Some((path.clone(), std::time::Instant::now()));
        }
    }

    /// Toggle all files reviewed/un-reviewed.
    /// If any file is un-reviewed, review all. Otherwise un-review all.
    pub fn toggle_all_reviewed(&mut self) {
        let all_paths: Vec<String> = self
            .files
            .iter()
            .filter_map(|f| f.lines.first().and_then(|l| l.file_path.clone()))
            .collect();

        let all_reviewed = all_paths.iter().all(|p| self.view.reviewed_files.contains_key(p));

        if all_reviewed {
            // Un-review all
            for path in &all_paths {
                self.view.reviewed_files.remove(path);
                self.view.collapsed_files.remove(path);
                self.view.manually_toggled.remove(path);
            }
        } else {
            // Review all
            for file in &self.files {
                if let Some(path) = file.lines.first().and_then(|l| l.file_path.clone()) {
                    self.view.reviewed_files.insert(path.clone(), file.content_hash);
                    self.view.collapsed_files.insert(path.clone());
                    self.view.manually_toggled.insert(path);
                }
            }
        }
        self.view.needs_inline_spans = true;
    }
}
