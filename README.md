# branchdiff

Terminal UI showing unified diff of current branch vs its base.

<img width="651" height="1180" alt="Screenshot 2026-03-02 at 21 00 12" src="https://github.com/user-attachments/assets/a30290ff-116b-44d3-826d-11024a6265f8" />
<img width="654" height="1178" alt="Screenshot 2026-03-02 at 21 10 11" src="https://github.com/user-attachments/assets/d9d0fa61-16a2-4283-8f6c-ca50fb23effc" />
<img width="651" height="1177" alt="Screenshot 2026-03-02 at 21 12 38" src="https://github.com/user-attachments/assets/b5a52f92-8a3c-4dc5-af49-dbd5c157d84e" />

![branchdiff screenshot](assets/screenshot.png)

## Features

- **Git and Jujutsu (jj)** support with automatic backend detection
- **Auto-switching**: detects `jj init` or `.jj` removal at runtime and seamlessly restarts
- Color-coded diff view with distinct colors per change layer (git: committed/staged/unstaged; jj: earlier/current/later commits)
- Inline diff highlighting showing exactly which characters changed
- Three view modes: context (default), changes-only, and full file
- Image diffs with side-by-side before/after panels
- Live file watching with auto-refresh on changes
- Mouse support: scrolling, click-to-collapse file sections, text selection (double-click word, triple-click line)
- Copy to clipboard: selection, file path, entire diff, or git patch format
- Non-interactive output modes for scripting (`--print`, `--diff`)

## Requirements

- **Git**: Any reasonably modern git (1.7+). Conflict detection requires Git 2.38+.
- **Jujutsu** (optional): If a `.jj` directory is present, branchdiff uses jj automatically. When remote tracking bookmarks exist (e.g. `main@origin`), branchdiff shows the full stack diff from `trunk()` to `@`, with earlier stack commits in teal and the current commit's changes in green. Falls back to `@-` vs `@` when no remote is configured.

## Installation

### Homebrew (macOS/Linux)

```bash
brew tap michaeldhopkins/tap
brew install branchdiff
```

### Shell installer (macOS/Linux)

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/michaeldhopkins/branchdiff/releases/latest/download/branchdiff-installer.sh | sh
```

### From source

```bash
cargo install --git https://github.com/michaeldhopkins/branchdiff
```

### Manual download

Download binaries from [GitHub Releases](https://github.com/michaeldhopkins/branchdiff/releases).

## Usage

```bash
branchdiff [path]
```

If no repository is found, branchdiff waits and automatically starts when `git init` or `jj init` is detected.

### Options

| Flag | Description |
|------|-------------|
| `-p`, `--print` | Print diff to stdout and exit (non-interactive mode) |
| `-d`, `--diff` | Output unified patch format to stdout (for `git apply` / `patch`) |
| `--no-auto-fetch` | Disable automatic fetching of base branch |
| `--benchmark N` | Run stress test rendering N frames (for profiling) |
| `-h`, `--help` | Print help |
| `-V`, `--version` | Print version |

### Profiling

The `--benchmark` flag runs a non-interactive stress test for profiling:

```bash
# Run 1000 frames of simulated usage
branchdiff --benchmark 1000

# Generate a profiling report with source attribution
./scripts/profile.py --frames 5000

# Or profile interactively with samply
cargo install samply
samply record ./target/profiling/branchdiff --benchmark 5000
```

The profiling script categorizes functions by source (branchdiff, ratatui, std, system) to help identify what's worth optimizing vs accepting from dependencies.

The benchmark simulates scrolling, file navigation, and view mode changes while rendering each frame.

## Keybindings

| Key | Action |
|-----|--------|
| `j` / `k` | Next / previous file |
| `↓` / `↑` | Scroll line |
| `Ctrl+d` / `PgDn` | Page down |
| `Ctrl+u` / `PgUp` | Page up |
| `g` / `Home` | Go to top |
| `G` / `End` | Go to bottom |
| `c` | Cycle view mode (context → changes → full) |
| `r` | Refresh |
| `y` | Copy selection |
| `p` | Copy current file path |
| `Y` | Copy entire diff |
| `D` | Copy git patch format |
| `?` | Toggle help |
| `q` / `Esc` | Quit |
| `Ctrl+c` | Copy selection (or quit if nothing selected) |

### Mouse

- Scroll wheel to scroll
- Click file headers to collapse/expand
- Click and drag to select text
- Double-click to select word, triple-click to select line

## Contributing

### Requirements

- Rust 1.91+ (edition 2024)

### Build and test

```bash
git clone https://github.com/michaeldhopkins/branchdiff
cd branchdiff
cargo build
cargo test
```

### Install local build

After making changes, install the binary locally:

```bash
cargo install --path .
```

### Before committing

Run clippy with warnings as errors (required by CI):

```bash
cargo clippy -- -D warnings
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Copyright (c) 2025-2026 Michael Hopkins.
