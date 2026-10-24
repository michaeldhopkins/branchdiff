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

#[test]
#[allow(clippy::too_many_lines)]
fn cli_comments_flag_stdin_and_autodetect_work_across_print_html_and_fingerprint() {
    use std::io::Write;
    use std::process::Stdio;

    let dir = TempDir::new().expect("create temp dir");
    let assert_git = |cmd: &mut Command, what: &str| {
        let out = cmd.output().unwrap_or_else(|e| panic!("run git {what}: {e}"));
        assert!(
            out.status.success(),
            "git {what} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    assert_git(git_in(dir.path()).args(["init"]), "init");
    std::fs::write(dir.path().join("notes.txt"), "old1\nkeep2\n").expect("write notes.txt");
    assert_git(git_in(dir.path()).args(["add", "."]), "add");
    assert_git(git_in(dir.path()).args(["commit", "-m", "base"]), "commit base");
    std::fs::write(dir.path().join("notes.txt"), "keep2\nnew2\n").expect("rewrite notes.txt");

    let path = std::env::var("PATH").expect("PATH is set in test env");

    // 1. Autodetect from .git/comments.jsonl in --print mode
    let default_comments = dir.path().join(".git/comments.jsonl");
    std::fs::write(
        &default_comments,
        "{\"path\":\"notes.txt\",\"line\":1,\"side\":\"old\",\"body\":\"why remove old1?\",\"author\":\"alice\"}\n\
         {\"path\":\"notes.txt\",\"line\":2,\"side\":\"new\",\"body\":\"nice addition\"}\n\
         {\"path\":\"notes.txt\",\"line\":99,\"side\":\"new\",\"body\":\"out of diff note\"}\n",
    )
    .expect("write .git/comments.jsonl");

    let print_out = Command::new(env!("CARGO_BIN_EXE_branchdiff"))
        .args([".", "--print", "--no-auto-fetch"])
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", &path)
        .env("HOME", dir.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run branchdiff --print");
    assert!(
        print_out.status.success(),
        "branchdiff --print failed: {}",
        String::from_utf8_lossy(&print_out.stderr)
    );
    let print_stdout = String::from_utf8_lossy(&print_out.stdout);
    assert!(print_stdout.contains("why remove old1?"), "missing old-line comment:\n{print_stdout}");
    assert!(print_stdout.contains("nice addition"), "missing new-line comment:\n{print_stdout}");
    assert!(
        print_stdout.contains("out of diff note"),
        "missing out-of-diff fallback comment:\n{print_stdout}"
    );
    assert!(
        print_stdout.contains("new line 99, outside diff"),
        "missing original line label on out-of-diff comment:\n{print_stdout}"
    );

    // 2. Explicit --comments <file> overrides autodetected .git/comments.jsonl
    let explicit_file = dir.path().join("custom.revdiff");
    std::fs::write(&explicit_file, "## notes.txt:2 (+)\nexplicit revdiff comment\n")
        .expect("write custom.revdiff");
    let explicit_out = Command::new(env!("CARGO_BIN_EXE_branchdiff"))
        .args([
            ".",
            "--print",
            "--no-auto-fetch",
            "--comments",
            explicit_file.to_str().expect("utf8 path"),
        ])
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", &path)
        .env("HOME", dir.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run branchdiff --print --comments");
    assert!(explicit_out.status.success());
    let explicit_stdout = String::from_utf8_lossy(&explicit_out.stdout);
    assert!(explicit_stdout.contains("explicit revdiff comment"));
    assert!(
        !explicit_stdout.contains("why remove old1?"),
        "explicit --comments must override autodetected file:\n{explicit_stdout}"
    );

    // 3. --html --comments - reads from stdin and escapes HTML/script payloads
    let mut child = Command::new(env!("CARGO_BIN_EXE_branchdiff"))
        .args([".", "--html", "--no-auto-fetch", "--comments", "-"])
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", &path)
        .env("HOME", dir.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn branchdiff --html --comments -");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(
            b"[{\"path\":\"notes.txt\",\"line\":2,\"side\":\"RIGHT\",\"body\":\"<script>alert(1)</script>\",\"user\":{\"login\":\"<b>bob</b>\"}}]",
        )
        .expect("write stdin");
    let html_out = child.wait_with_output().expect("wait --html");
    assert!(html_out.status.success());
    let html_stdout = String::from_utf8_lossy(&html_out.stdout);
    assert!(
        html_stdout.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "script payload must be escaped in HTML:\n{html_stdout}"
    );
    assert!(
        !html_stdout.contains("<script>alert(1)</script>"),
        "raw script tag must never appear in HTML:\n{html_stdout}"
    );
    assert!(html_stdout.contains("&lt;b&gt;bob&lt;/b&gt;"));

    // 4. --fingerprint changes when comments change (used by --html=live polling)
    let fp1 = Command::new(env!("CARGO_BIN_EXE_branchdiff"))
        .args([".", "--fingerprint", "--no-auto-fetch"])
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", &path)
        .env("HOME", dir.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("fingerprint 1");
    std::fs::write(
        &default_comments,
        "{\"path\":\"notes.txt\",\"line\":2,\"side\":\"new\",\"body\":\"updated comment\"}\n",
    )
    .expect("rewrite .git/comments.jsonl");
    let fp2 = Command::new(env!("CARGO_BIN_EXE_branchdiff"))
        .args([".", "--fingerprint", "--no-auto-fetch"])
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", &path)
        .env("HOME", dir.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("fingerprint 2");
    assert_ne!(fp1.stdout, fp2.stdout, "fingerprint must change when the comments file changes");
}

#[test]
fn diff_mode_writes_comments_before_each_file_and_the_patch_still_applies() {
    let dir = TempDir::new().expect("create temp dir");
    let run_git = |args: &[&str]| {
        let out = git_in(dir.path()).args(args).output().expect("run git");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    run_git(&["init"]);
    std::fs::write(dir.path().join("notes.txt"), "old1\nkeep2\n").expect("write notes.txt");
    run_git(&["add", "."]);
    run_git(&["commit", "-m", "base"]);
    std::fs::write(dir.path().join("notes.txt"), "keep2\nnew2\n").expect("rewrite notes.txt");
    let comments = dir.path().join("review.jsonl");
    std::fs::write(
        &comments,
        "{\"path\":\"notes.txt\",\"line\":2,\"body\":\"nice\\u001b]0;x\\u0007\",\"author\":\"alice\"}\n",
    )
    .expect("write review.jsonl");

    let out = Command::new(env!("CARGO_BIN_EXE_branchdiff"))
        .args([".", "--diff", "--no-auto-fetch", "--comments", "review.jsonl"])
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", std::env::var("PATH").expect("PATH is set in test env"))
        .env("HOME", dir.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run branchdiff --diff --comments");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let patch = String::from_utf8(out.stdout).expect("patch is UTF-8");
    assert!(
        patch.starts_with("# 💬 notes.txt · @alice · new line 2\n# │ nice ]0;x \ndiff --git"),
        "{patch}"
    );

    std::fs::write(dir.path().join("out.patch"), &patch).expect("write out.patch");
    run_git(&["apply", "--check", "-R", "out.patch"]);
}

#[test]
fn cli_head_and_highlight_flags_work_across_print_html_and_files_json() {
    let dir = TempDir::new().expect("create temp dir");
    let run_git = |args: &[&str]| {
        let out = git_in(dir.path()).args(args).output().expect("run git");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    run_git(&["init", "-b", "main"]);
    std::fs::write(dir.path().join("notes.txt"), "base\n").expect("write notes.txt");
    run_git(&["add", "."]);
    run_git(&["commit", "-m", "base"]);

    run_git(&["checkout", "-b", "feature"]);
    std::fs::write(dir.path().join("notes.txt"), "base\npushed\n").expect("write notes.txt");
    run_git(&["add", "."]);
    run_git(&["commit", "-m", "pushed"]);
    run_git(&["tag", "pushed-tip"]);

    std::fs::write(dir.path().join("notes.txt"), "base\npushed\nunpushed\n")
        .expect("write notes.txt");
    run_git(&["add", "."]);
    run_git(&["commit", "-m", "unpushed"]);

    // Switch back to main so `--head feature` renders without checking out feature.
    run_git(&["checkout", "main"]);

    let path = std::env::var("PATH").expect("PATH is set in test env");
    let run_bd = |extra_args: &[&str]| -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_branchdiff"))
            .args([
                ".", "--no-auto-fetch", "--head", "feature", "--highlight", "pushed-tip..feature",
            ])
            .args(extra_args)
            .current_dir(dir.path())
            .env_clear()
            .env("PATH", &path)
            .env("HOME", dir.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("run branchdiff");
        assert!(
            out.status.success(),
            "branchdiff {extra_args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("utf8 stdout")
    };

    let print_out = run_bd(&["--print"]);
    assert!(
        print_out
            .contains("★ highlighted (pushed-tip..feature): 1 line in 1 file (★ line, ┃ hunk)"),
        "{print_out}"
    );
    assert!(print_out.contains("[★ 1 highlighted]"), "{print_out}");

    let html_out = run_bd(&["--html=static"]);
    assert!(
        html_out.contains("<div class=\"legend\">★ highlighted (pushed-tip..feature): 1 line in 1 file (★ line, ┃ hunk)</div>"),
        "{html_out}"
    );
    assert!(html_out.contains("<span class=\"hl-badge\">★ 1 highlighted</span>"), "{html_out}");
    assert!(
        html_out.contains("class=\"line-committed line-highlighted hunk-highlighted\""),
        "{html_out}"
    );

    let files_json = run_bd(&["--files", "--json"]);
    assert!(
        files_json.contains("\"path\":\"notes.txt\",\"old_path\":null,\"status\":\"modified\",\"additions\":2,\"deletions\":0,\"highlighted\":true"),
        "{files_json}"
    );

    let unhighlighted_json = Command::new(env!("CARGO_BIN_EXE_branchdiff"))
        .args([".", "--no-auto-fetch", "--head", "feature", "--files", "--json"])
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", &path)
        .env("HOME", dir.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run branchdiff --files --json without --highlight");
    let unhighlighted_str = String::from_utf8(unhighlighted_json.stdout).expect("utf8 stdout");
    assert!(
        !unhighlighted_str.contains("\"highlighted\""),
        "omitting --highlight must omit highlighted key: {unhighlighted_str}"
    );
}
