//! Pure data model for `smod.toml`.
//!
//! This module is the leaf of the dependency graph: everything reads it and it
//! reads nothing. It has **no** filesystem or registry awareness — connecting a
//! [`Manifest`] to disk is [`crate::config`]'s job. The only responsibility
//! here is defining the manifest schema and the single place where manifest
//! TOML (de)serialization happens.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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
}
