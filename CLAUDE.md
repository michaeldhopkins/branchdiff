After building or making changes, always run `cargo install --path .` to install the binary.

Always run `cargo clippy -- -D warnings` before committing to match CI. Clippy warnings should never be allowed to go unaddressed. Using `#[allow(...)]` to suppress warnings is not acceptable - actually fix the underlying issue.

Run `cargo llvm-cov` to check test coverage after adding or modifying functionality.

Run `cargo audit` after adding or updating dependencies to check for security vulnerabilities.

When adding user-facing features (keybindings, commands, UI elements):
- Update README.md with feature description and keybindings
- Update the help menu in src/ui/help.rs
- Test the feature manually before committing

Constants and magic numbers:
- If a literal value is used more than once, extract it to a named constant
- Place shared constants in a central location (e.g., `src/image_diff.rs` for image-related constants)
- Clippy does not catch duplicate magic numbers - this requires manual vigilance
