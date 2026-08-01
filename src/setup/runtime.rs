use super::*;

/// Default number of newest unreferenced private runtimes retained by prune.
pub const DEFAULT_RUNTIME_RETENTION: usize = 2;
/// Maximum accepted retention window for one bounded prune request.
pub const MAX_RUNTIME_RETENTION: usize = 64;
const MAX_RUNTIME_ROOT_ENTRIES: usize = 512;
const MAX_RUNTIME_DIRECTORY_ENTRIES: usize = 8;

pub(super) fn runtime_install_plan(environment: &SetupEnvironment) -> Result<RuntimeInstallPlan> {
    let digest = file_digest(&environment.native_executable)?;
    let executable_name = runtime_executable_name(cfg!(windows));
    let destination = environment
        .runtime_root
        .join(environment.launcher.version())
        .join(executable_name);
    let install_required = if destination.exists() {
        let installed_digest = file_digest(&destination)?;
        if installed_digest != digest {
            return Err(Error::SetupFailure(format!(
                "private runtime identity mismatch at {}",
                destination.display()
            )));
        }
        false
    } else {
        true
    };
    Ok(RuntimeInstallPlan {
        source: environment.native_executable.clone(),
        destination,
        digest,
        install_required,
    })
}

pub(super) fn runtime_executable_name(windows: bool) -> &'static str {
    if windows {
        "leantoken.exe"
    } else {
        "leantoken"
    }
}

