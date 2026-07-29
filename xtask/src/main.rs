use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const PRODUCT: &str = "leantoken";
const SUPPORT: &str = "leantoken-test-support";
const SUITE: &str = "leantoken-test-suite";
const XTASK: &str = "leantoken-xtask";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), XtaskError> {
    let root = workspace_root();
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("check-test-architecture") => check_architecture(&root),
        Some("test") => run_test_command(&root, args.collect()),
        Some("test-focused") => focused_test_command(&root, args.collect()),
        Some(command) => Err(XtaskError::Usage(format!("unknown command `{command}`"))),
        None => Err(XtaskError::Usage(usage())),
    }
}

fn focused_test_command(root: &Path, args: Vec<String>) -> Result<(), XtaskError> {
    let Some(selector) = args.first() else {
        return Err(XtaskError::Usage(
            "cargo test-focused requires exactly one domain or test selector".to_owned(),
        ));
    };
    if args.len() != 1 || selector.is_empty() || selector.starts_with('-') {
        return Err(XtaskError::Usage(
            "cargo test-focused requires exactly one domain or test selector".to_owned(),
        ));
    }
    let suite_domain = [
        "indexing_repository",
        "storage",
        "retrieval",
        "protocol",
        "platform",
        "contracts",
    ]
    .into_iter()
    .find(|domain| {
        *domain == selector
            || selector.starts_with(&format!("{domain}::"))
            || selector.starts_with(&format!("domains::{domain}::"))
    });
    let command = if let Some(domain) = suite_domain {
        let filter = if selector == domain {
            format!("domains::{domain}")
        } else if selector.starts_with("domains::") {
            selector.clone()
        } else {
            format!("domains::{selector}")
        };
        cargo_command([
            "test",
            "--locked",
            "--package",
            SUITE,
            "--all-features",
            "--lib",
            &filter,
        ])
    } else if suite_has_test(root, selector)? {
        cargo_command([
            "test",
            "--locked",
            "--package",
            SUITE,
            "--all-features",
            "--lib",
            selector,
        ])
    } else {
        cargo_command([
            "test",
            "--locked",
            "--package",
            PRODUCT,
            "--all-features",
            "--lib",
            "--bins",
            "--test",
            "integration",
            selector,
            "--",
            "--test-threads=2",
        ])
    };
    let status = print_and_run(root, &command)?;
    if status.success() {
        Ok(())
    } else {
        Err(XtaskError::CommandFailed {
            command: command.join(" "),
            code: status.code(),
        })
    }
}

fn suite_has_test(root: &Path, selector: &str) -> Result<bool, XtaskError> {
    let command = cargo_command([
        "test",
        "--locked",
        "--package",
        SUITE,
        "--all-features",
        "--lib",
        selector,
        "--",
        "--list",
    ]);
    println!("==> {}", command.join(" "));
    let (program, args) = command
        .split_first()
        .ok_or_else(|| XtaskError::Usage("empty command".to_owned()))?;
    let output = Command::new(program)
        .args(args)
        .current_dir(root)
        .output()
        .map_err(XtaskError::Io)?;
    if !output.status.success() {
        return Err(XtaskError::CommandFailed {
            command: command.join(" "),
            code: output.status.code(),
        });
    }
    let selector_suffix = format!("::{selector}");
    Ok(String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        line.strip_suffix(": test")
            .is_some_and(|name| name == selector || name.ends_with(&selector_suffix))
    }))
}

