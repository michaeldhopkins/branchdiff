//! Launching external programs. Splits *what* to run (this module: pure
//! resolution + classification) from *how* to run it relative to the TUI (the
//! runner in `main`, which owns the terminal it must suspend). Future "opens"
//! reuse [`ExternalCommand`] + [`LaunchMode`] with no editor-specific code.

use std::path::Path;

use crate::vcs::VcsBackend;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// Suspend the TUI, run attached to the terminal, wait, then restore.
    Foreground,
    /// Spawn and return immediately; the TUI keeps running.
    Detached,
}

/// Plain data (not a `std::process::Command`) so it travels through `UpdateResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalCommand {
    pub program: String,
    pub args: Vec<String>,
    pub mode: LaunchMode,
}

/// Unknown editors default to `Foreground`: wrongly suspending a GUI editor only
/// flickers, but fire-and-forgetting a terminal editor corrupts the shared tty.
pub fn classify_mode(program: &str) -> LaunchMode {
    let name = Path::new(program)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(program);
    match name {
        "code" | "code-insiders" | "codium" | "cursor" | "subl" | "sublime_text" | "zed"
        | "xed" | "bbedit" | "acme" | "nvim-remote" => LaunchMode::Detached,
        _ => LaunchMode::Foreground,
    }
}

/// First non-empty of `$VISUAL`, `$EDITOR`, then the VCS-configured editor
/// (fetched separately since it needs a subprocess). `None` means nothing is
/// configured — the caller falls back to [`os_open_command`].
///
/// The editor string is whitespace-split, so a program path containing spaces
/// or shell quoting is not supported (the common `code --wait` form is).
pub fn resolve_editor(
    file: &Path,
    env_get: impl Fn(&str) -> Option<String>,
    vcs_editor: Option<String>,
) -> Option<ExternalCommand> {
    let raw = [env_get("VISUAL"), env_get("EDITOR"), vcs_editor]
        .into_iter()
        .flatten()
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty())?;

    let mut parts = raw.split_whitespace().map(String::from);
    let program = parts.next()?;
    let mut args: Vec<String> = parts.collect();
    args.push(file.to_string_lossy().into_owned());
    let mode = classify_mode(&program);
    Some(ExternalCommand { program, args, mode })
}

pub fn os_open_command(file: &Path) -> ExternalCommand {
    let f = file.to_string_lossy().into_owned();
    #[cfg(target_os = "macos")]
    let (program, args) = ("open".to_string(), vec![f]);
    #[cfg(target_os = "windows")]
    // `start`'s first quoted arg is the window title, hence the empty string.
    let (program, args) = (
        "cmd".to_string(),
        vec!["/C".to_string(), "start".to_string(), String::new(), f],
    );
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let (program, args) = ("xdg-open".to_string(), vec![f]);

    ExternalCommand {
        program,
        args,
        mode: LaunchMode::Detached,
    }
}

/// The editor configured in the active VCS (`git core.editor` / `jj ui.editor`).
/// Runs a subprocess, so call only when the env vars are unset.
pub fn vcs_configured_editor(backend: VcsBackend, repo_path: &Path) -> Option<String> {
    use std::process::Command;
    let output = match backend {
        VcsBackend::Git => Command::new("git")
            .args(["config", "--get", "core.editor"])
            .current_dir(repo_path)
            .output(),
        VcsBackend::Jj => Command::new("jj")
            .args(["config", "get", "ui.editor"])
            .current_dir(repo_path)
            .output(),
    }
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn file() -> PathBuf {
        PathBuf::from("/repo/src/main.rs")
    }
    fn path_arg() -> String {
        file().to_string_lossy().into_owned()
    }

    #[test]
    fn classify_terminal_gui_and_unknown() {
        assert_eq!(classify_mode("vim"), LaunchMode::Foreground);
        assert_eq!(classify_mode("/usr/bin/nvim"), LaunchMode::Foreground);
        assert_eq!(classify_mode("nano"), LaunchMode::Foreground);
        assert_eq!(classify_mode("code"), LaunchMode::Detached);
        assert_eq!(classify_mode("code.cmd"), LaunchMode::Detached);
        assert_eq!(classify_mode("subl"), LaunchMode::Detached);
        assert_eq!(classify_mode("zed"), LaunchMode::Detached);
        assert_eq!(classify_mode("my-weird-editor"), LaunchMode::Foreground);
    }

    #[test]
    fn resolve_prefers_visual_over_editor() {
        let cmd = resolve_editor(
            &file(),
            |k| match k {
                "VISUAL" => Some("vim".into()),
                "EDITOR" => Some("nano".into()),
                _ => None,
            },
            None,
        )
        .unwrap();
        assert_eq!(cmd.program, "vim");
        assert_eq!(cmd.mode, LaunchMode::Foreground);
        assert_eq!(cmd.args, vec![path_arg()]);
    }

    #[test]
    fn resolve_splits_args_and_classifies_gui() {
        let cmd = resolve_editor(
            &file(),
            |k| (k == "EDITOR").then(|| "code --wait".into()),
            None,
        )
        .unwrap();
        assert_eq!(cmd.program, "code");
        assert_eq!(cmd.args, vec!["--wait".to_string(), path_arg()]);
        assert_eq!(cmd.mode, LaunchMode::Detached);
    }

    #[test]
    fn resolve_falls_back_to_vcs_editor() {
        let cmd = resolve_editor(&file(), |_| None, Some("emacs".into())).unwrap();
        assert_eq!(cmd.program, "emacs");
        assert_eq!(cmd.mode, LaunchMode::Foreground);
    }

    #[test]
    fn resolve_none_when_unset_or_blank() {
        assert!(resolve_editor(&file(), |_| None, None).is_none());
        assert!(
            resolve_editor(
                &file(),
                |k| (k == "EDITOR").then(|| "   ".into()),
                Some(String::new()),
            )
            .is_none()
        );
    }

    #[test]
    fn os_open_is_detached() {
        let cmd = os_open_command(&file());
        assert_eq!(cmd.mode, LaunchMode::Detached);
        #[cfg(target_os = "macos")]
        assert_eq!(cmd.program, "open");
        #[cfg(target_os = "linux")]
        assert_eq!(cmd.program, "xdg-open");
        #[cfg(target_os = "windows")]
        assert_eq!(cmd.program, "cmd");
    }
}
