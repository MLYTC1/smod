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
    /// first failure.
    pub async fn install(&self, package_query: &str) -> Result<InstalledPackage, InstallError> {
        // 1. Confirm we are in a smod project.
        if !config::is_smod_project(&self.project_root) {
            return Err(InstallError::NotASmodProject {
                path: self.project_root.clone(),
            });
        }

        // 2. Resolve the package via the registry.
        let info = self.registry.get_package(package_query).await?;

        // 2a. Reject unsafe names before any of them become a filesystem path.
        package::validate_package_name(&info.name)?;

        // 3. Resolve where the archive lives, and 4. read its bytes.
        let archive_path = self.resolve_archive_path(&info.archive);
        let bytes = std::fs::read(&archive_path).map_err(|source| {
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
        })?;

        // 5. Compute and 6. verify the checksum.
        let checksum = Self::compute_checksum(&bytes);
        self.verify_checksum(&info, &checksum)?;

        // 7. Extract into a staging directory, then atomically swap it into
        //    place. Extraction never writes directly into the final directory,
        //    so a failed extraction can't leave partial files and a reinstall
        //    can't leave stale files from an older version.
        let modules_dir = config::modules_dir_in(&self.project_root);
        let install_path = modules_dir.join(&info.name);
        let staging_path = modules_dir.join(format!(".{}.tmp", info.name));
        self.install_into_place(&bytes, &staging_path, &install_path)?;

        // 8. Record in the lockfile, then 9. add to the manifest.
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
            },
            PackageInfo {
                name: "b".into(),
                version: "1.0.0".into(),
                description: "d".into(),
                author: "x".into(),
                program_id: "p".into(),
                archive: path_b.to_string_lossy().into(),
                checksum: None,
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
}