fn run_test_command(root: &Path, args: Vec<String>) -> Result<(), XtaskError> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(XtaskError::Usage(test_usage()));
    };
    match command {
        "product" => {
            if args.len() != 1 {
                return Err(XtaskError::Usage(
                    "`test product` does not accept additional arguments".to_owned(),
                ));
            }
            run_plan(root, TestPlan::default())
        }
        "list" if args.len() <= 2 => list_fixtures(root, args.get(1).map(String::as_str)),
        "list" => Err(XtaskError::Usage(
            "`test list` accepts at most one domain".to_owned(),
        )),
        "run" => exact_fixture_command(root, &args, "run"),
        "bless" => exact_fixture_command(root, &args, "bless"),
        "stress" if args.len() == 1 => run_plan(root, TestPlan::stress()?),
        "stress" => Err(XtaskError::Usage(
            "`test stress` does not accept additional arguments".to_owned(),
        )),
        "profile" if args.len() == 1 => run_plan(root, TestPlan::profile()),
        "profile" => Err(XtaskError::Usage(
            "`test profile` does not accept additional arguments".to_owned(),
        )),
        "plan" => {
            if args.iter().skip(1).any(|arg| arg != "--dry-run") {
                return Err(XtaskError::Usage(
                    "`test plan` accepts only --dry-run".to_owned(),
                ));
            }
            TestPlan::default().print();
            Ok(())
        }
        _ => Err(XtaskError::Usage(test_usage())),
    }
}

fn list_fixtures(root: &Path, domain: Option<&str>) -> Result<(), XtaskError> {
    if let Some(domain) = domain {
        validate_domain(domain)?;
    }
    let fixtures = root.join("fixtures");
    let cases = FixtureManifest::list(&fixtures, domain)?;
    if cases.is_empty() {
        println!(
            "No fixture cases found{}.",
            domain.map_or(String::new(), |domain| format!(" for {domain}"))
        );
    }
    for case in cases {
        println!("{}  ({})", case.identity, case.operation);
    }
    Ok(())
}

fn exact_fixture_command(root: &Path, args: &[String], action: &str) -> Result<(), XtaskError> {
    let Some(identity) = args.get(1) else {
        return Err(XtaskError::Usage(format!(
            "`cargo xtask test {action}` requires exactly <domain>/<case>"
        )));
    };
    if args.len() != 2 || !valid_fixture_identity(identity) {
        return Err(XtaskError::Usage(format!(
            "`cargo xtask test {action}` requires exactly <domain>/<case>"
        )));
    }
    let case_root = root.join("fixtures").join(identity);
    let mut case = FixtureManifest::load(&case_root)?;
    case.identity = identity.clone();
    println!(
        "Exact fixture selected: {} ({})",
        case.identity, case.operation
    );
    let mut command = cargo_command([
        "run",
        "--locked",
        "--package",
        SUITE,
        "--bin",
        "fixture-runner",
        "--",
        identity,
    ]);
    if action == "bless" {
        command.push("--bless".to_owned());
    }
    let status = print_and_run(root, &command)?;
    if status.success() {
        Ok(())
    } else {
        Err(XtaskError::CommandFailed {
            command: command.join(" "),
            code: status.code(),
        })
    }
}

fn run_plan(root: &Path, plan: TestPlan) -> Result<(), XtaskError> {
    for repetition in 1..=plan.repetitions {
        if plan.repetitions > 1 {
            println!("==> stress repetition {repetition}/{}", plan.repetitions);
        }
        for command in &plan.commands {
            let status = print_and_run(root, command)?;
            if !status.success() {
                return Err(XtaskError::CommandFailed {
                    command: command.join(" "),
                    code: status.code(),
                });
            }
        }
    }
    Ok(())
}

fn print_and_run(root: &Path, command: &[String]) -> Result<std::process::ExitStatus, XtaskError> {
    println!("==> {}", command.join(" "));
    let (program, args) = command
        .split_first()
        .ok_or_else(|| XtaskError::Usage("empty command".to_owned()))?;
    Command::new(program)
        .args(args)
        .current_dir(root)
        .status()
        .map_err(XtaskError::Io)
}

#[derive(Debug, Clone)]
struct TestPlan {
    commands: Vec<Vec<String>>,
    repetitions: usize,
}

