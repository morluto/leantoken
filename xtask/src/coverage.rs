use serde::{Deserialize, Serialize};
use std::path::Path;
use std::{collections::BTreeMap, fs};

const POLICY: &str = "ci/coverage-policy.json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    schema_version: u32,
    rationale: String,
    files: BTreeMap<String, Budget>,
    #[serde(default)]
    declarations: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Budget {
    owner: String,
    minimum_lines: f64,
    minimum_functions: f64,
    minimum_regions: f64,
    exception: Option<String>,
}

#[derive(Default, Serialize)]
struct Metric {
    covered: u64,
    count: u64,
}

pub(super) fn check_policy(root: &Path) -> Result<(), String> {
    policy(root).map(|_| ())
}

fn policy(root: &Path) -> Result<Policy, String> {
    let policy: Policy =
        serde_json::from_slice(&fs::read(root.join(POLICY)).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if policy.schema_version != 1 || policy.rationale.trim().is_empty() || policy.files.is_empty() {
        return Err(
            "coverage policy requires a version, baseline rationale and file budgets".into(),
        );
    }
    for (path, budget) in &policy.files {
        if !path.starts_with("src/")
            || !path.ends_with(".rs")
            || Path::new(path)
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            || !root.join(path).is_file()
            || budget.owner.trim().is_empty()
        {
            return Err(format!("invalid coverage policy path or owner: {path}"));
        }
        for value in [
            budget.minimum_lines,
            budget.minimum_functions,
            budget.minimum_regions,
        ] {
            if !value.is_finite() || !(0.0..=100.0).contains(&value) {
                return Err(format!("invalid coverage budget: {path}"));
            }
        }
        if budget.minimum_lines <= 30.0
            && budget
                .exception
                .as_ref()
                .is_none_or(|reason| reason.trim().is_empty())
        {
            return Err(format!(
                "low coverage requires an explicit reviewed gap: {path}"
            ));
        }
    }
    let inventory = source_inventory(root)?;
    for (path, test_only) in &inventory {
        if *test_only {
            continue;
        }
        if !policy.files.contains_key(path) && !policy.declarations.contains_key(path) {
            return Err(format!(
                "production source lacks a coverage disposition: {path}"
            ));
        }
    }
    for (path, reason) in &policy.declarations {
        if reason.trim().is_empty()
            || inventory.get(path) != Some(&false)
            || policy.files.contains_key(path)
        {
            return Err(format!(
                "invalid declaration-only coverage disposition: {path}"
            ));
        }
        let syntax =
            syn::parse_file(&fs::read_to_string(root.join(path)).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        struct Executable(bool);
        impl<'ast> syn::visit::Visit<'ast> for Executable {
            fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
                if !test_module(node) {
                    syn::visit::visit_item_mod(self, node);
                }
            }
            fn visit_item_fn(&mut self, _: &'ast syn::ItemFn) {
                self.0 = true;
            }
            fn visit_impl_item_fn(&mut self, _: &'ast syn::ImplItemFn) {
                self.0 = true;
            }
            fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
                self.0 |= node.default.is_some();
            }
            fn visit_item_macro(&mut self, _: &'ast syn::ItemMacro) {
                self.0 = true;
            }
            fn visit_expr_closure(&mut self, _: &'ast syn::ExprClosure) {
                self.0 = true;
            }
        }
        let mut executable = Executable(false);
        syn::visit::Visit::visit_file(&mut executable, &syntax);
        if executable.0 {
            return Err(format!(
                "declaration-only source gained executable logic: {path}"
            ));
        }
    }
    Ok(policy)
}

fn test_module(module: &syn::ItemMod) -> bool {
    fn requires_test(meta: &syn::Meta) -> bool {
        match meta {
            syn::Meta::Path(path) => path.is_ident("test"),
            syn::Meta::List(list) if list.path.is_ident("all") || list.path.is_ident("any") => {
                let Ok(conditions) = list.parse_args_with(
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                ) else {
                    return false;
                };
                if list.path.is_ident("all") {
                    conditions.iter().any(requires_test)
                } else {
                    !conditions.is_empty() && conditions.iter().all(requires_test)
                }
            }
            _ => false,
        }
    }
    module.attrs.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<syn::Meta>()
                .is_ok_and(|meta| requires_test(&meta))
    })
}

