mod ci;

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;
use syn::visit::Visit;
use toml_edit::{DocumentMut, Item, TableLike};

const PRODUCT: &str = "leantoken";
const SUPPORT: &str = "leantoken-test-support";
const SUITE: &str = "leantoken-test-suite";
const XTASK: &str = "leantoken-xtask";
const BENCHMARKS: &str = "leantoken-benchmarks";
const LOCAL_NEXTEST_PROFILE: &str = "local";
const CI_NEXTEST_PROFILE: &str = "ci";
const STRESS_NEXTEST_PROFILE: &str = "stress";
const TIMING_NEXTEST_PROFILE: &str = "profile";
const MAX_STRESS_REPETITIONS: usize = 100;
const MAX_NEXTEST_JUNIT_BYTES: u64 = 8 * 1024 * 1024;

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
        // Probe both packages: a name present in both is ambiguous and must
        // fail instead of silently running only one package.
        let product_match = focused_target_has_test(root, FocusedTestTarget::Product, selector)?;
        let suite_match = focused_target_has_test(root, FocusedTestTarget::Suite, selector)?;
        match (product_match, suite_match) {
            (true, true) => {
                return Err(XtaskError::Usage(format!(
                    "ambiguous test selector `{selector}` matches both {SUITE} and {PRODUCT}; use a domain-qualified selector or run the owning package directly"
                )));
            }
            (true, false) => {
                build_focused_test_command(FocusedTestTarget::Product, selector, false)
            }
            (false, true) => build_focused_test_command(FocusedTestTarget::Suite, selector, false),
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
        "product" => run_plan(root, TestPlan::product(parse_product_profile(&args[1..])?)),
        "stress" if args.len() == 1 => run_plan(root, TestPlan::stress()?),
        "stress" => Err(XtaskError::Usage(
            "`test stress` does not accept additional arguments".to_owned(),
        )),
        "profile" if args.len() == 1 => run_plan(root, TestPlan::profile()),
        "profile" => Err(XtaskError::Usage(
            "`test profile` does not accept additional arguments".to_owned(),
        )),
        "plan" => {
            let profile = parse_plan_profile(&args[1..])?;
            TestPlan::product(profile).print();
            Ok(())
        }
        _ => Err(XtaskError::Usage(test_usage())),
    }
}

fn parse_product_profile(args: &[String]) -> Result<&'static str, XtaskError> {
    match args {
        [] => Ok(LOCAL_NEXTEST_PROFILE),
        [flag, profile] if flag == "--profile" && profile == CI_NEXTEST_PROFILE => {
            Ok(CI_NEXTEST_PROFILE)
        }
        [flag, profile] if flag == "--profile" && profile == LOCAL_NEXTEST_PROFILE => {
            Ok(LOCAL_NEXTEST_PROFILE)
        }
        _ => Err(XtaskError::Usage(
            "`test product` accepts only --profile local|ci".to_owned(),
        )),
    }
}

fn parse_plan_profile(args: &[String]) -> Result<&'static str, XtaskError> {
    match args {
        [] => Ok(LOCAL_NEXTEST_PROFILE),
        [arg] if arg == "--dry-run" => Ok(LOCAL_NEXTEST_PROFILE),
        [flag, profile]
            if flag == "--profile"
                && matches!(profile.as_str(), LOCAL_NEXTEST_PROFILE | CI_NEXTEST_PROFILE) =>
        {
            parse_product_profile(args)
        }
        [dry_run, flag, profile]
            if dry_run == "--dry-run"
                && flag == "--profile"
                && matches!(profile.as_str(), LOCAL_NEXTEST_PROFILE | CI_NEXTEST_PROFILE) =>
        {
            parse_product_profile(&args[1..])
        }
        [flag, profile, dry_run]
            if flag == "--profile"
                && dry_run == "--dry-run"
                && matches!(profile.as_str(), LOCAL_NEXTEST_PROFILE | CI_NEXTEST_PROFILE) =>
        {
            parse_product_profile(&args[..2])
        }
        _ => Err(XtaskError::Usage(
            "`test plan` accepts only --dry-run and --profile local|ci".to_owned(),
        )),
    }
}

fn run_plan(root: &Path, plan: TestPlan) -> Result<(), XtaskError> {
    if plan.preserve_repetition_reports && plan.repetitions > 1 {
        prepare_repetition_reports(root)?;
    }
    for repetition in 1..=plan.repetitions {
        if plan.repetitions > 1 {
            println!("==> stress repetition {repetition}/{}", plan.repetitions);
        }
        for command in &plan.commands {
            if plan.preserve_repetition_reports && plan.repetitions > 1 {
                clear_current_stress_report(root)?;
            }
            let started = Instant::now();
            let status = print_and_run(root, command)?;
            println!(
                "==> {} completed in {:.2}s",
                plan.owner,
                started.elapsed().as_secs_f64()
            );
            let report = if plan.preserve_repetition_reports && plan.repetitions > 1 {
                preserve_repetition_report(root, repetition)
            } else {
                Ok(())
            };
            if !status.success() {
                if let Err(error) = report {
                    eprintln!("==> could not preserve failed stress report: {error}");
                }
                return Err(XtaskError::CommandFailed {
                    command: command.join(" "),
                    code: status.code(),
                });
            }
            report?;
        }
    }
    Ok(())
}

