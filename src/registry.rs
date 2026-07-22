//! The registry abstraction: the one seam between the rest of `smod` and
//! "where package metadata comes from."
//!
//! Everything above this layer ([`crate::installer`], `commands::search`,
//! `commands::info`) depends on the [`RegistryClient`] trait, never on a
//! concrete client type. That is what will let an `HttpRegistryClient` drop in
//! later without touching commands — see `ARCHITECTURE.md`.

use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors produced by a registry client.
#[derive(Debug, Error)]
pub enum RegistryError {
    /// No package with the requested name exists in the registry.
    #[error("package `{name}` not found in registry")]
    PackageNotFound { name: String },

    /// The registry source could not be read (e.g. a file-backed client).
    #[error("registry i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// The registry data could not be parsed.
    #[error("failed to parse registry data: {message}")]
    Parse { message: String },
}

/// The registry's view of a package.
///
/// Deliberately distinct from [`crate::package::Manifest`] (a project's own
/// description of itself). This is a plain, transport-agnostic
/// `Serialize`/`Deserialize` struct: an HTTP JSON response can deserialize
/// straight into it, exactly as the bundled `registry.json` does today.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageInfo {
    /// Package name.
    pub name: String,
    /// Latest published version.
    pub version: String,
    /// Human-readable description.
    pub description: String,
    /// Author.
    pub author: String,
    /// On-chain program id.
    pub program_id: String,
    /// Location of the package archive.
    ///
    /// Today this is a path (absolute paths are used as-is by tests; relative
    /// paths are resolved against the crate manifest dir). It will become a URL
    /// when HTTP support lands, without any change to this struct.
    pub archive: String,
    /// Optional archive checksum. When present, install verifies against it.
    #[serde(default)]
    pub checksum: Option<String>,
}

/// Where package metadata comes from.
///
/// Uses native `async fn` in traits (stabilized in Rust 1.75) rather than the
/// `async-trait` crate. The trade-off: this trait is **not object-safe** — you
/// cannot have a `Box<dyn RegistryClient>`. Every caller is instead generic
/// over a concrete `R: RegistryClient`, monomorphized at compile time. That has
/// been a non-issue because only one client is ever alive at a time, chosen at
/// compile time.
#[allow(async_fn_in_trait)]
pub trait RegistryClient {
    /// Return packages whose name or description matches `query`.
    async fn search(&self, query: &str) -> Result<Vec<PackageInfo>, RegistryError>;

    /// Return the package with exactly this name.
    async fn get_package(&self, name: &str) -> Result<PackageInfo, RegistryError>;

    /// Return every package known to the registry.
    ///
    /// Part of the trait contract (and exercised by tests); no command lists
    /// the whole registry today.
    #[allow(dead_code)]
    async fn list_packages(&self) -> Result<Vec<PackageInfo>, RegistryError>;
}

/// The on-disk / embedded registry document shape (`{ "packages": [...] }`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryData {
    #[serde(default)]
    packages: Vec<PackageInfo>,
}

/// An in-memory registry client, used for local testing and as the current
/// production backend (via [`MockRegistryClient::embedded`]).
#[derive(Debug, Clone)]
pub struct MockRegistryClient {
    packages: Vec<PackageInfo>,
}

impl MockRegistryClient {
    /// Parse a registry document from an in-memory JSON string. This is what
    /// tests use.
    pub fn from_json_str(json: &str) -> Result<Self, RegistryError> {
        let data: RegistryData = serde_json::from_str(json).map_err(|e| RegistryError::Parse {
            message: e.to_string(),
        })?;
        Ok(Self {
            packages: data.packages,
        })
    }

    /// Read and parse a registry document from a real path on disk.
    ///
    /// Currently exercised only by tests — it exists for the day a
    /// `--registry <path>` flag is added.
    #[allow(dead_code)]
    pub async fn from_file(path: impl AsRef<Path>) -> Result<Self, RegistryError> {
        let text = tokio::fs::read_to_string(path).await?;
        Self::from_json_str(&text)
    }