fn source_inventory(root: &Path) -> Result<BTreeMap<String, bool>, String> {
    fn visit(
        root: &Path,
        file: &Path,
        test_only: bool,
        root_module: bool,
        inventory: &mut BTreeMap<String, bool>,
    ) -> Result<(), String> {
        let path = file
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        if inventory
            .get(&path)
            .is_some_and(|previous| !*previous || test_only)
        {
            return Ok(());
        }
        inventory.insert(path, test_only);
        let syntax = syn::parse_file(&fs::read_to_string(file).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let parent = file.parent().ok_or("source has no parent")?;
        let directory = if root_module || file.file_name().is_some_and(|name| name == "mod.rs") {
            parent.to_path_buf()
        } else {
            parent.join(file.file_stem().ok_or("source has no stem")?)
        };
        modules(root, &syntax.items, &directory, test_only, inventory)
    }
    fn modules(
        root: &Path,
        items: &[syn::Item],
        directory: &Path,
        test_only: bool,
        inventory: &mut BTreeMap<String, bool>,
    ) -> Result<(), String> {
        for item in items {
            let syn::Item::Mod(module) = item else {
                continue;
            };
            let test_only = test_only || test_module(module);
            if let Some((_, items)) = &module.content {
                modules(
                    root,
                    items,
                    &directory.join(module.ident.to_string()),
                    test_only,
                    inventory,
                )?;
            } else {
                let file =
                    super::resolve_external_module(directory, module).map_err(|e| e.to_string())?;
                visit(root, &file, test_only, false, inventory)?;
            }
        }
        Ok(())
    }
    let mut inventory = BTreeMap::new();
    for name in ["src/lib.rs", "src/main.rs"] {
        let path = root.join(name);
        if path.is_file() {
            visit(root, &path, false, true, &mut inventory)?;
        }
    }
    let mut files = Vec::new();
    super::collect_rust_files(&root.join("src"), &mut files).map_err(|e| e.to_string())?;
    for file in files {
        let path = file
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        inventory.entry(path).or_insert(false);
    }
    Ok(inventory)
}

fn assess(root: &Path, report: &Path) -> Result<(), String> {
    let policy = policy(root)?;
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(report).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let data = report["data"]
        .as_array()
        .filter(|data| data.len() == 1)
        .ok_or("coverage must contain exactly one merged dataset")?;
    let files = data[0]["files"]
        .as_array()
        .ok_or("missing coverage files")?;
    let mut seen = std::collections::BTreeSet::new();
    let mut owners: BTreeMap<String, BTreeMap<String, Metric>> = BTreeMap::new();
    let mut failures = Vec::new();
    for file in files {
        let filename = file["filename"]
            .as_str()
            .ok_or("missing coverage filename")?;
        let Ok(relative) = Path::new(filename).strip_prefix(root) else {
            continue;
        };
        let path = relative.to_string_lossy().replace('\\', "/");
        if !path.starts_with("src/") {
            continue;
        }
        if !seen.insert(path.clone()) {
            return Err(format!("duplicate coverage file: {path}"));
        }
        let Some(budget) = policy.files.get(&path) else {
            failures.push(format!(
                "new production file needs an explicit coverage budget: {path}"
            ));
            continue;
        };
        for (name, minimum) in [
            ("lines", budget.minimum_lines),
            ("functions", budget.minimum_functions),
            ("regions", budget.minimum_regions),
        ] {
            let metric = &file["summary"][name];
            let count = metric["count"].as_u64().ok_or("missing coverage count")?;
            let covered = metric["covered"]
                .as_u64()
                .filter(|value| *value <= count)
                .ok_or("invalid covered count")?;
            let total = owners
                .entry(budget.owner.clone())
                .or_default()
                .entry(name.into())
                .or_default();
            total.count = total
                .count
                .checked_add(count)
                .ok_or("owner coverage count overflow")?;
            total.covered = total
                .covered
                .checked_add(covered)
                .ok_or("owner covered count overflow")?;
            let percent = if count == 0 {
                100.0
            } else {
                covered as f64 * 100.0 / count as f64
            };
            if percent < minimum {
                failures.push(format!("{path}: {name} {percent:.2}% below {minimum:.2}%"));
            }
        }
    }
    for path in policy.files.keys() {
        if !seen.contains(path) {
            failures.push(format!("missing production coverage: {path}"));
        }
    }
    let reviewed_gaps = policy.files.iter().filter_map(|(path, budget)| budget.exception.as_ref().map(|reason|
        (path, serde_json::json!({"owner": budget.owner, "reason": reason, "minimum_lines": budget.minimum_lines})))).collect::<BTreeMap<_, _>>();
    let summary = serde_json::json!({
        "owners": owners,
        "branch_coverage": "unavailable on the pinned stable toolchain; no branch claim",
        "excluded_production_files": [],
        "declaration_only_sources": policy.declarations,
        "reviewed_gaps": reviewed_gaps,
        "failures": failures,
    });
    fs::write(
        root.join("target/coverage/owners.json"),
        serde_json::to_vec_pretty(&summary).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

/// Instrument the authoritative product plan without changing its scheduler.
fn command(filter: Option<&str>) -> Vec<String> {
    let plan = super::TestPlan::product(super::CI_NEXTEST_PROFILE);
    let mut command = vec!["cargo".into(), "llvm-cov".into(), "nextest".into()];
    command.extend(plan.commands[0].iter().skip(3).cloned());
    command.extend(["--no-report", "--success-output", "immediate"].map(str::to_owned));
    if let Some(filter) = filter {
        command.extend(["--filterset".into(), filter.into()]);
    }
    command
}

pub(super) fn run(root: &Path, args: Vec<String>) -> Result<(), String> {
    if args.as_slice() == ["check-policy"] {
        return check_policy(root);
    }
    if let [operation, report] = args.as_slice()
        && operation == "assess"
    {
        fs::create_dir_all(root.join("target/coverage")).map_err(|e| e.to_string())?;
        return assess(root, Path::new(report));
    }
    let (dry_run, filter) = match args.as_slice() {
        [] => (false, None),
        [flag] if flag == "--dry-run" => (true, None),
        [flag, filter] if flag == "--filterset" => (false, Some(filter.as_str())),
        [dry, flag, filter] if dry == "--dry-run" && flag == "--filterset" => {
            (true, Some(filter.as_str()))
        }
        _ => return Err("usage: cargo xtask coverage [--dry-run] [--filterset FILTER]".into()),
    };
    let command = command(filter);
    if dry_run {
        println!("{}", command.join(" "));
        return Ok(());
    }
    let output = root.join("target/coverage");
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    for name in [
        "identity.json",
        "run.json",
        "owners.json",
        "coverage.json",
        "coverage-policy.json",
        "clean.json",
        "clean.stdout.log",
        "clean.stderr.log",
        "tests.json",
        "tests.stdout.log",
        "tests.stderr.log",
        "report.json",
        "report.stdout.log",
        "report.stderr.log",
    ] {
        match fs::remove_file(output.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    fs::write(
        output.join("run.json"),
        r#"{"product_coverage_valid":false,"diagnostics_complete":false}"#,
    )
    .map_err(|e| e.to_string())?;
    check_policy(root)?;
    let source_identity = source_identity(root)?;
    fs::copy(root.join(POLICY), output.join("coverage-policy.json")).map_err(|e| e.to_string())?;
    let mut identity = serde_json::json!({
        "test_command": command,
        "filtered_run": filter,
        "source_blake3": source_identity,
        "rustflags": std::env::var("RUSTFLAGS").unwrap_or_default(),
        "encoded_rustflags": std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default(),
        "subprocesses": "LLVM_PROFILE_FILE inherited by the hermetic process harness; abrupt kills may not flush profiles; lifecycle assertions remain composition evidence",
        "topology_blake3": blake3::hash(&fs::read(root.join("ci/test-topology.json")).map_err(|e| e.to_string())?).to_hex().to_string(),
        "policy_blake3": blake3::hash(&fs::read(root.join(POLICY)).map_err(|e| e.to_string())?).to_hex().to_string(),
    });
    for (key, program, arguments) in [
        ("revision", "git", vec!["rev-parse", "HEAD"]),
        ("rustc", "rustc", vec!["-vV"]),
        ("llvm_cov", "cargo", vec!["llvm-cov", "--version"]),
        ("nextest", "cargo", vec!["nextest", "--version"]),
    ] {
        let result = std::process::Command::new(program)
            .args(arguments)
            .current_dir(root)
            .output()
            .map_err(|e| e.to_string())?;
        if !result.status.success() {
            return Err(format!("cannot record {key}"));
        }
        identity[key] = String::from_utf8_lossy(&result.stdout).trim().into();
    }
    fs::write(
        output.join("identity.json"),
        serde_json::to_vec_pretty(&identity).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let clean = ["cargo", "llvm-cov", "clean", "--profraw-only"].map(str::to_owned);
    logged(root, &clean, "clean")?;
    let tests = logged(root, &command, "tests");
    let report_path = output.join("coverage.json");
    let report = vec![
        "cargo".into(),
        "llvm-cov".into(),
        "report".into(),
        "--verbose".into(),
        "--json".into(),
        "--failure-mode".into(),
        "any".into(),
        "--output-path".into(),
        report_path.to_string_lossy().into_owned(),
    ];
    logged(root, &report, "report")?;
    tests?;
    if self::source_identity(root)? != source_identity {
        return Err("coverage source inputs changed during execution; evidence is invalid".into());
    }
    // Focused coverage is diagnostic evidence; it must not update or satisfy a
    // complete product baseline.
    if filter.is_none() {
        assess(root, &report_path)?;
    }
    fs::write(output.join("run.json"), serde_json::json!({"product_coverage_valid": filter.is_none(), "diagnostics_complete": true}).to_string()).map_err(|e| e.to_string())?;
    Ok(())
}

fn source_identity(root: &Path) -> Result<String, String> {
    let listing = std::process::Command::new("git")
        .args(["ls-files", "-co", "--exclude-standard", "-z"])
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    if !listing.status.success() || listing.stdout.len() > 8 * 1024 * 1024 {
        return Err("cannot capture bounded coverage source inventory".into());
    }
    let mut paths = std::collections::BTreeSet::new();
    for path in listing
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path).map_err(|e| e.to_string())?;
        if [
            "src/", "tests/", "crates/", "xtask/", ".cargo/", ".config/", "ci/", "Cargo.",
        ]
        .iter()
        .any(|prefix| path.starts_with(prefix))
        {
            paths.insert(path);
        }
    }
    let mut hash = blake3::Hasher::new();
    let mut total = 0u64;
    for path in paths {
        let source = root.join(path);
        // Deleted tracked files participate as an explicit absent input.
        if !source.exists() {
            hash.update(path.as_bytes());
            hash.update(b"\0absent\0");
            continue;
        }
        let metadata = fs::symlink_metadata(&source).map_err(|e| e.to_string())?;
        if !metadata.is_file() {
            return Err(format!("coverage source is not a regular file: {path}"));
        }
        total = total
            .checked_add(metadata.len())
            .ok_or("coverage source byte overflow")?;
        if total > 128 * 1024 * 1024 {
            return Err("coverage source inventory exceeds 128 MiB".into());
        }
        let content = fs::read(source).map_err(|e| e.to_string())?;
        hash.update(&(path.len() as u64).to_le_bytes());
        hash.update(path.as_bytes());
        hash.update(&(content.len() as u64).to_le_bytes());
        hash.update(&content);
    }
    Ok(hash.finalize().to_hex().to_string())
}

fn logged(root: &Path, command: &[String], name: &str) -> Result<(), String> {
    let output = root.join("target/coverage");
    let stderr_path = output.join(format!("{name}.stderr.log"));
    let started = std::time::Instant::now();
    println!(
        "==> {} (logs: target/coverage/{name}.*.log)",
        command.join(" ")
    );
    let status = std::process::Command::new(&command[0])
        .args(&command[1..])
        .current_dir(root)
        .env("CARGO_TERM_COLOR", "never")
        .env("NO_COLOR", "1")
        .stdout(
            fs::File::create(output.join(format!("{name}.stdout.log")))
                .map_err(|e| e.to_string())?,
        )
        .stderr(fs::File::create(&stderr_path).map_err(|e| e.to_string())?)
        .status()
        .map_err(|e| e.to_string())?;
    fs::write(output.join(format!("{name}.json")), serde_json::json!({"seconds": started.elapsed().as_secs_f64(), "success": status.success(), "exit_code": status.code()}).to_string()).map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!(
            "{name} failed: {status}; see {}",
            stderr_path.display()
        ));
    }
    for path in [&stderr_path, &output.join(format!("{name}.stdout.log"))] {
        let diagnostics = fs::read_to_string(path).map_err(|e| e.to_string())?;
        if invalid_profile_diagnostics(&diagnostics) {
            return Err(format!(
                "coverage evidence is invalid: {name} profile diagnostic; see {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn invalid_profile_diagnostics(diagnostics: &str) -> bool {
    diagnostics.lines().any(|line| {
        let line = line.to_ascii_lowercase();
        line.contains("warning:")
            || line.contains("mismatched data")
            || line.contains("llvm profile error:")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_diagnostic_child() {
        match std::env::var("LEANTOKEN_COVERAGE_DIAGNOSTIC_TEST").as_deref() {
            Ok("stderr") => eprintln!("LLVM Profile Warning: profile write failed"),
            Ok("stdout") => println!("LLVM Profile Error: profile write failed"),
            _ => {}
        }
    }

    #[test]
    fn successful_test_process_with_profile_diagnostics_is_rejected() {
        let root = fixture();
        for stream in ["stderr", "stdout"] {
            // A child process owns its environment without mutating the parallel
            // test runner's environment.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "coverage::tests::logged_diagnostic_child",
                    "--nocapture",
                ])
                .env("LEANTOKEN_COVERAGE_DIAGNOSTIC_TEST", stream)
                .env("LEANTOKEN_COVERAGE_DIAGNOSTIC_ROOT", root.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }

    #[test]
    fn logged_diagnostic_child() {
        let Some(root) = std::env::var_os("LEANTOKEN_COVERAGE_DIAGNOSTIC_ROOT") else {
            return;
        };
        let command = vec![
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            "--exact".into(),
            "coverage::tests::profile_diagnostic_child".into(),
            "--nocapture".into(),
        ];
        let root = Path::new(&root);
        let error = logged(root, &command, "tests").unwrap_err();
        assert!(error.contains("tests profile diagnostic"), "{error}");
        let phase: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("target/coverage/tests.json")).unwrap())
                .unwrap();
        assert_eq!(phase["exit_code"], 0);
    }

    #[test]
    fn report_warnings_invalidate_evidence() {
        assert!(invalid_profile_diagnostics(
            "warning: 11 functions have mismatched data"
        ));
        assert!(invalid_profile_diagnostics(
            "Warning: unknown profile integrity issue"
        ));
        assert!(!invalid_profile_diagnostics(
            "Finished report saved to coverage.json"
        ));
    }

    #[test]
    fn coverage_preserves_product_selection_and_resource_profile() {
        let plan = super::super::TestPlan::product(super::super::CI_NEXTEST_PROFILE);
        let coverage = command(None);
        assert_eq!(&coverage[3..coverage.len() - 3], &plan.commands[0][3..]);
        assert_eq!(
            &coverage[coverage.len() - 3..],
            &["--no-report", "--success-output", "immediate"]
        );
        assert!(
            coverage
                .windows(2)
                .any(|pair| pair == ["--exclude", "leantoken-benchmarks"])
        );
    }

    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for directory in ["src", "ci", "target/coverage"] {
            fs::create_dir_all(root.path().join(directory)).unwrap();
        }
        fs::write(
            root.path().join("src/critical.rs"),
            "pub fn critical() {}\n",
        )
        .unwrap();
        fs::write(
            root.path().join(POLICY),
            serde_json::json!({
                "schema_version": 1, "rationale": "reviewed test baseline",
                "files": {"src/critical.rs": {"owner": "critical", "minimum_lines": 70.0,
                    "minimum_functions": 60.0, "minimum_regions": 50.0}}
            })
            .to_string(),
        )
        .unwrap();
        root
    }

    fn report(root: &Path, covered: u64) -> std::path::PathBuf {
        let report = root.join("report.json");
        let metric = serde_json::json!({"count": 100, "covered": covered});
        fs::write(
            &report,
            serde_json::json!({"data": [{"files": [{
                "filename": root.join("src/critical.rs"),
                "summary": {"lines": metric, "functions": metric, "regions": metric}
            }]}]})
            .to_string(),
        )
        .unwrap();
        report
    }

    #[test]
    fn uncovered_critical_file_cannot_be_masked_by_aggregate() {
        let root = fixture();
        let path = report(root.path(), 90);
        assess(root.path(), &path).unwrap();
        let path = report(root.path(), 20);
        let error = assess(root.path(), &path).unwrap_err();
        assert!(error.contains("critical.rs: lines 20.00% below 70.00%"));
        assert!(root.path().join("target/coverage/owners.json").is_file());
    }

    #[test]
    fn stale_paths_and_missing_profiles_fail_closed() {
        let root = fixture();
        let path = root.path().join("report.json");
        fs::write(&path, r#"{"data":[{"files":[]}]}"#).unwrap();
        assert!(
            assess(root.path(), &path)
                .unwrap_err()
                .contains("missing production coverage")
        );
        fs::remove_file(root.path().join("src/critical.rs")).unwrap();
        assert!(
            check_policy(root.path())
                .unwrap_err()
                .contains("invalid coverage policy path")
        );
    }

    #[test]
    fn unreported_unbudgeted_source_is_rejected_independently() {
        let root = fixture();
        let path = report(root.path(), 90);
        fs::write(root.path().join("src/unreported.rs"), "pub fn lost() {}\n").unwrap();
        assert!(
            assess(root.path(), &path)
                .unwrap_err()
                .contains("production source lacks a coverage disposition: src/unreported.rs")
        );
    }

    #[test]
    fn declaration_disposition_cannot_hide_new_executable_logic() {
        let root = fixture();
        let path = root.path().join(POLICY);
        let mut policy: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        policy["declarations"] =
            serde_json::json!({"src/facade.rs": "public reexport owner; review on logic changes"});
        fs::write(path, policy.to_string()).unwrap();
        fs::write(
            root.path().join("src/facade.rs"),
            "pub use std::path::Path;\n",
        )
        .unwrap();
        check_policy(root.path()).unwrap();
        fs::write(root.path().join("src/facade.rs"), "pub fn hidden() {}\n").unwrap();
        assert!(
            check_policy(root.path())
                .unwrap_err()
                .contains("gained executable logic")
        );
    }

    #[test]
    fn failed_preflight_cannot_reuse_an_old_success_report() {
        let root = fixture();
        fs::write(
            root.path().join("target/coverage/owners.json"),
            r#"{"failures":[]}"#,
        )
        .unwrap();
        fs::remove_file(root.path().join("src/critical.rs")).unwrap();
        assert!(run(root.path(), Vec::new()).is_err());
        assert!(!root.path().join("target/coverage/owners.json").exists());
        let status: serde_json::Value = serde_json::from_slice(
            &fs::read(root.path().join("target/coverage/run.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(status["product_coverage_valid"], false);
    }
}