impl Default for TestPlan {
    fn default() -> Self {
        Self {
            commands: vec![
                cargo_command([
                    "test",
                    "--locked",
                    "--workspace",
                    "--all-features",
                    "--lib",
                    "--bins",
                ]),
                cargo_command([
                    "test",
                    "--locked",
                    "--package",
                    PRODUCT,
                    "--all-features",
                    "--test",
                    "integration",
                    "--",
                    "--skip",
                    "process::",
                ]),
                cargo_command([
                    "test",
                    "--locked",
                    "--package",
                    PRODUCT,
                    "--all-features",
                    "--test",
                    "integration",
                    "process::",
                    "--",
                    "--test-threads=2",
                ]),
            ],
            repetitions: 1,
        }
    }
}

impl TestPlan {
    fn stress() -> Result<Self, XtaskError> {
        let repetitions = env::var("LEANTOKEN_STRESS_REPETITIONS")
            .unwrap_or_else(|_| "1".to_owned())
            .parse::<usize>()
            .map_err(|_| {
                XtaskError::Usage(
                    "LEANTOKEN_STRESS_REPETITIONS must be a positive integer".to_owned(),
                )
            })?;
        if repetitions == 0 {
            return Err(XtaskError::Usage(
                "LEANTOKEN_STRESS_REPETITIONS must be a positive integer".to_owned(),
            ));
        }
        Ok(Self {
            commands: vec![cargo_command([
                "test",
                "--locked",
                "--package",
                PRODUCT,
                "--all-features",
                "--test",
                "integration",
                "process::",
                "--",
                "--test-threads=2",
            ])],
            repetitions,
        })
    }
    fn profile() -> Self {
        Self {
            commands: vec![cargo_command([
                "nextest",
                "run",
                "--locked",
                "--workspace",
                "--all-features",
                "--status-level",
                "slow",
                "--final-status-level",
                "slow",
                "--failure-output",
                "final",
            ])],
            repetitions: 1,
        }
    }
    fn print(&self) {
        for command in &self.commands {
            println!("{}", command.join(" "));
        }
    }
}

fn cargo_command<const N: usize>(args: [&str; N]) -> Vec<String> {
    std::iter::once("cargo".to_owned())
        .chain(args.into_iter().map(str::to_owned))
        .collect()
}

#[derive(Debug, Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
    workspace_default_members: Vec<String>,
}
#[derive(Debug, Deserialize)]
struct Package {
    name: String,
    id: String,
    publish: Option<Vec<String>>,
    dependencies: Vec<Dependency>,
    targets: Vec<Target>,
    #[serde(default)]
    features: BTreeMap<String, Vec<String>>,
}
#[derive(Debug, Deserialize)]
struct Dependency {
    name: String,
}
#[derive(Debug, Deserialize)]
struct Target {
    name: String,
    kind: Vec<String>,
}

