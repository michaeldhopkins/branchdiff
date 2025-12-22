//! Performance benchmarks for branchdiff
//!
//! Run with: cargo bench
//! Quick run: cargo bench -- --quick
//! Single benchmark: cargo bench frame_context

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use std::time::Duration;

use branchdiff::app::{App, FrameContext, ViewMode};
use branchdiff::diff::{DiffLine, LineSource};

/// Create a synthetic diff with the given number of files and lines per file.
/// If `with_inline_diffs` is true, adds old_content to trigger inline span computation.
fn create_test_diff(file_count: usize, lines_per_file: usize, with_inline_diffs: bool) -> Vec<DiffLine> {
    let mut lines = Vec::with_capacity(file_count * (lines_per_file + 2));

    for f in 0..file_count {
        lines.push(DiffLine::file_header(&format!("src/file_{}.rs", f)));

        for l in 0..lines_per_file {
            let (source, prefix) = match l % 10 {
                0 => (LineSource::Committed, '+'),
                1 => (LineSource::Staged, '+'),
                2 => (LineSource::Unstaged, '+'),
                3 => (LineSource::DeletedBase, '-'),
                4 => (LineSource::DeletedCommitted, '-'),
                _ => (LineSource::Base, ' '),
            };

            let mut line = DiffLine::new(
                source,
                format!("    let variable_{} = some_content_here_{};", l, f),
                prefix,
                Some(l + 1),
            );

            // Add old_content for changed lines to trigger inline diff computation
            if with_inline_diffs && (source == LineSource::Committed || source == LineSource::Staged || source == LineSource::Unstaged) {
                line.old_content = Some(format!("    let old_var_{} = different_content_{};", l, f));
            }

            lines.push(line);
        }

        lines.push(DiffLine::new(LineSource::Base, String::new(), ' ', None));
    }

    lines
}

/// Benchmark FrameContext creation - runs every render frame
fn bench_frame_context(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame_context");
    group.measurement_time(Duration::from_secs(3));

    for (files, lines_per_file, label) in [
        (10, 100, "1k_lines"),
        (30, 300, "10k_lines"),
    ] {
        let diff = create_test_diff(files, lines_per_file, false);

        for mode in [ViewMode::Full, ViewMode::Context, ViewMode::ChangesOnly] {
            let mode_label = match mode {
                ViewMode::Full => "full",
                ViewMode::Context => "context",
                ViewMode::ChangesOnly => "changes",
            };

            let mut app = App::new_for_bench(diff.clone());
            app.view_mode = mode;
            app.viewport_height = 50;

            group.bench_with_input(
                BenchmarkId::new(mode_label, label),
                &(),
                |b, _| b.iter(|| black_box(FrameContext::new(&app))),
            );
        }
    }

    group.finish();
}

/// Benchmark inline span computation - the expensive diff highlighting
fn bench_inline_spans(c: &mut Criterion) {
    let mut group = c.benchmark_group("inline_spans");
    group.measurement_time(Duration::from_secs(3));

    // Create diff with inline diffs enabled
    let diff = create_test_diff(20, 200, true);
    let mut app = App::new_for_bench(diff);
    app.viewport_height = 50;

    // Count lines that need inline spans
    let lines_with_old: usize = app.lines.iter()
        .filter(|l| l.old_content.is_some())
        .count();

    group.bench_function(
        format!("ensure_visible_{}_candidates", lines_with_old),
        |b| {
            b.iter(|| {
                // Reset inline spans
                for line in &mut app.lines {
                    line.inline_spans.clear();
                }
                app.ensure_inline_spans_for_visible(50);
                black_box(app.lines.iter().filter(|l| !l.inline_spans.is_empty()).count())
            })
        },
    );

    group.finish();
}

