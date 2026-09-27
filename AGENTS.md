## Pre-Commit Checklist

Before every commit, verify:
1. [ ] `cargo clippy --all-targets -- -D warnings` passes
2. [ ] `cargo test` passes
3. [ ] Version bumped in `Cargo.toml` (patch for fixes, minor for features)
4. [ ] `cargo install --path .` run after version bump (also refreshes `Cargo.lock`, which CI checks with `--locked`)
5. [ ] Changelog regenerated — see [Releasing](#releasing-jj--ci) for the exact jj-aware command
6. [ ] No file under `src/` over 400 production lines (`tests/file_length.rs`; inline test modules and test-only module files are not counted). Files already over when the gate went in (2026-09-14) are pinned at that size and may only shrink. New code goes in a new module, never into a pinned file.

After building or making changes, always run `cargo install --path .` to install the binary.

Run `cargo clippy --all-targets -- -D warnings` before committing. CI's clippy skips test/bench code without `--all-targets`, so a plain `cargo clippy` can pass locally while CI fails — always pass `--all-targets`. Clippy warnings should never be allowed to go unaddressed. Using `#[allow(...)]` to suppress warnings is not acceptable - actually fix the underlying issue.

## Antipatterns (Not Caught by Lints)

These patterns require manual vigilance during code review:

**Stringly-typed error handling:**
- Don't use `.contains()` on error messages for control flow
- Error messages are localized and change between versions
- Use error types, exit codes, or structured data instead

**Panic in error handlers:**
- Don't use `panic!()` inside `unwrap_or_else` closures
- For programmer errors (should never happen), use `.expect("reason")`
- For runtime errors, return `Result` or provide a fallback

**Unnecessary cloning:**
- Watch for `.clone().or(.clone())` - use `match` to consume Options directly
- If a function takes `&T` but clones internally, consider taking `T` by value
- Cloning for thread spawning (`Arc::clone`, `PathBuf::clone`) is correct

**Code duplication:**
- If the same logic appears 3+ times, extract a helper function
- Common patterns: emit/flush loops, validation checks, state updates
- Lints can't detect semantic duplication - only humans can

**Local fixes that ignore root cause:**
- Adding `.clone()` to satisfy the borrow checker instead of restructuring
- Wrapping errors in strings instead of adding enum variants
- Suppressing warnings instead of fixing the underlying issue

Run `cargo llvm-cov` to check test coverage after adding or modifying functionality.

Run `cargo audit` after adding or updating dependencies to check for security vulnerabilities.

## Testing Requirements

**Every code change requires tests.** This is non-negotiable.

- New functions/methods: Add unit tests covering the happy path and at least one edge case
- Bug fixes: Write a failing test that reproduces the bug first, then fix it
- UI changes: Test the formatting/layout logic with unit tests
- Refactors: Ensure existing tests still pass; add tests if coverage gaps are found

If a change seems hard to test, that's a signal the code needs refactoring. Extract pure functions, separate logic from rendering, or introduce seams for dependency injection. Never skip tests because "it's too tangled" - untangle it first.

Run `cargo test` before every commit. A change without corresponding tests is incomplete.

### Unit Tests vs Integration Tests

**Unit tests** (in `src/`) test logic in isolation. They should cover the vast majority of code.

**Integration tests** (in `tests/integration/`) spawn the real binary in a PTY. Use them *only* for:
- Runtime interactions with external systems (git commands, file watching)
- Behavior that depends on terminal state (PTY dimensions, escape sequences)
- End-to-end flows that cross multiple system boundaries

If you need an integration test to verify rendering or UI logic, that's a design smell. The renderer should accept data (e.g., a `DiffResult`) and produce output - it shouldn't query git directly. Refactor to make the logic unit-testable.

**Examples:**
- Testing that view mode cycles correctly when pressing 'c' → integration test (requires real terminal input)
- Testing that `ViewMode::Context` filters lines correctly → unit test (pure logic on data)
- Testing that file watcher detects git changes → integration test (real filesystem/git)
- Testing that status bar formats "+3 -2" correctly → unit test (string formatting)

When adding user-facing features (keybindings, commands, UI elements):
- Update README.md with feature description and keybindings
- Update the help menu in `src/ui/modals/help.rs`
- Test the feature manually before committing

**Testing keybindings/external launches against ambient environment.** The
integration harness spawns `branchdiff` from `PATH`, so it runs the *installed*
binary — run `cargo install --path .` before integration tests or you'll test
stale code. Run integration tests with `--test-threads=1` (PTY tests are not
parallel-safe). And when a test sets `$EDITOR`, it MUST also set `$VISUAL=""`:
editor resolution prefers `$VISUAL`, then `$EDITOR`, then the VCS-configured
editor (`git core.editor` / `jj ui.editor`), so a developer's ambient `$VISUAL`
or git config silently shadows the test's mock. Such tests pass on CI (clean
env) but fail locally — neutralize the whole precedence chain in the test env.

