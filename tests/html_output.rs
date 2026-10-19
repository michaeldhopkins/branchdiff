use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn git_in(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    cmd.current_dir(dir);
    cmd
}

#[test]
fn html_mode_writes_a_page_naming_the_changed_file() {
    let dir = TempDir::new().expect("create temp dir");

    let assert_git = |cmd: &mut Command, what: &str| {
        let out = cmd.output().unwrap_or_else(|e| panic!("run git {what}: {e}"));
        assert!(
            out.status.success(),
            "git {what} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    let mut init = git_in(dir.path());
    assert_git(init.args(["init"]), "init");
    std::fs::write(dir.path().join("notes.txt"), "first\n").expect("write notes.txt");
    let mut add = git_in(dir.path());
    assert_git(add.args(["add", "."]), "add");
    let mut commit = git_in(dir.path());
    assert_git(commit.args(["commit", "-m", "base"]), "commit base");
    std::fs::write(dir.path().join("notes.txt"), "first\nsecond\n").expect("rewrite notes.txt");

    let path = std::env::var("PATH").expect("PATH is set in test env");
    let output = Command::new(env!("CARGO_BIN_EXE_branchdiff"))
        .args([".", "--html", "--no-auto-fetch"])
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", path)
        .env("HOME", dir.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run branchdiff --html");

    assert!(
        output.status.success(),
        "branchdiff --html failed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lower = stdout.to_lowercase();
    assert!(lower.contains("<html"), "stdout missing <html:\n{stdout}");
    assert!(lower.contains("</html>"), "stdout missing </html>:\n{stdout}");
    assert!(stdout.contains("notes.txt"), "stdout must name notes.txt:\n{stdout}");
    assert!(stdout.contains("second"), "stdout must contain 'second':\n{stdout}");
}
