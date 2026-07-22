//! The installation / removal workflows — the heart of the business-logic
//! layer.
//!
//! [`Installer`] is generic over the registry client so tests can swap in an
//! in-memory [`MockRegistryClient`](crate::registry::MockRegistryClient)
//! pointed at a temp-directory archive. Nothing here prints; every method
//! returns a `Result` with a concrete [`thiserror`] type so callers (and tests)
//! can branch on *why* something failed.

use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::config::{self, ConfigError};
use crate::lockfile::{self, LockedPackage, LockfileError};
use crate::package::{self, PackageNameError};
use crate::registry::{PackageInfo, RegistryClient, RegistryError};

/// Errors produced while installing a package.
#[derive(Debug, Error)]
pub enum InstallError {
    /// The target directory is not a smod project.
    #[error("not a smod project: {path} (run `smod init` first)")]
    NotASmodProject { path: PathBuf },

    /// The registry lookup failed.
    #[error(transparent)]
    Registry(#[from] RegistryError),

    /// The registry returned a package whose name is unsafe as a path component.
    #[error("registry returned an unsafe package name: {0}")]
    InvalidPackageName(#[from] PackageNameError),

    /// The resolved archive path does not exist.
    #[error("package archive not found at {path}")]
    ArchiveMissing { path: PathBuf },

    /// The archive exists but could not be read.
    #[error("failed to read archive at {path}: {source}")]
    ArchiveIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The archive's checksum did not match the registry's expected value.
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    /// The archive is not a valid zip.
    #[error("invalid package archive: {message}")]
    InvalidArchive { message: String },

    /// The archive contained an entry whose path is unsafe — absolute,
    /// drive-prefixed, or escaping the destination via `..`. The whole archive
    /// is rejected rather than the entry being silently skipped.
    #[error("unsafe path in package archive: {entry:?}")]
    UnsafeArchiveEntry { entry: String },

    /// Extraction failed with an I/O error.
    #[error("failed to extract archive: {source}")]
    ExtractIo {
        #[source]
        source: std::io::Error,
    },

    /// A lockfile operation failed.
    #[error(transparent)]
    Lockfile(#[from] LockfileError),

    /// A manifest operation failed.
    #[error(transparent)]
    Manifest(#[from] ConfigError),
}

/// Errors produced while removing a package.
#[derive(Debug, Error)]
pub enum RemoveError {
    /// The target directory is not a smod project.
    #[error("not a smod project: {path} (run `smod init` first)")]
    NotASmodProject { path: PathBuf },

    /// The requested name is unsafe as a path component.
    #[error("unsafe package name: {0}")]
    InvalidPackageName(#[from] PackageNameError),

    /// The package is not installed (absent from `smod.lock`).
    #[error("package `{name}` is not installed")]
    NotInstalled { name: String },

    /// Deleting the extracted module directory failed.
    #[error("failed to remove module directory {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A lockfile operation failed.
    #[error(transparent)]
    Lockfile(#[from] LockfileError),

    /// A manifest operation failed.
    #[error(transparent)]
    Manifest(#[from] ConfigError),
}

/// A stage of the single-package install pipeline, reported to a caller-supplied
/// progress callback as each step is entered.
///
/// This is how a command renders "resolving… / verifying… / extracting… /
/// updating lockfile…" progress without the business layer ever printing:
/// [`Installer::install_with_progress`] calls the callback with each stage, and
/// the command decides how (or whether) to display it. The variants are ordered
/// to match the pipeline in `ARCHITECTURE.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStage {
    /// Looking the package up in the registry.
    Resolving,
    /// Reading the archive and verifying its checksum.
    Verifying,
    /// Extracting the archive into `smod_modules/`.
    Extracting,
    /// Recording the install in `smod.lock` and `smod.toml`.
    UpdatingLockfile,
}

/// The result of a successful single-package install.
#[derive(Debug, Clone)]
pub struct InstalledPackage {
    /// The registry metadata for the installed package.
    pub info: PackageInfo,
    /// The checksum computed over the downloaded archive.
    pub checksum: String,
    /// Where the package was extracted (`smod_modules/<name>/`).
    pub install_path: PathBuf,
}

/// The outcome of installing one dependency during a batch `install_all`.
///
/// [`Failed`](DependencyOutcome::Failed) carries the original typed
/// [`InstallError`] rather than a rendered string, so callers can still
/// `match` on the exact failure reason. Rendering it for the user is the
/// command layer's job. (This enum is therefore neither `Clone` nor `Eq`,
/// because `InstallError` wraps non-clonable sources such as `io::Error`.)
#[derive(Debug)]
pub enum DependencyOutcome {
    /// Newly installed.
    Installed { name: String, version: String },
    /// Already present in `smod.lock`, so skipped.
    AlreadyInstalled { name: String, version: String },
    /// Failed to install; the batch continued past it.
    Failed { name: String, error: InstallError },
}

/// The outcome of considering one installed package for update during
/// [`Installer::update`].
///
/// Mirrors [`DependencyOutcome`]: [`Failed`](UpdateOutcome::Failed) carries the
/// original typed [`InstallError`] (a missing registry package, a checksum
/// mismatch, a failed extraction) rather than a rendered string, so callers can
/// still `match` on the exact reason. Rendering is the command layer's job.
/// (Not `Clone`/`Eq` for the same reason `DependencyOutcome` isn't:
/// `InstallError` wraps non-clonable sources such as `io::Error`.)
#[derive(Debug)]
pub enum UpdateOutcome {
    /// The registry offered a strictly newer version and it was reinstalled.
    Updated {
        name: String,
        from: String,
        to: String,
    },
    /// The installed version already matches (or exceeds) the registry's, so
    /// nothing was reinstalled.
    UpToDate { name: String, version: String },
    /// The update could not be completed; the batch continued past it. The
    /// previous installation is left intact (reinstalls are atomic).
    Failed { name: String, error: InstallError },
}

/// The result of verifying a single installed package during
/// [`Installer::verify`].
///
/// Like [`DependencyOutcome`] and [`UpdateOutcome`], this is a typed result the
/// business layer returns and the command layer renders — no strings are
/// printed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationStatus {
    /// The recomputed archive checksum matches the lockfile.
    Verified,
    /// The archive was found but its checksum no longer matches the lockfile —
    /// the package has been corrupted or tampered with since install.
    ChecksumMismatch { expected: String, actual: String },
    /// The extracted module directory (`smod_modules/<name>/`) is missing.
    ModuleMissing,
    /// The archive could not be located or read to recompute its checksum
    /// (e.g. absent from the registry, or unreadable on disk). Carries a
    /// rendered reason since the underlying causes are heterogeneous.
    ArchiveUnavailable { reason: String },
}

/// The verification outcome for one installed (locked) package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageVerification {
    /// Package name.
    pub name: String,
    /// The version recorded in `smod.lock`.
    pub version: String,
    /// What verification found.
    pub status: VerificationStatus,
}

/// Installs and inspects packages against a given registry client and project
/// root.
pub struct Installer<'a, R: RegistryClient> {
    registry: &'a R,
    project_root: PathBuf,
}

impl<'a, R: RegistryClient> Installer<'a, R> {
    /// Create an installer bound to a registry client and a project root.
    pub fn new(registry: &'a R, project_root: PathBuf) -> Self {
        Self {
            registry,
            project_root,
        }
    }

    /// Install a single package by name (or query the registry accepts).
    ///
    /// Runs the pipeline documented in `ARCHITECTURE.md`, returning at the
    /// first failure. This is a convenience wrapper over
    /// [`install_with_progress`](Installer::install_with_progress) with a no-op
    /// progress callback — use that variant when a command wants to render
    /// per-stage progress.
    pub async fn install(&self, package_query: &str) -> Result<InstalledPackage, InstallError> {
        self.install_with_progress(package_query, &mut |_| {}).await
    }

    /// Like [`install`](Installer::install), but reports each [`InstallStage`]
    /// to `on_stage` as it is entered.
    ///
    /// The callback is the seam that lets the CLI layer show progress without
    /// this module ever printing: it is invoked with each stage, and the caller
    /// decides how to render it. Passing a no-op closure (as [`install`] does)
    /// makes this behave exactly like a plain install.
    ///
    /// [`install`]: Installer::install
    pub async fn install_with_progress(
        &self,
        package_query: &str,
        on_stage: &mut dyn FnMut(InstallStage),
    ) -> Result<InstalledPackage, InstallError> {
        // 1. Confirm we are in a smod project.
        if !config::is_smod_project(&self.project_root) {
            return Err(InstallError::NotASmodProject {
                path: self.project_root.clone(),
            });
        }

        // 2. Resolve the package via the registry.
        on_stage(InstallStage::Resolving);
        let info = self.registry.get_package(package_query).await?;

        // 2a. Reject unsafe names before any of them become a filesystem path.
        package::validate_package_name(&info.name)?;

        // 3. Resolve where the archive lives, and 4. read its bytes.
        // 5. Compute and 6. verify the checksum.
        on_stage(InstallStage::Verifying);
        let bytes = self.read_archive(&info)?;
        let checksum = Self::compute_checksum(&bytes);
        self.verify_checksum(&info, &checksum)?;

        // 7. Extract into a staging directory, then atomically swap it into
        //    place. Extraction never writes directly into the final directory,
        //    so a failed extraction can't leave partial files and a reinstall
        //    can't leave stale files from an older version.
        on_stage(InstallStage::Extracting);
        let modules_dir = config::modules_dir_in(&self.project_root);
        let install_path = modules_dir.join(&info.name);
        let staging_path = modules_dir.join(format!(".{}.tmp", info.name));
        self.install_into_place(&bytes, &staging_path, &install_path)?;

        // 8. Record in the lockfile, then 9. add to the manifest.
        on_stage(InstallStage::UpdatingLockfile);
        self.write_lockfile(&info, &checksum)?;
        config::add_dependency(&self.project_root, &info.name, &info.version)?;

        // 10. Report success.
        Ok(InstalledPackage {
            info,
            checksum,
            install_path,
        })
    }

    /// Alias for [`Installer::install`].
    ///
    /// Both names exist because callers read more clearly with one or the
    /// other: `install_all` calls `install_one` (emphasizing "one of many"),
    /// while the single-package command calls `install`. They are the same
    /// operation.
    pub async fn install_one(&self, package_query: &str) -> Result<InstalledPackage, InstallError> {
        self.install(package_query).await
    }

    /// Install every dependency declared in `smod.toml`, skipping those already
    /// present in `smod.lock`.
    ///
    /// A single dependency failing does not abort the batch — it becomes a
    /// [`DependencyOutcome::Failed`]. Only project-level problems (no
    /// `smod.toml`, an unreadable `smod.lock`) fail the whole call.
    pub async fn install_all(&self) -> Result<Vec<DependencyOutcome>, InstallError> {
        if !config::is_smod_project(&self.project_root) {
            return Err(InstallError::NotASmodProject {
                path: self.project_root.clone(),
            });
        }

        let manifest = config::read_manifest(&self.project_root)?;
        let lock = lockfile::read(&self.project_root)?;

        let mut outcomes = Vec::with_capacity(manifest.smod.dependencies.len());
        for (name, declared_version) in &manifest.smod.dependencies {
            if lock.get(name).is_some() {
                // Present in the lockfile counts as installed (no version-diff
                // check yet — see `update`).
                outcomes.push(DependencyOutcome::AlreadyInstalled {
                    name: name.clone(),
                    version: declared_version.clone(),
                });
                continue;
            }

            match self.install_one(name).await {
                Ok(installed) => outcomes.push(DependencyOutcome::Installed {
                    name: installed.info.name,
                    version: installed.info.version,
                }),
                Err(error) => outcomes.push(DependencyOutcome::Failed {
                    name: name.clone(),
                    error,
                }),
            }
        }

        Ok(outcomes)
    }

    /// Update installed packages to the newest version the registry offers.
    ///
    /// The set of candidates is read from `smod.lock` (the installed packages).
    /// With `only = Some(name)`, only that installed package is considered;
    /// with `only = None`, every locked package is. For each candidate the
    /// registry is queried through the [`RegistryClient`] trait, its version is
    /// compared against the locked one, and if the registry is strictly newer
    /// the package is reinstalled by delegating to [`install_one`] — reusing
    /// the exact verified, atomic install pipeline (checksum, zip-slip checks,
    /// staged extraction + swap, lockfile and manifest updates). No install
    /// logic is duplicated here.
    ///
    /// Like [`install_all`], a single package failing (missing from the
    /// registry, checksum mismatch, extraction error) does not abort the batch:
    /// it becomes an [`UpdateOutcome::Failed`] carrying the typed error, and the
    /// prior installation is left intact because reinstalls are atomic. Only
    /// project-level problems (no `smod.toml`, an unreadable `smod.lock`) fail
    /// the whole call.
    ///
    /// [`install_one`]: Installer::install_one
    /// [`install_all`]: Installer::install_all
    pub async fn update(&self, only: Option<&str>) -> Result<Vec<UpdateOutcome>, InstallError> {
        if !config::is_smod_project(&self.project_root) {
            return Err(InstallError::NotASmodProject {
                path: self.project_root.clone(),
            });
        }

        let lock = lockfile::read(&self.project_root)?;

        // Snapshot the (name, installed version) pairs up front. Reinstalling a
        // package mid-loop rewrites `smod.lock`, so iterating a snapshot rather
        // than the live file keeps the loop stable.
        let targets: Vec<(String, String)> = lock
            .packages
            .iter()
            .filter(|p| match only {
                Some(name) => p.name == name,
                None => true,
            })
            .map(|p| (p.name.clone(), p.version.clone()))
            .collect();

        let mut outcomes = Vec::with_capacity(targets.len());
        for (name, installed_version) in targets {
            outcomes.push(self.update_one(&name, &installed_version).await);
        }
        Ok(outcomes)
    }

    /// Consider a single installed package for update, producing an
    /// [`UpdateOutcome`]. Never aborts the batch — every failure mode is folded
    /// into [`UpdateOutcome::Failed`] with its typed error preserved.
    async fn update_one(&self, name: &str, installed_version: &str) -> UpdateOutcome {
        // Query the registry through the trait seam — never a concrete client.
        let available = match self.registry.get_package(name).await {
            Ok(info) => info,
            Err(source) => {
                return UpdateOutcome::Failed {
                    name: name.to_string(),
                    error: InstallError::Registry(source),
                };
            }
        };

        // Only a *strictly newer* registry version counts as outdated.
        if package::compare_versions(&available.version, installed_version)
            != std::cmp::Ordering::Greater
        {
            return UpdateOutcome::UpToDate {
                name: name.to_string(),
                version: installed_version.to_string(),
            };
        }

        // Outdated: reuse the full install pipeline to reinstall atomically.
        match self.install_one(name).await {
            Ok(installed) => UpdateOutcome::Updated {
                name: name.to_string(),
                from: installed_version.to_string(),
                to: installed.info.version,
            },
            Err(error) => UpdateOutcome::Failed {
                name: name.to_string(),
                error,
            },
        }
    }

    /// Verify that every installed (locked) package is intact.
    ///
    /// The set of packages is read from `smod.lock`. For each one, the
    /// extracted module directory must exist, and the archive it was installed
    /// from (located through the [`RegistryClient`] trait) must still hash to
    /// the checksum recorded in the lockfile — recomputed with the same
    /// [`compute_checksum`] used at install time, so no checksum logic is
    /// duplicated. Each package folds into a [`PackageVerification`] rather than
    /// aborting the run, so a single corrupted package does not hide the status
    /// of the rest.
    ///
    /// Note: the lockfile records the checksum of the *archive*, so this detects
    /// a changed/corrupted archive or a tampered lockfile, plus a missing module
    /// directory. Only project-level problems (not a project, an unreadable
    /// `smod.lock`) fail the whole call.
    ///
    /// [`compute_checksum`]: Installer::compute_checksum
    pub async fn verify(&self) -> Result<Vec<PackageVerification>, InstallError> {
        if !config::is_smod_project(&self.project_root) {
            return Err(InstallError::NotASmodProject {
                path: self.project_root.clone(),
            });
        }

        let lock = lockfile::read(&self.project_root)?;
        let mut results = Vec::with_capacity(lock.packages.len());
        for locked in &lock.packages {
            results.push(self.verify_one(locked).await);
        }
        Ok(results)
    }

    /// Verify a single locked package, producing a [`PackageVerification`].
    /// Never returns `Err` — every failure mode is folded into a
    /// [`VerificationStatus`] so the batch is never aborted.
    async fn verify_one(&self, locked: &LockedPackage) -> PackageVerification {
        let name = locked.name.clone();
        let version = locked.version.clone();

        // 1. The extracted module directory must be present.
        let module_dir = config::modules_dir_in(&self.project_root).join(&name);
        if !module_dir.exists() {
            return PackageVerification {
                name,
                version,
                status: VerificationStatus::ModuleMissing,
            };
        }

        // 2. Locate the archive via the registry and recompute its checksum.
        let info = match self.registry.get_package(&name).await {
            Ok(info) => info,
            Err(source) => {
                return PackageVerification {
                    name,
                    version,
                    status: VerificationStatus::ArchiveUnavailable {
                        reason: source.to_string(),
                    },
                };
            }
        };
        let bytes = match self.read_archive(&info) {
            Ok(bytes) => bytes,
            Err(err) => {
                return PackageVerification {
                    name,
                    version,
                    status: VerificationStatus::ArchiveUnavailable {
                        reason: err.to_string(),
                    },
                };
            }
        };

        // 3. Compare the recomputed checksum against the lockfile's.
        let actual = Self::compute_checksum(&bytes);
        if actual.eq_ignore_ascii_case(&locked.checksum) {
            PackageVerification {
                name,
                version,
                status: VerificationStatus::Verified,
            }
        } else {
            PackageVerification {
                name,
                version,
                status: VerificationStatus::ChecksumMismatch {
                    expected: locked.checksum.clone(),
                    actual,
                },
            }
        }
    }

    /// Resolve `PackageInfo::archive` to an absolute filesystem path.
    ///
    /// Absolute paths are used as-is (tests point these at temp-dir fixtures);
    /// relative paths are resolved against this crate's manifest directory,
    /// i.e. "next to `Cargo.toml`." This is the one pipeline step that will
    /// change shape when HTTP support lands (the path becomes a URL).
    fn resolve_archive_path(&self, archive: &str) -> PathBuf {
        let path = Path::new(archive);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
        }
    }

    /// Resolve and read a package's archive bytes, mapping I/O failures onto the
    /// typed [`InstallError`] variants callers can branch on.
    ///
    /// Extracted so the install pipeline (step 3/4) and [`verify`] share exactly
    /// one archive-reading path rather than duplicating the resolve-then-read
    /// logic. This is also the single spot that becomes an HTTP fetch when HTTP
    /// support lands (see `ARCHITECTURE.md`).
    ///
    /// [`verify`]: Installer::verify
    fn read_archive(&self, info: &PackageInfo) -> Result<Vec<u8>, InstallError> {
        let archive_path = self.resolve_archive_path(&info.archive);
        std::fs::read(&archive_path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                InstallError::ArchiveMissing {
                    path: archive_path.clone(),
                }
            } else {
                InstallError::ArchiveIo {
                    path: archive_path.clone(),
                    source,
                }
            }
        })
    }

