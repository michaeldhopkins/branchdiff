## [0.62.2] - 2026-04-07

### Bug Fixes

- WCAG AA contrast for solarized HTML theme
## [0.62.1] - 2026-04-07

### Bug Fixes

- Hunk boundary sliding for cleaner function-level diffs
## [0.62.0] - 2026-04-07

### Features

- Move detection with block matching across files
## [0.61.1] - 2026-04-05

### Bug Fixes

- HTML output refinements — live-reload, collapsed files, styling

### Miscellaneous

- Regenerate Cargo.lock and changelog
## [0.61.0] - 2026-04-05

### Features

- HTML output mode with shared rendering engine
## [0.60.1] - 2026-04-05

### Bug Fixes

- Consistent bracket notation for status bar toggles

### Miscellaneous

- Regenerate changelog
## [0.60.0] - 2026-04-03

### Features

- Upstream divergence awareness with fork-point/trunk-tip toggle
## [0.59.1] - 2026-03-25

### Bug Fixes

- Gracefully fall back from CommitOnly/BookmarkOnly when revision context disappears
## [0.59.0] - 2026-03-17

### Features

- Auto-retry on transient VCS errors with exponential backoff

### Miscellaneous

- Regenerate changelog
## [0.58.0] - 2026-03-16

### Features

- Auto-copy on highlight and status bar selection
## [0.57.0] - 2026-03-10

### Features

- Add BookmarkOnly view mode for jj stacked bookmarks

### Bug Fixes

- Align help modal color legend indentation and shorten view mode label
## [0.56.2] - 2026-03-02

### Bug Fixes

- Rename [changes] view mode label to [changed lines only]
- Show [all lines] label for Full view mode in status bar
- Update Cargo.lock for v0.56.2
## [0.56.0] - 2026-03-02

### Features

- Show commit short hash in CommitOnly view mode label

### Bug Fixes

- Hide files with no current-commit changes in CommitOnly view
## [0.55.2] - 2026-03-02

### Bug Fixes

- Remove --keepParent from ditto to fix notarization ticket registration
- Remove --keepParent from ditto to fix notarization ticket registration
## [0.55.1] - 2026-03-02

### Miscellaneous

- Add formula and repo fields to tap dispatch payload
- Add Apple code signing and notarization for macOS binaries
## [0.55.0] - 2026-03-01

### Features

- Add Homebrew tap, man pages, shell completions, and changelog

### Bug Fixes

- Correct artifact download paths in release workflow
## [0.54.4] - 2026-02-28

### Bug Fixes

- Render inline deletion text in merged diff view
## [0.54.3] - 2026-02-28

### Bug Fixes

- Publish to crates.io before downloading artifacts to avoid payload size issue
## [0.54.2] - 2026-02-28

### Features

- Detect unstaged file renames using temp index
- Auto-collapse Rails schema files
- Add double-click word selection with smart boundaries
- Use histogram diff algorithm for better structural alignment
- Update help modal with new color scheme and status symbols
- Switch to background-based color highlighting with status symbols
- Add syntax highlighting with syntect
- Add git patch format output
- Add triple-click line selection
- Add per-character contrast checking for syntax highlighting
- Support nested .gitignore files with correct directory scoping
- Add file links detection for app-spec file pairs (Part 1)
- Add image diff detection and placeholder rendering
- Add image loading and metadata display for image diffs
- Implement terminal image rendering with StatefulImage
- Enable partial image rendering at viewport bottom
- Show repo directory name in status bar
- Prevent git lock collisions with external commands
- Add Swift, TypeScript, and extended syntax highlighting
- Detect git init and auto-initialize when started in non-git directory
- Add Jujutsu (jj) VCS backend
- Auto-detect VCS backend (jj preferred over git)
- Auto-detect VCS backend changes at runtime
- Jj full stack diff with per-commit color coding
- Add StackPosition type and wire through construction sites
- Jj mid-stack visibility with full stack diff
- Display jj stack position [N/M] in status bar
- VCS-aware help modal labels
- Show jj change IDs in status bar labels
- Jj-native gutter symbols and VCS-aware help legend
- Add search/find within diffs (/ or Ctrl+F)
- Add CommitOnly view mode for jj (shows only current commit changes with context)