fn stress_report_directory(root: &Path) -> PathBuf {
    root.join("target/nextest/stress/repetitions")
}

fn prepare_repetition_reports(root: &Path) -> Result<(), XtaskError> {
    let parent = ensure_workspace_directory(root, Path::new("target/nextest/stress"))?;
    let directory = parent.join("repetitions");
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if is_real_directory(&metadata) => {
            ensure_path_is_in_workspace(root, &directory)?;
            fs::remove_dir_all(&directory).map_err(XtaskError::Io)?;
        }
        Ok(_) => {
            return Err(XtaskError::Architecture(
                "stress report directory must not be a symlink or regular file".to_owned(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(XtaskError::Io(error)),
    }
    fs::create_dir(&directory).map_err(XtaskError::Io)?;
    let metadata = fs::symlink_metadata(&directory).map_err(XtaskError::Io)?;
    if !is_real_directory(&metadata) {
        return Err(XtaskError::Architecture(
            "stress report directory must be a real directory".to_owned(),
        ));
    }
    ensure_path_is_in_workspace(root, &directory)?;
    Ok(())
}

fn ensure_workspace_directory(root: &Path, relative: &Path) -> Result<PathBuf, XtaskError> {
    let canonical_root = root.canonicalize().map_err(XtaskError::Io)?;
    let mut directory = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(XtaskError::Architecture(
                "stress report paths must be workspace-relative components".to_owned(),
            ));
        };
        directory.push(component);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if is_real_directory(&metadata) => {}
            Ok(_) => {
                return Err(XtaskError::Architecture(format!(
                    "stress report ancestor {} must be a real directory",
                    directory.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&directory).map_err(XtaskError::Io)?;
                let metadata = fs::symlink_metadata(&directory).map_err(XtaskError::Io)?;
                if !is_real_directory(&metadata) {
                    return Err(XtaskError::Architecture(
                        "created stress report ancestor is not a real directory".to_owned(),
                    ));
                }
            }
            Err(error) => return Err(XtaskError::Io(error)),
        }
        let canonical = directory.canonicalize().map_err(XtaskError::Io)?;
        if !canonical.starts_with(&canonical_root) {
            return Err(XtaskError::Architecture(
                "stress report ancestor resolves outside the workspace".to_owned(),
            ));
        }
    }
    Ok(directory)
}

fn is_real_directory(metadata: &fs::Metadata) -> bool {
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    true
}

fn ensure_path_is_in_workspace(root: &Path, path: &Path) -> Result<(), XtaskError> {
    let canonical_root = root.canonicalize().map_err(XtaskError::Io)?;
    let canonical_path = path.canonicalize().map_err(XtaskError::Io)?;
    if !canonical_path.starts_with(canonical_root) {
        return Err(XtaskError::Architecture(
            "stress report path resolves outside the workspace".to_owned(),
        ));
    }
    Ok(())
}

fn validate_bounded_regular_report(
    root: &Path,
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), XtaskError> {
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(XtaskError::Architecture(
            "stress JUnit report must be a regular file".to_owned(),
        ));
    }
    if metadata.len() > MAX_NEXTEST_JUNIT_BYTES {
        return Err(XtaskError::Architecture(format!(
            "stress JUnit report exceeds its {MAX_NEXTEST_JUNIT_BYTES}-byte bound"
        )));
    }
    ensure_path_is_in_workspace(root, path)
}

fn clear_current_stress_report(root: &Path) -> Result<(), XtaskError> {
    let stress = ensure_workspace_directory(root, Path::new("target/nextest/stress"))?;
    let source = stress.join("junit.xml");
    match fs::symlink_metadata(&source) {
        Ok(metadata) => {
            validate_bounded_regular_report(root, &source, &metadata)?;
            fs::remove_file(source).map_err(XtaskError::Io)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(XtaskError::Io(error)),
    }
}

fn preserve_repetition_report(root: &Path, repetition: usize) -> Result<(), XtaskError> {
    let source = root.join("target/nextest/stress/junit.xml");
    let path_metadata = fs::symlink_metadata(&source).map_err(XtaskError::Io)?;
    validate_bounded_regular_report(root, &source, &path_metadata)?;
    let source_file = OpenOptions::new()
        .read(true)
        .open(&source)
        .map_err(XtaskError::Io)?;
    let opened_metadata = source_file.metadata().map_err(XtaskError::Io)?;
    validate_bounded_regular_report(root, &source, &opened_metadata)?;
    let current_metadata = fs::symlink_metadata(&source).map_err(XtaskError::Io)?;
    validate_bounded_regular_report(root, &source, &current_metadata)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if path_metadata.dev() != opened_metadata.dev()
            || path_metadata.ino() != opened_metadata.ino()
            || current_metadata.dev() != opened_metadata.dev()
            || current_metadata.ino() != opened_metadata.ino()
        {
            return Err(XtaskError::Architecture(
                "stress JUnit source changed while it was opened".to_owned(),
            ));
        }
    }

    let destination = stress_report_directory(root).join(format!("junit-{repetition:03}.xml"));
    ensure_path_is_in_workspace(root, &stress_report_directory(root))?;
    let mut destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .map_err(XtaskError::Io)?;
    let publication = (|| {
        let copied = io::copy(
            &mut source_file.take(MAX_NEXTEST_JUNIT_BYTES + 1),
            &mut destination_file,
        )
        .map_err(XtaskError::Io)?;
        if copied > MAX_NEXTEST_JUNIT_BYTES {
            return Err(XtaskError::Architecture(
                "stress JUnit report grew beyond its byte bound while copying".to_owned(),
            ));
        }
        destination_file.sync_all().map_err(XtaskError::Io)?;
        let metadata = destination_file.metadata().map_err(XtaskError::Io)?;
        Ok((copied, metadata))
    })();
    drop(destination_file);
    let (copied, opened_destination) = match publication {
        Ok(published) => published,
        Err(error) => {
            let _ = fs::remove_file(&destination);
            return Err(error);
        }
    };
    let validation = (|| {
        let destination_metadata = fs::symlink_metadata(&destination).map_err(XtaskError::Io)?;
        validate_bounded_regular_report(root, &destination, &destination_metadata)?;
        if opened_destination.len() != copied || destination_metadata.len() != copied {
            return Err(XtaskError::Architecture(
                "preserved stress JUnit report changed after publication".to_owned(),
            ));
        }
        Ok(())
    })();
    if let Err(error) = validation {
        let _ = fs::remove_file(&destination);
        return Err(error);
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
    owner: &'static str,
    commands: Vec<Vec<String>>,
    repetitions: usize,
    preserve_repetition_reports: bool,
}

impl TestPlan {
    fn product(profile: &'static str) -> Self {
        let commands = vec![nextest_command(&[
            "--locked",
            "--workspace",
            "--exclude",
            BENCHMARKS,
            "--all-features",
            "--profile",
            profile,
            "-j",
            product_test_jobs(),
        ])];
        Self {
            owner: "complete product graph",
            commands,
            repetitions: 1,
            preserve_repetition_reports: false,
        }
    }
    fn stress() -> Result<Self, XtaskError> {
        let repetitions = stress_repetitions(
            &env::var("LEANTOKEN_STRESS_REPETITIONS").unwrap_or_else(|_| "1".to_owned()),
        )?;
        Ok(Self::stress_with_repetitions(repetitions))
    }
    fn stress_with_repetitions(repetitions: usize) -> Self {
        Self {
            owner: "process lifecycle stress",
            commands: vec![nextest_command(&[
                "--locked",
                "--package",
                PRODUCT,
                "--all-features",
                "--test",
                "integration",
                "--filterset",
                "test(/^process::mcp_lifecycle_/)",
                "--profile",
                STRESS_NEXTEST_PROFILE,
                "-j",
                product_test_jobs(),
            ])],
            repetitions,
            preserve_repetition_reports: true,
        }
    }
    fn profile() -> Self {
        Self {
            owner: "product timing profile",
            commands: vec![nextest_command(&[
                "--locked",
                "--workspace",
                "--exclude",
                BENCHMARKS,
                "--all-features",
                "--profile",
                TIMING_NEXTEST_PROFILE,
                "-j",
                product_test_jobs(),
            ])],
            repetitions: 1,
            preserve_repetition_reports: false,
        }
    }
    fn print(&self) {
        for command in &self.commands {
            println!("{}", command.join(" "));
        }
    }
}

fn stress_repetitions(value: &str) -> Result<usize, XtaskError> {
    let repetitions = value.parse::<usize>().map_err(|_| {
        XtaskError::Usage(format!(
            "LEANTOKEN_STRESS_REPETITIONS must be between 1 and {MAX_STRESS_REPETITIONS}"
        ))
    })?;
    if !(1..=MAX_STRESS_REPETITIONS).contains(&repetitions) {
        return Err(XtaskError::Usage(format!(
            "LEANTOKEN_STRESS_REPETITIONS must be between 1 and {MAX_STRESS_REPETITIONS}"
        )));
    }
    Ok(repetitions)
}

fn cargo_command<const N: usize>(args: [&str; N]) -> Vec<String> {
    std::iter::once("cargo".to_owned())
        .chain(args.into_iter().map(str::to_owned))
        .collect()
}

fn nextest_command(args: &[&str]) -> Vec<String> {
    std::iter::once("cargo".to_owned())
        .chain(["nextest".to_owned(), "run".to_owned()])
        .chain(args.iter().map(|arg| (*arg).to_owned()))
        .collect()
}

fn product_test_jobs() -> &'static str {
    product_test_jobs_for_os(std::env::consts::OS)
}

fn product_test_jobs_for_os(os: &str) -> &'static str {
    match os {
        "macos" => "3",
        "linux" => "4",
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
    src_path: PathBuf,
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
    let integration_target = product
        .targets
        .iter()
        .find(|target| {
            target.name == "integration" && target.kind.iter().any(|kind| kind == "test")
        })
        .ok_or_else(|| XtaskError::Architecture("root integration target is missing".to_owned()))?;
    check_nextest_policy(root)?;
    check_test_inventory(root, &metadata, integration_target)?;
    check_ignored_test_policy(root)?;
    check_organizational_includes(root)?;
    println!(
        "test architecture: ok (workspace resolver 3, Cargo-owned targets, one-scheduler nextest policy, directed private packages, compiled ignored-test inventory, syntax-tree macro policy)"
    );
    Ok(())
}

fn check_nextest_policy(root: &Path) -> Result<(), XtaskError> {
    let path = root.join(".config/nextest.toml");
    let contents = fs::read_to_string(&path).map_err(XtaskError::Io)?;
    let document = contents.parse::<DocumentMut>().map_err(|error| {
        XtaskError::Architecture(format!("could not parse {}: {error}", path.display()))
    })?;
    let profiles = required_table(document.get("profile"), "profile")?;
    let profile_specs = [
        ("default", None, true, None),
        (
            LOCAL_NEXTEST_PROFILE,
            Some("default"),
            true,
            Some("junit.xml"),
        ),
        (
            CI_NEXTEST_PROFILE,
            Some("default"),
            false,
            Some("junit.xml"),
        ),
        (
            STRESS_NEXTEST_PROFILE,
            Some("default"),
            false,
            Some("junit.xml"),
        ),
        (
            TIMING_NEXTEST_PROFILE,
            Some(CI_NEXTEST_PROFILE),
            false,
            Some("junit.xml"),
        ),
    ];
    for (name, inherited, fail_fast, junit_path) in profile_specs {
        let profile = required_table(profiles.get(name), &format!("profile.{name}"))?;
        if profile.get("retries").and_then(Item::as_integer) != Some(0)
            || profile.get("flaky-result").and_then(Item::as_str) != Some("fail")
            || profile.get("fail-fast").and_then(Item::as_bool) != Some(fail_fast)
            || profile
                .get("global-timeout")
                .and_then(Item::as_str)
                .is_none_or(str::is_empty)
        {
            return Err(XtaskError::Architecture(format!(
                "profile.{name} must declare zero retries, flaky failures, fail-fast={fail_fast}, and a global timeout"
            )));
        }
        if name == "default" && profile.get("test-threads").and_then(Item::as_integer) != Some(4) {
            return Err(XtaskError::Architecture(
                "profile.default must retain a bounded four-thread direct-run fallback".to_owned(),
            ));
        }
        if profile.get("inherits").and_then(Item::as_str) != inherited {
            return Err(XtaskError::Architecture(format!(
                "profile.{name} has the wrong inheritance owner"
            )));
        }
        if let Some(expected_path) = junit_path {
            let junit = required_table(profile.get("junit"), &format!("profile.{name}.junit"))?;
            if junit.get("path").and_then(Item::as_str) != Some(expected_path) {
                return Err(XtaskError::Architecture(format!(
                    "profile.{name} must write JUnit evidence to {expected_path}"
                )));
            }
        }
        reject_retry_overrides(profile, name)?;
    }

    let groups = required_table(document.get("test-groups"), "test-groups")?;
    let expected_groups = [
        ("cheap", 8),
        ("cold-index-sqlite", 2),
        ("git-fixtures", 2),
        ("filesystem-watcher", 1),
        ("process-mcp", 4),
        ("extended", 1),
    ];
    for (name, max_threads) in expected_groups {
        let group = required_table(groups.get(name), &format!("test-groups.{name}"))?;
        if group.get("max-threads").and_then(Item::as_integer) != Some(max_threads) {
            return Err(XtaskError::Architecture(format!(
                "test group {name} must retain its checked max-threads={max_threads} bound"
            )));
        }
    }

    let default = required_table(profiles.get("default"), "profile.default")?;
    let overrides = default
        .get("overrides")
        .and_then(Item::as_array_of_tables)
        .ok_or_else(|| {
            XtaskError::Architecture(
                "profile.default must classify every resource owner through overrides".to_owned(),
            )
        })?;
    let expected_overrides: [(&str, &[&str]); 6] = [
        (
            "extended",
            &["package(leantoken-benchmarks)", "concurrency_profile"],
        ),
        ("process-mcp", &["process::", "mcp::", "domains::protocol"]),
        (
            "filesystem-watcher",
            &["watcher::", "setup::", "sandbox::", "domains::platform"],
        ),
        ("git-fixtures", &["repository::", "history|diff", "git"]),
        (
            "cold-index-sqlite",
            &[
                "storage::",
                "indexer::",
                "domains::(indexing_repository|storage)",
                "services::tests",
                "read|query_receipts|receipts|lifecycle|repository|reconciliation",
            ],
        ),
        ("cheap", &["all()"]),
    ];
    let mut classified = BTreeSet::new();
    for override_table in overrides {
        let group = override_table
            .get("test-group")
            .and_then(Item::as_str)
            .ok_or_else(|| {
                XtaskError::Architecture(
                    "every nextest override must assign a checked test group".to_owned(),
                )
            })?;
        if !classified.insert(group.to_owned()) {
            return Err(XtaskError::Architecture(format!(
                "test group {group} has more than one classification override"
            )));
        }
        let Some((_, fragments)) = expected_overrides
            .iter()
            .find(|(expected, _)| *expected == group)
        else {
            return Err(XtaskError::Architecture(format!(
                "unexpected nextest resource group {group}"
            )));
        };
        let filter = override_table
            .get("filter")
            .and_then(Item::as_str)
            .unwrap_or_default();
        if fragments.iter().any(|fragment| !filter.contains(fragment))
            || override_table.get("threads-required").is_none()
            || override_table.get("slow-timeout").is_none()
            || override_table
                .get("retries")
                .and_then(Item::as_integer)
                .is_some_and(|retries| retries != 0)
        {
            return Err(XtaskError::Architecture(format!(
                "test group {group} lacks its semantic filter, scheduler reservation, timeout, or zero-retry policy"
            )));
        }
    }
    let expected_group_names = expected_overrides
        .iter()
        .map(|(group, _)| (*group).to_owned())
        .collect::<BTreeSet<_>>();
    if classified != expected_group_names {
        return Err(XtaskError::Architecture(format!(
            "nextest resource classifications drifted: {classified:?}"
        )));
    }

    for test_name in [
        "services::tests::concurrent_consistency_requests_share_one_waiting_wave",
        "services::tests::index_search_read_and_hash_delta",
    ] {
        check_nextest_group_assignment(root, "cold-index-sqlite", test_name)?;
    }

    let plans = [
        (
            LOCAL_NEXTEST_PROFILE,
            TestPlan::product(LOCAL_NEXTEST_PROFILE),
        ),
        (CI_NEXTEST_PROFILE, TestPlan::product(CI_NEXTEST_PROFILE)),
        (STRESS_NEXTEST_PROFILE, TestPlan::stress_with_repetitions(1)),
        (TIMING_NEXTEST_PROFILE, TestPlan::profile()),
    ];
    for (profile, plan) in plans {
        if plan.commands.len() != 1
            || command_option(&plan.commands[0], "--profile") != Some(profile)
        {
            return Err(XtaskError::Architecture(format!(
                "xtask mode for {profile} must select exactly one authoritative nextest scheduler with its named profile"
            )));
        }
    }
    println!(
        "nextest policy: ok (one product scheduler, explicit profiles, zero retries, six bounded resource groups)"
    );
    Ok(())
}

fn check_nextest_group_assignment(
    root: &Path,
    group: &str,
    test_name: &str,
) -> Result<(), XtaskError> {
    let output = Command::new("cargo")
        .args([
            "nextest",
            "show-config",
            "test-groups",
            "--profile",
            CI_NEXTEST_PROFILE,
            "--groups",
            group,
            "--locked",
            "--workspace",
            "--exclude",
            BENCHMARKS,
            "--all-features",
            "--no-pager",
            test_name,
        ])
        .current_dir(root)
        .env("CARGO_TERM_COLOR", "never")
        .output()
        .map_err(XtaskError::Io)?;
    if !output.status.success() {
        return Err(XtaskError::Architecture(format!(
            "could not resolve nextest group for {test_name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    if output.stdout.len() > 64 * 1024 {
        return Err(XtaskError::Architecture(
            "nextest group resolution exceeded its diagnostic bound".to_owned(),
        ));
    }
    let resolved = String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.trim() == test_name);
    if !resolved {
        return Err(XtaskError::Architecture(format!(
            "compiled test {test_name} is not assigned to nextest group {group}"
        )));
    }
    Ok(())
}

fn required_table<'a>(
    item: Option<&'a Item>,
    context: &str,
) -> Result<&'a dyn TableLike, XtaskError> {
    item.and_then(Item::as_table_like).ok_or_else(|| {
        XtaskError::Architecture(format!("nextest configuration is missing table {context}"))
    })
}

fn reject_retry_overrides(profile: &dyn TableLike, name: &str) -> Result<(), XtaskError> {
    if profile
        .get("overrides")
        .and_then(Item::as_array_of_tables)
        .is_some_and(|overrides| {
            overrides.iter().any(|override_table| {
                override_table
                    .get("retries")
                    .and_then(Item::as_integer)
                    .is_some_and(|retries| retries != 0)
            })
        })
    {
        return Err(XtaskError::Architecture(format!(
            "profile.{name} contains a nonzero retry override"
        )));
    }
    Ok(())
}

fn command_option<'a>(command: &'a [String], option: &str) -> Option<&'a str> {
    command
        .windows(2)
        .find(|arguments| arguments[0] == option)
        .map(|arguments| arguments[1].as_str())
}

