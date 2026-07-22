//! `smod doctor` — diagnose the local environment.
//!
//! Intentionally unimplemented today: it fails clearly with a non-zero exit
//! code rather than silently doing nothing (see `ARCHITECTURE.md`).

use clap::Args;

/// Arguments for `smod doctor`.
#[derive(Args, Debug)]
pub struct DoctorArgs {}

/// Entry point for `smod doctor`.
pub async fn run(_args: DoctorArgs) -> anyhow::Result<()> {
    anyhow::bail!(
        "`smod doctor` is not implemented yet — environment diagnostics will live in their own \
         module when implemented (see the 'Adding a new command' checklist in ARCHITECTURE.md)"
    )
}
