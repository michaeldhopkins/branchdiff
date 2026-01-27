# branchdiff

Terminal UI showing unified diff of current branch vs main/master.
<img width="987" height="1172" alt="Screenshot 2026-01-26 at 22 30 08" src="https://github.com/user-attachments/assets/d5301f02-bb6e-4ba2-b15d-c5e1af7e165f" />

<img width="742" height="1144" alt="Screenshot 2026-01-21 at 22 39 43" src="https://github.com/user-attachments/assets/afc453b4-5b43-458e-91ae-7f739ff97f8e" />


![branchdiff screenshot](assets/screenshot.png)



## Features

- Color-coded diff view: cyan (committed), green (staged), yellow (unstaged), red (deleted)
- Inline diff highlighting for modified lines
- Context-only view to focus on changes
- Live file watching - auto-refreshes on changes
- Mouse support for scrolling and text selection
- Copy selection to clipboard

## Requirements

- **Git**: Works with any reasonably modern git (1.7+). Some features require newer versions:
  - Conflict detection (warning when base branch has diverged): Git 2.38+

## Installation

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

### Options

| Flag | Description |
|------|-------------|
| `-p`, `--print` | Print diff to stdout and exit (non-interactive mode) |
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
| `j` | Next file |
| `k` | Previous file |
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
| `?` | Toggle help |
| `q` / `Esc` / `Ctrl+c` | Quit |

## Contributing

### Requirements

- Rust 1.85+ (edition 2024)

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

All rights reserved. Copyright (c) 2025 Michael Hopkins.
