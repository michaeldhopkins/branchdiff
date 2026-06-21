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

fn stem(program: &str) -> &str {
    Path::new(program)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(program)
}

/// GUI editors that treat a folder as a project. Shared by [`classify_mode`]
/// (they launch detached) and [`opens_directory`] (they open a directory), so the
/// two stay in sync when an editor is added.
fn is_gui_workspace_editor(name: &str) -> bool {
    matches!(
        name,
        "code"
            | "code-insiders"
            | "codium"
            | "cursor"
            | "subl"
            | "sublime_text"
            | "zed"
            | "xed"
            | "bbedit"
            | "acme"
    )
}

/// Unknown editors default to `Foreground`: wrongly suspending a GUI editor only
/// flickers, but fire-and-forgetting a terminal editor corrupts the shared tty.
pub fn classify_mode(program: &str) -> LaunchMode {
    let name = stem(program);
    // nvim-remote hands off to a running nvim and returns immediately, so it
    // wants Detached too — but it's not a workspace editor (see opens_directory).
    if is_gui_workspace_editor(name) || name == "nvim-remote" {
        LaunchMode::Detached
    } else {
        LaunchMode::Foreground
    }
}

/// Editors that open a *directory* in a useful way: GUI workspace editors (which
/// treat a folder as a project) plus terminal editors with a built-in file
/// browser (vim → netrw, emacs → dired, helix → file picker). Editors not listed
/// here — nano, ed, micro, and unknowns — either error or do nothing on a folder,
/// so the caller falls back to the OS file manager instead.
pub fn opens_directory(program: &str) -> bool {
    let name = stem(program);
    // Terminal editors with a directory browser. (nvim-remote is excluded:
    // opening a directory through `nvr` is not meaningful.)
    is_gui_workspace_editor(name)
        || matches!(
            name,
            "vim" | "nvim" | "vi" | "view" | "emacs" | "emacsclient" | "hx" | "helix"
        )
}

/// First non-empty of `$VISUAL`, `$EDITOR`, then the VCS-configured editor
/// (fetched separately since it needs a subprocess), split into a program and
/// its leading args. `None` means nothing is configured.
///
/// The editor string is whitespace-split, so a program path containing spaces
/// or shell quoting is not supported (the common `code --wait` form is).
fn resolve_editor_parts(
    env_get: impl Fn(&str) -> Option<String>,
    vcs_editor: Option<String>,
) -> Option<(String, Vec<String>)> {
    let raw = [env_get("VISUAL"), env_get("EDITOR"), vcs_editor]
        .into_iter()
        .flatten()
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty())?;

    let mut parts = raw.split_whitespace().map(String::from);
    let program = parts.next()?;
    Some((program, parts.collect()))
}

/// Resolve the editor for a single `file`. `None` means nothing is configured —
/// the caller falls back to [`os_open_command`].
pub fn resolve_editor(
    file: &Path,
    env_get: impl Fn(&str) -> Option<String>,
    vcs_editor: Option<String>,
) -> Option<ExternalCommand> {
    let (program, mut args) = resolve_editor_parts(env_get, vcs_editor)?;
    args.push(file.to_string_lossy().into_owned());
    let mode = classify_mode(&program);
    Some(ExternalCommand { program, args, mode })
}

/// Resolve how to open a repo `dir`. If the configured editor can open a
/// directory ([`opens_directory`]), targets it; otherwise — no editor set, or one
/// that can't, like nano — falls back to the OS file manager via
/// [`os_open_command`], which always opens the folder somewhere visible.
pub fn resolve_dir_opener(
    dir: &Path,
    env_get: impl Fn(&str) -> Option<String>,
    vcs_editor: Option<String>,
) -> ExternalCommand {
    match resolve_editor_parts(env_get, vcs_editor) {
        Some((program, mut args)) if opens_directory(&program) => {
            args.push(dir.to_string_lossy().into_owned());
            let mode = classify_mode(&program);
            ExternalCommand { program, args, mode }
        }
        _ => os_open_command(dir),
    }
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
    fn opens_directory_gui_terminal_and_rejects_others() {
        // GUI workspace editors.
        assert!(opens_directory("code"));
        assert!(opens_directory("/usr/local/bin/cursor"));
        assert!(opens_directory("zed"));
        // Terminal editors with a directory browser.
        assert!(opens_directory("vim"));
        assert!(opens_directory("/usr/bin/nvim"));
        assert!(opens_directory("emacs"));
        assert!(opens_directory("hx"));
        // Cannot open a folder usefully.
        assert!(!opens_directory("nano"));
        assert!(!opens_directory("micro"));
        assert!(!opens_directory("ed"));
        assert!(!opens_directory("my-weird-editor"));
        // Detached for file-open, but not directory-capable.
        assert!(!opens_directory("nvim-remote"));
    }

    #[test]
    fn gui_workspace_editors_are_detached_and_dir_capable() {
        // The shared helper must keep both classifications in sync.
        for ed in ["code", "cursor", "zed", "subl", "bbedit"] {
            assert_eq!(classify_mode(ed), LaunchMode::Detached, "{ed} mode");
            assert!(opens_directory(ed), "{ed} should open a directory");
        }
    }

    fn dir() -> PathBuf {
        PathBuf::from("/repo")
    }
    fn dir_arg() -> String {
        dir().to_string_lossy().into_owned()
    }

    #[test]
    fn dir_opener_uses_dir_capable_editor() {
        let cmd = resolve_dir_opener(
            &dir(),
            |k| (k == "EDITOR").then(|| "code --wait".into()),
            None,
        );
        assert_eq!(cmd.program, "code");
        assert_eq!(cmd.args, vec!["--wait".to_string(), dir_arg()]);
        assert_eq!(cmd.mode, LaunchMode::Detached);
    }

    #[test]
    fn dir_opener_uses_terminal_editor_with_suspend() {
        let cmd = resolve_dir_opener(&dir(), |k| (k == "EDITOR").then(|| "vim".into()), None);
        assert_eq!(cmd.program, "vim");
        assert_eq!(cmd.args, vec![dir_arg()]);
        assert_eq!(cmd.mode, LaunchMode::Foreground);
    }

    #[test]
    fn dir_opener_falls_back_for_non_dir_editor() {
        // nano can't open a folder, so we open the OS file manager instead.
        let cmd = resolve_dir_opener(&dir(), |k| (k == "EDITOR").then(|| "nano".into()), None);
        assert_eq!(cmd, os_open_command(&dir()));
    }

    #[test]
    fn dir_opener_falls_back_when_unset() {
        let cmd = resolve_dir_opener(&dir(), |_| None, None);
        assert_eq!(cmd, os_open_command(&dir()));
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