fn check_organizational_includes(root: &Path) -> Result<(), XtaskError> {
    let mut rust_files = Vec::new();
    collect_rust_files(root, &mut rust_files).map_err(XtaskError::Io)?;
    let mut found = Vec::new();
    for path in rust_files {
        let source = path
            .strip_prefix(root)
            .expect("walked below repository root")
            .to_string_lossy()
            .replace('\\', "/");
        let contents = fs::read_to_string(&path).map_err(XtaskError::Io)?;
        let syntax = parse_rust_source(&source, &contents)?;
        if contains_include_macro(&syntax) {
            found.push(source);
        }
    }
    if !found.is_empty() {
        return Err(XtaskError::Architecture(format!(
            "compiled Rust sources invoke include!: {found:?}; migrate organizational includes to normal modules"
        )));
    }
    println!("organizational includes: ok (no include! invocation in parsed Rust syntax)");
    Ok(())
}

fn parse_rust_source(source: &str, contents: &str) -> Result<syn::File, XtaskError> {
    syn::parse_file(contents).map_err(|error| {
        XtaskError::Architecture(format!("failed to parse Rust source {source}: {error}"))
    })
}

fn contains_include_macro(syntax: &syn::File) -> bool {
    struct IncludeMacroVisitor {
        found: bool,
    }

    impl<'ast> Visit<'ast> for IncludeMacroVisitor {
        fn visit_macro(&mut self, node: &'ast syn::Macro) {
            if node
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "include")
            {
                self.found = true;
            }
            syn::visit::visit_macro(self, node);
        }
    }

    let mut visitor = IncludeMacroVisitor { found: false };
    visitor.visit_file(syntax);
    visitor.found
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

