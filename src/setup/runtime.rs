use super::*;

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
