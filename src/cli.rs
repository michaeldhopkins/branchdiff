use std::path::PathBuf;

use clap::Parser;

/// Output mode for the application
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputMode {
    /// Interactive TUI mode (default)
    #[default]
    Tui,
    /// Print branchdiff format to stdout
    Print,
    /// Output git patch format to stdout
    Diff,
    /// Output self-contained HTML to stdout
    Html,
}

/// Mutually exclusive output format flags.
/// clap's `group(multiple = false)` enforces only one can be set.
#[derive(clap::Args)]
#[group(multiple = false)]
#[expect(clippy::struct_excessive_bools, reason = "mutually exclusive clap flags, not independent state")]
pub struct OutputFlags {
    /// Print diff to stdout and exit (non-interactive mode)
    #[arg(short = 'p', long = "print")]
    print: bool,

    /// Output unified patch format to stdout (for use with git apply / patch)
    #[arg(short = 'd', long = "diff")]
    diff: bool,

    /// Output self-contained styled HTML to stdout
    #[arg(long = "html")]
    html: bool,
}

impl OutputFlags {
    pub fn mode(&self) -> OutputMode {
        if self.print {
            OutputMode::Print
        } else if self.diff {
            OutputMode::Diff
        } else if self.html {
            OutputMode::Html
        } else {
            OutputMode::Tui
        }
    }
}

#[derive(Parser)]
#[command(name = "branchdiff")]
#[command(about = "Terminal UI showing unified diff of current branch vs its base")]
#[command(version)]
pub struct Cli {
    /// Path to repository (default: current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Disable automatic fetching of base branch
    #[arg(long)]
    pub no_auto_fetch: bool,

    #[command(flatten)]
    pub output: OutputFlags,

    /// Run stress test for profiling (renders N frames with simulated input)
    #[arg(long, value_name = "FRAMES")]
    pub benchmark: Option<usize>,
}