pub(super) fn file_digest(path: &Path) -> Result<String> {
    let mut input = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub(super) fn install_runtime(plan: &RuntimeInstallPlan) -> Result<bool> {
    if !plan.install_required {
        return Ok(false);
    }
    let parent = plan
        .destination
        .parent()
        .ok_or_else(|| Error::SetupFailure("private runtime destination has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let mut staged = NamedTempFile::new_in(parent)?;
    let mut source = fs::File::open(&plan.source)?;
    std::io::copy(&mut source, staged.as_file_mut())?;
    staged
        .as_file_mut()
        .set_permissions(source.metadata()?.permissions())?;
    staged.as_file_mut().sync_all()?;
    if file_digest(staged.path())? != plan.digest {
        return Err(Error::SetupFailure(
            "staged private runtime digest mismatch".into(),
        ));
    }
    match staged.persist_noclobber(&plan.destination) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            if file_digest(&plan.destination)? == plan.digest {
                Ok(false)
            } else {
                Err(Error::SetupFailure(format!(
                    "private runtime identity mismatch at {}",
                    plan.destination.display()
                )))
            }
        }
        Err(error) => Err(Error::Io(error.error)),
    }
}

#[derive(Debug)]
pub(super) struct RuntimeInstallPlan {
    pub(super) source: PathBuf,
    pub(super) destination: PathBuf,
    pub(super) digest: String,
    pub(super) install_required: bool,
}

#[derive(Debug)]
struct RuntimeInventoryEntry {
    report: RuntimeEntryReport,
    parsed_version: semver::Version,
    directory: PathBuf,
}

/// Inspect the bounded application-owned private-runtime inventory.
pub fn list_runtimes() -> Result<RuntimeListReport> {
    let home = home_directory()
        .ok_or_else(|| Error::SetupFailure("could not determine the home directory".into()))?;
    runtime_inventory(&home).map(|inventory| RuntimeListReport {
        runtime_root: setup_runtime_root(&home),
        total_entries: inventory.entries.len(),
        total_bytes: inventory.total_bytes,
        ignored_entries: inventory.ignored_entries,
        entries: inventory
            .entries
            .into_iter()
            .map(|entry| entry.report)
            .collect(),
    })
}

/// Plan or apply reference-safe pruning of application-owned private runtimes.
pub fn prune_runtimes(request: RuntimePruneRequest) -> Result<RuntimePruneReport> {
    if request.keep_latest > MAX_RUNTIME_RETENTION {
        return Err(Error::InvalidRequest(format!(
            "runtime keep-latest must not exceed {MAX_RUNTIME_RETENTION}"
        )));
    }
    if !request.dry_run && !request.yes {
        return Err(Error::InvalidRequest(
            "runtime prune requires --yes or --dry-run".into(),
        ));
    }
    let home = home_directory()
        .ok_or_else(|| Error::SetupFailure("could not determine the home directory".into()))?;
    let runtime_root = setup_runtime_root(&home);
    let _setup_lock = (!request.dry_run)
        .then(|| acquire_setup_lock(&runtime_root))
        .transpose()?;
    if transaction_path(&runtime_root).exists() {
        return Err(Error::SetupFailure(format!(
            "interrupted setup requires recovery before runtime pruning: {}",
            transaction_path(&runtime_root).display()
        )));
    }
    let inventory = runtime_inventory(&home)?;
    let mut unreferenced_retained = 0_usize;
    let mut total_bytes_after = inventory.total_bytes;
    let mut results = Vec::with_capacity(inventory.entries.len());
    for entry in inventory.entries {
        let protected_reason = if !entry.report.referenced_by.is_empty() {
            Some("referenced_by_client")
        } else if entry.report.active {
            Some("active_process")
        } else if !entry.report.safely_prunable {
            Some("unrecognized_directory_contents")
        } else if unreferenced_retained < request.keep_latest {
            unreferenced_retained += 1;
            Some("retention")
        } else {
            None
        };
        if let Some(reason) = protected_reason {
            results.push(RuntimePruneResult {
                version: entry.report.version,
                path: entry.report.path,
                size_bytes: entry.report.size_bytes,
                action: "retained".into(),
                reason: reason.into(),
                error: None,
            });
            continue;
        }
        if request.dry_run {
            total_bytes_after = total_bytes_after.saturating_sub(entry.report.size_bytes);
            results.push(RuntimePruneResult {
                version: entry.report.version,
                path: entry.report.path,
                size_bytes: entry.report.size_bytes,
                action: "would_remove".into(),
                reason: "outside_retention".into(),
                error: None,
            });
            continue;
        }
        match managed_runtime_directory_is_exact(&entry.directory, &entry.report.path) {
            Ok(true) => {}
            Ok(false) => {
                results.push(RuntimePruneResult {
                    version: entry.report.version,
                    path: entry.report.path,
                    size_bytes: entry.report.size_bytes,
                    action: "retained".into(),
                    reason: "directory_changed_after_inventory".into(),
                    error: None,
                });
                continue;
            }
            Err(error) => {
                results.push(RuntimePruneResult {
                    version: entry.report.version,
                    path: entry.report.path,
                    size_bytes: entry.report.size_bytes,
                    action: "failed".into(),
                    reason: "directory_revalidation_failed".into(),
                    error: Some(error.to_string()),
                });
                continue;
            }
        }
        let removal =
            fs::remove_file(&entry.report.path).and_then(|()| fs::remove_dir(&entry.directory));
        match removal {
            Ok(()) => {
                sync_parent_directory(&entry.directory)?;
                total_bytes_after = total_bytes_after.saturating_sub(entry.report.size_bytes);
                results.push(RuntimePruneResult {
                    version: entry.report.version,
                    path: entry.report.path,
                    size_bytes: entry.report.size_bytes,
                    action: "removed".into(),
                    reason: "outside_retention".into(),
                    error: None,
                });
            }
            Err(error) => results.push(RuntimePruneResult {
                version: entry.report.version,
                path: entry.report.path,
                size_bytes: entry.report.size_bytes,
                action: "failed".into(),
                reason: "outside_retention".into(),
                error: Some(error.to_string()),
            }),
        }
    }
    Ok(RuntimePruneReport {
        runtime_root,
        dry_run: request.dry_run,
        total_bytes_before: inventory.total_bytes,
        total_bytes_after,
        results,
    })
}

struct RuntimeInventory {
    entries: Vec<RuntimeInventoryEntry>,
    total_bytes: u64,
    ignored_entries: usize,
}

fn runtime_inventory(home: &Path) -> Result<RuntimeInventory> {
    let runtime_root = setup_runtime_root(home);
    let directory = match fs::read_dir(&runtime_root) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RuntimeInventory {
                entries: Vec::new(),
                total_bytes: 0,
                ignored_entries: 0,
            });
        }
        Err(error) => return Err(error.into()),
    };
    let launcher = McpLauncher::current()?;
    let registrations = configured_registrations(home, &launcher)?;
    let current_executable = std::env::current_exe()?.canonicalize()?;
    let mut entries = Vec::new();
    let mut ignored_entries = 0_usize;
    for (index, item) in directory.enumerate() {
        if index >= MAX_RUNTIME_ROOT_ENTRIES {
            return Err(Error::SetupFailure(format!(
                "private runtime root entry limit exceeded: {}",
                runtime_root.display()
            )));
        }
        let item = item?;
        let file_type = item.file_type()?;
        let Some(version_name) = item.file_name().to_str().map(str::to_owned) else {
            ignored_entries += 1;
            continue;
        };
        if version_name == "setup.lock" || item.path() == transaction_path(&runtime_root) {
            continue;
        }
        let Ok(parsed_version) = semver::Version::parse(&version_name) else {
            ignored_entries += 1;
            continue;
        };
        if !file_type.is_dir() || file_type.is_symlink() {
            ignored_entries += 1;
            continue;
        }
        let executable = item.path().join(runtime_executable_name(cfg!(windows)));
        let metadata = match fs::symlink_metadata(&executable) {
            Ok(metadata)
                if metadata.file_type().is_file() && !metadata.file_type().is_symlink() =>
            {
                metadata
            }
            Ok(_) | Err(_) => {
                ignored_entries += 1;
                continue;
            }
        };
        let safely_prunable = managed_runtime_directory_is_exact(&item.path(), &executable)?;
        let referenced_by = registrations
            .iter()
            .filter(|registration| Path::new(&registration.command) == executable)
            .map(|registration| registration.client)
            .collect::<Vec<_>>();
        let active = executable
            .canonicalize()
            .is_ok_and(|path| path == current_executable);
        entries.push(RuntimeInventoryEntry {
            report: RuntimeEntryReport {
                version: version_name,
                path: executable,
                size_bytes: metadata.len(),
                referenced_by,
                active,
                safely_prunable,
            },
            parsed_version,
            directory: item.path(),
        });
    }
    entries.sort_by(|left, right| {
        right
            .parsed_version
            .cmp(&left.parsed_version)
            .then_with(|| left.report.path.cmp(&right.report.path))
    });
    let total_bytes = entries
        .iter()
        .map(|entry| entry.report.size_bytes)
        .fold(0_u64, u64::saturating_add);
    Ok(RuntimeInventory {
        entries,
        total_bytes,
        ignored_entries,
    })
}

