//! One-call heap counts for the in-process groups in `benches/performance.rs`.
//!
//! `cargo bench --bench alloc` prints `alloc\t<id>\t<blocks>\t<bytes>` and exits
//! non-zero when a count exceeds `benches/baselines.toml`. Counts are totals for
//! a single call at the bench profile (`opt-level = 3`), so a runner does not
//! move them. The `refresh` group is not here: it shells out to `jj`, and the
//! alloc job does not install `jj`.
//!
//! Pins tighten through `profile.sh alloc` with `WRITE=1`, not in this binary.

use std::hint::black_box;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use branchdiff::app::{App, FrameContext, ViewMode};
use branchdiff::diff::{DiffLine, LineSource};
use branchdiff::file_events::VcsLockState;
use branchdiff::input::AppAction;
use branchdiff::message::Message;
use branchdiff::update::{RefreshState, Timers, UpdateConfig, update};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

struct Pin {
    id: String,
    max_blocks: u64,
    max_bytes: u64,
}

struct Sample {
    id: String,
    blocks: u64,
    bytes: u64,
}

fn main() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("benches/baselines.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        eprintln!("alloc: cannot read {}: {err}", path.display());
        std::process::exit(2);
    });
    let pins = match parse_pins(&text) {
        Ok(pins) => pins,
        Err(err) => {
            eprintln!("alloc: {err}");
            std::process::exit(2);
        }
    };

    let _profiler = dhat::Profiler::builder().testing().build();
    let samples = measure_all();
    let _ = std::io::Write::flush(&mut std::io::stdout());

    let mut failed = false;
    for sample in &samples {
        match pins.iter().find(|pin| pin.id == sample.id) {
            None => {
                eprintln!("alloc: no pin for {}", sample.id);
                failed = true;
            }
            Some(pin) => {
                if sample.blocks > pin.max_blocks {
                    eprintln!(
                        "alloc: {} blocks {} exceed {}",
                        sample.id, sample.blocks, pin.max_blocks
                    );
                    failed = true;
                }
                if sample.bytes > pin.max_bytes {
                    eprintln!(
                        "alloc: {} bytes {} exceed {}",
                        sample.id, sample.bytes, pin.max_bytes
                    );
                    failed = true;
                }
            }
        }
    }
    for pin in &pins {
        if !samples.iter().any(|sample| sample.id == pin.id) {
            eprintln!("alloc: pin {} was not measured", pin.id);
            failed = true;
        }
    }
    if failed {
        std::process::exit(1);
    }
}

fn measure_all() -> Vec<Sample> {
    let mut samples = Vec::new();
    measure_frame_context(&mut samples);
    measure_inline_spans(&mut samples);
    measure_navigation(&mut samples);
    measure_view_mode(&mut samples);
    measure_context_mode(&mut samples);
    measure_update(&mut samples);
    samples
}

fn measure(samples: &mut Vec<Sample>, id: &str, f: impl FnOnce()) {
    let before = dhat::HeapStats::get();
    f();
    let after = dhat::HeapStats::get();
    let blocks = after.total_blocks - before.total_blocks;
    let bytes = after.total_bytes - before.total_bytes;
    println!("alloc\t{id}\t{blocks}\t{bytes}");
    samples.push(Sample { id: id.to_string(), blocks, bytes });
}

fn create_test_diff(
    file_count: usize,
    lines_per_file: usize,
    with_inline_diffs: bool,
) -> Vec<DiffLine> {
    let mut lines = Vec::with_capacity(file_count * (lines_per_file + 2));
    for f in 0..file_count {
        lines.push(DiffLine::file_header(&format!("src/file_{f}.rs")));
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
                format!("    let variable_{l} = some_content_here_{f};"),
                prefix,
                Some(l + 1),
            );
            if with_inline_diffs
                && (source == LineSource::Committed
                    || source == LineSource::Staged
                    || source == LineSource::Unstaged)
            {
                line.old_content = Some(format!("    let old_var_{l} = different_content_{f};"));
            }
            lines.push(line);
        }
        lines.push(DiffLine::new(LineSource::Base, String::new(), ' ', None));
    }
    lines
}

fn app_for(files: usize, lines_per_file: usize, inline: bool) -> App {
    let mut app = App::new_for_bench(create_test_diff(files, lines_per_file, inline));
    app.view.viewport_height = 50;
    app
}