fn check_test_inventory(
    root: &Path,
    metadata: &Metadata,
    integration_target: &Target,
) -> Result<(), XtaskError> {
    let integration_source =
        fs::canonicalize(&integration_target.src_path).map_err(XtaskError::Io)?;
    let tests_dir = integration_source.parent().ok_or_else(|| {
        XtaskError::Architecture("integration target has no source directory".into())
    })?;
    let independent_targets = metadata
        .packages
        .iter()
        .flat_map(|package| &package.targets)
        .filter_map(|target| fs::canonicalize(&target.src_path).ok())
        .collect::<BTreeSet<_>>();
    let actual = fs::read_dir(tests_dir)
        .map_err(XtaskError::Io)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .map(|path| fs::canonicalize(path).map_err(XtaskError::Io))
        .collect::<Result<BTreeSet<_>, _>>()?
        .into_iter()
        .filter(|path| path != &integration_source && !independent_targets.contains(path))
        .collect::<BTreeSet<_>>();
    let contents = fs::read_to_string(&integration_source).map_err(XtaskError::Io)?;
    let source = integration_source
        .strip_prefix(root)
        .unwrap_or(&integration_source)
        .display()
        .to_string();
    let syntax = parse_rust_source(&source, &contents)?;
    let registered = syntax
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(module) if module.content.is_none() => Some(module),
            _ => None,
        })
        .map(|module| resolve_external_module(tests_dir, module))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if actual != registered {
        return Err(XtaskError::Architecture(format!(
            "root test inventory drifted: Cargo target owns {}, registered modules are {:?}, owner files are {:?}",
            display_path(root, &integration_source),
            display_paths(root, &registered),
            display_paths(root, &actual),
        )));
    }
    println!(
        "test inventory: ok ({} root owners resolved from Cargo target and Rust modules)",
        actual.len()
    );
    Ok(())
}

