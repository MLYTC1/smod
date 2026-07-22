//! The `smod.lock` counterpart to [`crate::config`].
//!
//! Mirrors the read/write pattern of `config.rs`, with one deliberate
//! difference: a *missing* lockfile is not an error. "No lockfile yet" and "no
//! packages installed yet" are the same thing, so [`read`] returns an empty
//! [`Lockfile`] in that case.
//!
//! This module is nearly self-contained: its only internal dependency is the
//! shared, pure [`validate_package_name`](crate::package::validate_package_name)
//! gate in `package.rs`, which [`read`] applies to every entry so that a
//! hand-edited `smod.lock` cannot smuggle in a name that escapes
//! `smod_modules/` when it is later used as a path.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::package::{validate_package_name, PackageNameError};

/// The lockfile filename.
pub const LOCKFILE_FILE: &str = "smod.lock";

/// Errors produced by the lockfile layer.
#[derive(Debug, Error)]
pub enum LockfileError {
    /// The lockfile exists but could not be read or written.
    #[error("i/o error for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The lockfile exists but is not valid TOML / does not match the schema.
    #[error("invalid lockfile at {path}: {message}")]
    InvalidLockfile { path: PathBuf, message: String },

    /// The lockfile contains an entry whose name is unsafe as a path component.
    #[error("lockfile at {path} contains an unsafe package name: {source}")]
    InvalidPackageName {
        path: PathBuf,
        #[source]
        source: PackageNameError,
    },
}

/// A single locked (installed) package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedPackage {
    /// Package name.
    pub name: String,
    /// The exact installed version.
    pub version: String,
    /// Checksum of the installed archive.
    pub checksum: String,
    /// RFC 3339 timestamp of when the package was installed.
    pub installed_at: String,
}

/// The parsed contents of `smod.lock`, serializing as repeated `[[packages]]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockfile {
    /// The locked packages, in insertion order.
    #[serde(default)]
    pub packages: Vec<LockedPackage>,
}

impl Lockfile {
    /// Look up a locked package by name.
    pub fn get(&self, name: &str) -> Option<&LockedPackage> {
        self.packages.iter().find(|p| p.name == name)
    }

    /// Insert `pkg`, or replace the existing entry with the same name in place.
    ///
    /// This is what guarantees re-installing a package updates its entry
    /// instead of appending a duplicate.
    pub fn upsert(&mut self, pkg: LockedPackage) {
        match self.packages.iter_mut().find(|p| p.name == pkg.name) {
            Some(existing) => *existing = pkg,
            None => self.packages.push(pkg),
        }
    }

    /// Remove a package by name, returning `true` if it was present.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.packages.len();
        self.packages.retain(|p| p.name != name);
        self.packages.len() != before
    }
}

/// Path to `smod.lock` inside `dir`.
pub fn lockfile_path_in(dir: &Path) -> PathBuf {
    dir.join(LOCKFILE_FILE)
}

/// Read the lockfile at `dir/smod.lock`, returning an empty lockfile if none
/// exists yet.
pub fn read(dir: &Path) -> Result<Lockfile, LockfileError> {
    let path = lockfile_path_in(dir);
    if !path.is_file() {
        return Ok(Lockfile::default());
    }
    let text = std::fs::read_to_string(&path).map_err(|source| LockfileError::Io {
        path: path.clone(),
        source,
    })?;
    let lockfile: Lockfile = toml::from_str(&text).map_err(|e| LockfileError::InvalidLockfile {
        path: path.clone(),
        message: e.to_string(),
    })?;

    // Defense in depth: reject any entry whose name could escape `smod_modules/`
    // when later used as a path (e.g. a maliciously hand-edited lockfile).
    for pkg in &lockfile.packages {
        validate_package_name(&pkg.name).map_err(|source| LockfileError::InvalidPackageName {
            path: path.clone(),
            source,
        })?;
    }

    Ok(lockfile)
}

/// Serialize and write `lockfile` to `dir/smod.lock`.
pub fn write(dir: &Path, lockfile: &Lockfile) -> Result<(), LockfileError> {
    let path = lockfile_path_in(dir);
    let text = toml::to_string_pretty(lockfile).map_err(|e| LockfileError::InvalidLockfile {
        path: path.clone(),
        message: e.to_string(),
    })?;
    std::fs::write(&path, text).map_err(|source| LockfileError::Io { path, source })
}

// ---------------------------------------------------------------------------
// Timestamps
//
// A tiny Unix-timestamp -> RFC 3339 formatter, so `installed_at` doesn't
// require pulling in a date/time crate. `civil_from_days` is Howard Hinnant's
// public-domain calendar algorithm (http://howardhinnant.github.io/date_algorithms.html).
// ---------------------------------------------------------------------------

/// The current time as an RFC 3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`).
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_rfc3339(secs)
}