fn check_architecture(root: &Path) -> Result<(), XtaskError> {
    let output = Command::new("cargo")
        .args(["metadata", "--locked", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .output()
        .map_err(XtaskError::Io)?;
    if !output.status.success() {
        return Err(XtaskError::CommandFailed {
            command: "cargo metadata --locked --no-deps --format-version 1".to_owned(),
            code: output.status.code(),
        });
    }
    let metadata: Metadata = serde_json::from_slice(&output.stdout).map_err(XtaskError::Json)?;
    let packages = metadata
        .packages
        .iter()
        .map(|package| (package.name.as_str(), package))
        .collect::<BTreeMap<_, _>>();
    let expected = BTreeSet::from([PRODUCT, SUPPORT, SUITE, XTASK]);
    let actual = packages.keys().copied().collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(XtaskError::Architecture(format!(
            "expected packages {expected:?}, found {actual:?}"
        )));
    }
    let workspace_member_names = metadata
        .workspace_members
        .iter()
        .filter_map(|id| {
            metadata
                .packages
                .iter()
                .find(|package| &package.id == id)
                .map(|package| package.name.as_str())
        })
        .collect::<BTreeSet<_>>();
    if workspace_member_names != expected {
        return Err(XtaskError::Architecture(
            "workspace membership drifted".to_owned(),
        ));
    }
    let default_names = metadata
        .workspace_default_members
        .iter()
        .filter_map(|id| {
            metadata
                .packages
                .iter()
                .find(|package| &package.id == id)
                .map(|package| package.name.as_str())
        })
        .collect::<BTreeSet<_>>();
    if default_names != expected {
        return Err(XtaskError::Architecture(format!(
            "default workspace members drifted: {default_names:?}"
        )));
    }
    for package in packages.values() {
        if [SUPPORT, SUITE, XTASK].contains(&package.name.as_str())
            && package.publish != Some(Vec::new())
        {
            return Err(XtaskError::Architecture(format!(
                "private package {} is publishable",
                package.name
            )));
        }
        let names = package
            .dependencies
            .iter()
            .map(|dependency| dependency.name.as_str())
            .collect::<BTreeSet<_>>();
        if package.name == PRODUCT
            && package
                .features
                .keys()
                .any(|feature| feature.contains("test") || feature.contains("fixture"))
        {
            return Err(XtaskError::Architecture(
                "product has a test-only feature; keep test support in private packages".to_owned(),
            ));
        }
        match package.name.as_str() {
            PRODUCT
                if names
                    .intersection(&BTreeSet::from([SUPPORT, SUITE, XTASK]))
                    .next()
                    .is_some() =>
            {
                return Err(XtaskError::Architecture(
                    "product depends on private test packages".to_owned(),
                ));
            }
            SUPPORT
                if names
                    .intersection(&BTreeSet::from([PRODUCT, SUITE, XTASK]))
                    .next()
                    .is_some() =>
            {
                return Err(XtaskError::Architecture(
                    "test-support depends on product or suite".to_owned(),
                ));
            }
            SUITE
                if !names.contains(PRODUCT)
                    || !names.contains(SUPPORT)
                    || names.contains(XTASK) =>
            {
                return Err(XtaskError::Architecture(
                    "test-suite dependency direction is invalid".to_owned(),
                ));
            }
            XTASK
                if names
                    .intersection(&BTreeSet::from([PRODUCT, SUPPORT, SUITE]))
                    .next()
                    .is_some() =>
            {
                return Err(XtaskError::Architecture(
                    "xtask depends on workspace test or product packages".to_owned(),
                ));
            }
            _ => {}
        }
    }
    let product = packages[PRODUCT];
    if !product
        .targets
        .iter()
        .any(|target| target.name == "integration" && target.kind.iter().any(|kind| kind == "test"))
    {
        return Err(XtaskError::Architecture(
            "root integration target is missing".to_owned(),
        ));
    }
    check_test_inventory(root)?;
    check_ignored_test_policy(root)?;
    println!(
        "test architecture: ok (workspace resolver 3, one root process target, directed private packages)"
    );
    Ok(())
}

const ROOT_TEST_MODULES: &[&str] = &[
    "cli",
    "graph_signal_ablation_report",
    "model_ab_trajectory_report",
    "process",
    "representation_comparison",
    "services",
];

fn check_test_inventory(root: &Path) -> Result<(), XtaskError> {
    let tests_dir = root.join("tests");
    let expected = ROOT_TEST_MODULES
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    let actual = std::fs::read_dir(&tests_dir)
        .map_err(XtaskError::Io)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .filter_map(|path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_owned)
        })
        .filter(|name| name != "integration" && name != "benchmark_contract")
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(XtaskError::Architecture(format!(
            "root test inventory drifted: expected {expected:?}, found {actual:?}"
        )));
    }
    let integration =
        std::fs::read_to_string(tests_dir.join("integration.rs")).map_err(XtaskError::Io)?;
    for name in &actual {
        if !integration
            .lines()
            .any(|line| line.trim() == format!("{name},"))
        {
            return Err(XtaskError::Architecture(format!(
                "root test `{name}` is not registered in tests/integration.rs"
            )));
        }
    }
    println!("test inventory: ok ({} root owners)", actual.len());
    Ok(())
}

