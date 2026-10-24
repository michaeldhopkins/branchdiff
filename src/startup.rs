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

/// Default terminal dimensions for `--benchmark` when `COLUMNS`/`LINES` are unset.
pub const BENCHMARK_COLS: u16 = 120;
pub const BENCHMARK_LINES: u16 = 40;

/// Fixed or environment-injected dimensions (`COLUMNS` x `LINES`) for
/// `--benchmark`, so headless benchmark runs never query `/dev/tty` or `tput`.
pub fn benchmark_size(columns: Option<&str>, lines: Option<&str>) -> (u16, u16) {
    (
        columns.and_then(|s| s.trim().parse().ok()).filter(|&n| n > 0).unwrap_or(BENCHMARK_COLS),
        lines.and_then(|s| s.trim().parse().ok()).filter(|&n| n > 0).unwrap_or(BENCHMARK_LINES),
    )
}

/// Read `COLUMNS`/`LINES` (defaulting to 120x40) and, when `pin_env` is true,
/// export them so child `jj` commands never fall back to spawning `tput` when
/// there is no controlling terminal.
///
/// # Safety
/// Must be called on the main thread before any other threads are spawned.
#[doc(hidden)]
pub unsafe fn pin_headless_size_env(pin_env: bool) -> (u16, u16) {
    let (cols, rows) = benchmark_size(
        std::env::var("COLUMNS").ok().as_deref(),
        std::env::var("LINES").ok().as_deref(),
    );
    if pin_env {
        // SAFETY: Caller guarantees no other threads are running.
        unsafe {
            std::env::set_var("COLUMNS", cols.to_string());
            std::env::set_var("LINES", rows.to_string());
        }
    }
    (cols, rows)
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
        DiffLine::new(LineSource::Unstaged, "let x = 1;".to_string(), ' ', None)
            .with_file_path(path)
    }

    fn data(lines: Vec<DiffLine>) -> OutputData {
        OutputData {
            repo_name: "repo".into(),
            to_label: "to".into(),
            from_label: "from".into(),
            files: vec![OutputFile {
                path: "f".into(),
                lines,
                additions: 0,
                deletions: 0,
                collapsed: false,
                comments: Default::default(),
                highlight: Default::default(),
            }],
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

    #[test]
    fn benchmark_size_defaults_to_fixed_dimensions_and_honours_env_overrides() {
        assert_eq!(benchmark_size(None, None), (120, 40));
        assert_eq!(benchmark_size(Some("160"), Some("50")), (160, 50));
        assert_eq!(benchmark_size(Some(" 80 "), Some(" 24 ")), (80, 24));
        assert_eq!(benchmark_size(Some("1"), Some("1")), (1, 1));
        assert_eq!(benchmark_size(Some("0"), Some("0")), (120, 40));
        assert_eq!(benchmark_size(Some(""), Some("invalid")), (120, 40));
    }

    #[test]
    fn pin_headless_size_env_exports_columns_and_lines_when_requested() {
        // SAFETY: only this test touches COLUMNS and LINES in the unit binary.
        unsafe {
            std::env::remove_var("COLUMNS");
            std::env::remove_var("LINES");
            assert_eq!(pin_headless_size_env(false), (120, 40));
            assert!(std::env::var_os("COLUMNS").is_none());
            assert!(std::env::var_os("LINES").is_none());

            assert_eq!(pin_headless_size_env(true), (120, 40));
            assert_eq!(std::env::var("COLUMNS").as_deref(), Ok("120"));
            assert_eq!(std::env::var("LINES").as_deref(), Ok("40"));

            std::env::set_var("COLUMNS", "90");
            std::env::set_var("LINES", "25");
            assert_eq!(pin_headless_size_env(true), (90, 25));
            assert_eq!(std::env::var("COLUMNS").as_deref(), Ok("90"));
            assert_eq!(std::env::var("LINES").as_deref(), Ok("25"));
            std::env::remove_var("COLUMNS");
            std::env::remove_var("LINES");
        }
    }
}
