#[derive(Debug)]
struct SetupEnvironment {
    home: PathBuf,
    runtime_root: PathBuf,
    native_executable: PathBuf,
    launcher: McpLauncher,
    interactive: bool,
    persistent_cli: bool,
}

fn npx_resolved_from_local_project(executable: &Path, current_directory: &Path) -> bool {
    current_directory
        .ancestors()
        .any(|ancestor| executable.starts_with(ancestor.join("node_modules").join("leantoken")))
}

fn require_current_npx_setup(current: &str, latest: Option<&str>) -> Result<()> {
    let Some(latest) = latest else {
        return Err(Error::InvalidRequest(
            "could not verify the locally resolved npx release; retry online or pass \
             --allow-outdated for an intentional pinned setup"
                .into(),
        ));
    };
    match crate::upgrade::version_update_available(current, latest) {
        Some(false) => Ok(()),
        Some(true) => Err(Error::InvalidRequest(format!(
            "npx resolved stale local LeanToken v{current}; latest is v{latest}. Run \
             `npx --yes leantoken@latest setup`, or pass --allow-outdated for an intentional \
             rollback"
        ))),
        None => Err(Error::InvalidRequest(format!(
            "could not compare locally resolved LeanToken v{current} with npm v{latest}; pass \
             --allow-outdated only for an intentional pinned setup"
        ))),
    }
}

fn setup_runtime_root(home: &Path) -> PathBuf {
    let data_local = ProjectDirs::from("dev", "LeanToken", "leantoken")
        .map(|directories| directories.data_local_dir().to_path_buf());
    setup_runtime_root_from(home, data_local.as_deref())
}

fn setup_runtime_root_from(home: &Path, data_local: Option<&Path>) -> PathBuf {
    data_local
        .map_or_else(
            || home.join(".local").join("share").join("leantoken"),
            Path::to_path_buf,
        )
        .join("runtimes")
}

fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or({
            #[cfg(windows)]
            {
                std::env::var_os("USERPROFILE")
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
            }
            #[cfg(not(windows))]
            {
                None
            }
        })
        .or_else(|| BaseDirs::new().map(|directories| directories.home_dir().to_path_buf()))
}

pub(crate) fn diagnostic_state() -> SetupDiagnostic {
    let Some(home) = home_directory() else {
        return SetupDiagnostic {
            registration_status: "unknown",
            configured_clients: Vec::new(),
            discovery_status: "unknown",
            discovery_paths: Vec::new(),
        };
    };
    let configured =
        McpLauncher::current().and_then(|launcher| configured_clients(&home, &launcher));
    let (registration_status, configured_clients) = match configured {
        Ok(clients) if clients.is_empty() => ("not_registered", clients),
        Ok(clients) => ("registered", clients),
        Err(_) => ("unknown", Vec::new()),
    };
    let discovery_paths = [
        home.join(".agents/skills/leantoken/SKILL.md"),
        home.join(".claude/skills/leantoken/SKILL.md"),
    ]
    .into_iter()
    .filter(|path| {
        read_optional(path)
            .ok()
            .flatten()
            .is_some_and(|content| content.contains(DISCOVERY_SKILL_MARKER))
    })
    .collect::<Vec<_>>();
    SetupDiagnostic {
        registration_status,
        configured_clients,
        discovery_status: match discovery_paths.len() {
            0 => "missing",
            2 => "installed",
            _ => "partial",
        },
        discovery_paths,
    }
}

fn configured_clients(home: &Path, launcher: &McpLauncher) -> Result<Vec<SetupClient>> {
    SetupClient::ALL
        .into_iter()
        .filter_map(|client| {
            let resolved = resolve_client_edit(SetupOperation::Remove, client, &[], home, launcher);
            match resolved {
                Ok(edit) if matches!(edit.status, EditStatus::Removed) => Some(Ok(client)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect()
}
