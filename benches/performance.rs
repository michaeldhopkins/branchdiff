//! Performance benchmarks for branchdiff
//!
//! Run with: cargo bench
//! Save baseline: cargo bench -- --save-baseline before-framecontext
//! Compare: cargo bench -- --baseline before-framecontext

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use branchdiff::app::{App, ViewMode};
use branchdiff::diff::{DiffLine, LineSource};

/// Create a synthetic diff with the given number of files and lines per file
fn create_test_diff(file_count: usize, lines_per_file: usize) -> Vec<DiffLine> {
    let mut lines = Vec::with_capacity(file_count * (lines_per_file + 2));

    for f in 0..file_count {
        // File header
        lines.push(DiffLine::file_header(&format!("src/file_{}.rs", f)));

        for l in 0..lines_per_file {
            // Mix of different line sources
            let (source, prefix) = match l % 10 {
                0 => (LineSource::Committed, '+'),
                1 => (LineSource::Staged, '+'),
                2 => (LineSource::Unstaged, '+'),
                3 => (LineSource::DeletedBase, '-'),
                4 => (LineSource::DeletedCommitted, '-'),
                _ => (LineSource::Base, ' '),
            };
            lines.push(DiffLine::new(
                source,
                format!("    let line_{} = some_content_here_{};", l, f),
                prefix,
                Some(l + 1),
            ));
        }

        // Empty separator between files
        lines.push(DiffLine::new(LineSource::Base, String::new(), ' ', None));
    }

    lines
}

fn bench_displayable_lines(c: &mut Criterion) {
    let mut group = c.benchmark_group("displayable_lines");

    // Small: typical feature branch
    // Medium: larger refactor
    // Large: stress test
    for (files, lines_per_file, label) in [
        (10, 100, "small_10f_1k"),
        (50, 200, "medium_50f_10k"),
        (100, 500, "large_100f_50k"),
    ] {
        let diff = create_test_diff(files, lines_per_file);
        let total = diff.len();

        // Full mode
        {
            let mut app = App::new_for_bench(diff.clone());
            app.view_mode = ViewMode::Full;
            group.bench_with_input(
                BenchmarkId::new("full", format!("{}_{}lines", label, total)),
                &(),
                |b, _| b.iter(|| black_box(app.displayable_lines())),
            );
        }

        // Context mode
        {
            let mut app = App::new_for_bench(diff.clone());
            app.view_mode = ViewMode::Context;
            group.bench_with_input(
                BenchmarkId::new("context", format!("{}_{}lines", label, total)),
                &(),
                |b, _| b.iter(|| black_box(app.displayable_lines())),
            );
        }

        // ChangesOnly mode
        {
            let mut app = App::new_for_bench(diff.clone());
            app.view_mode = ViewMode::ChangesOnly;
            group.bench_with_input(
                BenchmarkId::new("changes_only", format!("{}_{}lines", label, total)),
                &(),
                |b, _| b.iter(|| black_box(app.displayable_lines())),
            );
        }
    }

    group.finish();
}

fn bench_visible_lines(c: &mut Criterion) {
    let mut group = c.benchmark_group("visible_lines");

    for (files, lines_per_file, label) in [
        (10, 100, "small"),
        (50, 200, "medium"),
        (100, 500, "large"),
    ] {
        let diff = create_test_diff(files, lines_per_file);

        // Full mode at different scroll positions
        for (scroll_pct, scroll_label) in [(0, "top"), (50, "middle"), (100, "bottom")] {
            let mut app = App::new_for_bench(diff.clone());
            app.view_mode = ViewMode::Full;
            app.viewport_height = 50;

            // Set scroll position
            let max_scroll = app.displayable_lines().len().saturating_sub(app.viewport_height);
            app.scroll_offset = (max_scroll * scroll_pct) / 100;

            group.bench_with_input(
                BenchmarkId::new("full", format!("{}_{}", label, scroll_label)),
                &(),
                |b, _| b.iter(|| black_box(app.visible_lines())),
            );
        }
    }

    group.finish();
}

fn bench_scroll_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("scroll_ops");

    let diff = create_test_diff(50, 200);

    // go_to_bottom (involves max_scroll_offset computation)
    {
        let mut app = App::new_for_bench(diff.clone());
        app.view_mode = ViewMode::Full;
        app.viewport_height = 50;

        group.bench_function("go_to_bottom_full", |b| {
            b.iter(|| {
                app.scroll_offset = 0;
                app.go_to_bottom();
                black_box(app.scroll_offset)
            })
        });
    }

    // go_to_bottom in context mode
    {
        let mut app = App::new_for_bench(diff.clone());
        app.view_mode = ViewMode::Context;
        app.viewport_height = 50;

        group.bench_function("go_to_bottom_context", |b| {
            b.iter(|| {
                app.scroll_offset = 0;
                app.go_to_bottom();
                black_box(app.scroll_offset)
            })
        });
    }

    // cycle_view_mode (involves anchor computation and restoration)
    {
        let mut app = App::new_for_bench(diff.clone());
        app.view_mode = ViewMode::Full;
        app.viewport_height = 50;
        app.scroll_offset = 100;

        group.bench_function("cycle_view_mode", |b| {
            b.iter(|| {
                app.cycle_view_mode();
                black_box(app.scroll_offset)
            })
        });
    }

    group.finish();
}

fn bench_context_mode_specifics(c: &mut Criterion) {
    let mut group = c.benchmark_group("context_mode");

    let diff = create_test_diff(50, 200);

    // build_context_lines_with_mapping (complex operation)
    {
        let app = App::new_for_bench(diff.clone());

        group.bench_function("build_context_lines_with_mapping", |b| {
            b.iter(|| black_box(app.build_context_lines_with_mapping()))
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_displayable_lines,
    bench_visible_lines,
    bench_scroll_operations,
    bench_context_mode_specifics
);
criterion_main!(benches);
