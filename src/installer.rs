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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyOutcome {
    /// Newly installed.
    Installed { name: String, version: String },
    /// Already present in `smod.lock`, so skipped.
    AlreadyInstalled { name: String, version: String },
    /// Failed to install; the batch continued past it.
    Failed { name: String, error: String },
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

        // 7. Extract into smod_modules/<name>/.
        let install_path = config::modules_dir_in(&self.project_root).join(&info.name);
        self.extract_package(&bytes, &install_path)?;

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
                    error: error.to_string(),
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
    /// Uses `enclosed_name()` to reject absolute paths and `..` components
    /// (zip-slip protection). If every entry shares one common top-level
    /// directory (the conventional `payment-stream/README.md` layout), that
    /// prefix is stripped — but only when doing so would not erase a root-level
    /// file.
    pub fn extract_package(&self, bytes: &[u8], dest: &Path) -> Result<(), InstallError> {
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| InstallError::InvalidArchive {
                message: e.to_string(),
            })?;

        // First pass: gather safe entry paths (normalized, forward slashes).
        let mut entries: Vec<(String, bool)> = Vec::with_capacity(archive.len());
        for i in 0..archive.len() {
            let file = archive
                .by_index(i)
                .map_err(|e| InstallError::InvalidArchive {
                    message: e.to_string(),
                })?;
            // Unsafe names (absolute, `..`) yield None and are skipped.
            if let Some(name) = file.enclosed_name() {
                let normalized = name.to_string_lossy().replace('\\', "/");
                if !normalized.is_empty() {
                    entries.push((normalized, file.is_dir()));
                }
            }
        }

        let strip = common_prefix(&entries);

        // Second pass: extract.
        std::fs::create_dir_all(dest).map_err(|source| InstallError::ExtractIo { source })?;
        for i in 0..archive.len() {
            let mut file = archive
                .by_index(i)
                .map_err(|e| InstallError::InvalidArchive {
                    message: e.to_string(),
                })?;
            let name = match file.enclosed_name() {
                Some(n) => n.to_string_lossy().replace('\\', "/"),
                None => continue,
            };
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
            if file.is_dir() {
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

        assert!(outcomes.contains(&DependencyOutcome::AlreadyInstalled {
            name: "a".into(),
            version: "1.0.0".into()
        }));
        assert!(outcomes.contains(&DependencyOutcome::Installed {
            name: "b".into(),
            version: "1.0.0".into()
        }));
        assert!(outcomes
            .iter()
            .any(|o| matches!(o, DependencyOutcome::Failed { name, .. } if name == "c")));
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
}
