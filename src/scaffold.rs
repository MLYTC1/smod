//! Project scaffolding — the business logic behind `smod new`.
//!
//! [`create_project`] generates a new project directory tree (`smod.toml`,
//! `src/lib.rs`, `README.md`) and returns a typed [`CreatedProject`] describing
//! what it wrote. Like every business module it never prints: rendering the
//! result is `commands::new`'s job.
//!
//! It lives in its own module rather than in [`crate::config`] because it does
//! more than the manifest boundary: it validates the package name (reusing the
//! shared [`validate_package_name`](crate::package::validate_package_name)
//! gate), creates a directory subtree, and writes several files. It still
//! *reuses* `config::write_manifest` and [`Manifest`] rather than reimplementing
//! manifest serialization, so `smod.toml` is generated exactly one way.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::config::{self, ConfigError};
use crate::package::{validate_package_name, Manifest, PackageNameError};

/// Errors produced while creating a new project.
#[derive(Debug, Error)]
pub enum NewProjectError {
    /// The requested name is not a valid package name.
    #[error("invalid package name: {0}")]
    InvalidName(#[from] PackageNameError),

    /// The destination directory already exists.
    #[error("destination already exists: {path} (choose another name or remove it)")]
    AlreadyExists { path: PathBuf },

    /// A file or directory could not be created.
    #[error("failed to create {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Writing `smod.toml` failed.
    #[error(transparent)]
    Manifest(#[from] ConfigError),
}

/// A successfully created project: its root and the files that were written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedProject {
    /// The project root directory (`<parent>/<name>`).
    pub root: PathBuf,
    /// The files created, in creation order, relative to nothing (full paths).
    pub files: Vec<PathBuf>,
}

/// Create a new project named `name` as a subdirectory of `parent`.
///
/// Lays out:
///
/// ```text
/// <name>/
/// ├── smod.toml
/// ├── src/
/// │   └── lib.rs
/// └── README.md
/// ```
///
/// The name is validated first (it becomes both a manifest field and a
/// directory name), and an existing destination is refused rather than
/// overwritten. `smod.toml` is generated via [`Manifest`] + `config`, so it is
/// always valid and consistent with what `smod init` writes.
pub fn create_project(parent: &Path, name: &str) -> Result<CreatedProject, NewProjectError> {
    validate_package_name(name)?;

    let root = parent.join(name);
    if root.exists() {
        return Err(NewProjectError::AlreadyExists { path: root });
    }

    // Create the directory tree first.
    let src_dir = root.join("src");
    mkdir_all(&src_dir)?;

    // smod.toml — reuse the manifest data model and the config writer so there
    // is exactly one place manifests are serialized.
    let manifest = Manifest::new(name);
    config::write_manifest(&root, &manifest)?;
    let manifest_path = config::manifest_path_in(&root);

    // src/lib.rs
    let lib_path = src_dir.join("lib.rs");
    write_file(&lib_path, &lib_rs_template(name))?;

    // README.md
    let readme_path = root.join("README.md");
    write_file(&readme_path, &readme_template(name))?;

    Ok(CreatedProject {
        root,
        files: vec![manifest_path, lib_path, readme_path],
    })
}

/// Create a directory (and any missing parents), mapping I/O errors to the
/// typed variant.
fn mkdir_all(path: &Path) -> Result<(), NewProjectError> {
    std::fs::create_dir_all(path).map_err(|source| NewProjectError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Write `contents` to `path`, mapping I/O errors to the typed variant.
fn write_file(path: &Path, contents: &str) -> Result<(), NewProjectError> {
    std::fs::write(path, contents).map_err(|source| NewProjectError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// The starter `src/lib.rs` for a new module.
fn lib_rs_template(name: &str) -> String {
    format!(
        "//! {name} — a smod module.\n\
         \n\
         /// Returns the name of this module.\n\
         pub fn name() -> &'static str {{\n    \
             {name:?}\n\
         }}\n\
         \n\
         #[cfg(test)]\n\
         mod tests {{\n    \
             use super::*;\n\
         \n    \
             #[test]\n    \
             fn has_a_name() {{\n        \
                 assert_eq!(name(), {name:?});\n    \
             }}\n\
         }}\n"
    )
}

/// The starter `README.md` for a new module.
fn readme_template(name: &str) -> String {
    format!(
        "# {name}\n\
         \n\
         A Solana module managed with [smod](https://github.com/MLYTC1/smod).\n\
         \n\
         ## Development\n\
         \n\
         ```bash\n\
         smod install    # install declared dependencies\n\
         smod list       # show declared and installed dependencies\n\
         ```\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::Manifest;
    use tempfile::TempDir;

    #[test]
    fn creates_the_expected_layout() {
        let tmp = TempDir::new().unwrap();
        let created = create_project(tmp.path(), "my-module").unwrap();

        assert_eq!(created.root, tmp.path().join("my-module"));
        assert!(created.root.join("smod.toml").is_file());
        assert!(created.root.join("src/lib.rs").is_file());
        assert!(created.root.join("README.md").is_file());
        assert_eq!(created.files.len(), 3);
    }

    #[test]
    fn generates_a_valid_manifest_with_the_given_name() {
        let tmp = TempDir::new().unwrap();
        create_project(tmp.path(), "my-module").unwrap();

        // It parses, and carries the requested name at the default version.
        let manifest = config::read_manifest(&tmp.path().join("my-module")).unwrap();
        assert_eq!(manifest, {
            let mut expected = Manifest::new("my-module");
            expected.version = manifest.version.clone();
            expected
        });
        assert_eq!(manifest.name, "my-module");
        assert_eq!(manifest.version, "0.1.0");
        assert!(manifest.smod.dependencies.is_empty());
    }

    #[test]
    fn lib_rs_and_readme_mention_the_name() {
        let tmp = TempDir::new().unwrap();
        create_project(tmp.path(), "payment-stream").unwrap();
        let root = tmp.path().join("payment-stream");

        let lib = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert!(lib.contains("payment-stream"));
        let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
        assert!(readme.starts_with("# payment-stream"));
    }

    #[test]
    fn rejects_invalid_names() {
        let tmp = TempDir::new().unwrap();
        for bad in ["../evil", "a/b", "..", "", "C:\\x"] {
            let err = create_project(tmp.path(), bad).unwrap_err();
            assert!(
                matches!(err, NewProjectError::InvalidName(_)),
                "name {bad:?} should be rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn refuses_to_overwrite_an_existing_directory() {
        let tmp = TempDir::new().unwrap();
        create_project(tmp.path(), "dup").unwrap();
        let err = create_project(tmp.path(), "dup").unwrap_err();
        assert!(matches!(err, NewProjectError::AlreadyExists { .. }));
    }
}