/// Benchmark scroll operations with FrameContext
fn bench_navigation(c: &mut Criterion) {
    let mut group = c.benchmark_group("navigation");
    group.measurement_time(Duration::from_secs(2));

    let diff = create_test_diff(30, 300, false);

    // Scroll operations
    {
        let mut app = App::new_for_bench(diff.clone());
        app.viewport_height = 50;
        app.scroll_offset = 100;

        group.bench_function("scroll_down_10", |b| {
            b.iter(|| {
                app.scroll_down(10);
                app.scroll_up(10); // Reset for next iteration
                black_box(app.scroll_offset)
            })
        });
    }

    // Page operations
    {
        let mut app = App::new_for_bench(diff.clone());
        app.viewport_height = 50;
        app.scroll_offset = 100;

        group.bench_function("page_down", |b| {
            b.iter(|| {
                app.page_down();
                app.page_up(); // Reset
                black_box(app.scroll_offset)
            })
        });
    }

    // go_to_bottom (needs max_scroll computation)
    {
        let mut app = App::new_for_bench(diff.clone());
        app.viewport_height = 50;

        group.bench_function("go_to_bottom", |b| {
            b.iter(|| {
                app.scroll_offset = 0;
                app.go_to_bottom();
                black_box(app.scroll_offset)
            })
        });
    }

    // Next/prev file navigation
    {
        let mut app = App::new_for_bench(diff.clone());
        app.viewport_height = 50;
        app.scroll_offset = 0;

        group.bench_function("next_file", |b| {
            b.iter(|| {
                app.next_file();
                black_box(app.scroll_offset)
            })
        });
    }

    group.finish();
}

/// Benchmark view mode cycling (involves anchor computation)
fn bench_view_mode(c: &mut Criterion) {
    let mut group = c.benchmark_group("view_mode");
    group.measurement_time(Duration::from_secs(2));

    let diff = create_test_diff(30, 300, false);

    let mut app = App::new_for_bench(diff);
    app.viewport_height = 50;
    app.scroll_offset = 150;

    group.bench_function("cycle_view_mode", |b| {
        b.iter(|| {
            app.cycle_view_mode();
            black_box((app.view_mode, app.scroll_offset))
        })
    });

    group.finish();
}

/// Benchmark context mode line building
fn bench_context_mode(c: &mut Criterion) {
    let mut group = c.benchmark_group("context_mode");
    group.measurement_time(Duration::from_secs(3));

    for (files, lines_per_file, label) in [
        (10, 100, "1k_lines"),
        (30, 300, "10k_lines"),
    ] {
        let diff = create_test_diff(files, lines_per_file, false);
        let app = App::new_for_bench(diff);

        group.bench_with_input(
            BenchmarkId::new("build_context_lines", label),
            &(),
            |b, _| b.iter(|| black_box(app.build_context_lines_with_mapping())),
        );
    }

    group.finish();
}

/// Benchmark message update handling
fn bench_update(c: &mut Criterion) {
    use branchdiff::input::AppAction;
    use branchdiff::message::Message;
    use branchdiff::update::{update, RefreshState, Timers, UpdateConfig};
    use std::path::PathBuf;

    let mut group = c.benchmark_group("update");
    group.measurement_time(Duration::from_secs(2));

    let diff = create_test_diff(20, 200, false);
    let repo_root = PathBuf::from("/tmp/test");
    let config = UpdateConfig::default();

    // Input message handling
    {
        let mut app = App::new_for_bench(diff.clone());
        app.viewport_height = 50;
        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers::default();

        group.bench_function("handle_scroll_input", |b| {
            b.iter(|| {
                let result = update(
                    Message::Input(AppAction::ScrollDown(5)),
                    &mut app,
                    &mut refresh_state,
                    &mut timers,
                    &config,
                    &repo_root,
                );
                app.scroll_up(5); // Reset
                black_box(result)
            })
        });
    }

    // Tick message (timer checks)
    {
        let mut app = App::new_for_bench(diff.clone());
        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers::default();

        group.bench_function("handle_tick", |b| {
            b.iter(|| {
                let result = update(
                    Message::Tick,
                    &mut app,
                    &mut refresh_state,
                    &mut timers,
                    &config,
                    &repo_root,
                );
                black_box(result)
            })
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_frame_context,
    bench_inline_spans,
    bench_navigation,
    bench_view_mode,
    bench_context_mode,
    bench_update,
);
criterion_main!(benches);