Constants and magic numbers:
- If a literal value is used more than once, extract it to a named constant
- Place shared constants in a central location (e.g., `src/image_diff.rs` for image-related constants)
- Clippy does not catch duplicate magic numbers - this requires manual vigilance

## Mutation Testing

Config lives in `.cargo/mutants.toml`; a bare `cargo mutants` picks it up.

**Two traps specific to this crate, both of which produced vacuous tests:**

- **`DEFAULT_FG` is the same RGB the syntax theme gives unstyled text**
  (`Rgb(200, 200, 200)` in both `ui::colors` and `syntax::theme`). So asserting
  `fg_color == DEFAULT_FG` does **not** prove a line skipped highlighting — a
  parsed-but-unstyled line is identical. Prove it with
  `HighlightCache::parse_count()` instead, which counts what actually reached
  the parser.
- **Upper-bound assertions on `parse_count()` cannot catch under-counting.**
  `assert!(count <= n)` passes just as happily when a counter stops incrementing.
  Every guarantee that matters here is "no *more* than one parse per exposed
  line", so pair those with at least one exact assertion over a case that really
  replays context.

**Measured on `src/syntax/cache.rs` (2026-08-04),** with the config above, at
`-j1`, nothing else running: 61 mutants in **25 min** — 41 caught, 1 missed,
16 unviable, 3 timeouts → **98%** (41/42, unviable excluded).

Baseline is 32s cold build + 20s test, so this tree is *test*-bound and
restricting the suite is the useful lever. Do not run it at `-j2` alongside
other work: doing so manufactured 8 timeouts that dropped to 3 when re-run
serially (the tell was 148-197s build times against 2-3s). Timing is sensitive
to the timeout setting — the same file took 33 min with `--timeout 300`, purely
because the three timeout mutants each waited longer before being called.

- The one survivor is **equivalent**: `MAX_CONTEXT_REPLAY = CHECKPOINT_EVERY * 2`
  mutated to `+ 2` still exceeds the 16-step guarantee the checkpoint grid
  already provides, so the backstop never binds either way and no test can tell.
  (`/ 2` **is** killable and is pinned by
  `the_longest_possible_context_walk_still_reaches_a_checkpoint`.)
- The 3 timeouts all break `advance_over`'s length guard, which lets over-long
  lines reach the parser — the suite grinds instead of failing, so **a clean run
  exits 3, not 0.** That is a real detection; do not exclude them.

**`src/ui/status_bar.rs`:** 13 caught, 2 missed. Both survivors are the padding
arithmetic in `draw_status_bar`'s *shorter-help* one-line arm, which is
unreachable: `status_bar_height` returns 1 only when the full help fits, and the
one-line layout tests that same condition first. The coupling is pinned
behaviourally by `a_one_line_status_bar_always_shows_the_full_help`, so if the
two thresholds ever diverge that arm goes live and the test fails.

**The test command is `--lib --bins`**, the set CI's test step runs.
`src/main.rs`, `src/html.rs` and `src/print.rs` belong to the binary and carry
their own test modules, so `--lib` alone (the config until 2026-09-27) reported
every mutant in them as missed. The PTY integration target stays out: it runs
the installed binary, and four of its tests fail on this Mac regardless of the
code.

**Per change** (`.github/workflows/mutants.yml`, not gating): every PR and push
to `main` runs `--in-diff` over the changed code, skipped with a warning above
16 selected mutants; each push to `main` also runs one **rotating slice**,
`--shard $((run_number % 220))/220`. The workflow installs a pinned jj, because
73 tests in `src/vcs/jj.rs` return early without it and their mutants would
come back missed, and sets `JJ_USER`/`JJ_EMAIL`: with no jj identity (an
empty `JJ_CONFIG` and `HOME`, as on a runner) ten of those tests fail, the
baseline fails, and cargo-mutants exits 4 having tested nothing. ci.yml never
installs jj, so these tests have not run in CI before. There is no whole-tree
sweep.

**Choosing N (2026-09-27).** The tree has 3,124 mutants. Slice `0/100` (32
mutants) took **23 min** at `-j2` with other builds loading the machine
(load average 25-55): ~43 s per mutant, the unit suite's 20 s stretched to ~90 s
per run by contention. Ten minutes is ~14 mutants, so N = 220. CI's runner is
uncontended but single-job, so re-measure from the slice job's timings once a
few have run, and adjust N if slices finish far from ten minutes.

**Slice 0 findings (2026-09-27): 0 caught, 28 missed, 4 unviable** — the slice
was all `src/main.rs` process-edge code, none of it unit-tested. Resolved:

