mod ci;

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::Instant;

const PRODUCT: &str = "leantoken";
const SUPPORT: &str = "leantoken-test-support";
const SUITE: &str = "leantoken-test-suite";
const XTASK: &str = "leantoken-xtask";
const BENCHMARKS: &str = "leantoken-benchmarks";
const PRODUCT_PARALLEL_LANES: usize = 2;
const PARALLEL_NEXTTEST_JOBS: &str = "2";
const PRODUCT_PHASE_NAMES: [&str; 3] = [
    "library and binary units",
    "ordinary integration",
    "executable and MCP process behavior",
];

#[derive(Clone, Copy)]
enum FocusedTestTarget {
    Suite,
    Product,
}

impl FocusedTestTarget {
    fn package(self) -> &'static str {
        match self {
            Self::Suite => SUITE,
            Self::Product => PRODUCT,
        }
    }

    fn target_args(self) -> &'static [&'static str] {
        match self {
            Self::Suite => &["--lib"],
            Self::Product => &["--lib", "--bins", "--test", "integration"],
        }
    }

    fn run_tail(self) -> &'static [&'static str] {
        match self {
            Self::Suite => &[],
            Self::Product => &["--", "--test-threads=2"],
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}

fn run() -> Result<(), XtaskError> {
    let root = workspace_root();
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("check-test-architecture") => check_architecture(&root),
        Some("ci") => ci::run(&root, args.collect()).map_err(XtaskError::Architecture),
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
            || selector.strip_prefix("domains::") == Some(*domain)
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
        if !focused_target_has_test(root, FocusedTestTarget::Suite, &filter)? {
            return Err(XtaskError::NoTestsMatched(selector.clone()));
        }
        build_focused_test_command(FocusedTestTarget::Suite, &filter, false)
    } else {
        let suite_match = focused_target_has_test(root, FocusedTestTarget::Suite, selector)?;
        let product_match = focused_target_has_test(root, FocusedTestTarget::Product, selector)?;
        match (suite_match, product_match) {
            (true, true) => {
                return Err(XtaskError::Usage(format!(
                    "ambiguous test selector `{selector}` matches both {SUITE} and {PRODUCT}; use a domain-qualified selector or run the owning package directly"
                )));
            }
            (true, false) => build_focused_test_command(FocusedTestTarget::Suite, selector, false),
            (false, true) => {
                build_focused_test_command(FocusedTestTarget::Product, selector, false)
            }
            (false, false) => {
                return Err(XtaskError::NoTestsMatched(selector.clone()));
            }
        }
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

fn build_focused_test_command(
    target: FocusedTestTarget,
    selector: &str,
    list: bool,
) -> Vec<String> {
    let mut command = cargo_command([
        "test",
        "--locked",
        "--package",
        target.package(),
        "--all-features",
    ]);
    command.extend(
        target
            .target_args()
            .iter()
            .map(|argument| (*argument).to_owned()),
    );
    command.push(selector.to_owned());
    if list {
        command.extend(["--", "--list"].into_iter().map(str::to_owned));
    } else {
        command.extend(
            target
                .run_tail()
                .iter()
                .map(|argument| (*argument).to_owned()),
        );
    }
    command
}

fn focused_target_has_test(
    root: &Path,
    target: FocusedTestTarget,
    selector: &str,
) -> Result<bool, XtaskError> {
    command_has_test(root, &build_focused_test_command(target, selector, true))
}

fn command_has_test(root: &Path, command: &[String]) -> Result<bool, XtaskError> {
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
    Ok(listed_test_count(&output.stdout) > 0)
}

fn listed_test_count(output: &[u8]) -> usize {
    String::from_utf8_lossy(output)
        .lines()
        .filter(|line| line.strip_suffix(": test").is_some())
        .count()
}

fn run_test_command(root: &Path, args: Vec<String>) -> Result<(), XtaskError> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(XtaskError::Usage(test_usage()));
    };
    match command {
        "product" => {
            if args.len() == 2 && args[1] == "--parallel" {
                return run_parallel_product_plan(root, TestPlan::product());
            }
            if args.len() != 1 {
                return Err(XtaskError::Usage(
                    "`test product` accepts only --parallel".to_owned(),
                ));
            }
            run_plan(root, TestPlan::product())
        }
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
            TestPlan::product().print();
            Ok(())
        }
        _ => Err(XtaskError::Usage(test_usage())),
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

fn run_parallel_product_plan(root: &Path, plan: TestPlan) -> Result<(), XtaskError> {
    if plan.repetitions != 1 || plan.commands.len() != PRODUCT_PHASE_NAMES.len() {
        return Err(XtaskError::Architecture(
            "parallel product plan shape drifted".to_owned(),
        ));
    }
    let (parallel, sequential) = plan.commands.split_at(PRODUCT_PARALLEL_LANES);
    let results = thread::scope(|scope| {
        parallel
            .iter()
            .enumerate()
            .map(|(index, command)| {
                scope.spawn(move || {
                    let started = Instant::now();
                    let status = print_and_run(root, command);
                    (index, status, started.elapsed())
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join())
            .collect::<Vec<_>>()
    });

    let mut first_failure = None;
    for result in results {
        match result {
            Ok((index, status, elapsed)) => {
                println!(
                    "==> {} completed in {:.2}s",
                    PRODUCT_PHASE_NAMES[index],
                    elapsed.as_secs_f64()
                );
                let error = match status {
                    Ok(status) if status.success() => None,
                    Ok(status) => Some(XtaskError::CommandFailed {
                        command: parallel[index].join(" "),
                        code: status.code(),
                    }),
                    Err(error) => Some(error),
                };
                if let Some(error) = error {
                    eprintln!("==> {} failed: {error}", PRODUCT_PHASE_NAMES[index]);
                    if first_failure.is_none() {
                        first_failure = Some(error);
                    }
                }
            }
            Err(_) if first_failure.is_none() => {
                first_failure = Some(XtaskError::Architecture(
                    "parallel product lane panicked".to_owned(),
                ));
            }
            Err(_) => {}
        }
    }
    if let Some(error) = first_failure {
        return Err(error);
    }

    for (offset, command) in sequential.iter().enumerate() {
        let index = PRODUCT_PARALLEL_LANES + offset;
        let started = Instant::now();
        let status = print_and_run(root, command)?;
        println!(
            "==> {} completed in {:.2}s",
            PRODUCT_PHASE_NAMES[index],
            started.elapsed().as_secs_f64()
        );
        if !status.success() {
            return Err(XtaskError::CommandFailed {
                command: command.join(" "),
                code: status.code(),
            });
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

impl TestPlan {
    fn product() -> Self {
        let commands = vec![
            nextest_command([
                "--locked",
                "--workspace",
                "--exclude",
                BENCHMARKS,
                "--all-features",
                "--lib",
                "--bins",
                "-j",
                PARALLEL_NEXTTEST_JOBS,
            ]),
            nextest_command([
                "--locked",
                "--package",
                PRODUCT,
                "--all-features",
                "--test",
                "integration",
                "-j",
                PARALLEL_NEXTTEST_JOBS,
                "--",
                "--skip",
                "process::",
            ]),
            nextest_command([
                "--locked",
                "--package",
                PRODUCT,
                "--all-features",
                "--test",
                "integration",
                "process::",
                "-j",
                process_test_jobs(),
            ]),
        ];
        Self {
            commands,
            repetitions: 1,
        }
    }
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
            commands: vec![nextest_command([
                "--locked",
                "--package",
                PRODUCT,
                "--all-features",
                "--test",
                "integration",
                "process::",
                "-j",
                process_test_jobs(),
            ])],
            repetitions,
        })
    }
    fn profile() -> Self {
        Self {
            commands: vec![nextest_command([
                "--locked",
                "--workspace",
                "--exclude",
                BENCHMARKS,
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

fn nextest_command<const N: usize>(args: [&str; N]) -> Vec<String> {
    std::iter::once("cargo".to_owned())
        .chain(["nextest".to_owned(), "run".to_owned()])
        .chain(args.into_iter().map(str::to_owned))
        .collect()
}

fn process_test_jobs() -> &'static str {
    process_test_jobs_for_os(std::env::consts::OS)
}

fn process_test_jobs_for_os(os: &str) -> &'static str {
    match os {
        "macos" => "3",
        "linux" | "windows" => "4",
        _ => "2",
    }
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
    ci::check_topology(root).map_err(XtaskError::Architecture)?;
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
    let expected = BTreeSet::from([PRODUCT, SUPPORT, SUITE, XTASK, BENCHMARKS]);
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
    let expected_default = BTreeSet::from([PRODUCT, SUPPORT, SUITE, XTASK]);
    if default_names != expected_default {
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
            BENCHMARKS
                if !names.contains(PRODUCT)
                    || names
                        .intersection(&BTreeSet::from([SUPPORT, SUITE, XTASK]))
                        .next()
                        .is_some() =>
            {
                return Err(XtaskError::Architecture(
                    "benchmarks must depend only on the product package".to_owned(),
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
    check_organizational_includes(root)?;
    check_service_snapshot_boundary(root)?;
    println!(
        "test architecture: ok (workspace resolver 3, one root process target, directed private packages, service snapshot boundary)"
    );
    Ok(())
}

fn check_service_snapshot_boundary(root: &Path) -> Result<(), XtaskError> {
    let mut rust_files = Vec::new();
    let facade = root.join("src/services.rs");
    if facade.is_file() {
        rust_files.push(facade);
    }
    collect_rust_files(&root.join("src/services"), &mut rust_files).map_err(XtaskError::Io)?;
    let mut leaked = BTreeSet::new();
    for path in rust_files {
        let source = path
            .strip_prefix(root)
            .expect("walked below repository root")
            .to_string_lossy()
            .replace('\\', "/");
        let contents = fs::read_to_string(&path).map_err(XtaskError::Io)?;
        if source.ends_with("/tests.rs") {
            continue;
        }
        if contents
            .lines()
            .any(|line| line.contains("ReadSession") || line.contains("begin_read("))
        {
            leaked.insert(source);
        }
    }
    if !leaked.is_empty() {
        return Err(XtaskError::Architecture(format!(
            "service modules must use the storage-owned IndexSnapshot; raw snapshot reads leaked into {leaked:?}"
        )));
    }
    println!("service snapshot boundary: ok (storage owns the raw read session)");
    Ok(())
}

fn check_organizational_includes(root: &Path) -> Result<(), XtaskError> {
    let mut rust_files = Vec::new();
    collect_rust_files(root, &mut rust_files).map_err(XtaskError::Io)?;
    let mut found = BTreeSet::new();
    let include_macro = "include!".to_owned() + "(";
    let include_prefix = include_macro.clone() + "\"";
    for path in rust_files {
        let source = path
            .strip_prefix(root)
            .expect("walked below repository root")
            .to_string_lossy()
            .replace('\\', "/");
        let contents = fs::read_to_string(&path).map_err(XtaskError::Io)?;
        for line in contents.lines() {
            let Some((_, suffix)) = line.split_once(&include_prefix) else {
                if line.contains(&include_macro) {
                    return Err(XtaskError::Architecture(format!(
                        "unsupported include! form in {source}; organizational includes must be migrated to normal modules"
                    )));
                }
                continue;
            };
            let Some(included) = suffix.split_once("\")").map(|(value, _)| value) else {
                return Err(XtaskError::Architecture(format!(
                    "malformed include! in {source}"
                )));
            };
            found.insert((source.clone(), included.to_owned()));
        }
    }
    if !found.is_empty() {
        return Err(XtaskError::Architecture(format!(
            "organizational include! usage remains: {found:?}; migrate it to a normal module"
        )));
    }
    println!("organizational includes: ok (none)");
    Ok(())
}

fn collect_rust_files(root: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".git" | "target")
            ) {
                continue;
            }
            collect_rust_files(&path, files)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn check_test_inventory(root: &Path) -> Result<(), XtaskError> {
    let tests_dir = root.join("tests");
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
    let integration =
        std::fs::read_to_string(tests_dir.join("integration.rs")).map_err(XtaskError::Io)?;
    let registered = integration
        .lines()
        .filter_map(|line| line.trim().strip_prefix("mod "))
        .filter_map(|line| line.strip_suffix(';'))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if actual != registered {
        return Err(XtaskError::Architecture(format!(
            "root test inventory drifted: expected {registered:?}, found {actual:?}"
        )));
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

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .expect("xtask lives below workspace root")
}
fn usage() -> String {
    "cargo xtask check-test-architecture | test-focused <selector> | test {product [--parallel]|stress|profile|plan}".to_owned()
}
fn test_usage() -> String {
    "cargo xtask test product [--parallel] | stress | profile | plan --dry-run".to_owned()
}

#[derive(Debug)]
enum XtaskError {
    Usage(String),
    NoTestsMatched(String),
    Io(std::io::Error),
    Json(serde_json::Error),
    Architecture(String),
    CommandFailed { command: String, code: Option<i32> },
}

impl XtaskError {
    fn exit_code(&self) -> u8 {
        match self {
            Self::CommandFailed {
                code: Some(code), ..
            } => u8::try_from(*code).unwrap_or(1),
            _ => 1,
        }
    }
}

impl fmt::Display for XtaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => write!(f, "{message}"),
            Self::NoTestsMatched(selector) => {
                write!(f, "no tests matched focused selector `{selector}`")
            }
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Json(error) => write!(f, "metadata JSON error: {error}"),
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
    use super::{
        BENCHMARKS, PARALLEL_NEXTTEST_JOBS, PRODUCT, PRODUCT_PARALLEL_LANES, TestPlan, XtaskError,
        listed_test_count, process_test_jobs_for_os, run_parallel_product_plan, workspace_root,
    };

    #[test]
    fn plan_contains_visible_locked_phases() {
        let plan = TestPlan::product();
        assert_eq!(plan.commands.len(), 3);
        assert!(
            plan.commands
                .iter()
                .all(|command| command.contains(&"--locked".to_owned()))
        );
        assert_eq!(PRODUCT_PARALLEL_LANES, 2);
        assert!(plan.commands[0].contains(&"--workspace".to_owned()));
        assert!(
            plan.commands[1]
                .windows(2)
                .any(|args| args == ["--package", PRODUCT])
        );
        assert!(
            plan.commands[1]
                .windows(2)
                .any(|args| args == ["--skip", "process::"])
        );
        assert!(!plan.commands[0].contains(&"process::".to_owned()));
        assert!(plan.commands[PRODUCT_PARALLEL_LANES].contains(&"process::".to_owned()));
        assert!(
            plan.commands[PRODUCT_PARALLEL_LANES]
                .windows(2)
                .any(|args| args == ["-j", process_test_jobs_for_os(std::env::consts::OS)])
        );
        for command in &plan.commands[..PRODUCT_PARALLEL_LANES] {
            assert!(
                command
                    .windows(2)
                    .any(|args| args == ["-j", PARALLEL_NEXTTEST_JOBS])
            );
        }
        assert!(
            plan.commands
                .iter()
                .filter(|command| command.contains(&"--workspace".to_owned()))
                .all(|command| command
                    .windows(2)
                    .any(|args| args == ["--exclude", BENCHMARKS]))
        );
    }

    #[test]
    fn process_worker_bound_matches_supported_runner_capacity() {
        assert_eq!(process_test_jobs_for_os("macos"), "3");
        assert_eq!(process_test_jobs_for_os("linux"), "4");
        assert_eq!(process_test_jobs_for_os("windows"), "4");
        assert_eq!(process_test_jobs_for_os("freebsd"), "2");
    }

    #[test]
    fn command_failures_preserve_the_child_exit_code() {
        let error = XtaskError::CommandFailed {
            command: "cargo test".to_owned(),
            code: Some(101),
        };
        assert_eq!(error.exit_code(), 101);
    }

    #[test]
    fn parallel_product_runner_executes_its_bounded_plan() {
        let command = vec!["cargo".to_owned(), "--version".to_owned()];
        let plan = TestPlan {
            commands: vec![command.clone(), command.clone(), command],
            repetitions: 1,
        };
        run_parallel_product_plan(&workspace_root(), plan).expect("parallel plan");
    }

    #[test]
    fn parallel_product_runner_preserves_a_lane_failure_and_stops() {
        let plan = TestPlan {
            commands: vec![
                vec![
                    "cargo".to_owned(),
                    "--invalid-parallel-test-flag".to_owned(),
                ],
                vec!["cargo".to_owned(), "--version".to_owned()],
                vec!["this-program-must-not-run".to_owned()],
            ],
            repetitions: 1,
        };
        let error =
            run_parallel_product_plan(&workspace_root(), plan).expect_err("failed lane passed");
        match error {
            XtaskError::CommandFailed { command, code } => {
                assert!(command.contains("--invalid-parallel-test-flag"));
                assert!(code.is_some());
            }
            error => panic!("unexpected error: {error}"),
        }
    }

    #[test]
    fn listed_test_count_ignores_harness_summaries() {
        let output = b"domains::retrieval::same_name: test\n\
                       1 test, 0 benchmarks\n\
                       services::search::other_name: test\n";
        assert_eq!(listed_test_count(output), 2);
        assert_eq!(listed_test_count(b"0 tests, 0 benchmarks\n"), 0);
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
