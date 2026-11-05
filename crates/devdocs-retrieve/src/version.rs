//! The version policy (docs/implementation/08 §Version resolution).
//!
//! The policy decides which indexed version serves a request and which
//! `version_status` the result carries. The declared rules:
//!
//! - `verified`: the served version equals the requested exact version.
//! - `alias`: `latest` or a major alias (`16`) resolved by the declared
//!   rule — the highest active version, or the highest active version
//!   within the requested major. A request with no version at all serves
//!   the highest active version under the same alias rule.
//! - `fallback`: the requested version is absent; the nearest available
//!   version serves. Same major first, closest version distance; when no
//!   version shares the major, the highest active version serves. Counts
//!   as a miss for grading.
//! - `absent`: the library has no active sources at all — the cold-miss
//!   path, exit 2.

use std::cmp::Ordering;

use devdocs_model::VersionStatus;

/// One exact version string as the store and the refs spell it: bare
/// (no leading `v`), dot-separated numeric components.
///
/// Ordering never derives from the inner string; it goes through
/// [`Version::cmp_version`] (and the free [`version_cmp`] on raw
/// `&str`s).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version(String);

impl Version {
    /// The version string as the store and the refs spell it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Compares two versions by their dotted numeric components,
    /// numerically and left to right, padding missing components with
    /// zero (`16` < `16.2` < `16.2.9`). Non-numeric suffixes compare
    /// lexically as a last resort.
    #[must_use]
    pub fn cmp_version(&self, other: &Version) -> Ordering {
        version_cmp(&self.0, &other.0)
    }
}

impl From<String> for Version {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Version {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

/// Compares two version strings by their dotted numeric components,
/// numerically and left to right, padding missing components with zero
/// (`16` < `16.2` < `16.2.9`). Non-numeric suffixes compare lexically as
/// a last resort.
#[must_use]
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    let mut left = a.split('.');
    let mut right = b.split('.');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(l), Some(r)) => {
                let ln = l
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<u64>()
                    .unwrap_or(0);
                let rn = r
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<u64>()
                    .unwrap_or(0);
                if ln != rn {
                    return ln.cmp(&rn);
                }
                // Numeric heads equal: a bare component outranks one with
                // a pre-release suffix (`1.0.0` > `1.0.0-beta`); two
                // suffixes compare lexically; equal components fall
                // through to the next pair.
                if l != r {
                    let l_suffix = &l[ln.to_string().len()..];
                    let r_suffix = &r[rn.to_string().len()..];
                    return match (l_suffix.is_empty(), r_suffix.is_empty()) {
                        // defensive: unreachable — equal numeric heads with
                        // both suffixes empty implies `l == r`, excluded by
                        // the `l != r` check above; the arm keeps the match
                        // total.
                        (true, true) => l.cmp(r),
                        (true, false) => Ordering::Greater,
                        (false, true) => Ordering::Less,
                        (false, false) => l_suffix.cmp(r_suffix),
                    };
                }
            }
        }
    }
}

/// Which version serves a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    /// The version that serves, when the library has active sources.
    pub version: Option<Version>,
    /// The status the served results carry.
    pub status: VersionStatus,
}

/// Picks the version that serves a request from the library's available
/// versions (descending version order is not required; the picker sorts).
/// The request arrives raw — it can be `latest`, a major alias, an exact
/// version, or absent.
#[must_use]
pub fn pick(available: &[Version], requested: Option<&str>) -> Served {
    if available.is_empty() {
        return Served {
            version: None,
            status: VersionStatus::Absent,
        };
    }
    let Some(requested) = requested else {
        // No version requested: the default rule serves the highest active
        // version, an alias resolution.
        let highest = highest(available);
        return Served {
            version: highest,
            status: VersionStatus::Alias,
        };
    };
    if requested.eq_ignore_ascii_case("latest") {
        return Served {
            version: highest(available),
            status: VersionStatus::Alias,
        };
    }
    if available.iter().any(|v| v.as_str() == requested) {
        return Served {
            version: Some(Version::from(requested)),
            status: VersionStatus::Verified,
        };
    }
    if is_major_alias(requested) {
        let same_major: Vec<Version> = available
            .iter()
            .filter(|v| major_of(v).as_deref() == Some(requested))
            .cloned()
            .collect();
        if !same_major.is_empty() {
            return Served {
                version: highest(&same_major),
                status: VersionStatus::Alias,
            };
        }
    }
    // Fallback: the nearest available version — same major first, closest
    // numeric distance; never silently `latest` (docs/08 §Version
    // resolution policy).
    Served {
        version: nearest(available, &Version::from(requested)),
        status: VersionStatus::Fallback,
    }
}