fn check_ignored_test_policy(root: &Path) -> Result<(), XtaskError> {
    let allowed = root.join("src/services/concurrency_profile.rs");
    let ignore_marker = ["#[", "ignore"].concat();
    let mut ignored = Vec::new();
    for directory in ["src", "tests", "crates", "xtask"] {
        collect_ignored_tests(
            &root.join(directory),
            &allowed,
            &ignore_marker,
            &mut ignored,
        )?;
    }
    if !ignored.is_empty() {
        return Err(XtaskError::Architecture(format!(
            "ignored tests are not allowed outside the documented manual profiler: {ignored:?}"
        )));
    }
    println!("ignored-test policy: ok (manual release profiler is the only exception)");
    Ok(())
}

fn collect_ignored_tests(
    directory: &Path,
    allowed: &Path,
    ignore_marker: &str,
    ignored: &mut Vec<String>,
) -> Result<(), XtaskError> {
    if !directory.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(directory).map_err(XtaskError::Io)? {
        let path = entry.map_err(XtaskError::Io)?.path();
        if path.is_dir() {
            collect_ignored_tests(&path, allowed, ignore_marker, ignored)?;
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && path != allowed
            && std::fs::read_to_string(&path)
                .map_err(XtaskError::Io)?
                .contains(ignore_marker)
        {
            ignored.push(path.display().to_string());
        }
    }
    Ok(())
}

fn validate_domain(domain: &str) -> Result<(), XtaskError> {
    if domain.is_empty()
        || domain == "."
        || domain == ".."
        || domain.contains('/')
        || domain.contains('\\')
    {
        return Err(XtaskError::Usage(
            "fixture domain must be one path component".to_owned(),
        ));
    }
    Ok(())
}

fn valid_fixture_identity(identity: &str) -> bool {
    identity.split('/').count() == 2
        && identity
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains('\\'))
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .expect("xtask lives below workspace root")
}
fn usage() -> String {
    "cargo xtask check-test-architecture | test-focused <selector> | test {product|list|run|bless|stress|profile|plan}".to_owned()
}
fn test_usage() -> String {
    "cargo xtask test product | list [domain] | run <domain>/<case> | bless <domain>/<case> | stress | profile | plan --dry-run".to_owned()
}

#[derive(Debug, Clone)]
struct FixtureManifest {
    identity: String,
    operation: String,
}

