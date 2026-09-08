use std::{
    io::{self, Read},
    path::Path,
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use command_group::CommandGroup;

const TIMEOUT: Duration = Duration::from_secs(10);
const OUTPUT_LIMIT: usize = 64 * 1024;

/// Checked Git setup for disposable repositories, independent of product Git code.
pub struct GitFixture;

impl GitFixture {
    /// Initialize an empty repository with a deterministic branch and identity.
    pub fn init(root: &Path) {
        Self::run(root, &["init", "--initial-branch=main"]);
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("core.autocrlf", "false"),
        ] {
            Self::run(root, &["config", key, value]);
        }
        let output = Self::run(root, &["rev-parse", "--is-inside-work-tree"]);
        assert_eq!(output.stdout, b"true\n", "Git fixture must be a work tree");
    }

    /// Commit all changes and return the verified commit object identity.
    pub fn commit_all(root: &Path, message: &str) -> String {
        Self::run(root, &["add", "-A"]);
        Self::run(root, &["commit", "-m", message]);
        let output = Self::run(root, &["rev-parse", "--verify", "HEAD^{commit}"]);
        String::from_utf8(output.stdout)
            .expect("Git object identity is UTF-8")
            .trim()
            .to_owned()
    }

    /// Execute a fixture operation, failing at the exact unsuccessful command.
    ///
    /// Each operation has a ten-second deadline and a 64-KiB bound per output
    /// stream. Children are terminated as a process group (a job on Windows).
    /// The temporary home and empty hook/template directories live beside the
    /// repository, so `git add -A` cannot accidentally commit runner artifacts.
    pub fn run(root: &Path, arguments: &[&str]) -> Output {
        let root = root.canonicalize().expect("Git fixture root");
        let home = tempfile::Builder::new()
            .prefix(".git-fixture-home-")
            .tempdir_in(root.parent().expect("fixture root has a parent"))
            .expect("Git fixture home");
        let mut command = Command::new("git");
        configure(&mut command, &root, home.path());
        command.args(arguments);
        let started = Instant::now();
        let output = capture(&mut command, TIMEOUT).unwrap_or_else(|error| {
            panic!(
                "Git fixture {arguments:?} in . failed after {:?}: {error}",
                started.elapsed()
            )
        });
        assert!(
            output.status.success(),
            "Git fixture {arguments:?} in . failed ({}) after {:?}; policy=isolated-v1\nstdout: {}\nstderr: {}",
            output.status,
            started.elapsed(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}

fn configure(command: &mut Command, root: &Path, home: &Path) {
    command.env_clear().current_dir(root);
    // Keep only executable lookup and Windows loader inputs from the host.
    for key in [
        "PATH",
        "SystemRoot",
        "windir",
        "COMSPEC",
        "PATHEXT",
        "SystemDrive",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    for key in [
        "HOME",
        "USERPROFILE",
        "XDG_CONFIG_HOME",
        "TEMP",
        "TMP",
        "TMPDIR",
    ] {
        command.env(key, home);
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join("empty.gitconfig"))
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("LC_ALL", "C")
        .env("LANG", "C");
    for setting in [
        "commit.gpgsign=false",
        "tag.gpgsign=false",
        "core.autocrlf=false",
        "core.fsmonitor=false",
        "core.untrackedCache=false",
        "init.defaultBranch=main",
    ] {
        command.args(["-c", setting]);
    }
    command
        .arg("-c")
        .arg(format!("core.hooksPath={}", home.display()));
    command
        .arg("-c")
        .arg(format!("init.templateDir={}", home.display()));
}

fn read_bounded(mut stream: impl Read, exceeded: Arc<AtomicBool>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            return Ok(bytes);
        }
        let remaining = OUTPUT_LIMIT - bytes.len();
        bytes.extend_from_slice(&buffer[..count.min(remaining)]);
        if count > remaining {
            exceeded.store(true, Ordering::Release);
            return Ok(bytes);
        }
    }
}

fn capture(command: &mut Command, timeout: Duration) -> io::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.group_spawn()?;
    let stdout = child.inner().stdout.take().expect("piped stdout");
    let stderr = child.inner().stderr.take().expect("piped stderr");
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout_limit = Arc::clone(&exceeded);
    let stderr_limit = Arc::clone(&exceeded);
    let stdout = thread::spawn(move || read_bounded(stdout, stdout_limit));
    let stderr = thread::spawn(move || read_bounded(stderr, stderr_limit));
    let started = Instant::now();
    let status = loop {
        if exceeded.load(Ordering::Acquire) {
            break Err(io::Error::other(
                "Git fixture output exceeded 64 KiB per stream",
            ));
        }
        if started.elapsed() >= timeout {
            break Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Git fixture command timed out",
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(error) => break Err(error),
        }
    };
    // A helper can retain a pipe after the Git leader exits. Always terminate
    // descendants before joining readers, including on the successful path.
    let _ = child.kill();
    let _ = child.wait();
    let stdout = stdout
        .join()
        .map_err(|_| io::Error::other("Git stdout reader panicked"))??;
    let stderr = stderr
        .join()
        .map_err(|_| io::Error::other("Git stderr reader panicked"))??;
    let status = status.map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "{error}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            ),
        )
    })?;
    if exceeded.load(Ordering::Acquire) {
        return Err(io::Error::other(
            "Git fixture output exceeded 64 KiB per stream",
        ));
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commits_have_explicit_branch_identity_and_preserve_lf_bytes() {
        let root = tempfile::tempdir().unwrap();
        GitFixture::init(root.path());
        std::fs::write(root.path().join("source.txt"), b"first\nsecond\n").unwrap();
        let first = GitFixture::commit_all(root.path(), "initial");
        assert!(first.len() >= 40 && first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            GitFixture::run(root.path(), &["branch", "--show-current"]).stdout,
            b"main\n"
        );
        assert_eq!(
            GitFixture::run(root.path(), &["show", "HEAD:source.txt"]).stdout,
            b"first\nsecond\n"
        );
        assert!(
            GitFixture::run(root.path(), &["status", "--porcelain"])
                .stdout
                .is_empty()
        );
        std::fs::write(root.path().join("source.txt"), b"changed\n").unwrap();
        let second = GitFixture::commit_all(root.path(), "second");
        assert_ne!(first, second);
        assert_eq!(
            GitFixture::run(root.path(), &["rev-parse", "HEAD^"]).stdout,
            format!("{first}\n").as_bytes()
        );
    }

    #[test]
    fn inherited_routing_and_global_configuration_are_ignored() {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let hostile = home.path().join("hostile.gitconfig");
        std::fs::write(&hostile, "[include]\npath = invalid\n[commit]\ngpgsign = true\n[init]\ndefaultBranch = unexpected\n").unwrap();
        let mut command = Command::new("git");
        command
            .env("GIT_DIR", home.path().join("missing"))
            .env("GIT_CONFIG_GLOBAL", &hostile)
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "init.defaultBranch")
            .env("GIT_CONFIG_VALUE_0", "unexpected");
        configure(&mut command, root.path(), home.path());
        command.arg("init");
        assert!(capture(&mut command, TIMEOUT).unwrap().status.success());
        assert_eq!(
            GitFixture::run(root.path(), &["symbolic-ref", "HEAD"]).stdout,
            b"refs/heads/main\n"
        );
        GitFixture::run(root.path(), &["config", "user.name", "Test"]);
        GitFixture::run(root.path(), &["config", "user.email", "test@example.com"]);
        GitFixture::run(root.path(), &["config", "commit.gpgsign", "true"]);
        GitFixture::run(
            root.path(),
            &["config", "gpg.program", "nonexistent-fixture-signer"],
        );
        std::fs::write(root.path().join("file"), "fixture\n").unwrap();
        GitFixture::commit_all(root.path(), "signing disabled");
    }

    #[test]
    fn failed_commit_reports_the_operation_and_git_diagnostics() {
        let root = tempfile::tempdir().unwrap();
        GitFixture::init(root.path());
        let failure = std::panic::catch_unwind(|| GitFixture::commit_all(root.path(), "empty"))
            .expect_err("empty commit must fail at fixture setup");
        let message = failure.downcast_ref::<String>().unwrap();
        assert!(
            message.contains("commit") && message.contains("empty"),
            "{message}"
        );
        assert!(message.contains("nothing to commit"), "{message}");
        assert!(
            message.contains("stdout:") && message.contains("stderr:"),
            "{message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn repository_hooks_cannot_run_during_fixture_commits() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        GitFixture::init(root.path());
        let hooks = root.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        std::fs::write(&hook, "#!/bin/sh\ntouch hook-ran\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        GitFixture::run(
            root.path(),
            &["config", "core.hooksPath", hooks.to_str().unwrap()],
        );
        std::fs::write(root.path().join("file"), "fixture\n").unwrap();
        GitFixture::commit_all(root.path(), "hooks disabled");
        assert!(!root.path().join("hook-ran").exists());
    }

    fn child_command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "git::tests::child_fixture", "--nocapture"])
            .env("LEANTOKEN_GIT_FIXTURE_CHILD", mode);
        command
    }

    #[test]
    #[allow(clippy::zombie_processes)] // The outer capture owner must reap this process group.
    fn child_fixture() {
        match std::env::var("LEANTOKEN_GIT_FIXTURE_CHILD").as_deref() {
            Ok("timeout") => thread::sleep(Duration::from_secs(60)),
            Ok("descendant") => {
                child_command("timeout")
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .expect("spawn descendant retaining both capture pipes");
            }
            Ok("output") => {
                use std::io::Write;
                std::io::stdout()
                    .write_all(&vec![b'x'; OUTPUT_LIMIT * 2])
                    .unwrap();
            }
            _ => {}
        }
    }

    #[test]
    fn commands_have_a_deadline_and_output_bound() {
        let error = capture(&mut child_command("timeout"), Duration::from_millis(100)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let error = capture(&mut child_command("output"), TIMEOUT).unwrap_err();
        assert!(error.to_string().contains("exceeded 64 KiB"), "{error}");
    }

    #[test]
    fn exited_leader_cannot_leave_descendants_holding_capture_pipes() {
        let started = Instant::now();
        let result = capture(&mut child_command("descendant"), Duration::from_millis(250));
        // Platforms differ in whether group polling reports the leader exit
        // immediately. Either outcome must close the descendant's pipes.
        if let Err(error) = result {
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "capture waited for the descendant's 60-second delay"
        );
    }
}
