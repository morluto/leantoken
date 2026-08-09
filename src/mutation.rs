use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MutationMode {
    DryRun,
    Apply,
}

impl MutationMode {
    pub(crate) fn parse(
        dry_run: bool,
        confirmed: bool,
        missing_confirmation: &'static str,
    ) -> Result<Self> {
        if dry_run {
            Ok(Self::DryRun)
        } else if confirmed {
            Ok(Self::Apply)
        } else {
            Err(Error::InvalidRequest(missing_confirmation.into()))
        }
    }

    pub(crate) const fn is_dry_run(self) -> bool {
        matches!(self, Self::DryRun)
    }
}
