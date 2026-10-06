use std::path::PathBuf;

use clap::Parser;

/// Output mode for the application
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
// Provide our own version flag so the primary short is `-v` (the convention users
// reach for); `-V` is kept as an alias to avoid breaking muscle memory.
#[command(version, disable_version_flag = true)]
pub struct Cli {
    /// Print version
    #[arg(short = 'v', short_alias = 'V', long = "version", action = clap::ArgAction::Version)]
    version: Option<bool>,

    /// Path to repository (default: current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Compare against this base instead of the detected one.
    ///
    /// jj: any revset (e.g. `main@origin`, `develop`, a change id).
    /// git: a branch name, `origin/`-qualified name, or commit.
    ///
    /// Use this when the detected base is wrong — most often a jj repo whose
    /// `trunk()` was pinned to the wrong remote when it was created.
    #[arg(long, value_name = "REV")]
    pub base: Option<String>,

    /// Disable automatic fetching of base branch
    #[arg(long)]
    pub no_auto_fetch: bool,

    #[command(flatten)]
    pub output: OutputFlags,

    /// Run stress test for profiling (renders N frames with simulated input)
    #[arg(long, value_name = "FRAMES")]
    pub benchmark: Option<usize>,
}

impl Cli {
    /// Whether to fetch the base branch in the background: on unless
    /// `--no-auto-fetch`.
    pub fn auto_fetch(&self) -> bool {
        !self.no_auto_fetch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use clap::error::ErrorKind;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn each_output_flag_selects_its_mode() {
        let mode = |args: &[&str]| {
            Cli::try_parse_from(std::iter::once("branchdiff").chain(args.iter().copied()))
                .map(|cli| cli.output.mode())
                .ok()
        };
        assert_eq!(mode(&[]), Some(OutputMode::Tui));
        assert_eq!(mode(&["--print"]), Some(OutputMode::Print));
        assert_eq!(mode(&["-d"]), Some(OutputMode::Diff));
        assert_eq!(mode(&["--html"]), Some(OutputMode::Html));
    }

    /// `Cli` does not derive `Debug`, so `unwrap_err()` (which requires the `Ok`
    /// type be `Debug`) won't compile — assert on the kind directly.
    fn version_kind(arg: &str) -> Option<ErrorKind> {
        Cli::try_parse_from(["branchdiff", arg]).err().map(|e| e.kind())
    }

    #[test]
    fn auto_fetch_is_on_by_default_and_off_with_the_flag() {
        let parse = |args: &[&str]| {
            let Ok(cli) = Cli::try_parse_from(args) else { panic!("{args:?} should parse") };
            cli
        };
        assert!(parse(&["branchdiff"]).auto_fetch());
        assert!(!parse(&["branchdiff", "--no-auto-fetch"]).auto_fetch());
    }

    #[test]
    fn version_flag_accepts_lowercase_v() {
        // The original bug: `-v` was rejected while only `-V` worked.
        assert_eq!(version_kind("-v"), Some(ErrorKind::DisplayVersion));
    }

    #[test]
    fn version_flag_keeps_uppercase_and_long_aliases() {
        for arg in ["-V", "--version"] {
            assert_eq!(version_kind(arg), Some(ErrorKind::DisplayVersion), "expected version output for {arg}");
        }
    }
}