fn managed_runtime_directory_is_exact(directory: &Path, executable: &Path) -> Result<bool> {
    let directory_metadata = fs::symlink_metadata(directory)?;
    if !directory_metadata.file_type().is_dir() || directory_metadata.file_type().is_symlink() {
        return Ok(false);
    }
    let executable_metadata = fs::symlink_metadata(executable)?;
    if !executable_metadata.file_type().is_file() || executable_metadata.file_type().is_symlink() {
        return Ok(false);
    }
    let mut matching = false;
    for (index, entry) in fs::read_dir(directory)?.enumerate() {
        if index >= MAX_RUNTIME_DIRECTORY_ENTRIES {
            return Ok(false);
        }
        let entry = entry?;
        if entry.path() != executable || matching {
            return Ok(false);
        }
        matching = true;
    }
    Ok(matching)
}

/// Print a private-runtime inventory as JSON or concise human-readable output.
pub fn print_runtime_list(report: &RuntimeListReport, json_output: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    if json_output {
        serde_json::to_writer(&mut output, report)?;
        output.write_all(b"\n")?;
        return Ok(());
    }
    writeln!(
        output,
        "Private runtime root: {}",
        report.runtime_root.display()
    )?;
    writeln!(
        output,
        "{} runtime(s), {} bytes; {} ignored root entries",
        report.total_entries, report.total_bytes, report.ignored_entries
    )?;
    for entry in &report.entries {
        let clients = entry
            .referenced_by
            .iter()
            .map(|client| client.display_name())
            .collect::<Vec<_>>()
            .join(",");
        writeln!(
            output,
            "{}  {} bytes  {}{}  {}",
            entry.version,
            entry.size_bytes,
            if entry.active { "active" } else { "inactive" },
            if entry.safely_prunable {
                ""
            } else {
                ",unrecognized"
            },
            if clients.is_empty() {
                "unreferenced".into()
            } else {
                format!("referenced_by={clients}")
            }
        )?;
    }
    Ok(())
}

/// Print a private-runtime prune report as JSON or concise human-readable output.
pub fn print_runtime_prune(report: &RuntimePruneReport, json_output: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    if json_output {
        serde_json::to_writer(&mut output, report)?;
        output.write_all(b"\n")?;
        return Ok(());
    }
    writeln!(
        output,
        "Private runtime prune{}: {} -> {} bytes",
        if report.dry_run { " dry-run" } else { "" },
        report.total_bytes_before,
        report.total_bytes_after
    )?;
    for result in &report.results {
        writeln!(
            output,
            "{}  {}  {} bytes  {}{}",
            result.action,
            result.version,
            result.size_bytes,
            result.reason,
            result
                .error
                .as_deref()
                .map_or_else(String::new, |error| format!("  {error}"))
        )?;
    }
    Ok(())
}