    /// Compute the SHA-256 checksum of `bytes` as a lowercase hex string.
    pub fn compute_checksum(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    }

    /// Verify `actual` against the registry's expected checksum, if one was
    /// provided. When the registry carries no checksum, verification is a no-op
    /// (there is nothing to compare against).
    pub fn verify_checksum(&self, info: &PackageInfo, actual: &str) -> Result<(), InstallError> {
        if let Some(expected) = &info.checksum {
            if !expected.eq_ignore_ascii_case(actual) {
                return Err(InstallError::ChecksumMismatch {
                    expected: expected.clone(),
                    actual: actual.to_string(),
                });
            }
        }
        Ok(())
    }

    /// Extract a zip archive's bytes into `dest`.
    ///
    /// Security model: the archive is validated *in full before anything is
    /// written*. Every entry must have a safe relative path per
    /// `enclosed_name()` (which rejects absolute paths, drive prefixes, and
    /// `..` components — "zip slip"). A single unsafe entry rejects the whole
    /// archive with [`InstallError::UnsafeArchiveEntry`]; unsafe entries are
    /// never silently skipped, and because validation precedes the write pass,
    /// a rejected archive leaves no partially-extracted files behind.
    ///
    /// If every entry shares one common top-level directory (the conventional
    /// `payment-stream/README.md` layout), that prefix is stripped — but only
    /// when doing so would not erase a root-level file.
    pub fn extract_package(&self, bytes: &[u8], dest: &Path) -> Result<(), InstallError> {
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| InstallError::InvalidArchive {
                message: e.to_string(),
            })?;

