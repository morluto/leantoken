use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
pub struct GitRepository {
    root: PathBuf,
}

#[derive(Debug)]
pub enum GitError {
    Io(std::io::Error),
    Command {
        args: String,
        status: Option<i32>,
        stderr: String,
    },
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "git I/O error: {error}"),
            Self::Command {
                args,
                status,
                stderr,
            } => write!(f, "git {args} failed ({status:?}): {stderr}"),
        }
    }
}
impl std::error::Error for GitError {}
impl From<std::io::Error> for GitError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl GitRepository {
    pub fn init(root: impl Into<PathBuf>) -> Result<Self, GitError> {
        let repository = Self { root: root.into() };
        fs::create_dir_all(&repository.root)?;
        repository.run(["-c", "init.defaultBranch=main", "init"])?;
        repository.run(["config", "user.name", "LeanToken Test"])?;
        repository.run(["config", "user.email", "leantoken-test@example.invalid"])?;
        Ok(repository)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn commit_all(&self, message: &str) -> Result<String, GitError> {
        self.run(["add", "--all"])?;
        self.run(["commit", "--quiet", "-m", message])?;
        self.output(["rev-parse", "HEAD"])
    }

    pub fn branch(&self, name: &str) -> Result<(), GitError> {
        self.run(["switch", "-c", name]).map(|_| ())
    }

    pub fn worktree(&self, path: impl AsRef<Path>, branch: &str) -> Result<(), GitError> {
        let path = path.as_ref().to_string_lossy().into_owned();
        self.run(["worktree", "add", "-b", branch, &path])
            .map(|_| ())
    }

    fn run<const N: usize>(&self, args: [&str; N]) -> Result<(), GitError> {
        let output = self.command(args).output()?;
        if !output.status.success() {
            return Err(GitError::Command {
                args: args.join(" "),
                status: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(())
    }

    fn output<const N: usize>(&self, args: [&str; N]) -> Result<String, GitError> {
        let output = self.command(args).output()?;
        if !output.status.success() {
            return Err(GitError::Command {
                args: args.join(" "),
                status: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn command<const N: usize>(&self, args: [&str; N]) -> Command {
        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                self.root.join(".leantoken-test-global-config"),
            )
            .env("GIT_TERMINAL_PROMPT", "0");
        command
    }
}

#[cfg(test)]
mod tests {
    use super::GitRepository;
    use crate::Sandbox;
    use std::fs;

    #[test]
    fn local_repository_has_a_deterministic_commit_identity() {
        let sandbox = Sandbox::new(
            module_path!(),
            "local_repository_has_a_deterministic_commit_identity",
        )
        .expect("sandbox");
        let repository = GitRepository::init(sandbox.repo()).expect("init git repository");
        fs::write(sandbox.repo().join("README.md"), "fixture\n").unwrap();
        let commit = repository
            .commit_all("initial fixture")
            .expect("commit fixture");
        assert_eq!(commit.len(), 40);
        assert!(repository.root().join(".git").is_dir());
    }
}
