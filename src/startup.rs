//! Decisions `main` makes before it hands over to a renderer or the TUI.
//!
//! `main` itself only wires these to argv, stdout and the terminal, none of which
//! a unit test can observe. Keeping the choices here lets them be tested.

use ratatui::layout::Rect;

use crate::app::ViewMode;
use crate::cli::OutputMode;
use crate::image_diff::ImageCache;
use crate::output::OutputData;

/// What to do when VCS detection fails at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectFailure {
    /// A repository is present, so the failure is the caller's `--base` or a
    /// real VCS error: report that error, not "no repository".
    Propagate,
    /// No repository, and a non-interactive mode has nothing to wait for.
    NotARepo,
    /// No repository yet: the TUI waits for `git init` / `jj init`.
    WaitForRepo,
}

pub fn on_detect_failure(repo_dir_present: bool, mode: OutputMode) -> DetectFailure {
    if repo_dir_present {
        DetectFailure::Propagate
    } else if mode != OutputMode::Tui {
        DetectFailure::NotARepo
    } else {
        DetectFailure::WaitForRepo
    }
}

/// Whether `mode` renders once to stdout instead of running the TUI.
pub fn is_one_shot(mode: OutputMode) -> bool {
    mode != OutputMode::Tui
}

/// The view mode a one-shot renderer uses: `--print` shows everything, the
/// others default to context.
pub fn view_mode_for(mode: OutputMode) -> ViewMode {
    match mode {
        OutputMode::Print => ViewMode::Full,
        _ => ViewMode::Context,
    }
}

/// Image paths to load before rendering, in first-seen order and without
/// repeats. Only HTML embeds images; nothing else needs them loaded.
pub fn images_to_preload(mode: OutputMode, data: &OutputData, cache: &ImageCache) -> Vec<String> {
    if mode != OutputMode::Html {
        return Vec::new();
    }
    let mut paths: Vec<String> = Vec::new();
    for line in data.files.iter().flat_map(|f| &f.lines) {
        if line.is_image_marker()
            && let Some(path) = &line.file_path
            && !cache.contains(path)
            && !paths.contains(path)
        {
            paths.push(path.clone());
        }
    }
    paths
}

/// Where the "waiting for a repository" message goes: vertically centred on
/// `line_count` lines, with a line of padding either side, clipped to `area`.
pub fn waiting_message_area(area: Rect, line_count: u16) -> Rect {
    let y = area.height / 2;
    Rect {
        x: 0,
        y: y.saturating_sub(line_count / 2),
        width: area.width,
        height: (line_count + 2).min(area.height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffLine, LineSource};
    use crate::image_diff::ImageDiffState;
    use crate::output::OutputFile;

    #[test]
    fn a_present_repo_always_propagates_the_real_error() {
        for mode in [OutputMode::Tui, OutputMode::Print, OutputMode::Diff, OutputMode::Html] {
            assert_eq!(on_detect_failure(true, mode), DetectFailure::Propagate, "{mode:?}");
        }
    }

    #[test]
    fn no_repo_waits_only_in_the_tui() {
        assert_eq!(on_detect_failure(false, OutputMode::Tui), DetectFailure::WaitForRepo);
        for mode in [OutputMode::Print, OutputMode::Diff, OutputMode::Html] {
            assert_eq!(on_detect_failure(false, mode), DetectFailure::NotARepo, "{mode:?}");
        }
    }

    #[test]
    fn every_mode_but_the_tui_is_one_shot() {
        assert!(!is_one_shot(OutputMode::Tui));
        for mode in [OutputMode::Print, OutputMode::Diff, OutputMode::Html] {
            assert!(is_one_shot(mode), "{mode:?}");
        }
    }

    #[test]
    fn print_shows_everything_and_the_rest_default_to_context() {
        assert_eq!(view_mode_for(OutputMode::Print), ViewMode::Full);
        assert_eq!(view_mode_for(OutputMode::Html), ViewMode::Context);
        assert_eq!(view_mode_for(OutputMode::Diff), ViewMode::Context);
    }

    fn image(path: &str) -> DiffLine {
        DiffLine::new(LineSource::Unstaged, "[image]".to_string(), ' ', None).with_file_path(path)
    }

    fn text(path: &str) -> DiffLine {
        DiffLine::new(LineSource::Unstaged, "let x = 1;".to_string(), ' ', None).with_file_path(path)
    }

    fn data(lines: Vec<DiffLine>) -> OutputData {
        OutputData {
            repo_name: "repo".into(),
            to_label: "to".into(),
            from_label: "from".into(),
            files: vec![OutputFile { path: "f".into(), lines, additions: 0, deletions: 0, collapsed: false }],
            total_additions: 0,
            total_deletions: 0,
        }
    }

    fn loaded() -> ImageDiffState {
        ImageDiffState { before: None, after: None }
    }

    #[test]
    fn html_preloads_each_uncached_image_once_in_order() {
        let d = data(vec![image("b.png"), text("c.rs"), image("a.png"), image("b.png")]);
        let mut cache = ImageCache::new();
        assert_eq!(images_to_preload(OutputMode::Html, &d, &cache), ["b.png", "a.png"]);

        cache.insert("b.png".into(), loaded());
        assert_eq!(images_to_preload(OutputMode::Html, &d, &cache), ["a.png"]);
    }

    #[test]
    fn only_html_preloads_images() {
        let d = data(vec![image("a.png")]);
        let cache = ImageCache::new();
        for mode in [OutputMode::Tui, OutputMode::Print, OutputMode::Diff] {
            assert!(images_to_preload(mode, &d, &cache).is_empty(), "{mode:?}");
        }
    }

    #[test]
    fn the_waiting_message_is_centred_with_padding() {
        let area = Rect { x: 0, y: 0, width: 80, height: 24 };
        assert_eq!(waiting_message_area(area, 4), Rect { x: 0, y: 10, width: 80, height: 6 });
        assert_eq!(waiting_message_area(area, 7), Rect { x: 0, y: 9, width: 80, height: 9 });
    }

    #[test]
    fn the_waiting_message_is_clipped_to_a_short_terminal() {
        let area = Rect { x: 0, y: 0, width: 30, height: 5 };
        assert_eq!(waiting_message_area(area, 4), Rect { x: 0, y: 0, width: 30, height: 5 });
    }
}
