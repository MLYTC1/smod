//! Pure data model for `smod.toml`.
//!
//! This module is the leaf of the dependency graph: everything reads it and it
//! reads nothing. It has **no** filesystem or registry awareness — connecting a
//! [`Manifest`] to disk is [`crate::config`]'s job. The only responsibility
//! here is defining the manifest schema and the single place where manifest
//! TOML (de)serialization happens.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Error returned when a package name is not safe to use.
///
/// A package name is used both as a `[smod.dependencies]` key *and* as a
/// filesystem path component (`smod_modules/<name>/`). Because the second use
/// is a security boundary, names that could escape that directory — separators,
/// `.`/`..`, absolute paths — are rejected up front by
/// [`validate_package_name`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PackageNameError {
    /// The name was empty.
    #[error("package name must not be empty")]
    Empty,

    /// The name was `.` or `..`, which are reserved path components.
    #[error("package name `{0}` is reserved and cannot be used")]
    Reserved(String),

    /// The name contained a character outside the allowed set.
    #[error("package name `{name}` contains an invalid character {ch:?} (allowed: letters, digits, `-`, `_`, `.`)")]
    InvalidCharacter { name: String, ch: char },
}

/// Validate that `name` is safe to use as both a manifest key and a filesystem
/// path component.
///
/// This is the single, centralized gate that every path-building site funnels
/// through. It is deliberately a conservative allow-list: a name may contain
/// only ASCII letters, digits, `-`, `_`, and `.`, and may not be `.` or `..`.
/// That rejects every path-traversal shape — `/` and `\` separators, absolute
/// paths like `/abs` or `C:\x` (the `\`/`:` are not allowed), and `..`
/// components — without needing to reason about platform-specific path parsing.
///
/// Valid: `payment-stream`, `token`, `my_module`.
/// Rejected: `""`, `"."`, `".."`, `"../evil"`, `"a/b"`, `"a\\b"`, `"/abs"`,
/// `"C:\\x"`.
pub fn validate_package_name(name: &str) -> Result<(), PackageNameError> {
    if name.is_empty() {
        return Err(PackageNameError::Empty);
    }
    if name == "." || name == ".." {
        return Err(PackageNameError::Reserved(name.to_string()));
    }
    for ch in name.chars() {
        let allowed = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.');
        if !allowed {
            return Err(PackageNameError::InvalidCharacter {
                name: name.to_string(),
                ch,
            });
        }
    }
    Ok(())
}

/// A project's own description of itself, as stored in `smod.toml`.
///
/// This is deliberately kept distinct from [`crate::registry::PackageInfo`]
/// (the registry's view of a package). A manifest describes a project *before*
/// it is published; it does not, for example, carry a checksum of itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Package name.
    pub name: String,
    /// Semantic version string.
    pub version: String,
    /// Optional human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Optional SPDX license expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Optional on-chain program id this module targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_id: Option<String>,
    /// Optional source repository URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// The `[smod]` section, which holds the dependency table.
    #[serde(default)]
    pub smod: SmodSection,
}

/// The `[smod]` table of a manifest.
///
/// Currently this holds only the `[smod.dependencies]` table, stored as a
/// [`BTreeMap`] so the on-disk file diffs predictably (keys are always sorted)
/// and duplicate dependencies are structurally impossible.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmodSection {
    /// The `[smod.dependencies]` table: `name -> version requirement`.
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
}