/// Format a Unix timestamp (seconds since epoch) as an RFC 3339 UTC string.
pub fn format_rfc3339(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs_of_day = unix_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert a count of days since the Unix epoch (1970-01-01) into a
/// `(year, month, day)` civil date, using Howard Hinnant's algorithm.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn pkg(name: &str, version: &str) -> LockedPackage {
        LockedPackage {
            name: name.to_string(),
            version: version.to_string(),
            checksum: "abc123".to_string(),
            installed_at: "1970-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn missing_lockfile_reads_as_empty() {
        let tmp = TempDir::new().unwrap();
        let lock = read(tmp.path()).unwrap();
        assert!(lock.packages.is_empty());
    }

    #[test]
    fn write_then_read_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let mut lock = Lockfile::default();
        lock.upsert(pkg("token-vault", "1.0.0"));
        write(tmp.path(), &lock).unwrap();
        let read_back = read(tmp.path()).unwrap();
        assert_eq!(lock, read_back);
    }

    #[test]
    fn upsert_replaces_in_place_no_duplicates() {
        let mut lock = Lockfile::default();
        lock.upsert(pkg("token-vault", "1.0.0"));
        lock.upsert(pkg("token-vault", "2.0.0"));
        assert_eq!(lock.packages.len(), 1);
        assert_eq!(lock.get("token-vault").unwrap().version, "2.0.0");
    }

    #[test]
    fn remove_reports_presence() {
        let mut lock = Lockfile::default();
        lock.upsert(pkg("token-vault", "1.0.0"));
        assert!(lock.remove("token-vault"));
        assert!(!lock.remove("token-vault"));
    }

    #[test]
    fn invalid_lockfile_is_an_error() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(lockfile_path_in(tmp.path()), "not [ valid").unwrap();
        let err = read(tmp.path()).unwrap_err();
        assert!(matches!(err, LockfileError::InvalidLockfile { .. }));
    }

    #[test]
    fn read_rejects_malicious_package_names() {
        for bad in ["../evil", "../../outside", "/absolute/path", "..", "a/b"] {
            let tmp = TempDir::new().unwrap();
            let toml = format!(
                "[[packages]]\nname = {bad:?}\nversion = \"1.0.0\"\n\
                 checksum = \"x\"\ninstalled_at = \"1970-01-01T00:00:00Z\"\n"
            );
            std::fs::write(lockfile_path_in(tmp.path()), toml).unwrap();
            let err = read(tmp.path()).unwrap_err();
            assert!(
                matches!(err, LockfileError::InvalidPackageName { .. }),
                "name {bad:?} should be rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn read_accepts_valid_package_names() {
        let tmp = TempDir::new().unwrap();
        let mut lock = Lockfile::default();
        lock.upsert(pkg("payment-stream", "1.0.0"));
        lock.upsert(pkg("my_module", "2.0.0"));
        write(tmp.path(), &lock).unwrap();
        assert!(read(tmp.path()).is_ok());
    }

    // --- civil_from_days edge cases -------------------------------------

    #[test]
    fn epoch_is_1970_01_01() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn leap_day_2000() {
        // 2000-02-29 is day 11016 since epoch.
        let days = 30 * 365 + 7 /* leap years 1972..2000 */ + 31 + 28;
        assert_eq!(civil_from_days(days), (2000, 2, 29));
    }

    #[test]
    fn non_leap_february_1971() {
        // 1971-02-28
        assert_eq!(civil_from_days(365 + 31 + 27), (1971, 2, 28));
        // 1971-03-01 (no Feb 29 in 1971)
        assert_eq!(civil_from_days(365 + 31 + 28), (1971, 3, 1));
    }

    #[test]
    fn century_non_leap_1900_vs_leap_2000() {
        // 1900 is NOT a leap year; 2000 IS. Verify via pre-epoch date.
        // 1900-02-28 then 1900-03-01 (no Feb 29).
        let days_1900_02_28 = civil_days(1900, 2, 28);
        assert_eq!(civil_from_days(days_1900_02_28), (1900, 2, 28));
        assert_eq!(civil_from_days(days_1900_02_28 + 1), (1900, 3, 1));
    }

    #[test]
    fn pre_epoch_date() {
        // 1969-12-31 is day -1.
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn year_boundary() {
        assert_eq!(civil_from_days(364), (1970, 12, 31));
        assert_eq!(civil_from_days(365), (1971, 1, 1));
    }

    #[test]
    fn format_rfc3339_epoch() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        // A known instant: 2021-01-01T00:00:00Z == 1609459200.
        assert_eq!(format_rfc3339(1_609_459_200), "2021-01-01T00:00:00Z");
    }

    /// Inverse of `civil_from_days`, used only to build test fixtures.
    fn civil_days(y: i64, m: u32, d: u32) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
        let doy = (153 * mp + 2) / 5 + d as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }
}
