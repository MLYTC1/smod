//! The filesystem boundary for `smod.toml`.
//!
//! This is the only module that reads and writes a project's manifest to disk,
//! and the only module that mutates the `[smod.dependencies]` table. It groups
//! three concerns: location & detection, read/write, and dependency editing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::package::Manifest;

/// The manifest filename.
pub const MANIFEST_FILE: &str = "smod.toml";
/// The directory installed modules are extracted into.
pub const MODULES_DIR: &str = "smod_modules";

/// Errors produced by the config layer.
///
/// [`ManifestNotFound`](ConfigError::ManifestNotFound) is kept distinct from
/// [`Io`](ConfigError::Io) on purpose: a missing file is the "run `smod init`"
/// case, whereas an existing-but-unreadable file (permissions, or it's a
/// directory) is a genuine I/O problem that shouldn't be misreported.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// No `smod.toml` exists at the expected location.
    #[error("no smod.toml found at {path} (run `smod init` first)")]
    ManifestNotFound { path: PathBuf },

    /// The manifest exists but could not be read or written.
    #[error("i/o error for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The manifest exists but is not valid TOML / does not match the schema.
    #[error("invalid manifest at {path}: {message}")]
    InvalidManifest { path: PathBuf, message: String },
}

// ---------------------------------------------------------------------------
// Location & detection
// ---------------------------------------------------------------------------

/// Path to `smod.toml` inside `dir`.
pub fn manifest_path_in(dir: &Path) -> PathBuf {
    dir.join(MANIFEST_FILE)
}

/// Path to the `smod_modules/` directory inside `dir`.
pub fn modules_dir_in(dir: &Path) -> PathBuf {
    dir.join(MODULES_DIR)
}

/// Whether `dir` (exactly, no ancestor search) is a smod project.
pub fn is_smod_project(dir: &Path) -> bool {
    manifest_path_in(dir).is_file()
}

/// Search `start` and its ancestors for the nearest smod project root, like
/// `cargo` and `npm` do. Returns `None` if none is found.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| is_smod_project(dir))
        .map(Path::to_path_buf)
}

/// Like [`find_project_root`], but returns a friendly [`anyhow::Error`] that a
/// command can simply `?` on. This is the sanctioned bridge from the config
/// layer's detection logic into the command layer's error style.
pub fn require_project_root(start: PathBuf) -> anyhow::Result<PathBuf> {
    find_project_root(&start).ok_or_else(|| {
        anyhow::anyhow!(
            "not inside a smod project (no {} found in {} or any parent) — run `smod init`",
            MANIFEST_FILE,
            start.display()
        )
    })
}

// ---------------------------------------------------------------------------
// Read / write
// ---------------------------------------------------------------------------

/// Read and parse the manifest at `dir/smod.toml`.
pub fn read_manifest(dir: &Path) -> Result<Manifest, ConfigError> {
    let path = manifest_path_in(dir);
    if !path.is_file() {
        return Err(ConfigError::ManifestNotFound { path });
    }
    let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Io {
        path: path.clone(),
        source,
    })?;
    Manifest::from_toml_str(&text).map_err(|e| ConfigError::InvalidManifest {
        path,
        message: e.to_string(),
    })
}

/// Serialize and write `manifest` to `dir/smod.toml`.
pub fn write_manifest(dir: &Path, manifest: &Manifest) -> Result<(), ConfigError> {
    let path = manifest_path_in(dir);
    let text = manifest
        .to_toml_string()
        .map_err(|e| ConfigError::InvalidManifest {
            path: path.clone(),
            message: e.to_string(),
        })?;
    std::fs::write(&path, text).map_err(|source| ConfigError::Io { path, source })
}

/// Alias for [`write_manifest`], named so the "read → mutate → save" shape of
/// the dependency-editing functions below reads naturally.
pub fn save_manifest(dir: &Path, manifest: &Manifest) -> Result<(), ConfigError> {
    write_manifest(dir, manifest)
}