impl Manifest {
    /// Create a fresh manifest for a brand new project (used by `smod init`).
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: "0.1.0".to_string(),
            description: None,
            author: None,
            license: None,
            program_id: None,
            repository: None,
            smod: SmodSection::default(),
        }
    }

    /// Serialize this manifest to a pretty TOML string.
    ///
    /// This and [`Manifest::from_toml_str`] are the *only* place manifest TOML
    /// (de)serialization happens.
    ///
    /// ```ignore
    /// // Ignored because `smod` is a bin-only crate with no lib target for a
    /// // doctest to link against. The same coverage exists as a `#[test]`.
    /// use smod::package::Manifest;
    /// let m = Manifest::new("payment-stream");
    /// let toml = m.to_toml_string().unwrap();
    /// assert!(toml.contains("name = \"payment-stream\""));
    /// ```
    pub fn to_toml_string(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Parse a manifest from a TOML string.
    pub fn from_toml_str(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_manifest_has_sensible_defaults() {
        let m = Manifest::new("payment-stream");
        assert_eq!(m.name, "payment-stream");
        assert_eq!(m.version, "0.1.0");
        assert!(m.description.is_none());
        assert!(m.smod.dependencies.is_empty());
    }

    #[test]
    fn roundtrips_through_toml() {
        let mut m = Manifest::new("payment-stream");
        m.description = Some("Streaming payments".to_string());
        m.program_id = Some("Prog1111111111111111111111111111111111111111".to_string());
        m.smod
            .dependencies
            .insert("token-vault".to_string(), "1.2.0".to_string());

        let text = m.to_toml_string().expect("serialize");
        let parsed = Manifest::from_toml_str(&text).expect("parse");
        assert_eq!(m, parsed);
    }

    #[test]
    fn serialization_matches_the_doctest_expectation() {
        // Non-ignored equivalent of the `to_toml_string` doctest above.
        let m = Manifest::new("payment-stream");
        let toml = m.to_toml_string().expect("serialize");
        assert!(toml.contains("name = \"payment-stream\""));
    }

    #[test]
    fn dependencies_are_sorted_for_predictable_diffs() {
        let mut m = Manifest::new("demo");
        m.smod.dependencies.insert("zeta".into(), "1.0.0".into());
        m.smod.dependencies.insert("alpha".into(), "2.0.0".into());
        let text = m.to_toml_string().expect("serialize");
        let alpha = text.find("alpha").expect("alpha present");
        let zeta = text.find("zeta").expect("zeta present");
        assert!(alpha < zeta, "BTreeMap should sort keys ascending");
    }

    #[test]
    fn missing_optional_fields_parse_as_none() {
        let text = "name = \"x\"\nversion = \"0.1.0\"\n";
        let m = Manifest::from_toml_str(text).expect("parse");
        assert!(m.author.is_none());
        assert!(m.license.is_none());
        assert!(m.smod.dependencies.is_empty());
    }

    #[test]
    fn invalid_toml_is_an_error() {
        assert!(Manifest::from_toml_str("this is not = = toml").is_err());
    }

    // --- package name validation ----------------------------------------

    #[test]
    fn valid_package_names_are_accepted() {
        for name in [
            "payment-stream",
            "token",
            "my_module",
            "a",
            "v1.2.0",
            "A-b_c9",
        ] {
            assert!(
                validate_package_name(name).is_ok(),
                "expected `{name}` to be valid"
            );
        }
    }

    #[test]
    fn empty_name_is_rejected() {
        assert_eq!(validate_package_name(""), Err(PackageNameError::Empty));
    }

    #[test]
    fn dot_and_dotdot_are_rejected() {
        assert_eq!(
            validate_package_name("."),
            Err(PackageNameError::Reserved(".".to_string()))
        );
        assert_eq!(
            validate_package_name(".."),
            Err(PackageNameError::Reserved("..".to_string()))
        );
    }

    #[test]
    fn path_traversal_and_separators_are_rejected() {
        for name in [
            "../evil",
            "../../outside",
            "/absolute/path",
            "C:\\something",
            "a/b",
            "a\\b",
            "foo/../bar",
            ".ssh/authorized_keys",
        ] {
            assert!(
                matches!(
                    validate_package_name(name),
                    Err(PackageNameError::InvalidCharacter { .. })
                ),
                "expected `{name}` to be rejected as an invalid character"
            );
        }
    }

    #[test]
    fn whitespace_and_control_chars_are_rejected() {
        for name in ["with space", "tab\there", "null\0byte"] {
            assert!(validate_package_name(name).is_err());
        }
    }
}