fn measure_frame_context(samples: &mut Vec<Sample>) {
    for (files, lines_per_file, label) in [(10, 100, "1k_lines"), (30, 300, "10k_lines")] {
        for mode in [ViewMode::Full, ViewMode::Context, ViewMode::CommitOnly] {
            let mode_label = match mode {
                ViewMode::Full => "full",
                ViewMode::Context => "context",
                ViewMode::CommitOnly => "commit_only",
                ViewMode::BookmarkOnly => "bookmark_only",
            };
            let mut app = app_for(files, lines_per_file, false);
            app.view.view_mode = mode;
            let id = format!("frame_context/{mode_label}/{label}");
            measure(samples, &id, || {
                black_box(FrameContext::new(&app));
            });
        }
    }
}

fn measure_inline_spans(samples: &mut Vec<Sample>) {
    let mut app = app_for(20, 200, true);
    let candidates = app.lines().iter().filter(|line| line.old_content.is_some()).count();
    let id = format!("inline_spans/ensure_visible_{candidates}_candidates");
    measure(samples, &id, || {
        app.clear_inline_spans();
        app.ensure_inline_spans_for_visible(50);
        black_box(app.lines().iter().filter(|line| !line.inline_spans.is_empty()).count());
    });
}

fn measure_navigation(samples: &mut Vec<Sample>) {
    let diff = create_test_diff(30, 300, false);

    let mut app = App::new_for_bench(diff.clone());
    app.view.viewport_height = 50;
    app.view.scroll_offset = 100;
    measure(samples, "navigation/scroll_down_10", || {
        app.scroll_down(10);
        app.scroll_up(10);
        black_box(app.view.scroll_offset);
    });

    let mut app = App::new_for_bench(diff.clone());
    app.view.viewport_height = 50;
    app.view.scroll_offset = 100;
    measure(samples, "navigation/page_down", || {
        app.page_down();
        app.page_up();
        black_box(app.view.scroll_offset);
    });

    let mut app = App::new_for_bench(diff.clone());
    app.view.viewport_height = 50;
    measure(samples, "navigation/go_to_bottom", || {
        app.view.scroll_offset = 0;
        app.go_to_bottom();
        black_box(app.view.scroll_offset);
    });

    let mut app = App::new_for_bench(diff);
    app.view.viewport_height = 50;
    measure(samples, "navigation/next_file", || {
        app.next_file();
        black_box(app.view.scroll_offset);
    });
}

fn measure_view_mode(samples: &mut Vec<Sample>) {
    let mut app = app_for(30, 300, false);
    app.view.scroll_offset = 150;
    measure(samples, "view_mode/cycle_view_mode", || {
        app.cycle_view_mode();
        black_box((app.view.view_mode, app.view.scroll_offset));
    });
}

fn measure_context_mode(samples: &mut Vec<Sample>) {
    for (files, lines_per_file, label) in [(10, 100, "1k_lines"), (30, 300, "10k_lines")] {
        let app = app_for(files, lines_per_file, false);
        let id = format!("context_mode/build_context_lines/{label}");
        measure(samples, &id, || {
            black_box(app.build_context_lines_with_mapping());
        });
    }
}

/// Minimal Vcs stub. The update benches only send Input and Tick, which do not call it.
struct BenchVcs;

impl branchdiff::vcs::Vcs for BenchVcs {
    fn repo_path(&self) -> &Path {
        Path::new("/bench")
    }
    fn comparison_context(&self) -> anyhow::Result<branchdiff::vcs::ComparisonContext> {
        unimplemented!()
    }
    fn refresh(&self, _: &Arc<AtomicBool>) -> anyhow::Result<branchdiff::vcs::RefreshResult> {
        unimplemented!()
    }
    fn single_file_diff(&self, _: &str) -> Option<branchdiff::diff::FileDiff> {
        unimplemented!()
    }
    fn base_identifier(&self) -> anyhow::Result<String> {
        unimplemented!()
    }
    fn base_file_bytes(&self, _: &str) -> anyhow::Result<Option<Vec<u8>>> {
        unimplemented!()
    }
    fn working_file_bytes(&self, _: &str) -> anyhow::Result<Option<Vec<u8>>> {
        unimplemented!()
    }
    fn fetch(&self) -> anyhow::Result<()> {
        unimplemented!()
    }
    fn has_conflicts(&self) -> anyhow::Result<bool> {
        unimplemented!()
    }
    fn is_locked(&self) -> bool {
        false
    }
    fn watch_paths(&self) -> branchdiff::vcs::VcsWatchPaths {
        unimplemented!()
    }
    fn classify_event(&self, _: &Path) -> branchdiff::vcs::VcsEventType {
        unimplemented!()
    }
    fn backend(&self) -> branchdiff::vcs::VcsBackend {
        branchdiff::vcs::VcsBackend::Git
    }
    fn current_revision_id(&self) -> anyhow::Result<String> {
        unimplemented!()
    }
}