/// The highest version by [`Version::cmp_version`].
fn highest(available: &[Version]) -> Option<Version> {
    available.iter().max_by(|a, b| a.cmp_version(b)).cloned()
}

/// A major alias: one dotted component, all digits (`16`, not `16.2`).
fn is_major_alias(requested: &str) -> bool {
    requested.split('.').count() == 1 && requested.chars().all(|c| c.is_ascii_digit())
}

/// The leading numeric component of a version.
fn major_of(version: &Version) -> Option<String> {
    let head = version.as_str().split('.').next()?;
    if head.chars().all(|c| c.is_ascii_digit()) && !head.is_empty() {
        Some(head.to_owned())
    } else {
        None
    }
}

/// The nearest available version to a request: same major first, then the
/// closest numeric distance `|Δmajor|·10⁶ + |Δminor|·10³ + |Δpatch|` with
/// missing components read as zero; with no shared major the same
/// distance rule applies across majors, so the closest major serves.
fn nearest(available: &[Version], requested: &Version) -> Option<Version> {
    let req_major = major_of(requested).unwrap_or_default();
    let same_major: Vec<&Version> = available
        .iter()
        .filter(|v| major_of(v).as_deref() == Some(req_major.as_str()))
        .collect();
    let candidates: Vec<&Version> = if same_major.is_empty() {
        available.iter().collect()
    } else {
        same_major
    };
    candidates
        .into_iter()
        .min_by_key(|v| version_distance(v, requested))
        .cloned()
}

/// The numeric distance between two versions on their dotted components,
/// weighted per position (docs/implementation/08 §Version resolution:
/// "the nearest available version").
fn version_distance(candidate: &Version, requested: &Version) -> u64 {
    let mut cand = candidate.as_str().split('.').map(component_head);
    let mut req = requested.as_str().split('.').map(component_head);
    let mut distance: u64 = 0;
    let mut weight: u64 = 1_000_000;
    for _ in 0..3 {
        let c = cand.next().unwrap_or(0);
        let r = req.next().unwrap_or(0);
        distance = distance.saturating_add(c.abs_diff(r).saturating_mul(weight));
        weight /= 1_000;
    }
    distance
}