### Bug Fixes

- Improve large repo warning to show actual behavior
- Exclude visual line wrap newlines from copied text
- Set viewport_height before initial FrameContext creation
- Show only actual changes for renamed files
- Uncollapse restored files that were collapsed due to deletion
- Correct inline span highlighting for modified base lines
- Account for wrapped line heights in visible range calculation
- Use unicode display width for wrapped line height calculation
- Count modified base lines in changed_line_count
- Sanitize control characters to prevent ghost artifacts at terminal edges
- Prevent index out of bounds in provenance map for unequal line counts
- Use fallback picker initialization and add cache tests
- Reserve image space before protocols exist
- Correct scrolling and image height calculations
- Use actual font size from Picker for image height calculations
- Include binary and image files in files list for accurate counts
- Tighten image spacing in high-fidelity view
- Use display dimensions for image layout centering
- Use pub(super) visibility for internal diff submodules
- Address adversarial review feedback
- Default to Context view mode instead of Full
- JS line comments no longer bleed into subsequent lines
- Reset syntax highlight state on each render to prevent flickering
- Harden jj backend — rename parsing, binary detection, UTF-8 handling
- Single_file_diff rename support, deduplicate base_identifier
- Render pure deletion inline diffs as split del/ins lines
- Prevent jj refresh cancel storm and update base label live
- Strip jj tracking marker from bookmark labels
- Suppress jj self-triggered refresh loop and add retry logic
- Propagate VCS refresh errors to UI warning banner
- Immediate Full refresh on jj revision changes
- VCS paths bypass gitignore filter for event detection
- Suppress self-triggered VCS events and add post-refresh staleness check
- Update benchmarks for ViewState refactor and update() signature change
- Show change ID before bookmark in jj status labels
- Prevent panic when searching in empty diff
- Is_noisy_path should check relative paths, not absolute
- Add cargo build step before test so integration tests find the binary
- Use GITHUB_PATH to prepend target/debug to PATH for integration tests
- Skip flaky PTY integration tests in CI, run unit tests only

### Refactor

- *(test)* Extract shared TestAppBuilder to reduce duplication
- Extract LineSource classification methods and provenance helpers
- Use stdlib char boundary methods, add rust-version
- Consolidate wrapped_line_height into single source of truth
- Fix antipatterns identified in code review
- Split diff/mod.rs into focused modules
- Use DiffInput struct for readable function calls
- Extract collapse logic to app/collapse.rs
- Extract helper functions from handle_input
- Extract ViewState from App struct
- Extract VCS abstraction layer for future jj support
- Wire Vcs trait through entire app, eliminating direct git calls
- Split vcs/git.rs god module into submodules
- Split update.rs god module into submodules
- Relocate misplaced tests and remove redundancies
- Extract shared VCS types and functions into vcs/shared.rs
- Unify VCS retry logic into shared run_vcs_with_retry
- Replace stringly-typed VCS dispatch with VcsBackend enum

### Documentation

- Add Contributing section to README
- Add semver guidelines to CLAUDE.md
- Add committing section linking semver to commit workflow
- Add antipatterns section to CLAUDE.md
- Add pre-commit checklist to CLAUDE.md for version bump reminder
- Add guidance on unit tests vs integration tests
- Update README with jj support, --diff flag, and current features

### Performance

- Increase debounce time and deduplicate file events
- Use PollWatcher on WSL for reliable file watching
- Add retry logic for transient git errors
- Use platform-native recursive file watching
- Skip rendering when UI state unchanged
- Reduce git subprocess overhead
- Parallelize git operations and fix initial render viewport
- Fix jj VCS idle CPU and subprocess spam
- Batch git cat-file calls to reduce 3*N subprocesses to 3

### Testing

- Improve test suite quality
- Add tests for selection range and span highlighting
- Add comprehensive unit tests for 4-way diff algorithm
- Add comprehensive unit tests for inline diff computation
- Add comprehensive unit tests for color utilities
- Add additional edge case tests for selection utilities
- Add integration tests using portable-pty + vt100
- Add unit tests for diff_view.rs and update module

