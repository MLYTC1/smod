//! Pure data model for `smod.toml`.
//!
//! This module is the leaf of the dependency graph: everything reads it and it
//! reads nothing. It has **no** filesystem or registry awareness — connecting a
//! [`Manifest`] to disk is [`crate::config`]'s job. The only responsibility
//! here is defining the manifest schema and the single place where manifest
//! TOML (de)serialization happens.

use std::cmp::Ordering;
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

/// Compare two dotted version strings (e.g. `1.0.0` vs `1.1.0`).
///
/// This is the smallest correct version ordering `smod` needs today: versions
/// are compared component-by-component after splitting on `.`, numerically when
/// both components parse as integers, and lexically as a conservative fallback
/// otherwise. Missing trailing components are treated as `0`, so `1.2` and
/// `1.2.0` compare equal.
///
/// It deliberately does *not* pull in a full semver crate or introduce a
/// `Version` type — package versions remain plain strings in the manifest,
/// lockfile, and registry. `update` uses this to decide whether the registry
/// offers a strictly newer version than what is installed.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let mut a_parts = a.split('.');
    let mut b_parts = b.split('.');
    loop {
        match (a_parts.next(), b_parts.next()) {
            (None, None) => return Ordering::Equal,
            (a_part, b_part) => {
                // A missing component (one version has fewer parts) is treated
                // as `0`, so `1.2` == `1.2.0`.
                let a_str = a_part.unwrap_or("0");
                let b_str = b_part.unwrap_or("0");
                let ordering = match (a_str.parse::<u64>(), b_str.parse::<u64>()) {
                    (Ok(a_num), Ok(b_num)) => a_num.cmp(&b_num),
                    // Non-numeric component: fall back to a byte-wise compare.
                    _ => a_str.cmp(b_str),
                };
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
}

/// Error returned when a dependency version requirement cannot be parsed.
///
/// Kept intentionally small: `smod` supports only the subset of requirement
/// syntax it actually needs (see [`VersionReq`]), so the only ways to be
/// invalid are being empty or not being a dotted numeric version after the
/// (optional) operator.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VersionReqError {
    /// The requirement string was empty (or only whitespace).
    #[error("version requirement must not be empty")]
    Empty,

    /// The requirement was not a supported operator followed by a dotted
    /// numeric version (e.g. `>=1.0.0`, `^1.0`, `1.2.3`).
    #[error("invalid version requirement `{0}` (expected e.g. `1.2.3`, `>=1.0.0`, or `^1.0`)")]
    Invalid(String),
}

/// A parsed dependency version requirement.
///
/// This is the smallest useful subset of semver-style requirements `smod`
/// needs, built on top of [`compare_versions`] rather than pulling in a full
/// semver crate. Three shapes are supported:
///
/// - `"1.2.3"` (or `"=1.2.3"`) — [`Exact`](VersionReq::Exact): the version must
///   compare equal (so `1.2` and `1.2.0` are equal, per [`compare_versions`]).
/// - `">=1.0.0"` — [`AtLeast`](VersionReq::AtLeast): the version must be greater
///   than or equal to the bound.
/// - `"^1.0"` — [`Caret`](VersionReq::Caret): the version must be at least the
///   bound and below the next incompatible release, where "incompatible" is
///   determined by the left-most non-zero component (`^1.0` allows `>=1.0.0,
///   <2.0.0`; `^0.2.3` allows `>=0.2.3, <0.3.0`).
///
/// Resolution lives here in the data-model layer, never in a command: a command
/// parses a requirement and asks whether a concrete version satisfies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionReq {
    /// The version must equal this one exactly.
    Exact(String),
    /// The version must be greater than or equal to this one.
    AtLeast(String),
    /// The version must be caret-compatible with this one.
    Caret(String),
}

impl VersionReq {
    /// Parse a requirement string into a [`VersionReq`].
    ///
    /// Leading/trailing whitespace (and whitespace after the operator) is
    /// ignored. Only the operators documented on [`VersionReq`] are supported;
    /// anything else — an empty string, an unknown operator, or a non-numeric
    /// version — is a [`VersionReqError`].
    pub fn parse(input: &str) -> Result<Self, VersionReqError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(VersionReqError::Empty);
        }

        let (make, version): (fn(String) -> VersionReq, &str) =
            if let Some(rest) = trimmed.strip_prefix(">=") {
                (VersionReq::AtLeast, rest)
            } else if let Some(rest) = trimmed.strip_prefix('^') {
                (VersionReq::Caret, rest)
            } else if let Some(rest) = trimmed.strip_prefix('=') {
                (VersionReq::Exact, rest)
            } else {
                (VersionReq::Exact, trimmed)
            };

        let version = version.trim();
        Self::require_numeric_version(version, trimmed)?;
        Ok(make(version.to_string()))
    }

    /// Whether `version` satisfies this requirement.
    pub fn matches(&self, version: &str) -> bool {
        match self {
            VersionReq::Exact(bound) => compare_versions(version, bound) == Ordering::Equal,
            VersionReq::AtLeast(bound) => compare_versions(version, bound) != Ordering::Less,
            VersionReq::Caret(bound) => {
                // At least the bound, and strictly below the next incompatible
                // release.
                compare_versions(version, bound) != Ordering::Less
                    && compare_versions(version, &caret_upper_bound(bound)) == Ordering::Less
            }
        }
    }

    /// Validate that `version` is a non-empty, dotted, all-numeric version.
    ///
    /// `original` is the full requirement string (operator included) so the
    /// error message points at what the user actually wrote.
    fn require_numeric_version(version: &str, original: &str) -> Result<(), VersionReqError> {
        if version.is_empty() {
            return Err(VersionReqError::Invalid(original.to_string()));
        }
        for part in version.split('.') {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(VersionReqError::Invalid(original.to_string()));
            }
        }
        Ok(())
    }
}

