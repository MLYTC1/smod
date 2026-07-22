//! `smod publish` — publish the current package.
//!
//! Intentionally unimplemented today: it fails clearly with a non-zero exit
//! code rather than silently doing nothing (see `ARCHITECTURE.md`).

use clap::Args;

/// Arguments for `smod publish`.
#[derive(Args, Debug)]
pub struct PublishArgs {
    /// Perform a dry run without uploading. (Reserved for the real impl.)
    #[arg(long)]
    pub dry_run: bool,
}

/// Entry point for `smod publish`.
pub async fn run(_args: PublishArgs) -> anyhow::Result<()> {
    anyhow::bail!(
        "`smod publish` is not implemented yet — publishing requires a real registry backend \
         (see the HTTP section of ARCHITECTURE.md)"
    )
}