impl FixtureManifest {
    fn load(root: &Path) -> Result<Self, XtaskError> {
        if !root.is_dir() {
            return Err(XtaskError::Fixture(format!(
                "{}: case directory is missing",
                root.display()
            )));
        }
        let manifest = root.join("case.toml");
        let contents = std::fs::read_to_string(&manifest).map_err(XtaskError::Io)?;
        let mut schema = None;
        let mut operation = None;
        for line in contents
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            let Some((key, value)) = line.split_once('=') else {
                return Err(XtaskError::Fixture(format!(
                    "{}: expected key = value",
                    manifest.display()
                )));
            };
            let value = value.trim().trim_matches('"');
            match key.trim() {
                "schema" if schema.is_none() => schema = value.parse::<u32>().ok(),
                "operation" if operation.is_none() => operation = Some(value.to_owned()),
                "schema" | "operation" => {
                    return Err(XtaskError::Fixture(format!(
                        "{}: duplicate manifest key `{key}`",
                        manifest.display()
                    )));
                }
                key => {
                    return Err(XtaskError::Fixture(format!(
                        "{}: unknown key `{key}`",
                        manifest.display()
                    )));
                }
            }
        }
        if schema != Some(1) {
            return Err(XtaskError::Fixture(format!(
                "{}: schema must be 1",
                manifest.display()
            )));
        }
        let operation = operation.filter(|value| !value.is_empty()).ok_or_else(|| {
            XtaskError::Fixture(format!("{}: operation is required", manifest.display()))
        })?;
        for filename in ["request.json", "expected.json"] {
            if !root.join(filename).is_file() {
                return Err(XtaskError::Fixture(format!(
                    "{}: missing {filename}",
                    root.display()
                )));
            }
        }
        for entry in std::fs::read_dir(root).map_err(XtaskError::Io)? {
            let path = entry.map_err(XtaskError::Io)?.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if !matches!(
                name,
                "case.toml" | "request.json" | "expected.json" | "repo"
            ) {
                return Err(XtaskError::Fixture(format!(
                    "{}: unknown fixture file `{name}`",
                    path.display()
                )));
            }
            if name == "repo" && !path.is_dir() {
                return Err(XtaskError::Fixture(format!(
                    "{}: repo fixture must be a directory",
                    path.display()
                )));
            }
        }
        let identity = root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                XtaskError::Fixture(format!("{}: invalid case directory", root.display()))
            })?
            .to_owned();
        Ok(Self {
            identity,
            operation,
        })
    }

    fn list(fixtures: &Path, domain: Option<&str>) -> Result<Vec<Self>, XtaskError> {
        let root = domain.map_or_else(|| fixtures.to_path_buf(), |domain| fixtures.join(domain));
        if !root.exists() {
            return Ok(Vec::new());
        }
        let identity_root = fixtures.to_path_buf();
        let mut cases = Vec::new();
        collect_fixture_manifests(&root, &identity_root, &mut cases)?;
        cases.sort_by(|left, right| left.identity.cmp(&right.identity));
        for pair in cases.windows(2) {
            if pair[0].identity == pair[1].identity {
                return Err(XtaskError::Fixture(format!(
                    "duplicate fixture identity `{}`",
                    pair[0].identity
                )));
            }
        }
        Ok(cases)
    }
}

fn collect_fixture_manifests(
    root: &Path,
    identity_root: &Path,
    cases: &mut Vec<FixtureManifest>,
) -> Result<(), XtaskError> {
    if root.join("case.toml").is_file() {
        let mut case = FixtureManifest::load(root)?;
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
    for entry in std::fs::read_dir(root).map_err(XtaskError::Io)? {
        let path = entry.map_err(XtaskError::Io)?.path();
        if path.is_dir() {
            collect_fixture_manifests(&path, identity_root, cases)?;
        }
    }
    Ok(())
}

#[derive(Debug)]
enum XtaskError {
    Usage(String),
    Io(std::io::Error),
    Json(serde_json::Error),
    Fixture(String),
    Architecture(String),
    CommandFailed { command: String, code: Option<i32> },
}
impl fmt::Display for XtaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => write!(f, "{message}"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Json(error) => write!(f, "metadata JSON error: {error}"),
            Self::Fixture(message) => write!(f, "fixture error: {message}"),
            Self::Architecture(message) => write!(f, "test architecture check failed: {message}"),
            Self::CommandFailed { command, code } => {
                write!(f, "command failed ({code:?}): {command}")
            }
        }
    }
}
impl std::error::Error for XtaskError {}

#[cfg(test)]
mod tests {
    use super::TestPlan;
    #[test]
    fn plan_contains_visible_locked_phases() {
        let plan = TestPlan::default();
        assert_eq!(plan.commands.len(), 3);
        assert!(
            plan.commands
                .iter()
                .all(|command| command.contains(&"--locked".to_owned()))
        );
    }

    #[test]
    fn profile_reports_slow_tests_without_retries() {
        let plan = TestPlan::profile();
        let command = &plan.commands[0];
        assert!(
            command
                .windows(2)
                .any(|args| args == ["--status-level", "slow"])
        );
        assert!(
            command
                .windows(2)
                .any(|args| args == ["--final-status-level", "slow"])
        );
        assert!(
            command
                .windows(2)
                .any(|args| args == ["--failure-output", "final"])
        );
    }
}
