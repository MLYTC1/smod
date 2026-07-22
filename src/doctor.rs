//! Environment diagnostics for `smod doctor`.
//!
//! This is business logic, not a command: it inspects a project and returns a
//! typed [`DoctorReport`] describing what it found. It never prints — rendering
//! the report (colors, symbols, exit code) is `commands::doctor`'s job, exactly
//! like every other business module.
//!
//! It lives in its own module rather than being bolted onto `config`/`lockfile`
//! because a diagnostic is inherently cross-cutting: it composes the detection
//! logic in [`crate::config`], the lockfile reader in [`crate::lockfile`], the
//! [`RegistryClient`] abstraction, and the verification workflow in
//! [`crate::installer`]. Reusing those instead of re-implementing them is what
//! keeps the checks honest and duplication-free.

use std::path::Path;

use crate::config;
use crate::installer::{Installer, VerificationStatus};
use crate::lockfile;
use crate::registry::RegistryClient;

/// Whether a single diagnostic check passed or failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// The check passed.
    Success,
    /// The check failed — the message explains why and how to fix it.
    Failure,
}

/// The result of one diagnostic check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    /// Short label for the thing being checked (e.g. `"manifest"`).
    pub name: String,
    /// Whether it passed.
    pub status: CheckStatus,
    /// A human-readable detail line.
    pub message: String,
}

impl DoctorCheck {
    fn success(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Success,
            message: message.into(),
        }
    }

    fn failure(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Failure,
            message: message.into(),
        }
    }
}

/// The full outcome of a `smod doctor` run: an ordered list of checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    /// The checks that were run, in the order they were performed.
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    /// Whether every check passed.
    pub fn is_healthy(&self) -> bool {
        self.checks.iter().all(|c| c.status == CheckStatus::Success)
    }

    /// How many checks failed.
    pub fn failure_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.status == CheckStatus::Failure)
            .count()
    }
}

