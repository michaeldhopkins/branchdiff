After building or making changes, always run `cargo install --path .` to install the binary.

Always run `cargo clippy` before committing and fix any warnings. Clippy warnings should never be allowed to go unaddressed. Using `#[allow(...)]` to suppress warnings is not acceptable - actually fix the underlying issue.