/// The numeric head of one dotted component.
fn component_head(component: &str) -> u64 {
    component
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

/// Whether the query text matches a migration question pattern
/// (docs/implementation/08 §Version resolution: "upgrade", "migrate",
/// "breaking changes").
#[must_use]
pub fn is_migration_query(text: &str) -> bool {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect::<String>()
                .to_lowercase()
        })
        .collect();
    words.iter().any(|w| {
        w.contains("upgrade")
            || w.contains("migrate")
            || w.contains("migration")
            || w == "breaking"
            || w.contains("breaking-changes")
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{Served, Version, is_major_alias, is_migration_query, pick, version_cmp};
    use devdocs_model::VersionStatus;
    use std::cmp::Ordering;

    fn versions(list: &[&str]) -> Vec<Version> {
        list.iter().map(|s| Version::from(*s)).collect()
    }

    /// Numeric component-wise comparison with zero padding.
    #[test]
    fn version_comparison_is_numeric() {
        assert_eq!(version_cmp("16", "16.2.9"), Ordering::Less);
        assert_eq!(version_cmp("16.2.9", "16"), Ordering::Greater);
        assert_eq!(version_cmp("16.2.9", "16.10.0"), Ordering::Less);
        assert_eq!(version_cmp("16.2.9", "16.2.9"), Ordering::Equal);
        assert_eq!(version_cmp("2.0", "10.0"), Ordering::Less);
        assert_eq!(version_cmp("1.0.0-beta", "1.0.0"), Ordering::Less);
        assert_eq!(version_cmp("1.0.0-beta", "1.0.0-rc"), Ordering::Less);
        assert_eq!(version_cmp("1.0.0-rc", "1.0.0-beta"), Ordering::Greater);
    }

    /// The newtype compares through the same rule as [`version_cmp`].
    #[test]
    fn the_newtype_compares_through_version_cmp() {
        assert_eq!(
            Version::from("16").cmp_version(&Version::from("16.2.9")),
            Ordering::Less
        );
        assert_eq!(
            Version::from("1.0.0").cmp_version(&Version::from("1.0.0-beta")),
            Ordering::Greater
        );
    }

    /// An exact request serves itself as `verified`.
    #[test]
    fn exact_requests_verify() {
        let available = versions(&["15.0.0", "16.2.9"]);
        assert_eq!(
            pick(&available, Some("16.2.9")),
            Served {
                version: Some(Version::from("16.2.9")),
                status: VersionStatus::Verified,
            }
        );
    }

    /// `latest` and the no-request default both serve the highest active
    /// version under the alias rule.
    #[test]
    fn latest_and_default_serve_the_highest() {
        let available = versions(&["15.0.0", "16.2.9"]);
        for request in [None, Some("latest"), Some("LATEST")] {
            assert_eq!(
                pick(&available, request),
                Served {
                    version: Some(Version::from("16.2.9")),
                    status: VersionStatus::Alias,
                }
            );
        }
    }

    /// A major alias serves the highest version within that major.
    #[test]
    fn major_aliases_serve_within_the_major() {
        let available = versions(&["15.0.0", "16.2.9", "16.2.10"]);
        assert_eq!(
            pick(&available, Some("16")),
            Served {
                version: Some(Version::from("16.2.10")),
                status: VersionStatus::Alias,
            }
        );
    }

    /// An absent version serves the nearest version as `fallback`, same
    /// major first.
    #[test]
    fn absent_versions_fall_back_to_the_nearest() {
        let available = versions(&["15.0.0", "16.2.9"]);
        assert_eq!(
            pick(&available, Some("16.2.10")),
            Served {
                version: Some(Version::from("16.2.9")),
                status: VersionStatus::Fallback,
            }
        );
        // No version shares the major: the distance rule applies across
        // majors, so the closest major serves — 15 is nearer to 9 than 16.
        assert_eq!(
            pick(&available, Some("9.0.0")),
            Served {
                version: Some(Version::from("15.0.0")),
                status: VersionStatus::Fallback,
            }
        );
        assert_eq!(
            pick(&available, Some("20.0.0")),
            Served {
                version: Some(Version::from("16.2.9")),
                status: VersionStatus::Fallback,
            }
        );
    }

    /// An empty index is the absent status.
    #[test]
    fn an_empty_index_is_absent() {
        assert_eq!(
            pick(&versions(&[]), Some("16.2.9")),
            Served {
                version: None,
                status: VersionStatus::Absent,
            }
        );
    }

    /// A major alias with no version in that major falls through to the
    /// nearest version as `fallback`.
    #[test]
    fn major_aliases_without_that_major_fall_back() {
        let available = versions(&["15.0.0"]);
        assert_eq!(
            pick(&available, Some("16")),
            Served {
                version: Some(Version::from("15.0.0")),
                status: VersionStatus::Fallback,
            }
        );
    }

    /// A request that is not even a numeric head falls back across the
    /// whole index — no available version shares its major.
    #[test]
    fn non_numeric_requests_fall_back_to_the_nearest() {
        let available = versions(&["1.0.0"]);
        assert_eq!(
            pick(&available, Some("beta")),
            Served {
                version: Some(Version::from("1.0.0")),
                status: VersionStatus::Fallback,
            }
        );
    }

    /// Major-alias detection accepts bare major numbers only.
    #[test]
    fn major_alias_detection() {
        assert!(is_major_alias("16"));
        assert!(is_major_alias("0"));
        assert!(!is_major_alias("16.2"));
        assert!(!is_major_alias("v16"));
        assert!(!is_major_alias("latest"));
    }

    /// Migration question patterns are detected case-insensitively.
    #[test]
    fn migration_patterns_match() {
        assert!(is_migration_query("How do I upgrade from v15?"));
        assert!(is_migration_query("migrate my app to 16"));
        assert!(is_migration_query("What are the breaking changes?"));
        assert!(!is_migration_query("how does routing work"));
    }
}