/// Run the full diagnostic against the project containing `start_dir`.
///
/// Never returns `Err`: every problem is captured as a failing [`DoctorCheck`]
/// so the command can present the *whole* picture rather than aborting at the
/// first fault. The registry is passed in (rather than constructed here) so the
/// same seam that lets an `HttpRegistryClient` drop in elsewhere applies to
/// diagnostics too.
pub async fn diagnose<R: RegistryClient>(start_dir: &Path, registry: &R) -> DoctorReport {
    let mut checks = Vec::new();

    // 1. Is this (or an ancestor) a valid smod project?
    let project_root = match config::find_project_root(start_dir) {
        Some(root) => {
            checks.push(DoctorCheck::success(
                "smod project",
                format!("found at {}", root.display()),
            ));
            root
        }
        None => {
            checks.push(DoctorCheck::failure(
                "smod project",
                format!(
                    "no {} found in {} or any parent — run `smod init`",
                    config::MANIFEST_FILE,
                    start_dir.display()
                ),
            ));
            // Without a project root the remaining checks have nothing to run
            // against, so stop here with the single actionable failure.
            return DoctorReport { checks };
        }
    };

    // 2. Is smod.toml present and readable?
    match config::read_manifest(&project_root) {
        Ok(manifest) => checks.push(DoctorCheck::success(
            "manifest",
            format!(
                "{} is readable ({} dependencies declared)",
                config::MANIFEST_FILE,
                manifest.smod.dependencies.len()
            ),
        )),
        Err(err) => checks.push(DoctorCheck::failure("manifest", err.to_string())),
    }

    // 3. Is smod.lock present and valid (if it exists)?
    let lockfile_path = lockfile::lockfile_path_in(&project_root);
    if !lockfile_path.exists() {
        checks.push(DoctorCheck::success(
            "lockfile",
            format!(
                "{} not present yet (nothing installed)",
                lockfile::LOCKFILE_FILE
            ),
        ));
    } else {
        match lockfile::read(&project_root) {
            Ok(lock) => checks.push(DoctorCheck::success(
                "lockfile",
                format!(
                    "{} is valid ({} package(s) locked)",
                    lockfile::LOCKFILE_FILE,
                    lock.packages.len()
                ),
            )),
            Err(err) => checks.push(DoctorCheck::failure("lockfile", err.to_string())),
        }
    }

    // 4. Is the registry available?
    match registry.list_packages().await {
        Ok(packages) => checks.push(DoctorCheck::success(
            "registry",
            format!("available ({} package(s))", packages.len()),
        )),
        Err(err) => checks.push(DoctorCheck::failure("registry", err.to_string())),
    }

    // 5-7. Archives accessible, modules present, checksums valid.
    //
    // All three are derived from a single `Installer::verify` pass, so the
    // archive-reading and checksum logic is reused verbatim rather than
    // duplicated here.
    let installer = Installer::new(registry, project_root);
    match installer.verify().await {
        Ok(results) => {
            let missing_modules: Vec<&str> = results
                .iter()
                .filter(|r| matches!(r.status, VerificationStatus::ModuleMissing))
                .map(|r| r.name.as_str())
                .collect();
            let unavailable_archives: Vec<&str> = results
                .iter()
                .filter(|r| matches!(r.status, VerificationStatus::ArchiveUnavailable { .. }))
                .map(|r| r.name.as_str())
                .collect();
            let bad_checksums: Vec<&str> = results
                .iter()
                .filter(|r| matches!(r.status, VerificationStatus::ChecksumMismatch { .. }))
                .map(|r| r.name.as_str())
                .collect();

            checks.push(check_list(
                "installed modules",
                results.len(),
                &missing_modules,
                "all present",
                "missing module director(ies) for",
            ));
            checks.push(check_list(
                "package archives",
                results.len(),
                &unavailable_archives,
                "all accessible",
                "could not access archive(s) for",
            ));
            checks.push(check_list(
                "checksums",
                results.len(),
                &bad_checksums,
                "all match the lockfile",
                "checksum mismatch for",
            ));
        }
        Err(err) => {
            // `verify` only errors on project-level problems already surfaced
            // above (e.g. an unreadable lockfile). Record the per-package checks
            // as failures referencing the cause rather than silently dropping
            // them.
            for name in ["installed modules", "package archives", "checksums"] {
                checks.push(DoctorCheck::failure(
                    name,
                    format!("could not verify installed packages: {err}"),
                ));
            }
        }
    }

    DoctorReport { checks }
}