- Tests added: `force_repaint` (a screen wiped behind ratatui's back is redrawn,
  using `TestBackend`), `run_external` (a program that cannot start is an
  error), `launch_or_flash` (that error becomes a flash).
- Logic moved out of `main.rs` into tested library code: the detect-failure
  policy, one-shot mode, the one-shot view mode and HTML image preloading
  (`src/startup.rs`), the waiting screen's layout (`startup::waiting_message_area`),
  the `$VISUAL`/`$EDITOR` lookup policy (`external::vcs_editor_if_unset`), and
  `Cli::auto_fetch`.
- Excluded in `.cargo/mutants.toml`, each with its reason: the terminal Drop
  guards, `main`, `run_waiting_for_vcs`, `open_*_in_editor` and the env-binding
  `vcs_editor_if_unset` wrapper. What remains in them is wiring to the TTY,
  environment, argv or a spawned editor. Later slices reach more of `main.rs`
  (`run_main_app`, `run_app`, the watcher setup); extract before excluding.

Run the per-change check locally with the prefix override, or it silently
selects nothing:

```sh
git -c diff.mnemonicPrefix=false -c diff.noprefix=false diff main -- src/ > /tmp/b.diff
cargo mutants --no-shuffle -j1 --in-diff /tmp/b.diff
```

## Fuzzing

cargo-fuzz targets live in `fuzz/`, a standalone workspace built only by
`cargo +nightly fuzz` (the `rust-fuzzing` skill has the general method). CI:
`fuzz-replay.yml` replays each target's saved corpus on every push to `main` and
every PR (the gate); `fuzz.yml` gives each target ~180s of mutation on each push
to `main` through `fuzz/burst.sh` and saves the grown corpus (not a gate, no
schedule; `workflow_dispatch` takes a longer budget). `tests/fuzz_targets_wired.rs`
fails if a `[[bin]]` in `fuzz/Cargo.toml` is missing from either workflow.

```sh
cargo +nightly fuzz build
fuzz/burst.sh fuzz/target/aarch64-apple-darwin/release/diff_patch diff_patch 60
fuzz/target/aarch64-apple-darwin/release/diff_patch -runs=0 fuzz/corpus/diff_patch   # replay
```

The crate-private parsers are reached through `src/fuzz_api.rs`, compiled only
under `--cfg fuzzing` (which cargo-fuzz sets), so the published library API does
not grow. A new target that needs a private function gets a wrapper there.

| Target | Kind | Asserts |
|---|---|---|
| `vcs_output` | never-panics | Every parser over git/jj stdout: `git status --porcelain`, `diff --name-status`, `--numstat`, `git --version`, jj `diff --summary`/`--stat`, the rev-metadata template. |
| `cat_file_batch` | roundtrip + never-panics | A `git cat-file --batch` stream built from fuzzed responses (blob, tree, `missing`, `ambiguous`, with fuzzed paths) reads back as exactly its blobs; a raw stream never panics or allocates what a header claims. |
| `diff_patch` | structural invariant | `compute_four_way_diff` over four fuzzed file versions, turned into the `--diff` patch and applied to the base, gives the working tree; hunk headers match their bodies. |

**Input vocabulary.** `fuzz/dict/vcs_output.dict` and `cat_file_batch.dict` are
the literals the parsers branch on (`src/vcs/git/parse.rs`,
`src/vcs/git/changed_files.rs`, `src/vcs/jj.rs`); update them when a parser
starts matching a new token. `diff_patch` input is text,
`base\0head\0index\0working` (a missing part repeats the previous one), so seeds
are written by hand and byte mutation produces layers that share lines. It was
first written with an `arbitrary` struct of line tokens and ran 60s without
reaching the modified-line bug the first hand-written text seed hit at once.

**Bugs found (2026-09-26), each with a unit test and a `seed-*`:**
- `vcs_output`: a status line whose columns end inside a multi-byte character panicked on a byte slice.
- `cat_file_batch`: a header's claimed size was allocated up front (OOM abort);
  an error response for a path like `a blob 5` was read as a blob and
  desynchronised every later response.
- `diff_patch`: `--diff` wrote a modified line as context, so a branch that only
  edited lines gave an empty patch; deletions were ordered by a trace that
  ignored branch modifications; a staged edit of a committed line claimed to come
  from base; modification pairs could cross.

**What the `diff_patch` frame hides.** HEAD, index and working are always
present, so `check_file_deletion` (a file absent at one layer) is not exercised.
The comparison is modulo trailing whitespace and on `str::lines()`, because
branchdiff trims each line's end and `lines()` drops the `\r` of `\r\n`: as a
result `--diff` output does not apply with plain `git apply` to a file with CRLF
endings or trailing whitespace on a context line (verified 2026-09-26), and a
whitespace-only change is invisible. That is a known limitation, not something
this target checks.

