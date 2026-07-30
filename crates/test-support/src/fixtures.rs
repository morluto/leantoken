use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_FIXTURE_LIST_ENTRIES: usize = 10_000;
const MAX_FIXTURE_LIST_DEPTH: usize = 64;

#[derive(Debug, Clone)]
pub struct FixtureCase {
    pub identity: String,
    pub root: PathBuf,
    pub operation: String,
    pub request: PathBuf,
    pub expected: PathBuf,
}

#[derive(Debug)]
pub enum FixtureError {
    Io(std::io::Error),
    Invalid { path: PathBuf, message: String },
    Duplicate(String),
}
impl fmt::Display for FixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "fixture I/O error: {error}"),
            Self::Invalid { path, message } => {
                write!(f, "invalid fixture {}: {message}", path.display())
            }
            Self::Duplicate(identity) => write!(f, "duplicate fixture identity: {identity}"),
        }
    }
}
impl std::error::Error for FixtureError {}
impl From<std::io::Error> for FixtureError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl FixtureCase {
    pub fn load(root: impl Into<PathBuf>) -> Result<Self, FixtureError> {
        let root = root.into();
        if !root.is_dir() {
            return Err(invalid(&root, "case directory is missing"));
        }
        let manifest_path = root.join("case.toml");
        let contents = fs::read_to_string(&manifest_path)?;
        let mut schema = None;
        let mut schema_seen = false;
        let mut operation = None;
        let mut operation_seen = false;
        for line in contents
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            let Some((key, value)) = line.split_once('=') else {
                return Err(invalid(&manifest_path, "expected key = value"));
            };
            let value = value.trim().trim_matches('"');
            match key.trim() {
                "schema" if !schema_seen => {
                    schema_seen = true;
                    schema = value.parse::<u32>().ok();
                }
                "operation" if !operation_seen => {
                    operation_seen = true;
                    operation = Some(value.to_owned());
                }
                "schema" | "operation" => {
                    return Err(invalid(&manifest_path, "duplicate manifest key"));
                }
                _ => return Err(invalid(&manifest_path, "unknown manifest key")),
            }
        }
        if schema != Some(1) {
            return Err(invalid(&manifest_path, "schema must be 1"));
        }
        let operation = operation
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid(&manifest_path, "operation is required"))?;
        let request = root.join("request.json");
        let expected = root.join("expected.json");
        for file in [&request, &expected] {
            if !file.is_file() {
                return Err(invalid(file, "required contract file is missing"));
            }
        }
        for entry in fs::read_dir(&root)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if !matches!(
                name,
                "case.toml" | "request.json" | "expected.json" | "repo"
            ) {
                return Err(invalid(
                    &path,
                    "unknown fixture file; domain runners own additional artifacts",
                ));
            }
            if name == "repo" && !path.is_dir() {
                return Err(invalid(&path, "repo fixture must be a directory"));
            }
        }
        let identity = root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| invalid(&root, "case directory has no UTF-8 name"))?
            .to_owned();
        Ok(Self {
            identity,
            root,
            operation,
            request,
            expected,
        })
    }

    pub fn list(root: impl AsRef<Path>, domain: Option<&str>) -> Result<Vec<Self>, FixtureError> {
        Self::list_with_bounds(
            root.as_ref(),
            domain,
            MAX_FIXTURE_LIST_ENTRIES,
            MAX_FIXTURE_LIST_DEPTH,
        )
    }

    fn list_with_bounds(
        fixtures_root: &Path,
        domain: Option<&str>,
        max_entries: usize,
        max_depth: usize,
    ) -> Result<Vec<Self>, FixtureError> {
        let fixtures_root = fixtures_root.to_path_buf();
        let root = domain.map_or_else(
            || fixtures_root.clone(),
            |domain| fixtures_root.join(domain),
        );
        if !root.exists() {
            return Ok(Vec::new());
        }
        let identity_root = fixtures_root;
        let mut cases = Vec::new();
        let mut entries = 0;
        collect(
            &root,
            &identity_root,
            &mut cases,
            &mut entries,
            max_entries,
            0,
            max_depth,
        )?;
        cases.sort_by(|a, b| a.identity.cmp(&b.identity).then(a.root.cmp(&b.root)));
        for pair in cases.windows(2) {
            if pair[0].identity == pair[1].identity {
                return Err(FixtureError::Duplicate(pair[0].identity.clone()));
            }
        }
        Ok(cases)
    }
}