fn resolve_external_module(directory: &Path, module: &syn::ItemMod) -> Result<PathBuf, XtaskError> {
    let explicit = module
        .attrs
        .iter()
        .find(|attribute| attribute.path().is_ident("path"))
        .map(module_path_value)
        .transpose()?;
    let candidate = if let Some(relative) = explicit {
        directory.join(relative)
    } else {
        let flat = directory.join(format!("{}.rs", module.ident));
        let nested = directory.join(module.ident.to_string()).join("mod.rs");
        match (flat.is_file(), nested.is_file()) {
            (true, false) => flat,
            (false, true) => nested,
            (true, true) => {
                return Err(XtaskError::Architecture(format!(
                    "module {} has both flat and nested source files",
                    module.ident
                )));
            }
            (false, false) => {
                return Err(XtaskError::Architecture(format!(
                    "module {} does not resolve below {}",
                    module.ident,
                    directory.display()
                )));
            }
        }
    };
    fs::canonicalize(candidate).map_err(XtaskError::Io)
}

fn module_path_value(attribute: &syn::Attribute) -> Result<PathBuf, XtaskError> {
    let syn::Meta::NameValue(name_value) = &attribute.meta else {
        return Err(XtaskError::Architecture(
            "module #[path] must use a string name-value attribute".into(),
        ));
    };
    let syn::Expr::Lit(expression) = &name_value.value else {
        return Err(XtaskError::Architecture(
            "module #[path] must contain a string literal".into(),
        ));
    };
    let syn::Lit::Str(path) = &expression.lit else {
        return Err(XtaskError::Architecture(
            "module #[path] must contain a string literal".into(),
        ));
    };
    Ok(PathBuf::from(path.value()))
}

fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn display_paths(root: &Path, paths: &BTreeSet<PathBuf>) -> Vec<String> {
    paths.iter().map(|path| display_path(root, path)).collect()
}

fn check_ignored_test_policy(root: &Path) -> Result<(), XtaskError> {
    let output = Command::new("cargo")
        .args([
            "test",
            "--locked",
            "--workspace",
            "--all-features",
            "--lib",
            "--bins",
            "--tests",
            "--",
            "--ignored",
            "--list",
            "--format",
            "terse",
        ])
        .current_dir(root)
        .output()
        .map_err(XtaskError::Io)?;
    if !output.status.success() {
        return Err(XtaskError::CommandFailed {
            command: "cargo test --locked --workspace --all-features --lib --bins --tests -- --ignored --list --format terse".into(),
            code: output.status.code(),
        });
    }
    let mut ignored = parse_compiled_test_list(&String::from_utf8_lossy(&output.stdout));
    ignored.sort();
    let allowed = vec!["services::concurrency_profile::release_concurrency_matrix".to_owned()];
    if ignored != allowed {
        return Err(XtaskError::Architecture(format!(
            "compiled ignored-test inventory drifted: expected {allowed:?}, found {ignored:?}"
        )));
    }
    println!(
        "ignored-test policy: ok (compiled inventory contains only the manual release profiler)"
    );
    Ok(())
}

