//! Telling people a new version of Weir is out. When they ask for it
//! ([`crate::Settings::update_check`]), the daemon asks GitHub for Weir's
//! newest release once each time it starts, and says what it found
//! ([`UpdateStatus`]). Weir never installs anything: it came by a package,
//! COPR, the AUR or a build, and each has its own way to update.

use serde::{Deserialize, Serialize};

/// A release of Weir, as GitHub lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Release {
    /// Its version, such as `1.3.0`.
    pub version: String,
    /// Its title.
    pub name: String,
    /// What is new in it, as Markdown.
    pub notes: String,
    /// Its page on GitHub, with the packages to download.
    pub url: String,
    /// When it was published, as GitHub writes it (`2026-10-20T18:04:11Z`).
    pub published_at: String,
}

/// What the daemon knows about Weir's newest release.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdateStatus {
    /// Whether a check is under way.
    #[serde(default)]
    pub checking: bool,
    /// When the last check ended, in milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at_ms: Option<u64>,
    /// The newest release the last check found. Releases whose packages
    /// are not up yet are passed over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<Release>,
    /// Whether `latest` is newer than the daemon.
    #[serde(default)]
    pub newer: bool,
    /// Why the last check failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How to update the way Weir was installed here, in words.
    #[serde(default)]
    pub how_to_update: String,
}

/// A version such as `1.3.0` or `v1.3.0` as numbers, for comparing.
/// `None` when it is not three numbers, as with `1.3.0-rc1`.
pub fn version_numbers(version: &str) -> Option<[u32; 3]> {
    let mut parts = version.trim().trim_start_matches('v').split('.');
    let mut out = [0; 3];
    for n in &mut out {
        *n = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// Whether `candidate` is a later version than `current`. A version that
/// cannot be read is never later.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (version_numbers(candidate), version_numbers(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert_eq!(version_numbers("v1.10.2"), Some([1, 10, 2]));
        assert!(is_newer("v1.10.0", "1.9.3"));
        assert!(is_newer("2.0.0", "1.99.99"));
        assert!(!is_newer("v1.3.0", "1.3.0"));
        assert!(!is_newer("1.2.9", "1.3.0"));
        assert!(!is_newer("1.4.0-rc1", "1.3.0"));
        assert!(!is_newer("1.4", "1.3.0"));
        assert!(!is_newer("1.4.0.1", "1.3.0"));
    }
}
