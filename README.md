# branchdiff

Terminal UI showing unified diff of current branch vs main/master.

![branchdiff screenshot](assets/screenshot.png)

## Features

- Color-coded diff view: cyan (committed), green (staged), yellow (unstaged), red (deleted)
- Inline diff highlighting for modified lines
- Context-only view to focus on changes
- Live file watching - auto-refreshes on changes
- Mouse support for scrolling and text selection
- Copy selection to clipboard

## Installation

```bash
cargo install --path .
```

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

# Profile with samply (recommended)
cargo install samply
samply record branchdiff --benchmark 5000
```

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
| `?` | Toggle help |
| `q` / `Esc` / `Ctrl+c` | Quit |

## License

All rights reserved. Copyright (c) 2025 Michael Hopkins.