        // First pass: validate and record every entry. An entry whose path is
        // not a safe relative path makes the whole archive invalid.
        let mut names: Vec<String> = Vec::with_capacity(archive.len());
        let mut is_dir: Vec<bool> = Vec::with_capacity(archive.len());
        for i in 0..archive.len() {
            let file = archive
                .by_index(i)
                .map_err(|e| InstallError::InvalidArchive {
                    message: e.to_string(),
                })?;
            // `None` == unsafe (absolute, drive-prefixed, or contains `..`).
            let safe = file
                .enclosed_name()
                .ok_or_else(|| InstallError::UnsafeArchiveEntry {
                    entry: file.name().to_string(),
                })?;
            names.push(safe.to_string_lossy().replace('\\', "/"));
            is_dir.push(file.is_dir());
        }

        // Compute the strippable wrapper directory from the safe, non-empty
        // names only. (An empty name is a benign current-dir/root marker that
        // resolves to `dest` itself — it must not disable stripping.)
        let entries: Vec<(String, bool)> = names
            .iter()
            .cloned()
            .zip(is_dir.iter().copied())
            .filter(|(name, _)| !name.is_empty())
            .collect();
        let strip = common_prefix(&entries);

        // Second pass: extract. Every `names[i]` here is already known safe.
        std::fs::create_dir_all(dest).map_err(|source| InstallError::ExtractIo { source })?;
        for i in 0..archive.len() {
            let name = &names[i];
            // Benign marker entry that resolves to `dest` itself; no content.
            if name.is_empty() {
                continue;
            }

            let relative = match &strip {
                Some(prefix) => match name.strip_prefix(&format!("{prefix}/")) {
                    Some(rest) if !rest.is_empty() => rest.to_string(),
                    // This is the wrapper directory entry itself; nothing to do.
                    _ => continue,
                },
                None => name.clone(),
            };

            let target = dest.join(&relative);
            let mut file = archive
                .by_index(i)
                .map_err(|e| InstallError::InvalidArchive {
                    message: e.to_string(),
                })?;
            if is_dir[i] {
                std::fs::create_dir_all(&target)
                    .map_err(|source| InstallError::ExtractIo { source })?;
            } else {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|source| InstallError::ExtractIo { source })?;
                }
                let mut buf = Vec::with_capacity(file.size() as usize);
                file.read_to_end(&mut buf)
                    .map_err(|source| InstallError::ExtractIo { source })?;
                let mut out = std::fs::File::create(&target)
                    .map_err(|source| InstallError::ExtractIo { source })?;
                out.write_all(&buf)
                    .map_err(|source| InstallError::ExtractIo { source })?;
            }
        }

        Ok(())
    }

    /// Extract `bytes` into `staging`, then atomically move it into
    /// `final_path`, making installation all-or-nothing.
    ///
    /// - Extraction always targets a fresh `staging` directory, never
    ///   `final_path` directly.
    /// - On **any** failure (extraction or swap), `staging` is removed and an
    ///   existing `final_path` is left untouched.
    /// - On success, the previous `final_path` (if any) is removed and
    ///   `staging` is renamed onto it, so stale files from an older version
    ///   never survive a reinstall.
    fn install_into_place(
        &self,
        bytes: &[u8],
        staging: &Path,
        final_path: &Path,
    ) -> Result<(), InstallError> {
        // Clear any leftover staging directory from a previously interrupted
        // run before extracting into it.
        if staging.exists() {
            std::fs::remove_dir_all(staging)
                .map_err(|source| InstallError::ExtractIo { source })?;
        }

        // Extract into the staging directory; clean it up on failure.
        if let Err(err) = self.extract_package(bytes, staging) {
            let _ = std::fs::remove_dir_all(staging);
            return Err(err);
        }

        // Swap the staging directory into its final location; clean it up on
        // failure so a botched swap doesn't leave the staging dir behind.
        if let Err(err) = Self::swap_into_place(staging, final_path) {
            let _ = std::fs::remove_dir_all(staging);
            return Err(err);
        }

        Ok(())
    }

    /// Replace `final_path` with `staging`: remove the existing final directory
    /// (if any), then rename `staging` onto it. The rename is atomic within the
    /// same directory.
    fn swap_into_place(staging: &Path, final_path: &Path) -> Result<(), InstallError> {
        if final_path.exists() {
            std::fs::remove_dir_all(final_path)
                .map_err(|source| InstallError::ExtractIo { source })?;
        }
        std::fs::rename(staging, final_path).map_err(|source| InstallError::ExtractIo { source })
    }

    /// Upsert the package into `smod.lock`.
    pub fn write_lockfile(&self, info: &PackageInfo, checksum: &str) -> Result<(), InstallError> {
        let mut lock = lockfile::read(&self.project_root)?;
        lock.upsert(LockedPackage {
            name: info.name.clone(),
            version: info.version.clone(),
            checksum: checksum.to_string(),
            installed_at: lockfile::now_rfc3339(),
        });
        lockfile::write(&self.project_root, &lock)?;
        Ok(())
    }
}

