## Pre-Commit Checklist

Before every commit, verify:
1. [ ] `cargo clippy -- -D warnings` passes
2. [ ] `cargo test` passes
3. [ ] Version bumped in `Cargo.toml` (patch for fixes, minor for features)
4. [ ] `cargo install --path .` run after version bump

After building or making changes, always run `cargo install --path .` to install the binary.

Always run `cargo clippy -- -D warnings` before committing to match CI. Clippy warnings should never be allowed to go unaddressed. Using `#[allow(...)]` to suppress warnings is not acceptable - actually fix the underlying issue.

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

When adding user-facing features (keybindings, commands, UI elements):
- Update README.md with feature description and keybindings
- Update the help menu in src/ui/help.rs
- Test the feature manually before committing

Constants and magic numbers:
- If a literal value is used more than once, extract it to a named constant
- Place shared constants in a central location (e.g., `src/image_diff.rs` for image-related constants)
- Clippy does not catch duplicate magic numbers - this requires manual vigilance

## Committing

Before committing, bump the version in `Cargo.toml` according to semver rules below. Every commit that changes behavior or fixes bugs requires a version bump. Run `cargo install --path .` after bumping to update `Cargo.lock`.

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

Commits prefixed with `feat:` should bump minor. Commits prefixed with `fix:` should bump patch. Commits prefixed with `chore:`, `refactor:`, `test:`, `docs:`, `perf:`, `build:` should bump patch (or nothing if purely internal).