fn collect(
    root: &Path,
    identity_root: &Path,
    cases: &mut Vec<FixtureCase>,
    entries: &mut usize,
    max_entries: usize,
    depth: usize,
    max_depth: usize,
) -> Result<(), FixtureError> {
    if depth > max_depth {
        return Err(invalid(root, "fixture listing exceeded its depth bound"));
    }
    if root.join("case.toml").is_file() {
        let mut case = FixtureCase::load(root)?;
        case.identity = root
            .strip_prefix(identity_root)
            .unwrap_or(root)
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        cases.push(case);
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        *entries += 1;
        if *entries > max_entries {
            return Err(invalid(root, "fixture listing exceeded its entry bound"));
        }
        if entry.file_type()?.is_dir() {
            collect(
                &path,
                identity_root,
                cases,
                entries,
                max_entries,
                depth + 1,
                max_depth,
            )?;
        }
    }
    Ok(())
}
fn invalid(path: &Path, message: &str) -> FixtureError {
    FixtureError::Invalid {
        path: path.to_path_buf(),
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::FixtureCase;
    use std::fs;
    use std::io;
    use std::path::Path;

    #[cfg(unix)]
    fn create_directory_symlink(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn create_directory_symlink(target: &Path, link: &Path) -> io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    #[cfg(not(any(unix, windows)))]
    fn create_directory_symlink(_target: &Path, _link: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory symlinks are unsupported on this platform",
        ))
    }

    #[cfg(unix)]
    fn remove_directory_symlink(link: &Path) -> io::Result<()> {
        fs::remove_file(link)
    }

    #[cfg(windows)]
    fn remove_directory_symlink(link: &Path) -> io::Result<()> {
        fs::remove_dir(link)
    }

    #[cfg(not(any(unix, windows)))]
    fn remove_directory_symlink(link: &Path) -> io::Result<()> {
        fs::remove_file(link)
    }

    #[test]
    fn rejects_unknown_contract_files() {
        let root =
            std::env::temp_dir().join(format!("leantoken-fixture-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("case.toml"), "schema = 1\noperation = \"test\"\n").unwrap();
        fs::write(root.join("request.json"), "{}\n").unwrap();
        fs::write(root.join("expected.json"), "{}\n").unwrap();
        fs::write(root.join("unexpected.json"), "{}\n").unwrap();
        assert!(FixtureCase::load(&root).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lists_cases_with_domain_qualified_identities() {
        let root = std::env::temp_dir().join(format!(
            "leantoken-fixture-list-test-{}",
            std::process::id()
        ));
        let case = root.join("storage/reopen");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&case).unwrap();
        fs::write(
            case.join("case.toml"),
            "schema = 1\noperation = \"storage\"\n",
        )
        .unwrap();
        fs::write(case.join("request.json"), "{}\n").unwrap();
        fs::write(case.join("expected.json"), "{}\n").unwrap();
        let cases = FixtureCase::list(&root, None).unwrap();
        assert_eq!(cases[0].identity, "storage/reopen");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn fixture_listing_fails_closed_at_entry_and_depth_bounds() {
        let root = std::env::temp_dir().join(format!(
            "leantoken-fixture-list-bounds-test-{}",
            std::process::id()
        ));
        let case = root.join("storage/reopen");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&case).unwrap();
        fs::write(
            case.join("case.toml"),
            "schema = 1\noperation = \"storage\"\n",
        )
        .unwrap();
        fs::write(case.join("request.json"), "{}\n").unwrap();
        fs::write(case.join("expected.json"), "{}\n").unwrap();

        let entry_error = FixtureCase::list_with_bounds(&root, None, 1, 64)
            .expect_err("entry bound was not enforced");
        assert!(entry_error.to_string().contains("entry bound"));
        let depth_error = FixtureCase::list_with_bounds(&root, None, 10, 0)
            .expect_err("depth bound was not enforced");
        assert!(depth_error.to_string().contains("depth bound"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn fixture_listing_does_not_follow_directory_symlinks() {
        let root = std::env::temp_dir().join(format!(
            "leantoken-fixture-list-symlink-test-{}",
            std::process::id()
        ));
        let outside = std::env::temp_dir().join(format!(
            "leantoken-fixture-list-symlink-outside-{}",
            std::process::id()
        ));
        let case = outside.join("protocol/catalog");
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&case).unwrap();
        fs::write(
            case.join("case.toml"),
            "schema = 1\noperation = \"protocol_catalog\"\n",
        )
        .unwrap();
        fs::write(case.join("request.json"), "{}\n").unwrap();
        fs::write(case.join("expected.json"), "{}\n").unwrap();
        let link = root.join("linked-domain");
        if let Err(error) = create_directory_symlink(&outside, &link) {
            let _ = fs::remove_dir_all(&root);
            let _ = fs::remove_dir_all(&outside);
            eprintln!("skipping directory-symlink assertion: {error}");
            return;
        }

        assert!(FixtureCase::list(&root, None).unwrap().is_empty());

        remove_directory_symlink(&link).unwrap();
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn rejects_duplicate_manifest_keys() {
        let root = std::env::temp_dir().join(format!(
            "leantoken-fixture-duplicate-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("case.toml"),
            "schema = 1\nschema = 1\noperation = \"test\"\n",
        )
        .unwrap();
        fs::write(root.join("request.json"), "{}\n").unwrap();
        fs::write(root.join("expected.json"), "{}\n").unwrap();
        assert!(FixtureCase::load(&root).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_invalid_duplicate_schema_keys() {
        let root = std::env::temp_dir().join(format!(
            "leantoken-fixture-invalid-duplicate-schema-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("case.toml"),
            "schema = invalid\nschema = 1\noperation = \"test\"\n",
        )
        .unwrap();
        fs::write(root.join("request.json"), "{}\n").unwrap();
        fs::write(root.join("expected.json"), "{}\n").unwrap();
        assert!(FixtureCase::load(&root).is_err());
        let _ = fs::remove_dir_all(root);
    }
}