fn parse_compiled_test_list(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.trim().strip_suffix(": test"))
        .map(str::to_owned)
        .collect()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .expect("xtask lives below workspace root")
}
fn usage() -> String {
    "cargo xtask check-test-architecture | test-focused <selector> | test {product [--profile local|ci]|stress|profile|plan}".to_owned()
}
fn test_usage() -> String {
    "cargo xtask test product [--profile local|ci] | stress | profile | plan [--profile local|ci] --dry-run".to_owned()
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
        BENCHMARKS, CI_NEXTEST_PROFILE, LOCAL_NEXTEST_PROFILE, MAX_NEXTEST_JUNIT_BYTES,
        STRESS_NEXTEST_PROFILE, TIMING_NEXTEST_PROFILE, TestPlan, XtaskError,
        clear_current_stress_report, contains_include_macro, listed_test_count, module_path_value,
        parse_compiled_test_list, parse_plan_profile, parse_product_profile,
        prepare_repetition_reports, preserve_repetition_report, product_test_jobs_for_os, run_plan,
        stress_repetitions, workspace_root,
    };

    #[test]
    fn syntax_macro_policy_ignores_text_and_catches_token_formatting() {
        let harmless = syn::parse_file(
            r##"
                /// Do not restore an `include!("legacy.rs")` organization hack.
                const POLICY: &str = "include!(\"legacy.rs\")";
            "##,
        )
        .expect("harmless syntax");
        assert!(!contains_include_macro(&harmless));

        let invocation = syn::parse_file(
            r#"
                include!
                (
                    "owner.rs"
                );
            "#,
        )
        .expect("formatted include syntax");
        assert!(contains_include_macro(&invocation));
    }

    #[test]
    fn module_path_and_compiled_inventory_parsers_are_structural() {
        let syntax = syn::parse_file("#[path = \"owners/services.rs\"] mod services;")
            .expect("module syntax");
        let syn::Item::Mod(module) = &syntax.items[0] else {
            panic!("expected module");
        };
        let path_attribute = module
            .attrs
            .iter()
            .find(|attribute| attribute.path().is_ident("path"))
            .expect("path attribute");
        assert_eq!(
            module_path_value(path_attribute).expect("path value"),
            std::path::PathBuf::from("owners/services.rs")
        );
        assert_eq!(
            parse_compiled_test_list(
                "ordinary::case: test\nmanual::profile: test\n0 tests, 0 benchmarks\n"
            ),
            vec!["ordinary::case", "manual::profile"]
        );
    }

    #[test]
    fn product_plan_uses_one_locked_scheduler_for_the_complete_graph() {
        let plan = TestPlan::product(CI_NEXTEST_PROFILE);
        assert_eq!(plan.commands.len(), 1);
        let command = &plan.commands[0];
        assert!(command.contains(&"--locked".to_owned()));
        assert!(command.contains(&"--workspace".to_owned()));
        assert!(
            command
                .windows(2)
                .any(|args| args == ["--exclude", BENCHMARKS])
        );
        assert!(
            command
                .windows(2)
                .any(|args| args == ["--profile", CI_NEXTEST_PROFILE])
        );
        assert!(
            command
                .windows(2)
                .any(|args| args == ["-j", product_test_jobs_for_os(std::env::consts::OS)])
        );
        assert!(!command.contains(&"--skip".to_owned()));
        assert!(!command.contains(&"--lib".to_owned()));
        assert!(!command.contains(&"--test".to_owned()));
    }

    #[test]
    fn product_scheduler_has_one_platform_global_bound() {
        assert_eq!(product_test_jobs_for_os("linux"), "4");
        assert_eq!(product_test_jobs_for_os("macos"), "3");
        assert_eq!(product_test_jobs_for_os("windows"), "2");
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
    fn sequential_plan_runner_executes_its_bounded_plan() {
        let command = vec!["cargo".to_owned(), "--version".to_owned()];
        let plan = TestPlan {
            owner: "test plan",
            commands: vec![command],
            repetitions: 1,
            preserve_repetition_reports: false,
        };
        run_plan(&workspace_root(), plan).expect("sequential plan");
    }

    #[test]
    fn sequential_plan_runner_preserves_a_command_failure() {
        let plan = TestPlan {
            owner: "test plan",
            commands: vec![vec![
                "cargo".to_owned(),
                "--invalid-product-test-flag".to_owned(),
            ]],
            repetitions: 1,
            preserve_repetition_reports: false,
        };
        let error = run_plan(&workspace_root(), plan).expect_err("failed command passed");
        match error {
            XtaskError::CommandFailed { command, code } => {
                assert!(command.contains("--invalid-product-test-flag"));
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
                .any(|args| args == ["--profile", TIMING_NEXTEST_PROFILE])
        );
    }

    #[test]
    fn stress_selects_only_lifecycle_process_tests_with_its_named_profile() {
        let plan = TestPlan::stress_with_repetitions(2);
        assert_eq!(plan.repetitions, 2);
        let command = &plan.commands[0];
        assert!(
            command
                .windows(2)
                .any(|args| args == ["--profile", STRESS_NEXTEST_PROFILE])
        );
        assert!(
            command
                .windows(2)
                .any(|args| args == ["--filterset", "test(/^process::mcp_lifecycle_/)"])
        );
    }

    #[test]
    fn stress_repetition_reports_are_bounded_preserved_and_reset() {
        let root = tempfile::tempdir().expect("workspace");
        let stress = root.path().join("target/nextest/stress");
        std::fs::create_dir_all(&stress).expect("stress directory");
        std::fs::write(stress.join("junit.xml"), "<testsuites />").expect("JUnit report");

        prepare_repetition_reports(root.path()).expect("prepare reports");
        preserve_repetition_report(root.path(), 1).expect("preserve report");
        let report = stress.join("repetitions/junit-001.xml");
        assert_eq!(std::fs::read_to_string(&report).unwrap(), "<testsuites />");

        clear_current_stress_report(root.path()).expect("clear current report");
        assert!(!stress.join("junit.xml").exists());
        assert!(preserve_repetition_report(root.path(), 2).is_err());
        std::fs::write(stress.join("junit.xml"), "<testsuites tests=\"2\" />")
            .expect("fresh JUnit report");
        preserve_repetition_report(root.path(), 2).expect("preserve fresh report");

        prepare_repetition_reports(root.path()).expect("reset reports");
        assert!(!report.exists());
        assert_eq!(stress_repetitions("1").unwrap(), 1);
        assert_eq!(stress_repetitions("100").unwrap(), 100);
        assert!(stress_repetitions("0").is_err());
        assert!(stress_repetitions("101").is_err());
    }

    #[test]
    fn stress_repetition_report_rejects_oversized_sources() {
        let root = tempfile::tempdir().expect("workspace");
        prepare_repetition_reports(root.path()).expect("prepare reports");
        let source = root.path().join("target/nextest/stress/junit.xml");
        let file = std::fs::File::create(&source).expect("JUnit report");
        file.set_len(MAX_NEXTEST_JUNIT_BYTES + 1)
            .expect("oversized sparse report");

        assert!(preserve_repetition_report(root.path(), 1).is_err());
        assert!(
            !root
                .path()
                .join("target/nextest/stress/repetitions/junit-001.xml")
                .exists()
        );
    }

    #[cfg(unix)]
    #[test]
    fn stress_repetition_report_rejects_symlinked_ancestors() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside directory");
        std::fs::write(outside.path().join("sentinel"), "unchanged").expect("sentinel");
        symlink(outside.path(), root.path().join("target")).expect("target symlink");

        assert!(prepare_repetition_reports(root.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("sentinel")).unwrap(),
            "unchanged"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stress_repetition_report_never_follows_a_destination_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("workspace");
        let outside = tempfile::NamedTempFile::new().expect("outside report");
        std::fs::write(outside.path(), "unchanged").expect("outside contents");
        prepare_repetition_reports(root.path()).expect("prepare reports");
        let stress = root.path().join("target/nextest/stress");
        std::fs::write(stress.join("junit.xml"), "<testsuites />").expect("JUnit report");
        symlink(outside.path(), stress.join("repetitions/junit-001.xml"))
            .expect("destination symlink");

        assert!(preserve_repetition_report(root.path(), 1).is_err());
        assert_eq!(
            std::fs::read_to_string(outside.path()).unwrap(),
            "unchanged"
        );
    }

    #[test]
    fn product_and_dry_run_profiles_are_explicit_and_bounded() {
        assert_eq!(parse_product_profile(&[]).unwrap(), LOCAL_NEXTEST_PROFILE);
        assert_eq!(
            parse_product_profile(&["--profile".to_owned(), "ci".to_owned()]).unwrap(),
            CI_NEXTEST_PROFILE
        );
        assert!(parse_product_profile(&["--parallel".to_owned()]).is_err());
        assert_eq!(
            parse_plan_profile(&[
                "--dry-run".to_owned(),
                "--profile".to_owned(),
                "ci".to_owned()
            ])
            .unwrap(),
            CI_NEXTEST_PROFILE
        );
    }
}