/// Ensure `dir/smod_modules/` exists, creating it if necessary, and return it.
pub fn ensure_modules_dir(dir: &Path) -> Result<PathBuf, ConfigError> {
    let path = modules_dir_in(dir);
    std::fs::create_dir_all(&path).map_err(|source| ConfigError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// Dependency editing
// ---------------------------------------------------------------------------

/// Add (or update) a dependency in `[smod.dependencies]`.
///
/// Because the table is a [`BTreeMap`], re-adding an existing name simply
/// updates its version in place — duplicates are structurally impossible.
pub fn add_dependency(dir: &Path, name: &str, version: &str) -> Result<(), ConfigError> {
    let mut manifest = read_manifest(dir)?;
    manifest
        .smod
        .dependencies
        .insert(name.to_string(), version.to_string());
    save_manifest(dir, &manifest)
}

/// Remove a dependency from `[smod.dependencies]`.
///
/// Returns `true` if the dependency was present and removed, `false` if it was
/// not declared in the first place.
pub fn remove_dependency(dir: &Path, name: &str) -> Result<bool, ConfigError> {
    let mut manifest = read_manifest(dir)?;
    let existed = manifest.smod.dependencies.remove(name).is_some();
    save_manifest(dir, &manifest)?;
    Ok(existed)
}

/// Return a copy of the `[smod.dependencies]` table.
///
/// Part of the documented config API; currently consumed by tests and reserved
/// for commands that need the raw table without the rest of the manifest.
#[allow(dead_code)]
pub fn list_dependencies(dir: &Path) -> Result<BTreeMap<String, String>, ConfigError> {
    Ok(read_manifest(dir)?.smod.dependencies)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_project(dir: &Path) {
        write_manifest(dir, &Manifest::new("demo")).expect("write manifest");
    }

    #[test]
    fn detects_project_only_when_manifest_present() {
        let tmp = TempDir::new().unwrap();
        assert!(!is_smod_project(tmp.path()));
        init_project(tmp.path());
        assert!(is_smod_project(tmp.path()));
    }

    #[test]
    fn find_project_root_searches_ancestors() {
        let tmp = TempDir::new().unwrap();
        init_project(tmp.path());
        let nested = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        let found = find_project_root(&nested).expect("root found");
        assert_eq!(found, tmp.path());
    }

    #[test]
    fn require_project_root_errors_outside_a_project() {
        let tmp = TempDir::new().unwrap();
        assert!(require_project_root(tmp.path().to_path_buf()).is_err());
    }

    #[test]
    fn read_manifest_missing_is_manifest_not_found() {
        let tmp = TempDir::new().unwrap();
        let err = read_manifest(tmp.path()).unwrap_err();
        assert!(matches!(err, ConfigError::ManifestNotFound { .. }));
    }

    #[test]
    fn read_manifest_invalid_is_invalid_manifest() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(manifest_path_in(tmp.path()), "not [ valid").unwrap();
        let err = read_manifest(tmp.path()).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidManifest { .. }));
    }

    #[test]
    fn add_dependency_is_idempotent_on_name() {
        let tmp = TempDir::new().unwrap();
        init_project(tmp.path());
        add_dependency(tmp.path(), "token-vault", "1.0.0").unwrap();
        add_dependency(tmp.path(), "token-vault", "2.0.0").unwrap();
        let deps = list_dependencies(tmp.path()).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps.get("token-vault").map(String::as_str), Some("2.0.0"));
    }

    #[test]
    fn remove_dependency_reports_presence() {
        let tmp = TempDir::new().unwrap();
        init_project(tmp.path());
        add_dependency(tmp.path(), "token-vault", "1.0.0").unwrap();
        assert!(remove_dependency(tmp.path(), "token-vault").unwrap());
        assert!(!remove_dependency(tmp.path(), "token-vault").unwrap());
    }

    #[test]
    fn ensure_modules_dir_creates_directory() {
        let tmp = TempDir::new().unwrap();
        let path = ensure_modules_dir(tmp.path()).unwrap();
        assert!(path.is_dir());
        assert_eq!(path, modules_dir_in(tmp.path()));
    }
}
