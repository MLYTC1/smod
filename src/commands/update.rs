//! `smod update` — update installed packages.
//!
//! Intentionally unimplemented today: it fails clearly with a non-zero exit
//! code rather than silently doing nothing. When implemented, this is expected
//! to reuse `installer::remove_package` and `Installer::install_one` (see
//! `ARCHITECTURE.md`).

use clap::Args;

/// Arguments for `smod update`.
#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// A specific package to update. If omitted, all packages are updated.
    pub package: Option<String>,
}

/// Entry point for `smod update`.
pub async fn run(_args: UpdateArgs) -> anyhow::Result<()> {
    anyhow::bail!(
        "`smod update` is not implemented yet — it needs version-diff detection against the \
         lockfile before it can reinstall changed dependencies (see ARCHITECTURE.md)"
    )
}
