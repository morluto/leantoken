use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct RepoBuilder {
    root: PathBuf,
}

#[derive(Debug)]
pub enum RepoError {
    Io(std::io::Error),
    InvalidPath(PathBuf),
}

impl fmt::Display for RepoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "repository fixture I/O error: {error}"),
            Self::InvalidPath(path) => write!(
                f,
                "repository fixture path escapes root: {}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for RepoError {}
impl From<std::io::Error> for RepoError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl RepoBuilder {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, RepoError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn directory(&self, path: impl AsRef<Path>) -> Result<&Self, RepoError> {
        let path = self.safe(path.as_ref())?;
        fs::create_dir_all(path)?;
        Ok(self)
    }

    pub fn file(
        &self,
        path: impl AsRef<Path>,
        contents: impl AsRef<[u8]>,
    ) -> Result<&Self, RepoError> {
        let path = self.safe(path.as_ref())?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, contents)?;
        Ok(self)
    }

    pub fn binary_file(
        &self,
        path: impl AsRef<Path>,
        contents: impl AsRef<[u8]>,
    ) -> Result<&Self, RepoError> {
        self.file(path, contents)
    }

    #[cfg(unix)]
    pub fn symlink(
        &self,
        source: impl AsRef<Path>,
        destination: impl AsRef<Path>,
    ) -> Result<&Self, RepoError> {
        let destination = self.safe(destination.as_ref())?;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        std::os::unix::fs::symlink(source, destination)?;
        Ok(self)
    }

    #[cfg(windows)]
    pub fn symlink(
        &self,
        source: impl AsRef<Path>,
        destination: impl AsRef<Path>,
    ) -> Result<&Self, RepoError> {
        let destination = self.safe(destination.as_ref())?;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let source = source.as_ref();
        if source.extension().is_some() {
            std::os::windows::fs::symlink_file(source, destination)?;
        } else {
            std::os::windows::fs::symlink_dir(source, destination)?;
        }
        Ok(self)
    }

    fn safe(&self, path: &Path) -> Result<PathBuf, RepoError> {
        if path.is_absolute()
            || path
                .components()
                .any(|component| component == std::path::Component::ParentDir)
        {
            return Err(RepoError::InvalidPath(path.to_path_buf()));
        }
        Ok(self.root.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::RepoBuilder;

    #[test]
    fn builds_nested_text_and_binary_files() {
        let root = std::env::temp_dir().join(format!("leantoken-repo-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let builder = RepoBuilder::new(&root).unwrap();
        builder
            .file("src/main.rs", "fn main() {}\n")
            .unwrap()
            .binary_file("blob", [0, 1, 2])
            .unwrap();
        assert_eq!(std::fs::read(root.join("blob")).unwrap(), [0, 1, 2]);
        let _ = std::fs::remove_dir_all(root);
    }
}