### Miscellaneous

- Add codebase statistics script
- Add coverage and security audit to dev workflow
- Update Cargo.lock for unicode-width dependency
- Upgrade ratatui to 0.30 and ratatui-image to 10
- Set version to 0.44.1 based on semver history
- Bump version to 0.50.0
- Add MIT/Apache-2.0 dual license for crates.io publishing
- Replace cargo-dist with simple version-driven CI/release workflows
- Update Cargo.lock for v0.54.2
## [0.1.0] - 2025-12-23

### Features

- Auto-fetch base branch and detect merge conflicts
- Show changed line count in status bar instead of total lines
- Add three view modes (full, context, changes-only)
- Width-aware inline diff display
- Responsive status bar for narrow terminals
- Auto-collapse lock files by default
- Show canceled lines for committed additions deleted in working tree
- Add non-interactive print mode (-p/--print)
- J/k navigates between files instead of scrolling
- Add character-level background highlighting for multiline diffs
- Show pure additions/deletions as single wrapped lines
- Add color legend to help modal
- Improve inline diff readability with better coalescing
- Add Ctrl+C to copy selection
- Improve deleted file handling
- Show +/- line counts in status bar with GitHub-style colors
- Add FrameContext and benchmark infrastructure for performance optimization
- Integrate FrameContext into main render loop
- Add FrameContext-based navigation functions
- Add --benchmark flag for profiling
- Add profiling script with source attribution
- Add `p` to copy current file path with visual feedback
- Add `Y` to copy entire diff to clipboard
- Filter file watcher events using .gitignore rules
- Watch newly created directories dynamically
- Add performance warnings for large repos and diffs
- Graceful degradation for git version compatibility

### Bug Fixes

- Refresh getting stuck after cancellation
- Status bar showing wrong branch name after refresh
- Esc dismisses help modal instead of quitting
- Show files in new untracked directories
- Don't show canceled lines for modified additions
- Swap dim/bright red for better color pairing
- Keep local base branch in sync with origin
- Position canceled lines near their original location
- Preserve manual expand/collapse state across refreshes
- Correct position of canceled lines in multi-stage diffs
- Improve refresh resilience in noisy environments
- Handle UTF-8 and empty repos gracefully
- Show inline modifications in context view mode
- Clamp scroll percentage to 100% and handle UTF-8 in wrapping
- *(ci)* Correct dtolnay/rust-toolchain action name
- Align local clippy with CI (-D warnings)
- *(test)* Configure git user in cloned repos for CI

### Other

- Deduplicate refresh logic, use &Path, use extend()
- Extract magic numbers to named constants in ui.rs
- Refresh view when staging/unstaging files

### Refactor

- Reorganize diff.rs into diff/ module
- Reorganize app.rs into app/ module
- Reorganize ui.rs into ui/ module
- Reorganize modals into directory structure
- Remove redundant comments
- Improve code organization and reduce duplication
- Address code review feedback
- Fix clippy warnings and tighten lint configuration
- Extract max_scroll_offset to remove duplication
- Remove deprecated functions and clean up FrameContext migration
- Add message router and DiffViewModel for better testability
- Rewrite benchmarks to test current hot paths
- Remove deprecated test-only methods and fix clippy warnings
- Remove all #[allow(...)] suppressions

### Documentation

- Fix misleading comment about Unix fd limit fallback

### Performance

- Reduce debounce 100ms→20ms, add refresh pipeline tests
- Add parallel file processing with rayon
- Add incremental single-file updates
- Implement lazy inline diff computation
- Add dirty flag to skip redundant inline span computation
- Optimize navigation to use indices instead of cloning lines

### Testing

- Add unit tests for status bar layout logic
- Add unit tests for character-level highlighting

### Miscellaneous

- Add .DS_Store to .gitignore
- Mark as unlicensed (all rights reserved)
- Remove redundant comments
- Add clippy lints to prevent architectural regression
- Add profiling profile for samply
- Add CI workflow and cargo-dist release automation
- Upgrade to Rust Edition 2024 with let-chains
