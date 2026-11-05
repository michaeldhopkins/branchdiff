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
use branchdiff::html_page::Refresh;
use branchdiff::input::AppAction;
use branchdiff::message::Message;
use branchdiff::update::{RefreshState, Timers, UpdateConfig, update};

#[path = "support/html_fixture.rs"]
mod html_fixture;

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
    verify_parse_pins();
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
    measure_html(&mut samples);
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

    let mut app = App::new_for_bench(diff.clone());
    app.view.viewport_height = 50;
    measure(samples, "navigation/go_to_top", || {
        app.view.scroll_offset = black_box(100);
        black_box(&mut app).go_to_top();
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

#[derive(Default)]
struct Record {
    id: Option<String>,
    max_blocks: Option<u64>,
    max_bytes: Option<u64>,
    arch_bytes: Option<u64>,
    has_median: bool,
    removed: Option<String>,
}

impl Record {
    fn finish(mut self, pins: &mut Vec<Pin>) -> Result<(), String> {
        self.max_bytes = self.arch_bytes.take().or(self.max_bytes);
        let Some(id) = self.id.take() else {
            return Err("baselines.toml: a [[bench]] record has no id".to_string());
        };
        if id.is_empty() {
            return Err("baselines.toml: a bench id is empty".to_string());
        }
        if self.removed.is_some() {
            if self.max_blocks.is_some() || self.max_bytes.is_some() || self.has_median {
                return Err(format!("baselines.toml: {id} sets removed alongside pin fields"));
            }
            return Ok(());
        }
        match (self.max_blocks, self.max_bytes) {
            (Some(max_blocks), Some(max_bytes)) => pins.push(Pin { id, max_blocks, max_bytes }),
            (None, None) => {}
            _ => {
                return Err(format!(
                    "baselines.toml: {id} sets only one of max_blocks and max_bytes"
                ));
            }
        }
        Ok(())
    }

    fn set(&mut self, key: &str, value: &str, line_no: usize) -> Result<(), String> {
        match key {
            "id" => self.id = Some(unquote(value, line_no)?),
            "max_blocks" => self.max_blocks = Some(parse_u64(value, line_no)?),
            "max_bytes" => self.max_bytes = Some(parse_u64(value, line_no)?),
            "max_bytes_x86_64" => {
                let bytes = parse_u64(value, line_no)?;
                if cfg!(target_arch = "x86_64") {
                    self.arch_bytes = Some(bytes);
                }
            }
            "ref_median_ns" => {
                let _ = parse_u64(value, line_no)?;
                self.has_median = true;
            }
            "removed" => {
                let reason = unquote(value, line_no)?;
                if reason.trim().is_empty() || reason.contains('\n') || reason.contains('\r') {
                    return Err(format!(
                        "baselines.toml:{line_no}: removed must be one non-empty line"
                    ));
                }
                self.removed = Some(reason);
            }
            other => return Err(format!("baselines.toml:{line_no}: unknown key {other}")),
        }
        Ok(())
    }
}

/// The subset of `baselines.toml` this bench reads: `[[bench]]` records that set
/// `max_blocks` and `max_bytes`. Records with only `ref_median_ns` or `removed`
/// are ignored here; combining `removed` with any pin field is rejected.
fn parse_pins(text: &str) -> Result<Vec<Pin>, String> {
    let mut pins = Vec::new();
    let mut current: Option<Record> = None;
    for (number, raw) in text.lines().enumerate() {
        let line_no = number + 1;
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line == "[[bench]]" {
            if let Some(record) = current.replace(Record::default()) {
                record.finish(&mut pins)?;
            }
            continue;
        }
        let Some(record) = current.as_mut() else {
            return Err(format!("baselines.toml:{line_no}: expected [[bench]]"));
        };
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("baselines.toml:{line_no}: expected key = value"));
        };
        record.set(key.trim(), value.trim(), line_no)?;
    }
    if let Some(record) = current {
        record.finish(&mut pins)?;
    }
    Ok(pins)
}

fn verify_parse_pins() {
    let ok = concat!(
        "[[bench]]\nid = \"active\"\nmax_blocks = 2\nmax_bytes = 64\nref_median_ns = 100\n",
        "[[bench]]\nid = \"retired\"\nremoved = \"Folded into active.\"\n",
    );
    let pins = parse_pins(ok).expect("documented removed bench must parse");
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].id, "active");

    let empty = "[[bench]]\nid = \"retired\"\nremoved = \"   \"\n";
    assert!(parse_pins(empty).is_err(), "empty removed reason must fail");

    let with_blocks =
        "[[bench]]\nid = \"retired\"\nmax_blocks = 1\nmax_bytes = 8\nremoved = \"no\"\n";
    assert!(parse_pins(with_blocks).is_err(), "removed with allocation pins must fail");

    let with_median = "[[bench]]\nid = \"retired\"\nref_median_ns = 10\nremoved = \"no\"\n";
    assert!(parse_pins(with_median).is_err(), "removed with ref_median_ns must fail");
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

fn measure_html(samples: &mut Vec<Sample>) {
    let images = branchdiff::image_diff::ImageCache::new();
    // One warmed thread, so the count does not depend on how files are scheduled.
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    for (changed, files, label) in html_fixture::SIZES {
        let data = html_fixture::output_data(changed, files);
        // The first render compiles the syntax's regexes and builds any pool;
        // the pin is for a page rendered by a warm process.
        let mut warm = Vec::new();
        branchdiff::html::render_html_in(&pool, &mut warm, &data, &images, Refresh::Live).unwrap();
        let mut page = Vec::with_capacity(warm.len());
        let id = format!("html/render/{label}");
        measure(samples, &id, || {
            branchdiff::html::render_html_in(&pool, &mut page, &data, &images, Refresh::Live)
                .unwrap();
            black_box(page.len());
        });
    }
}