/// Remove a package: delete `smod_modules/<name>/`, drop its `smod.lock` entry,
/// and drop its `smod.toml` dependency entry.
///
/// A free function rather than a method — removal needs a project root and a
/// name, but no registry.
pub fn remove_package(project_root: &Path, name: &str) -> Result<(), RemoveError> {
    if !config::is_smod_project(project_root) {
        return Err(RemoveError::NotASmodProject {
            path: project_root.to_path_buf(),
        });
    }

    // Reject unsafe names before `name` is joined into a path to delete.
    package::validate_package_name(name)?;

    let mut lock = lockfile::read(project_root)?;
    if lock.get(name).is_none() {
        return Err(RemoveError::NotInstalled {
            name: name.to_string(),
        });
    }

    let module_dir = config::modules_dir_in(project_root).join(name);
    if module_dir.exists() {
        std::fs::remove_dir_all(&module_dir).map_err(|source| RemoveError::Io {
            path: module_dir.clone(),
            source,
        })?;
    }

    lock.remove(name);
    lockfile::write(project_root, &lock)?;
    config::remove_dependency(project_root, name)?;
    Ok(())
}

/// Determine the single common top-level directory shared by all entries, if
/// one exists and stripping it would not erase a root-level file.
fn common_prefix(entries: &[(String, bool)]) -> Option<String> {
    let mut candidate: Option<String> = None;
    for (path, is_dir) in entries {
        let first = path.split('/').next().unwrap_or("");
        if first.is_empty() {
            return None;
        }
        match &candidate {
            None => candidate = Some(first.to_string()),
            Some(c) if c != first => return None,
            _ => {}
        }
        // A root-level *file* equal to the candidate means the candidate is a
        // file, not a wrapping directory — do not strip.
        if path == first && !is_dir {
            return None;
        }
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::Manifest;
    use crate::registry::MockRegistryClient;
    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;

    /// Build a zip archive in memory. Each entry is `(path, contents)`.
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

    /// Set up a temp project with a fixture archive on disk, and a registry
    /// pointing at it. Returns `(tempdir, registry, archive_checksum)`.
    fn scaffold(
        pkg_name: &str,
        archive_bytes: &[u8],
        checksum: Option<String>,
    ) -> (TempDir, MockRegistryClient) {
        let tmp = TempDir::new().unwrap();
        // Make it a project.
        config::write_manifest(tmp.path(), &Manifest::new("host-project")).unwrap();
        // Write the archive to disk.
        let archive_path = tmp.path().join("archive.zip");
        std::fs::write(&archive_path, archive_bytes).unwrap();

        let info = PackageInfo {
            name: pkg_name.to_string(),
            version: "1.0.0".to_string(),
            description: "test package".to_string(),
            author: "tester".to_string(),
            program_id: "Prog".to_string(),
            archive: archive_path.to_string_lossy().to_string(),
            checksum,
            dependencies: Default::default(),
        };
        let registry = MockRegistryClient::from_packages(vec![info]);
        (tmp, registry)
    }

    /// Build a `PackageInfo` pointing at an on-disk archive (no checksum).
    fn pkg_info(name: &str, version: &str, archive: &Path) -> PackageInfo {
        PackageInfo {
            name: name.to_string(),
            version: version.to_string(),
            description: "d".into(),
            author: "a".into(),
            program_id: "p".into(),
            archive: archive.to_string_lossy().to_string(),
            checksum: None,
            dependencies: Default::default(),
        }
    }

    #[test]
    fn checksum_is_stable_and_hex() {
        let a = Installer::<MockRegistryClient>::compute_checksum(b"hello");
        let b = Installer::<MockRegistryClient>::compute_checksum(b"hello");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn install_extracts_updates_lock_and_manifest() {
        let zip = make_zip(&[
            ("payment-stream/README.md", b"# readme"),
            ("payment-stream/src/lib.rs", b"pub fn go() {}"),
        ]);
        let (tmp, registry) = scaffold("payment-stream", &zip, None);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());

        let installed = installer.install("payment-stream").await.unwrap();

        // Files extracted with the wrapper dir stripped.
        assert!(installed.install_path.join("README.md").is_file());
        assert!(installed.install_path.join("src/lib.rs").is_file());

        // Lockfile updated.
        let lock = lockfile::read(tmp.path()).unwrap();
        assert_eq!(lock.get("payment-stream").unwrap().version, "1.0.0");

        // Manifest updated.
        let deps = config::list_dependencies(tmp.path()).unwrap();
        assert_eq!(
            deps.get("payment-stream").map(String::as_str),
            Some("1.0.0")
        );
    }

    #[tokio::test]
    async fn install_with_progress_reports_stages_in_order() {
        let zip = make_zip(&[("p/lib.rs", b"v1")]);
        let (tmp, registry) = scaffold("p", &zip, None);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());

        let mut stages = Vec::new();
        installer
            .install_with_progress("p", &mut |stage| stages.push(stage))
            .await
            .unwrap();

        assert_eq!(
            stages,
            vec![
                InstallStage::Resolving,
                InstallStage::Verifying,
                InstallStage::Extracting,
                InstallStage::UpdatingLockfile,
            ]
        );
    }

    #[tokio::test]
    async fn install_not_a_project_errors() {
        let zip = make_zip(&[("p/f.txt", b"x")]);
        let (tmp, registry) = scaffold("p", &zip, None);
        // Remove the manifest so it is no longer a project.
        std::fs::remove_file(config::manifest_path_in(tmp.path())).unwrap();
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        let err = installer.install("p").await.unwrap_err();
        assert!(matches!(err, InstallError::NotASmodProject { .. }));
    }

    #[tokio::test]
    async fn install_unknown_package_is_registry_error() {
        let zip = make_zip(&[("p/f.txt", b"x")]);
        let (tmp, registry) = scaffold("p", &zip, None);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        let err = installer.install("does-not-exist").await.unwrap_err();
        assert!(matches!(err, InstallError::Registry(_)));
    }

    #[tokio::test]
    async fn install_missing_archive_errors() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        let info = PackageInfo {
            name: "ghost".into(),
            version: "1.0.0".into(),
            description: "d".into(),
            author: "a".into(),
            program_id: "p".into(),
            archive: tmp.path().join("nope.zip").to_string_lossy().to_string(),
            checksum: None,
            dependencies: Default::default(),
        };
        let registry = MockRegistryClient::from_packages(vec![info]);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        let err = installer.install("ghost").await.unwrap_err();
        assert!(matches!(err, InstallError::ArchiveMissing { .. }));
    }

    #[tokio::test]
    async fn install_checksum_mismatch_errors() {
        let zip = make_zip(&[("p/f.txt", b"x")]);
        let (tmp, registry) = scaffold("p", &zip, Some("00badchecksum00".into()));
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        let err = installer.install("p").await.unwrap_err();
        assert!(matches!(err, InstallError::ChecksumMismatch { .. }));
    }

    #[tokio::test]
    async fn install_matching_checksum_succeeds() {
        let zip = make_zip(&[("p/f.txt", b"x")]);
        let checksum = Installer::<MockRegistryClient>::compute_checksum(&zip);
        let (tmp, registry) = scaffold("p", &zip, Some(checksum));
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        assert!(installer.install("p").await.is_ok());
    }

    #[tokio::test]
    async fn install_invalid_archive_errors() {
        let (tmp, registry) = scaffold("p", b"not a zip at all", None);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        let err = installer.install("p").await.unwrap_err();
        assert!(matches!(err, InstallError::InvalidArchive { .. }));
    }

    #[tokio::test]
    async fn extract_leaves_root_level_files_alone() {
        // Regression: an archive with a single root-level file must not have
        // that file mistaken for a directory prefix and dropped.
        let zip = make_zip(&[("only.txt", b"content")]);
        let (tmp, registry) = scaffold("root-pkg", &zip, None);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        let installed = installer.install("root-pkg").await.unwrap();
        assert!(installed.install_path.join("only.txt").is_file());
    }

    #[tokio::test]
    async fn extract_strips_common_wrapper_dir() {
        let zip = make_zip(&[("wrapper/a.txt", b"a"), ("wrapper/nested/b.txt", b"b")]);
        let (tmp, registry) = scaffold("wrapped", &zip, None);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        let installed = installer.install("wrapped").await.unwrap();
        assert!(installed.install_path.join("a.txt").is_file());
        assert!(installed.install_path.join("nested/b.txt").is_file());
        assert!(!installed.install_path.join("wrapper").exists());
    }

    #[tokio::test]
    async fn extract_keeps_multiple_root_entries() {
        let zip = make_zip(&[("a.txt", b"a"), ("b.txt", b"b")]);
        let (tmp, registry) = scaffold("multi", &zip, None);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        let installed = installer.install("multi").await.unwrap();
        assert!(installed.install_path.join("a.txt").is_file());
        assert!(installed.install_path.join("b.txt").is_file());
    }

    // --- security: zip-slip / unsafe archive entries --------------------

    #[tokio::test]
    async fn extract_rejects_parent_dir_traversal() {
        let zip = make_zip(&[("../evil.txt", b"pwned")]);
        let (tmp, registry) = scaffold("p", &zip, None);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        let err = installer.install("p").await.unwrap_err();
        assert!(
            matches!(err, InstallError::UnsafeArchiveEntry { .. }),
            "got {err:?}"
        );
        // Nothing escaped, and nothing was partially extracted.
        assert!(!tmp.path().parent().unwrap().join("evil.txt").exists());
        assert!(!config::modules_dir_in(tmp.path()).join("p").exists());
    }

    #[tokio::test]
    async fn extract_rejects_absolute_paths() {
        let zip = make_zip(&[("/absolute/evil.txt", b"pwned")]);
        let (tmp, registry) = scaffold("p", &zip, None);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        let err = installer.install("p").await.unwrap_err();
        assert!(
            matches!(err, InstallError::UnsafeArchiveEntry { .. }),
            "got {err:?}"
        );
        assert!(!config::modules_dir_in(tmp.path()).join("p").exists());
    }

    #[tokio::test]
    async fn extract_rejects_archive_of_only_invalid_entries() {
        let zip = make_zip(&[("../a.txt", b"a"), ("../../b.txt", b"b")]);
        let (tmp, registry) = scaffold("p", &zip, None);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        let err = installer.install("p").await.unwrap_err();
        assert!(
            matches!(err, InstallError::UnsafeArchiveEntry { .. }),
            "got {err:?}"
        );
        assert!(!config::modules_dir_in(tmp.path()).join("p").exists());
    }

    #[tokio::test]
    async fn extract_rejects_mixed_valid_and_unsafe_without_partial_install() {
        // A safe entry precedes an unsafe one: the whole archive must be
        // rejected, and the safe entry must NOT have been written.
        let zip = make_zip(&[("pkg/good.txt", b"good"), ("../evil.txt", b"evil")]);
        let (tmp, registry) = scaffold("pkg", &zip, None);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        let err = installer.install("pkg").await.unwrap_err();
        assert!(
            matches!(err, InstallError::UnsafeArchiveEntry { .. }),
            "got {err:?}"
        );
        assert!(!tmp.path().parent().unwrap().join("evil.txt").exists());
        // No partial extraction of the good file.
        assert!(!config::modules_dir_in(tmp.path()).join("pkg").exists());
        // And the install was not recorded.
        assert!(lockfile::read(tmp.path()).unwrap().get("pkg").is_none());
    }

    // --- atomic installation --------------------------------------------

    #[tokio::test]
    async fn failed_extraction_leaves_no_temp_directory() {
        // "a" is a file, but "a/b" needs "a" to be a directory: extraction
        // creates the staging dir, writes "a", then fails writing "a/b".
        let zip = make_zip(&[("a", b"x"), ("a/b", b"y")]);
        let (tmp, registry) = scaffold("p", &zip, None);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());

        let err = installer.install("p").await.unwrap_err();
        assert!(matches!(err, InstallError::ExtractIo { .. }), "got {err:?}");

        let modules = config::modules_dir_in(tmp.path());
        assert!(
            !modules.join(".p.tmp").exists(),
            "staging directory must be cleaned up on failure"
        );
        assert!(
            !modules.join("p").exists(),
            "no partial final directory should exist"
        );
    }

    #[tokio::test]
    async fn failed_reinstall_does_not_corrupt_existing_installation() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();

        // Good v1 install.
        let zip1 = make_zip(&[("p/keep.txt", b"keep")]);
        let a1 = tmp.path().join("v1.zip");
        std::fs::write(&a1, &zip1).unwrap();
        let reg1 = MockRegistryClient::from_packages(vec![pkg_info("p", "1.0.0", &a1)]);
        Installer::new(&reg1, tmp.path().to_path_buf())
            .install("p")
            .await
            .unwrap();

        let install_path = config::modules_dir_in(tmp.path()).join("p");
        assert!(install_path.join("keep.txt").is_file());

        // Failing reinstall: checksum mismatch (fails before the swap).
        let zip2 = make_zip(&[("p/replace.txt", b"replace")]);
        let a2 = tmp.path().join("v2.zip");
        std::fs::write(&a2, &zip2).unwrap();
        let mut bad = pkg_info("p", "2.0.0", &a2);
        bad.checksum = Some("deadbeefdeadbeef".into());
        let reg2 = MockRegistryClient::from_packages(vec![bad]);
        let err = Installer::new(&reg2, tmp.path().to_path_buf())
            .install("p")
            .await
            .unwrap_err();
        assert!(matches!(err, InstallError::ChecksumMismatch { .. }));

        // The previous installation is completely intact.
        assert!(install_path.join("keep.txt").is_file());
        assert!(!install_path.join("replace.txt").exists());
        assert!(!config::modules_dir_in(tmp.path()).join(".p.tmp").exists());
        assert_eq!(
            lockfile::read(tmp.path())
                .unwrap()
                .get("p")
                .unwrap()
                .version,
            "1.0.0"
        );
    }

    #[tokio::test]
    async fn reinstall_removes_stale_files() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        let install_path = config::modules_dir_in(tmp.path()).join("p");

        // v1 ships old.txt + lib.rs.
        let zip1 = make_zip(&[("p/old.txt", b"old"), ("p/lib.rs", b"v1")]);
        let a1 = tmp.path().join("v1.zip");
        std::fs::write(&a1, &zip1).unwrap();
        let reg1 = MockRegistryClient::from_packages(vec![pkg_info("p", "1.0.0", &a1)]);
        Installer::new(&reg1, tmp.path().to_path_buf())
            .install("p")
            .await
            .unwrap();
        assert!(install_path.join("old.txt").is_file());

        // v2 drops old.txt and adds new.txt.
        let zip2 = make_zip(&[("p/lib.rs", b"v2"), ("p/new.txt", b"new")]);
        let a2 = tmp.path().join("v2.zip");
        std::fs::write(&a2, &zip2).unwrap();
        let reg2 = MockRegistryClient::from_packages(vec![pkg_info("p", "2.0.0", &a2)]);
        Installer::new(&reg2, tmp.path().to_path_buf())
            .install("p")
            .await
            .unwrap();

        assert!(
            !install_path.join("old.txt").exists(),
            "stale file from v1 must be gone after reinstall"
        );
        assert!(install_path.join("new.txt").is_file());
        assert!(!config::modules_dir_in(tmp.path()).join(".p.tmp").exists());
    }

    #[tokio::test]
    async fn successful_reinstall_replaces_package_contents() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        let install_path = config::modules_dir_in(tmp.path()).join("p");

        let zip1 = make_zip(&[("p/lib.rs", b"v1")]);
        let a1 = tmp.path().join("v1.zip");
        std::fs::write(&a1, &zip1).unwrap();
        let reg1 = MockRegistryClient::from_packages(vec![pkg_info("p", "1.0.0", &a1)]);
        Installer::new(&reg1, tmp.path().to_path_buf())
            .install("p")
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(install_path.join("lib.rs")).unwrap(),
            "v1"
        );

        let zip2 = make_zip(&[("p/lib.rs", b"v2")]);
        let a2 = tmp.path().join("v2.zip");
        std::fs::write(&a2, &zip2).unwrap();
        let reg2 = MockRegistryClient::from_packages(vec![pkg_info("p", "2.0.0", &a2)]);
        Installer::new(&reg2, tmp.path().to_path_buf())
            .install("p")
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(install_path.join("lib.rs")).unwrap(),
            "v2"
        );
        assert_eq!(
            lockfile::read(tmp.path())
                .unwrap()
                .get("p")
                .unwrap()
                .version,
            "2.0.0"
        );
    }

    #[tokio::test]
    async fn install_all_reports_per_dependency_outcomes() {
        // Two valid packages, declared as deps; one already locked.
        let zip_a = make_zip(&[("a/f.txt", b"a")]);
        let zip_b = make_zip(&[("b/f.txt", b"b")]);
        let tmp = TempDir::new().unwrap();
        let path_a = tmp.path().join("a.zip");
        let path_b = tmp.path().join("b.zip");
        std::fs::write(&path_a, &zip_a).unwrap();
        std::fs::write(&path_b, &zip_b).unwrap();

        let mut manifest = Manifest::new("host");
        manifest
            .smod
            .dependencies
            .insert("a".into(), "1.0.0".into());
        manifest
            .smod
            .dependencies
            .insert("b".into(), "1.0.0".into());
        manifest
            .smod
            .dependencies
            .insert("c".into(), "1.0.0".into());
        config::write_manifest(tmp.path(), &manifest).unwrap();

        // Pre-lock "a" so it reports AlreadyInstalled.
        let mut lock = lockfile::Lockfile::default();
        lock.upsert(LockedPackage {
            name: "a".into(),
            version: "1.0.0".into(),
            checksum: "x".into(),
            installed_at: "1970-01-01T00:00:00Z".into(),
        });
        lockfile::write(tmp.path(), &lock).unwrap();

        let registry = MockRegistryClient::from_packages(vec![
            PackageInfo {
                name: "a".into(),
                version: "1.0.0".into(),
                description: "d".into(),
                author: "x".into(),
                program_id: "p".into(),
                archive: path_a.to_string_lossy().into(),
                checksum: None,
                dependencies: Default::default(),
            },
            PackageInfo {
                name: "b".into(),
                version: "1.0.0".into(),
                description: "d".into(),
                author: "x".into(),
                program_id: "p".into(),
                archive: path_b.to_string_lossy().into(),
                checksum: None,
                dependencies: Default::default(),
            },
            // Note: "c" is declared but not in the registry -> Failed.
        ]);

        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        let outcomes = installer.install_all().await.unwrap();

        assert!(outcomes.iter().any(|o| matches!(
            o,
            DependencyOutcome::AlreadyInstalled { name, version }
                if name == "a" && version == "1.0.0"
        )));
        assert!(outcomes.iter().any(|o| matches!(
            o,
            DependencyOutcome::Installed { name, version }
                if name == "b" && version == "1.0.0"
        )));

        // The failed outcome preserves the original typed `InstallError`, so a
        // caller can match on the exact variant instead of parsing a string.
        let failed = outcomes
            .iter()
            .find(|o| matches!(o, DependencyOutcome::Failed { name, .. } if name == "c"))
            .expect("`c` should have failed");
        match failed {
            DependencyOutcome::Failed { error, .. } => assert!(
                matches!(
                    error,
                    InstallError::Registry(RegistryError::PackageNotFound { .. })
                ),
                "expected a typed registry error, got {error:?}"
            ),
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn remove_deletes_module_lock_and_dependency() {
        let zip = make_zip(&[("p/f.txt", b"x")]);
        let (tmp, registry) = scaffold("p", &zip, None);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        installer.install("p").await.unwrap();

        remove_package(tmp.path(), "p").unwrap();

        assert!(!config::modules_dir_in(tmp.path()).join("p").exists());
        assert!(lockfile::read(tmp.path()).unwrap().get("p").is_none());
        assert!(!config::list_dependencies(tmp.path())
            .unwrap()
            .contains_key("p"));
    }

    #[test]
    fn remove_not_installed_errors() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        let err = remove_package(tmp.path(), "ghost").unwrap_err();
        assert!(matches!(err, RemoveError::NotInstalled { .. }));
    }

    #[test]
    fn remove_not_a_project_errors() {
        let tmp = TempDir::new().unwrap();
        let err = remove_package(tmp.path(), "ghost").unwrap_err();
        assert!(matches!(err, RemoveError::NotASmodProject { .. }));
    }

    // --- security: package-name path traversal --------------------------

    #[tokio::test]
    async fn install_rejects_malicious_registry_names_without_escaping() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        // A real archive on disk, so a failure to validate is the *only* thing
        // that can stop an escape (the pipeline would otherwise read it).
        let zip = make_zip(&[("evil.txt", b"pwned")]);
        let archive_path = tmp.path().join("archive.zip");
        std::fs::write(&archive_path, &zip).unwrap();

        for bad in [
            "../evil",
            "../../outside",
            "/absolute/path",
            "a/b",
            "a\\b",
            ".",
        ] {
            let info = PackageInfo {
                name: bad.to_string(),
                version: "1.0.0".into(),
                description: "d".into(),
                author: "a".into(),
                program_id: "p".into(),
                archive: archive_path.to_string_lossy().into(),
                checksum: None,
                dependencies: Default::default(),
            };
            let registry = MockRegistryClient::from_packages(vec![info]);
            let installer = Installer::new(&registry, tmp.path().to_path_buf());
            let err = installer.install(bad).await.unwrap_err();
            assert!(
                matches!(err, InstallError::InvalidPackageName(_)),
                "name {bad:?} should be rejected, got {err:?}"
            );
        }

        // Nothing escaped the project: no sibling dirs were created, and the
        // modules dir was never populated.
        let parent = tmp.path().parent().unwrap();
        assert!(!parent.join("evil").exists());
        assert!(!parent.join("outside").exists());
        assert!(!config::modules_dir_in(tmp.path()).join("evil").exists());
    }

    #[test]
    fn remove_rejects_malicious_names() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        for bad in ["../evil", "..", "a/b", "C:\\x", ""] {
            let err = remove_package(tmp.path(), bad).unwrap_err();
            assert!(
                matches!(err, RemoveError::InvalidPackageName(_)),
                "name {bad:?} should be rejected, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn install_still_works_for_valid_names() {
        // Guard against over-restriction of the validator.
        let zip = make_zip(&[("my_module/lib.rs", b"// ok")]);
        let (tmp, registry) = scaffold("my_module", &zip, None);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        let installed = installer.install("my_module").await.unwrap();
        assert!(installed.install_path.join("lib.rs").is_file());
    }

    // --- update ---------------------------------------------------------

    /// Install `name` at `version` from an in-memory zip, returning the temp
    /// project so the test can then point a new registry at a newer archive.
    async fn install_v(tmp: &TempDir, name: &str, version: &str, contents: &[u8]) {
        let zip = make_zip(&[(&format!("{name}/lib.rs"), contents)]);
        let archive = tmp.path().join(format!("{name}-{version}.zip"));
        std::fs::write(&archive, &zip).unwrap();
        let registry = MockRegistryClient::from_packages(vec![pkg_info(name, version, &archive)]);
        Installer::new(&registry, tmp.path().to_path_buf())
            .install(name)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn update_replaces_outdated_package() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        let install_path = config::modules_dir_in(tmp.path()).join("payment-stream");

        // Installed: payment-stream 1.0.0.
        install_v(&tmp, "payment-stream", "1.0.0", b"v1").await;
        let checksum_v1 = lockfile::read(tmp.path())
            .unwrap()
            .get("payment-stream")
            .unwrap()
            .checksum
            .clone();

        // Registry: payment-stream 1.1.0 (a different archive).
        let zip2 = make_zip(&[("payment-stream/lib.rs", b"v2")]);
        let a2 = tmp.path().join("payment-stream-1.1.0.zip");
        std::fs::write(&a2, &zip2).unwrap();
        let registry =
            MockRegistryClient::from_packages(vec![pkg_info("payment-stream", "1.1.0", &a2)]);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());

        let outcomes = installer.update(None).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(
            matches!(
                &outcomes[0],
                UpdateOutcome::Updated { name, from, to }
                    if name == "payment-stream" && from == "1.0.0" && to == "1.1.0"
            ),
            "got {:?}",
            outcomes[0]
        );

        // Lockfile carries the new version and a new checksum.
        let lock = lockfile::read(tmp.path()).unwrap();
        let locked = lock.get("payment-stream").unwrap();
        assert_eq!(locked.version, "1.1.0");
        assert_ne!(locked.checksum, checksum_v1, "checksum must be updated");

        // The extracted contents were replaced.
        assert_eq!(
            std::fs::read_to_string(install_path.join("lib.rs")).unwrap(),
            "v2"
        );

        // The manifest dependency was bumped too.
        let deps = config::list_dependencies(tmp.path()).unwrap();
        assert_eq!(
            deps.get("payment-stream").map(String::as_str),
            Some("1.1.0")
        );
    }

    #[tokio::test]
    async fn update_up_to_date_does_not_reinstall() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();

        install_v(&tmp, "p", "1.0.0", b"v1").await;

        // Stamp a sentinel `installed_at`; a reinstall would overwrite it.
        let mut lock = lockfile::read(tmp.path()).unwrap();
        let mut locked = lock.get("p").unwrap().clone();
        locked.installed_at = "1970-01-01T00:00:00Z".to_string();
        lock.upsert(locked);
        lockfile::write(tmp.path(), &lock).unwrap();

        // Registry offers the *same* version 1.0.0 — nothing to do.
        let archive = tmp.path().join("p-1.0.0.zip");
        let registry = MockRegistryClient::from_packages(vec![pkg_info("p", "1.0.0", &archive)]);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());

        let outcomes = installer.update(None).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(
            matches!(
                &outcomes[0],
                UpdateOutcome::UpToDate { name, version }
                    if name == "p" && version == "1.0.0"
            ),
            "got {:?}",
            outcomes[0]
        );

        // The sentinel timestamp survives: no reinstall happened.
        let after = lockfile::read(tmp.path()).unwrap();
        assert_eq!(after.get("p").unwrap().installed_at, "1970-01-01T00:00:00Z");
    }

    #[tokio::test]
    async fn update_missing_registry_package_is_typed_error() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();

        // A locked package the registry knows nothing about.
        let mut lock = lockfile::Lockfile::default();
        lock.upsert(LockedPackage {
            name: "ghost".into(),
            version: "1.0.0".into(),
            checksum: "x".into(),
            installed_at: "1970-01-01T00:00:00Z".into(),
        });
        lockfile::write(tmp.path(), &lock).unwrap();

        let registry = MockRegistryClient::from_packages(vec![]);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());

        let outcomes = installer.update(None).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            UpdateOutcome::Failed { name, error } => {
                assert_eq!(name, "ghost");
                assert!(
                    matches!(
                        error,
                        InstallError::Registry(RegistryError::PackageNotFound { .. })
                    ),
                    "expected a typed registry error, got {error:?}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_failed_checksum_keeps_existing_installation_safe() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        let install_path = config::modules_dir_in(tmp.path()).join("p");

        // Good v1 install.
        install_v(&tmp, "p", "1.0.0", b"v1").await;
        assert_eq!(
            std::fs::read_to_string(install_path.join("lib.rs")).unwrap(),
            "v1"
        );

        // Registry offers 2.0.0 but with a checksum that won't match — the
        // reinstall must fail before touching the existing install.
        let zip2 = make_zip(&[("p/lib.rs", b"v2")]);
        let a2 = tmp.path().join("p-2.0.0.zip");
        std::fs::write(&a2, &zip2).unwrap();
        let mut bad = pkg_info("p", "2.0.0", &a2);
        bad.checksum = Some("deadbeefdeadbeef".into());
        let registry = MockRegistryClient::from_packages(vec![bad]);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());

        let outcomes = installer.update(None).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            UpdateOutcome::Failed { name, error } => {
                assert_eq!(name, "p");
                assert!(
                    matches!(error, InstallError::ChecksumMismatch { .. }),
                    "got {error:?}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }

        // The previous installation is completely intact.
        assert_eq!(
            std::fs::read_to_string(install_path.join("lib.rs")).unwrap(),
            "v1"
        );
        assert!(!config::modules_dir_in(tmp.path()).join(".p.tmp").exists());
        assert_eq!(
            lockfile::read(tmp.path())
                .unwrap()
                .get("p")
                .unwrap()
                .version,
            "1.0.0"
        );
    }

    #[tokio::test]
    async fn update_multiple_dependencies_reports_each() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();

        // Installed: `a` 1.0.0 (will be outdated) and `b` 1.0.0 (stays current).
        install_v(&tmp, "a", "1.0.0", b"a-v1").await;
        install_v(&tmp, "b", "1.0.0", b"b-v1").await;

        // Also locked: `c`, which the registry doesn't know about.
        let mut lock = lockfile::read(tmp.path()).unwrap();
        lock.upsert(LockedPackage {
            name: "c".into(),
            version: "1.0.0".into(),
            checksum: "x".into(),
            installed_at: "1970-01-01T00:00:00Z".into(),
        });
        lockfile::write(tmp.path(), &lock).unwrap();

        // Registry: `a` 1.1.0 (newer), `b` 1.0.0 (same), no `c`.
        let zip_a2 = make_zip(&[("a/lib.rs", b"a-v2")]);
        let a2 = tmp.path().join("a-1.1.0.zip");
        std::fs::write(&a2, &zip_a2).unwrap();
        let b_archive = tmp.path().join("b-1.0.0.zip");
        let registry = MockRegistryClient::from_packages(vec![
            pkg_info("a", "1.1.0", &a2),
            pkg_info("b", "1.0.0", &b_archive),
        ]);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());

        let outcomes = installer.update(None).await.unwrap();
        assert_eq!(outcomes.len(), 3);

        assert!(
            outcomes.iter().any(|o| matches!(
                o,
                UpdateOutcome::Updated { name, from, to }
                    if name == "a" && from == "1.0.0" && to == "1.1.0"
            )),
            "expected `a` to be Updated, got {outcomes:?}"
        );
        assert!(
            outcomes.iter().any(|o| matches!(
                o,
                UpdateOutcome::UpToDate { name, version }
                    if name == "b" && version == "1.0.0"
            )),
            "expected `b` to be UpToDate, got {outcomes:?}"
        );
        let c_failed = outcomes
            .iter()
            .find(|o| matches!(o, UpdateOutcome::Failed { name, .. } if name == "c"))
            .expect("`c` should have failed");
        match c_failed {
            UpdateOutcome::Failed { error, .. } => assert!(
                matches!(
                    error,
                    InstallError::Registry(RegistryError::PackageNotFound { .. })
                ),
                "expected a typed registry error, got {error:?}"
            ),
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn update_not_a_project_errors() {
        let tmp = TempDir::new().unwrap();
        // No manifest -> not a project.
        let registry = MockRegistryClient::from_packages(vec![]);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        let err = installer.update(None).await.unwrap_err();
        assert!(matches!(err, InstallError::NotASmodProject { .. }));
    }

    #[tokio::test]
    async fn update_targeted_only_touches_requested_package() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();

        install_v(&tmp, "a", "1.0.0", b"a-v1").await;
        install_v(&tmp, "b", "1.0.0", b"b-v1").await;

        // Registry has newer versions for both, but we only ask for `a`.
        let zip_a2 = make_zip(&[("a/lib.rs", b"a-v2")]);
        let a2 = tmp.path().join("a-1.1.0.zip");
        std::fs::write(&a2, &zip_a2).unwrap();
        let zip_b2 = make_zip(&[("b/lib.rs", b"b-v2")]);
        let b2 = tmp.path().join("b-1.1.0.zip");
        std::fs::write(&b2, &zip_b2).unwrap();
        let registry = MockRegistryClient::from_packages(vec![
            pkg_info("a", "1.1.0", &a2),
            pkg_info("b", "1.1.0", &b2),
        ]);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());

        let outcomes = installer.update(Some("a")).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(&outcomes[0], UpdateOutcome::Updated { name, .. } if name == "a"));

        // `b` was left at its installed version.
        assert_eq!(
            lockfile::read(tmp.path())
                .unwrap()
                .get("b")
                .unwrap()
                .version,
            "1.0.0"
        );
    }

    // --- verify ---------------------------------------------------------

    #[tokio::test]
    async fn verify_reports_verified_for_intact_package() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        let zip = make_zip(&[("p/lib.rs", b"v1")]);
        let archive = tmp.path().join("p.zip");
        std::fs::write(&archive, &zip).unwrap();
        let registry = MockRegistryClient::from_packages(vec![pkg_info("p", "1.0.0", &archive)]);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        installer.install("p").await.unwrap();

        let results = installer.verify().await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "p");
        assert_eq!(results[0].status, VerificationStatus::Verified);
    }

    #[tokio::test]
    async fn verify_detects_modified_archive() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        let zip = make_zip(&[("p/lib.rs", b"v1")]);
        let archive = tmp.path().join("p.zip");
        std::fs::write(&archive, &zip).unwrap();
        let registry = MockRegistryClient::from_packages(vec![pkg_info("p", "1.0.0", &archive)]);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        installer.install("p").await.unwrap();

        // Tamper with the archive after install; its checksum no longer matches
        // what the lockfile recorded at install time.
        let tampered = make_zip(&[("p/lib.rs", b"tampered contents")]);
        std::fs::write(&archive, &tampered).unwrap();

        let results = installer.verify().await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            matches!(
                results[0].status,
                VerificationStatus::ChecksumMismatch { .. }
            ),
            "got {:?}",
            results[0].status
        );
        assert_ne!(results[0].status, VerificationStatus::Verified);
    }

    #[tokio::test]
    async fn verify_detects_missing_module_directory() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        let zip = make_zip(&[("p/lib.rs", b"v1")]);
        let archive = tmp.path().join("p.zip");
        std::fs::write(&archive, &zip).unwrap();
        let registry = MockRegistryClient::from_packages(vec![pkg_info("p", "1.0.0", &archive)]);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        installer.install("p").await.unwrap();

        // Delete the extracted module directory: still locked, but not present.
        let install_path = config::modules_dir_in(tmp.path()).join("p");
        std::fs::remove_dir_all(&install_path).unwrap();

        let results = installer.verify().await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerificationStatus::ModuleMissing);
    }

    #[tokio::test]
    async fn verify_reports_archive_unavailable_when_registry_lacks_package() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        let zip = make_zip(&[("p/lib.rs", b"v1")]);
        let archive = tmp.path().join("p.zip");
        std::fs::write(&archive, &zip).unwrap();
        // Install with a registry that knows `p`...
        let registry = MockRegistryClient::from_packages(vec![pkg_info("p", "1.0.0", &archive)]);
        Installer::new(&registry, tmp.path().to_path_buf())
            .install("p")
            .await
            .unwrap();

        // ...but verify with a registry that has forgotten it. The module dir is
        // present, so we reach — and fail — the archive-location step.
        let empty = MockRegistryClient::from_packages(vec![]);
        let installer = Installer::new(&empty, tmp.path().to_path_buf());
        let results = installer.verify().await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            matches!(
                results[0].status,
                VerificationStatus::ArchiveUnavailable { .. }
            ),
            "got {:?}",
            results[0].status
        );
    }

    #[tokio::test]
    async fn verify_empty_when_nothing_installed() {
        let tmp = TempDir::new().unwrap();
        config::write_manifest(tmp.path(), &Manifest::new("host")).unwrap();
        let registry = MockRegistryClient::from_packages(vec![]);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        assert!(installer.verify().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn verify_not_a_project_errors() {
        let tmp = TempDir::new().unwrap();
        let registry = MockRegistryClient::from_packages(vec![]);
        let installer = Installer::new(&registry, tmp.path().to_path_buf());
        let err = installer.verify().await.unwrap_err();
        assert!(matches!(err, InstallError::NotASmodProject { .. }));
    }
}