**Not fuzzed, and why.**
- vcs-runner's own parsing (`read_working_file`, run/retry helpers): that crate's
  own fuzzing covers it. Only branchdiff's code is fuzzed here.
- Image decoding (`image`, `resvg`) and syntax highlighting (`syntect`):
  third-party parsers that branchdiff only calls.
- HTML output (`src/html.rs`) is in the binary, not the library, so the fuzz
  crate cannot reach it; every text it writes goes through `html_escape`, which
  is unit-tested.
- Terminal control sequences: branchdiff does not promise to strip them. `-p`
  writes file content through verbatim, so an escape sequence in a changed file
  reaches the terminal. There is no sanitising code to fuzz; adding it is a
  product decision.
- jj's change-id and stack-position output is split by line and compared, with
  no indexing that could panic.

## Committing

Before committing, bump the version in `Cargo.toml` according to semver rules below. Every commit that changes behavior or fixes bugs requires a version bump. Run `cargo install --path .` after bumping to update `Cargo.lock`.

## Releasing (jj + CI)

This repo is colocated under **jj** (Jujutsu); use `jj`, not raw `git`. Releases
are fully automated by `.github/workflows/release.yml` — **do not create tags
manually.**

**What CI does on every push to `main`:** reads the version from `Cargo.toml`;
if a `v<version>` tag does not already exist, it builds the platform binaries,
**creates and pushes the `v<version>` tag itself**, generates release notes with
`git cliff --latest --strip all`, publishes the GitHub release, and dispatches
the Homebrew formula update. Implications:
- The only "release trigger" is a version bump landing on `main`. If you forget
  to bump `Cargo.toml`, the push is a silent no-op (no release).
- Never pre-tag locally — a pre-existing `v<version>` tag makes CI *skip* the
  release entirely (and the Homebrew dispatch with it).
- The committed `CHANGELOG.md` keeps `## [unreleased]` as the heading for the
  new entry. CI's `git cliff --latest` produces the release notes; the heading
  only rolls to `## [x.y.z]` on the *next* changelog regeneration (once the tag
  exists). Don't hand-edit it to a version number.

**Regenerating the changelog under jj (the non-obvious part).** `git cliff`
walks git's `HEAD`, but in a colocated jj repo `HEAD` tracks `@-` (the parent of
the working copy), *not* the working-copy commit `@`. So running `git cliff`
right after `jj describe` will NOT see your new commit — it silently rewrites the
changelog for the *previous* state. The working sequence:

```
jj describe -m "feat: ..."          # describe the working-copy commit @
jj new                              # start an empty child; now HEAD == your commit
git cliff --output CHANGELOG.md     # walks HEAD, so your commit is included
jj squash                           # fold the CHANGELOG edit back into the feat commit
```

**Pushing** (only with explicit user approval): `jj bookmark set main -r <feat-commit>`
then `jj git push --bookmark main`. After pushing, watch the run with
`gh run list --workflow=release.yml` / `gh run watch <id>`.

## Versioning (Semver)

This project uses semantic versioning:

**PATCH bump (0.x.Y → 0.x.Y+1)** for:
- Bug fixes
- Performance improvements (no new features)
- Internal refactoring
- Documentation updates
- Dependency updates (non-breaking)
- Test additions

**MINOR bump (0.X.y → 0.X+1.0)** for:
- New user-facing features (keybindings, commands, view modes)
- New CLI flags or arguments
- New output formats
- Visual enhancements that add capability

**MAJOR bump (X.y.z → X+1.0.0)** for:
- Breaking changes to CLI arguments (removing/renaming flags)
- Breaking changes to output format (for `-p` print mode)
- Removing keybindings or features users depend on

For this TUI app with CLI mode, "breaking change" means:
- Scripts using `-p` print mode would break
- Users' muscle memory for keybindings would be invalidated
- Documented behavior changes incompatibly

**The crate also publishes a library** (`src/lib.rs`, and `release.yml` runs
`cargo publish`). Nothing is known to depend on it — branchdiff is consumed as a
CLI, and crates.io mainly serves `cargo install` — but the API is public
regardless, so **removing or re-signing a `pub` item bumps MINOR**, not patch,
even when nothing user-facing changes. Check with
`git diff main -- src/ | grep -E '^[+-] *pub '` before choosing the bump; a
signature change buried in a refactor is easy to miss, and shipping one as a
patch breaks `cargo build` for anyone who did depend on it.

Commits prefixed with `feat:` should bump minor. Commits prefixed with `fix:` should bump patch. Commits prefixed with `chore:`, `refactor:`, `test:`, `docs:`, `perf:`, `build:` should bump patch (or nothing if purely internal).