    /// Parse the copy of `registry.json` baked into the binary at compile time.
    ///
    /// **This is what every real command uses today.** It makes `smod search` /
    /// `smod info` / `smod install` work immediately regardless of the current
    /// working directory — the same way a real HTTP-backed client would not
    /// depend on `cwd`.
    ///
    /// The `.expect` here is justified: the embedded `registry.json` is a
    /// build-time asset under our own control, and every corruption of it is
    /// already covered by a test that would fail CI.
    pub fn embedded() -> Self {
        Self::from_json_str(include_str!("../registry.json"))
            .expect("embedded registry.json must be valid")
    }

    /// Construct directly from a list of packages (test convenience).
    #[cfg(test)]
    pub fn from_packages(packages: Vec<PackageInfo>) -> Self {
        Self { packages }
    }
}

impl RegistryClient for MockRegistryClient {
    async fn search(&self, query: &str) -> Result<Vec<PackageInfo>, RegistryError> {
        let needle = query.to_lowercase();
        let mut matches: Vec<PackageInfo> = self
            .packages
            .iter()
            .filter(|p| {
                p.name.to_lowercase().contains(&needle)
                    || p.description.to_lowercase().contains(&needle)
            })
            .cloned()
            .collect();
        matches.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(matches)
    }

    async fn get_package(&self, name: &str) -> Result<PackageInfo, RegistryError> {
        self.packages
            .iter()
            .find(|p| p.name == name)
            .cloned()
            .ok_or_else(|| RegistryError::PackageNotFound {
                name: name.to_string(),
            })
    }

    async fn list_packages(&self) -> Result<Vec<PackageInfo>, RegistryError> {
        let mut all = self.packages.clone();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{
        "packages": [
            {
                "name": "payment-stream",
                "version": "1.0.0",
                "description": "Streaming payments on Solana",
                "author": "smod",
                "program_id": "Pay111",
                "archive": "./packages/payment-stream.zip",
                "checksum": "deadbeef"
            },
            {
                "name": "token-vault",
                "version": "2.1.0",
                "description": "A secure token vault",
                "author": "smod",
                "program_id": "Vlt222",
                "archive": "./packages/token-vault.zip"
            }
        ]
    }"#;

    #[tokio::test]
    async fn search_matches_name_and_description() {
        let client = MockRegistryClient::from_json_str(JSON).unwrap();
        let by_name = client.search("vault").await.unwrap();
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].name, "token-vault");

        let by_desc = client.search("solana").await.unwrap();
        assert_eq!(by_desc.len(), 1);
        assert_eq!(by_desc[0].name, "payment-stream");
    }

    #[tokio::test]
    async fn search_is_case_insensitive_and_sorted() {
        let client = MockRegistryClient::from_json_str(JSON).unwrap();
        let results = client.search("A").await.unwrap();
        // Both descriptions contain "a"; results must be name-sorted.
        assert_eq!(results[0].name, "payment-stream");
        assert_eq!(results[1].name, "token-vault");
    }

    #[tokio::test]
    async fn get_package_found_and_not_found() {
        let client = MockRegistryClient::from_json_str(JSON).unwrap();
        let found = client.get_package("token-vault").await.unwrap();
        assert_eq!(found.version, "2.1.0");
        assert!(found.checksum.is_none());

        let err = client.get_package("nope").await.unwrap_err();
        assert!(matches!(err, RegistryError::PackageNotFound { .. }));
    }

    #[tokio::test]
    async fn list_packages_is_sorted() {
        let client = MockRegistryClient::from_json_str(JSON).unwrap();
        let all = client.list_packages().await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name, "payment-stream");
    }

    #[test]
    fn invalid_json_is_a_parse_error() {
        let err = MockRegistryClient::from_json_str("{ not json").unwrap_err();
        assert!(matches!(err, RegistryError::Parse { .. }));
    }

    #[tokio::test]
    async fn from_file_reads_disk() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("registry.json");
        std::fs::write(&path, JSON).unwrap();
        let client = MockRegistryClient::from_file(&path).await.unwrap();
        assert_eq!(client.list_packages().await.unwrap().len(), 2);
    }

    #[test]
    fn embedded_registry_is_valid() {
        // Guards the `.expect` in `embedded()`.
        let client = MockRegistryClient::embedded();
        assert!(!client.packages.is_empty());
    }
}