/// The exclusive upper bound for a caret requirement: the next version that is
/// considered incompatible, per the left-most non-zero component.
///
/// `base` is assumed already validated as a dotted numeric version (as
/// guaranteed by [`VersionReq::parse`]), so component parsing cannot fail.
fn caret_upper_bound(base: &str) -> String {
    let parts: Vec<u64> = base.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    let major = parts.first().copied().unwrap_or(0);
    let minor = parts.get(1).copied().unwrap_or(0);
    let patch = parts.get(2).copied().unwrap_or(0);
    if major > 0 {
        format!("{}.0.0", major + 1)
    } else if minor > 0 {
        format!("0.{}.0", minor + 1)
    } else {
        format!("0.0.{}", patch + 1)
    }
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

    // --- version comparison ---------------------------------------------

    #[test]
    fn compare_versions_orders_by_component() {
        use std::cmp::Ordering;
        assert_eq!(compare_versions("1.1.0", "1.0.0"), Ordering::Greater);
        assert_eq!(compare_versions("1.0.0", "1.1.0"), Ordering::Less);
        assert_eq!(compare_versions("2.0.0", "1.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("1.0.0", "1.0.0"), Ordering::Equal);
    }

    #[test]
    fn compare_versions_treats_missing_components_as_zero() {
        use std::cmp::Ordering;
        assert_eq!(compare_versions("1.2", "1.2.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.2.1", "1.2"), Ordering::Greater);
    }

    #[test]
    fn compare_versions_numeric_not_lexical() {
        use std::cmp::Ordering;
        // Lexically "10" < "9"; numerically 10 > 9.
        assert_eq!(compare_versions("1.10.0", "1.9.0"), Ordering::Greater);
    }

    #[test]
    fn compare_versions_falls_back_to_lexical_for_non_numeric() {
        use std::cmp::Ordering;
        assert_eq!(compare_versions("1.0.0", "1.0.0-beta"), Ordering::Less);
        assert_eq!(
            compare_versions("1.0.0-rc2", "1.0.0-rc1"),
            Ordering::Greater
        );
    }

    // --- version requirements -------------------------------------------

    #[test]
    fn version_req_parses_each_supported_shape() {
        assert_eq!(
            VersionReq::parse("1.0.0"),
            Ok(VersionReq::Exact("1.0.0".into()))
        );
        assert_eq!(
            VersionReq::parse("=1.0.0"),
            Ok(VersionReq::Exact("1.0.0".into()))
        );
        assert_eq!(
            VersionReq::parse(">=1.0.0"),
            Ok(VersionReq::AtLeast("1.0.0".into()))
        );
        assert_eq!(
            VersionReq::parse("^1.0"),
            Ok(VersionReq::Caret("1.0".into()))
        );
        // Whitespace around the requirement and operator is tolerated.
        assert_eq!(
            VersionReq::parse("  >= 1.2.3 "),
            Ok(VersionReq::AtLeast("1.2.3".into()))
        );
    }

    #[test]
    fn version_req_exact_match() {
        let req = VersionReq::parse("1.0.0").unwrap();
        assert!(req.matches("1.0.0"));
        // `1.0` == `1.0.0` per compare_versions.
        assert!(req.matches("1.0"));
        assert!(!req.matches("1.0.1"));
        assert!(!req.matches("2.0.0"));
    }

    #[test]
    fn version_req_newer_compatible_version_matches() {
        // `>=` accepts anything at or above the bound.
        let at_least = VersionReq::parse(">=1.0.0").unwrap();
        assert!(at_least.matches("1.0.0"));
        assert!(at_least.matches("1.5.0"));
        assert!(at_least.matches("2.0.0"));

        // `^1.0` accepts newer 1.x releases (a newer *compatible* version).
        let caret = VersionReq::parse("^1.0").unwrap();
        assert!(caret.matches("1.0.0"));
        assert!(caret.matches("1.2.0"));
        assert!(caret.matches("1.99.99"));

        // Caret against a 0.x base is compatible only within the same minor.
        let caret_zero = VersionReq::parse("^0.2.3").unwrap();
        assert!(caret_zero.matches("0.2.3"));
        assert!(caret_zero.matches("0.2.9"));
    }

    #[test]
    fn version_req_incompatible_version_does_not_match() {
        // Below a `>=` bound.
        assert!(!VersionReq::parse(">=1.0.0").unwrap().matches("0.9.0"));

        // A caret bump past the compatible range.
        let caret = VersionReq::parse("^1.0").unwrap();
        assert!(!caret.matches("2.0.0"));
        assert!(!caret.matches("0.9.0"));

        // 0.x caret: a minor bump is incompatible.
        let caret_zero = VersionReq::parse("^0.2.3").unwrap();
        assert!(!caret_zero.matches("0.3.0"));
        assert!(!caret_zero.matches("0.2.2"));
    }

    #[test]
    fn version_req_invalid_requirements_are_errors() {
        assert_eq!(VersionReq::parse(""), Err(VersionReqError::Empty));
        assert_eq!(VersionReq::parse("   "), Err(VersionReqError::Empty));
        assert!(matches!(
            VersionReq::parse("^"),
            Err(VersionReqError::Invalid(_))
        ));
        assert!(matches!(
            VersionReq::parse(">="),
            Err(VersionReqError::Invalid(_))
        ));
        assert!(matches!(
            VersionReq::parse(">=abc"),
            Err(VersionReqError::Invalid(_))
        ));
        assert!(matches!(
            VersionReq::parse("1..0"),
            Err(VersionReqError::Invalid(_))
        ));
    }
}