/// Build a check that passes when `offenders` is empty, otherwise fails and
/// lists the offending package names.
fn check_list(
    name: &str,
    total: usize,
    offenders: &[&str],
    ok_message: &str,
    fail_prefix: &str,
) -> DoctorCheck {
    if offenders.is_empty() {
        let detail = if total == 0 {
            "no packages installed".to_string()
        } else {
            format!("{total} package(s): {ok_message}")
        };
        DoctorCheck::success(name, detail)
    } else {
        DoctorCheck::failure(name, format!("{fail_prefix}: {}", offenders.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::installer::Installer;
    use crate::package::Manifest;
    use crate::registry::{MockRegistryClient, PackageInfo};
    use std::io::{Cursor, Write};
    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;

    fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts = SimpleFileOptions::default();
            for (path, contents) in entries {
                zip.start_file(*path, opts).unwrap();
                zip.write_all(contents).unwrap();
            }
            zip.finish().unwrap();
        }
        buf
    }

    fn pkg_info(name: &str, version: &str, archive: &Path) -> PackageInfo {
        PackageInfo {
            name: name.to_string(),
            version: version.to_string(),
            description: "d".into(),
            author: "a".into(),
            program_id: "p".into(),
            archive: archive.to_string_lossy().to_string(),
            checksum: None,
        }
    }

    /// Find a check by name (panicking if absent) for concise assertions.
    fn check<'a>(report: &'a DoctorReport, name: &str) -> &'a DoctorCheck {
        report
            .checks
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("missing check `{name}` in {report:?}"))
    }

    #[tokio::test]
    async fn healthy_project_passes_every_check() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        let zip = make_zip(&[("p/lib.rs", b"v1")]);
        let archive = tmp.path().join("p.zip");
        std::fs::write(&archive, &zip).unwrap();
        let registry = MockRegistryClient::from_packages(vec![pkg_info("p", "1.0.0", &archive)]);
        Installer::new(&registry, tmp.path().to_path_buf())
            .install("p")
            .await
            .unwrap();

        let report = diagnose(tmp.path(), &registry).await;
        assert!(report.is_healthy(), "expected healthy, got {report:?}");
        assert_eq!(report.failure_count(), 0);
        assert_eq!(check(&report, "checksums").status, CheckStatus::Success);
    }

    #[tokio::test]
    async fn missing_manifest_is_not_a_project() {
        let tmp = TempDir::new().unwrap();
        let registry = MockRegistryClient::from_packages(vec![]);
        let report = diagnose(tmp.path(), &registry).await;

        assert!(!report.is_healthy());
        // Not a project: the run stops at the single actionable failure.
        assert_eq!(report.checks.len(), 1);
        assert_eq!(check(&report, "smod project").status, CheckStatus::Failure);
    }

    #[tokio::test]
    async fn invalid_lockfile_is_reported() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        std::fs::write(lockfile::lockfile_path_in(tmp.path()), "not [ valid toml").unwrap();
        let registry = MockRegistryClient::from_packages(vec![]);

        let report = diagnose(tmp.path(), &registry).await;
        assert!(!report.is_healthy());
        assert_eq!(check(&report, "lockfile").status, CheckStatus::Failure);
        // The project and manifest checks still passed around it.
        assert_eq!(check(&report, "smod project").status, CheckStatus::Success);
        assert_eq!(check(&report, "manifest").status, CheckStatus::Success);
    }

    #[tokio::test]
    async fn missing_installed_module_is_reported() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        let zip = make_zip(&[("p/lib.rs", b"v1")]);
        let archive = tmp.path().join("p.zip");
        std::fs::write(&archive, &zip).unwrap();
        let registry = MockRegistryClient::from_packages(vec![pkg_info("p", "1.0.0", &archive)]);
        Installer::new(&registry, tmp.path().to_path_buf())
            .install("p")
            .await
            .unwrap();

        // Remove the extracted module dir: locked but not present on disk.
        std::fs::remove_dir_all(config::modules_dir_in(tmp.path()).join("p")).unwrap();

        let report = diagnose(tmp.path(), &registry).await;
        assert!(!report.is_healthy());
        assert_eq!(
            check(&report, "installed modules").status,
            CheckStatus::Failure
        );
    }

    #[tokio::test]
    async fn tampered_archive_fails_checksum_check() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        let zip = make_zip(&[("p/lib.rs", b"v1")]);
        let archive = tmp.path().join("p.zip");
        std::fs::write(&archive, &zip).unwrap();
        let registry = MockRegistryClient::from_packages(vec![pkg_info("p", "1.0.0", &archive)]);
        Installer::new(&registry, tmp.path().to_path_buf())
            .install("p")
            .await
            .unwrap();

        // Corrupt the archive after install.
        std::fs::write(&archive, make_zip(&[("p/lib.rs", b"tampered")])).unwrap();

        let report = diagnose(tmp.path(), &registry).await;
        assert!(!report.is_healthy());
        assert_eq!(check(&report, "checksums").status, CheckStatus::Failure);
    }

    #[tokio::test]
    async fn empty_project_is_healthy() {
        // A freshly `init`ed project with nothing installed is healthy.
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        let registry = MockRegistryClient::from_packages(vec![]);
        let report = diagnose(tmp.path(), &registry).await;
        assert!(report.is_healthy(), "got {report:?}");
    }
}