fn measure_update(samples: &mut Vec<Sample>) {
    let diff = create_test_diff(20, 200, false);
    let bench_vcs = BenchVcs;
    let config = UpdateConfig::default();

    let mut app = App::new_for_bench(diff.clone());
    app.view.viewport_height = 50;
    let mut refresh_state = RefreshState::Idle;
    let mut vcs_lock = VcsLockState::default();
    let mut timers = Timers::default();
    measure(samples, "update/handle_scroll_input", || {
        let result = update(
            Message::Input(AppAction::ScrollDown(5)),
            &mut app,
            &mut refresh_state,
            &mut vcs_lock,
            &mut timers,
            &config,
            &bench_vcs,
        );
        app.scroll_up(5);
        black_box(result);
    });

    let mut app = App::new_for_bench(diff);
    let mut refresh_state = RefreshState::Idle;
    let mut vcs_lock = VcsLockState::default();
    let mut timers = Timers::default();
    measure(samples, "update/handle_tick", || {
        let result = update(
            Message::Tick,
            &mut app,
            &mut refresh_state,
            &mut vcs_lock,
            &mut timers,
            &config,
            &bench_vcs,
        );
        black_box(result);
    });
}

/// The subset of `baselines.toml` this bench reads: `[[bench]]` records that set
/// `max_blocks` and `max_bytes`. Records with only `ref_median_ns` are the
/// wall-clock pins and are ignored here.
fn parse_pins(text: &str) -> Result<Vec<Pin>, String> {
    let mut pins = Vec::new();
    let mut id: Option<String> = None;
    let mut max_blocks: Option<u64> = None;
    let mut max_bytes: Option<u64> = None;
    let mut in_record = false;

    let finish = |pins: &mut Vec<Pin>,
                  id: &mut Option<String>,
                  blocks: &mut Option<u64>,
                  bytes: &mut Option<u64>|
     -> Result<(), String> {
        let Some(id) = id.take() else {
            return Err("baselines.toml: a [[bench]] record has no id".to_string());
        };
        if id.is_empty() {
            return Err("baselines.toml: a bench id is empty".to_string());
        }
        match (*blocks, *bytes) {
            (Some(max_blocks), Some(max_bytes)) => pins.push(Pin { id, max_blocks, max_bytes }),
            (None, None) => {}
            _ => {
                return Err(format!(
                    "baselines.toml: {id} sets only one of max_blocks and max_bytes"
                ));
            }
        }
        *blocks = None;
        *bytes = None;
        Ok(())
    };

    for (number, raw) in text.lines().enumerate() {
        let line_no = number + 1;
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line == "[[bench]]" {
            if in_record {
                finish(&mut pins, &mut id, &mut max_blocks, &mut max_bytes)?;
            }
            in_record = true;
            id = None;
            max_blocks = None;
            max_bytes = None;
            continue;
        }
        if !in_record {
            return Err(format!("baselines.toml:{line_no}: expected [[bench]]"));
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("baselines.toml:{line_no}: expected key = value"));
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "id" => {
                id = Some(unquote(value, line_no)?);
            }
            "max_blocks" => max_blocks = Some(parse_u64(value, line_no)?),
            "max_bytes" => max_bytes = Some(parse_u64(value, line_no)?),
            "ref_median_ns" => {
                let _ = parse_u64(value, line_no)?;
            }
            other => {
                return Err(format!("baselines.toml:{line_no}: unknown key {other}"));
            }
        }
    }
    if in_record {
        finish(&mut pins, &mut id, &mut max_blocks, &mut max_bytes)?;
    }
    Ok(pins)
}

fn unquote(value: &str, line_no: usize) -> Result<String, String> {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        Ok(value[1..value.len() - 1].to_string())
    } else {
        Err(format!("baselines.toml:{line_no}: id must be a quoted string"))
    }
}

fn parse_u64(value: &str, line_no: usize) -> Result<u64, String> {
    value.parse::<u64>().map_err(|_| format!("baselines.toml:{line_no}: expected a whole number"))
}
