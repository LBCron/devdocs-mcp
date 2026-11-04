//! Version resolution (docs/implementation/08 §Version resolution).
//!
//! Precedence, highest first: explicit flags, project lockfiles, query-text
//! mentions, then the unfiltered fallback. Lockfile parsing is per the
//! docs/08 table; every parser is a pure function from file content to
//! `(library, version)` entries so tests hammer them without touching the
//! filesystem. A lockfile entry without a usable version still names the
//! library (the pyproject.toml case — the request without the pin).

use std::path::Path;
use std::path::PathBuf;

/// The query a command carries: the text plus the explicit flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// The raw query text.
    pub text: String,
    /// The `--library` flag value, when given.
    pub library: Option<String>,
    /// The `--version` flag value, when given.
    pub version: Option<String>,
}

impl Query {
    /// Builds a query from text and flags.
    #[must_use]
    pub fn new(text: &str, library: Option<&str>, version: Option<&str>) -> Self {
        Self {
            text: text.to_owned(),
            library: library.map(str::to_owned),
            version: version.map(str::to_owned),
        }
    }
}

/// One dependency entry a lockfile names: the library, and the pinned
/// version when the format carries one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dep {
    /// The library name.
    pub library: String,
    /// The pinned version, when the lockfile pins one.
    pub version: Option<String>,
}

/// The lockfile signals found in the project directory or its ancestors.
///
/// Detection stops at the first directory that carries any of the
/// docs/08 lockfile files, so a nested lockfile wins over a parent one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectContext {
    /// The entries from the detected lockfiles, in detection order.
    pub deps: Vec<Dep>,
    /// The lockfile files that produced the entries, for diagnostics.
    pub files: Vec<PathBuf>,
}

impl ProjectContext {
    /// An empty context: no lockfile signals.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Walks `start` and its ancestors and parses the first directory that
    /// carries any of the docs/08 lockfile files. Missing files and files
    /// that fail to parse are skipped silently — a lockfile is a weak
    /// signal, never a hard failure.
    #[must_use]
    pub fn detect(start: &Path) -> Self {
        const LOCKFILES: [&str; 6] = [
            "Cargo.lock",
            "package-lock.json",
            "pnpm-lock.yaml",
            "uv.lock",
            "pyproject.toml",
            "go.mod",
        ];
        for dir in start.ancestors() {
            let present: Vec<_> = LOCKFILES
                .iter()
                .map(|name| dir.join(name))
                .filter(|path| path.is_file())
                .collect();
            if present.is_empty() {
                continue;
            }
            let mut deps = Vec::new();
            for path in &present {
                let Ok(content) = std::fs::read_to_string(path) else {
                    continue;
                };
                let parsed = match path.file_name().and_then(|n| n.to_str()) {
                    Some("Cargo.lock") => parse_cargo_lock(&content),
                    Some("package-lock.json") => parse_package_lock(&content),
                    Some("pnpm-lock.yaml") => parse_pnpm_lock(&content),
                    Some("uv.lock") => parse_uv_lock(&content),
                    Some("pyproject.toml") => parse_pyproject(&content),
                    Some("go.mod") => parse_go_mod(&content),
                    _ => Vec::new(),
                };
                deps.extend(parsed);
            }
            if deps.is_empty() {
                continue;
            }
            return Self {
                deps,
                files: present,
            };
        }
        Self::empty()
    }
}

/// Where the resolved `(library, version)` came from
/// (docs/implementation/08 §Version resolution, order highest first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionSource {
    /// Explicit `--library` / `--version` flags.
    Flag,
    /// A project lockfile.
    Lockfile,
    /// A query-text mention, for example `next.js 16`.
    QueryMention,
    /// Nothing resolved: the search runs unfiltered over all approved
    /// sources.
    Unfiltered,
}

/// The resolution result (docs/implementation/13 §M4 sketch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The library the query scopes to, when one resolved.
    pub library: Option<String>,
    /// The version the query requests, when one resolved. This is the
    /// *requested* version; whether the index serves it is the version
    /// policy's decision, not the resolver's.
    pub version: Option<String>,
    /// Where the resolution came from.
    pub source: ResolutionSource,
}

/// Resolves the `(library, version)` a query should scope to.
///
/// Per-field precedence: a flag beats the lockfile in its field, and a
/// field the flags leave open falls to the lockfile — but a lockfile
/// entry never donates its version to a library the flag named unless
/// the entry is that library. The resolution source is the strongest
/// layer that contributed anything.
#[must_use]
pub fn resolve(query: &Query, ctx: &ProjectContext) -> Resolved {
    // The lockfile contributes only to a library the flag leaves open,
    // and only an entry that names the flagged library — an unrelated
    // lockfile entry never donates its version to a flagged library.
    let lockfile = if query.library.is_some() {
        ctx.deps.iter().find(|dep| {
            query
                .library
                .as_deref()
                .is_some_and(|flag| dep.library.eq_ignore_ascii_case(flag))
        })
    } else {
        ctx.deps.first()
    };

    let mut library = query
        .library
        .clone()
        .or_else(|| lockfile.map(|dep| dep.library.clone()));
    let mut version = query
        .version
        .clone()
        .or_else(|| lockfile.and_then(|dep| dep.version.clone()));

    let source = if query.library.is_some() || query.version.is_some() {
        ResolutionSource::Flag
    } else if lockfile.is_some() {
        ResolutionSource::Lockfile
    } else {
        match mention(&query.text) {
            Some(dep) => {
                library = Some(dep.library);
                version = dep.version;
                ResolutionSource::QueryMention
            }
            None => ResolutionSource::Unfiltered,
        }
    };

    Resolved {
        library,
        version,
        source,
    }
}

/// Extracts a weak `(library, version)` hint from the query text
/// (docs/implementation/08 §Version resolution: `next.js 16`).
///
/// Recognizes `name@version` and `name version` when the second token
/// looks like a version (a leading digit, optionally with a `v`). Names
/// with no version stay a library-only hint.
#[must_use]
fn mention(text: &str) -> Option<Dep> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    for (i, token) in tokens.iter().enumerate() {
        if let Some((name, version)) = token.split_once('@')
            && is_name(name)
            && is_version_shaped(version)
        {
            return Some(Dep {
                library: name.to_owned(),
                version: Some(version.to_owned()),
            });
        }
        let next = tokens.get(i + 1)?;
        if is_name(token) && is_version_shaped(next) {
            return Some(Dep {
                library: (*token).to_owned(),
                version: Some((*next).to_owned()),
            });
        }
    }
    None
}

/// A plausible library name: starts with a letter, carries no version
/// punctuation that would make it a version.
fn is_name(token: &str) -> bool {
    token.chars().next().is_some_and(char::is_alphabetic)
        && token.chars().all(|c| {
            c.is_alphanumeric() || c == '.' || c == '-' || c == '_' || c == '@' || c == '/'
        })
}

/// A version-shaped token: a leading digit, optionally behind a `v`.
fn is_version_shaped(token: &str) -> bool {
    let digits = token.strip_prefix('v').unwrap_or(token);
    digits.chars().next().is_some_and(|c| c.is_ascii_digit())
        && digits
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == '_')
}

/// Parses `Cargo.lock` content: `[[package]]` rows (docs/08 lockfile
/// table).
#[must_use]
pub fn parse_cargo_lock(content: &str) -> Vec<Dep> {
    parse_toml_packages(content)
}

/// Parses `uv.lock` content: `[[package]]` rows (docs/08 lockfile table).
#[must_use]
pub fn parse_uv_lock(content: &str) -> Vec<Dep> {
    parse_toml_packages(content)
}

/// The shared `[[package]] name/version` reader for the TOML lockfiles.
fn parse_toml_packages(content: &str) -> Vec<Dep> {
    #[derive(serde::Deserialize)]
    struct Lockfile {
        #[serde(default)]
        package: Vec<PackageRow>,
    }
    #[derive(serde::Deserialize)]
    struct PackageRow {
        name: Option<String>,
        version: Option<String>,
    }
    let Ok(lock) = toml::from_str::<Lockfile>(content) else {
        return Vec::new();
    };
    lock.package
        .into_iter()
        .filter_map(|row| {
            let name = row.name.as_deref().map(normalize_crate_name)??;
            Some(Dep {
                library: name,
                version: row.version,
            })
        })
        .collect()
}

/// Cargo crate names normalize `_` to `-`; the index keys libraries by the
/// normalized spelling.
fn normalize_crate_name(name: &str) -> Option<String> {
    let normalized = name.replace('_', "-");
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

/// Parses `package-lock.json` content: the `packages` map with
/// `node_modules/`-prefixed keys, plus the legacy top-level `dependencies`
/// map (docs/08 lockfile table).
#[must_use]
pub fn parse_package_lock(content: &str) -> Vec<Dep> {
    #[derive(serde::Deserialize)]
    struct Lockfile {
        #[serde(default)]
        packages: std::collections::BTreeMap<String, PackageRow>,
        #[serde(default)]
        dependencies: std::collections::BTreeMap<String, PackageRow>,
    }
    #[derive(serde::Deserialize)]
    struct PackageRow {
        version: Option<String>,
    }
    let Ok(lock) = serde_json::from_str::<Lockfile>(content) else {
        return Vec::new();
    };
    let mut deps: Vec<Dep> = lock
        .packages
        .into_iter()
        .filter_map(|(key, row)| {
            // The root entry ("") and non-package keys carry no install
            // name; only the `node_modules/` keys name dependencies.
            let name = key.strip_prefix("node_modules/")?;
            Some(Dep {
                library: name.to_owned(),
                version: row.version,
            })
        })
        .collect();
    // Legacy v1 lockfiles name dependencies only under the top map.
    if deps.is_empty() {
        deps.extend(lock.dependencies.into_iter().map(|(name, row)| Dep {
            library: name,
            version: row.version,
        }));
    }
    deps
}

/// Parses `pnpm-lock.yaml` content: the `packages` section keys in the
/// `name@version` shape, with peer-suffixes stripped (docs/08 lockfile
/// table).
#[must_use]
pub fn parse_pnpm_lock(content: &str) -> Vec<Dep> {
    #[derive(serde::Deserialize)]
    struct Lockfile {
        #[serde(default)]
        packages: std::collections::BTreeMap<String, serde_yaml_ng::Value>,
    }
    let Ok(lock) = serde_yaml_ng::from_str::<Lockfile>(content) else {
        return Vec::new();
    };
    lock.packages
        .keys()
        .filter_map(|key| {
            let entry = key.trim_end_matches(':').split('(').next()?;
            let (name, version) = split_name_version(entry)?;
            Some(Dep {
                library: name.to_owned(),
                version: Some(version.to_owned()),
            })
        })
        .collect()
}

/// Splits `name@version` (scoped `@scope/name@version` included).
fn split_name_version(entry: &str) -> Option<(&str, &str)> {
    let at = if let Some(rest) = entry.strip_prefix('@') {
        rest.find('@').map(|i| i + 1)?
    } else {
        entry.find('@')?
    };
    let (name, version) = entry.split_at(at);
    let version = version.strip_prefix('@')?;
    if name.is_empty() || version.is_empty() {
        return None;
    }
    Some((name, version))
}

/// Parses `pyproject.toml` content: `[project] dependencies` requirement
/// strings — the request without the pin (docs/08 lockfile table).
#[must_use]
pub fn parse_pyproject(content: &str) -> Vec<Dep> {
    #[derive(Default, serde::Deserialize)]
    struct Project {
        #[serde(default)]
        dependencies: Vec<String>,
    }
    #[derive(serde::Deserialize)]
    struct Pyproject {
        #[serde(default)]
        project: Project,
    }
    let Ok(file) = toml::from_str::<Pyproject>(content) else {
        return Vec::new();
    };
    file.project
        .dependencies
        .iter()
        .filter_map(|req| {
            // Requirement strings: `name`, `name>=1`, `name[extra]>=1`,
            // `name @ url`. The name ends at the first specifier char.
            let end = req
                .find(['<', '>', '=', '!', '~', ';', '[', '@', ' '])
                .unwrap_or(req.len());
            let name = req[..end].trim().to_owned();
            if name.is_empty() {
                return None;
            }
            // PEP 503 normalization: lowercase, runs of `-_.` to `-`.
            let normalized = name
                .to_lowercase()
                .split(['-', '_', '.'])
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("-");
            Some(Dep {
                library: normalized,
                version: None,
            })
        })
        .collect()
}

/// Parses `go.mod` content: `require` lines and `require (...)` blocks
/// (docs/08 lockfile table). The leading `v` of Go versions is stripped so
/// the pin matches the index's bare version spelling.
#[must_use]
pub fn parse_go_mod(content: &str) -> Vec<Dep> {
    let mut deps = Vec::new();
    let mut in_block = false;
    for line in content.lines() {
        let line = line.split("//").next().unwrap_or(line).trim();
        if in_block {
            if line == ")" {
                in_block = false;
            } else if let Some(dep) = require_dep(line) {
                deps.push(dep);
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("require (") {
            if rest.trim().is_empty() || rest.trim() == ")" {
                in_block = rest.trim().is_empty();
            } else if let Some(dep) = require_dep(rest.trim().trim_end_matches(')').trim()) {
                deps.push(dep);
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("require ")
            && let Some(dep) = require_dep(rest.trim())
        {
            deps.push(dep);
        }
    }
    deps
}

/// Parses one `module vX.Y.Z` require entry.
fn require_dep(line: &str) -> Option<Dep> {
    let mut parts = line.split_whitespace();
    let module = parts.next()?;
    let version = parts.next().and_then(|v| v.strip_prefix('v'))?;
    if !is_name(module) || version.is_empty() {
        return None;
    }
    Some(Dep {
        library: module.to_owned(),
        version: Some(version.to_owned()),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{
        Dep, ProjectContext, Query, ResolutionSource, parse_cargo_lock, parse_go_mod,
        parse_package_lock, parse_pnpm_lock, parse_pyproject, parse_uv_lock, resolve,
    };

    /// Cargo.lock rows become library/version entries with normalized
    /// names.
    #[test]
    fn cargo_lock_rows_become_entries() {
        let content = "[[package]]\nname = \"serde\"\nversion = \"1.0.229\"\n\n\
                       [[package]]\nname = \"serde_json\"\nversion = \"1.0.154\"\n";
        assert_eq!(
            parse_cargo_lock(content),
            vec![
                Dep {
                    library: "serde".to_owned(),
                    version: Some("1.0.229".to_owned()),
                },
                Dep {
                    library: "serde-json".to_owned(),
                    version: Some("1.0.154".to_owned()),
                },
            ]
        );
    }

    /// A Cargo.lock entry without a usable name is skipped, not an error.
    #[test]
    fn cargo_lock_rows_without_names_are_skipped() {
        assert_eq!(parse_cargo_lock("garbage {"), Vec::new());
        assert_eq!(
            parse_cargo_lock("[[package]]\nversion = \"1.0\"\n"),
            Vec::new()
        );
    }

    /// package-lock.json v3: the `packages` map keys strip their
    /// `node_modules/` prefix; the root entry is skipped.
    #[test]
    fn package_lock_v3_strips_node_modules() {
        let content = r#"{
            "name": "app", "version": "1.0.0",
            "packages": {
                "": {"name": "app", "version": "1.0.0"},
                "node_modules/next": {"version": "16.2.9"},
                "node_modules/next/node_modules/react": {"version": "18.3.1"}
            }
        }"#;
        assert_eq!(
            parse_package_lock(content),
            vec![
                Dep {
                    library: "next".to_owned(),
                    version: Some("16.2.9".to_owned()),
                },
                Dep {
                    library: "next/node_modules/react".to_owned(),
                    version: Some("18.3.1".to_owned()),
                },
            ]
        );
    }

    /// package-lock.json v1 (legacy): the top-level `dependencies` map
    /// names the dependencies when `packages` is absent.
    #[test]
    fn package_lock_v1_falls_back_to_dependencies() {
        let content = r#"{
            "name": "app", "version": "1.0.0",
            "dependencies": {"react": {"version": "18.3.1"}}
        }"#;
        assert_eq!(
            parse_package_lock(content),
            vec![Dep {
                library: "react".to_owned(),
                version: Some("18.3.1".to_owned()),
            }]
        );
    }

    /// pnpm-lock.yaml: the `packages` keys split at the version `@`, with
    /// peer suffixes and scoped names handled.
    #[test]
    fn pnpm_lock_keys_split_at_version() {
        let content = "packages:\n\
                       \x20 'next@16.2.9': {}\n\
                       \x20 '@babel/core@7.26.0': {}\n\
                       \x20 'react@18.3.1(react-dom@18.3.1)': {}\n\
                       importers:\n\
                       \x20 '.': {}\n";
        // BTreeMap key order: `@babel/core` sorts before `next`.
        assert_eq!(
            parse_pnpm_lock(content),
            vec![
                Dep {
                    library: "@babel/core".to_owned(),
                    version: Some("7.26.0".to_owned()),
                },
                Dep {
                    library: "next".to_owned(),
                    version: Some("16.2.9".to_owned()),
                },
                Dep {
                    library: "react".to_owned(),
                    version: Some("18.3.1".to_owned()),
                },
            ]
        );
    }

    /// uv.lock rows parse like Cargo.lock rows.
    #[test]
    fn uv_lock_rows_become_entries() {
        let content = "[[package]]\nname = \"click\"\nversion = \"8.1.8\"\n";
        assert_eq!(
            parse_uv_lock(content),
            vec![Dep {
                library: "click".to_owned(),
                version: Some("8.1.8".to_owned()),
            }]
        );
    }

    /// pyproject.toml dependencies name libraries without pins; PEP 503
    /// normalization applies.
    #[test]
    fn pyproject_dependencies_carry_no_pin() {
        let content = "[project]\nname = \"app\"\ndependencies = [\n  \"Fast_API>=0.1\",\n  \"uvicorn[standard]\",\n  \"httpx @ https://example.test/httpx.whl\",\n]\n";
        assert_eq!(
            parse_pyproject(content),
            vec![
                Dep {
                    library: "fast-api".to_owned(),
                    version: None,
                },
                Dep {
                    library: "uvicorn".to_owned(),
                    version: None,
                },
                Dep {
                    library: "httpx".to_owned(),
                    version: None,
                },
            ]
        );
    }

    /// go.mod require lines and blocks parse; the leading `v` drops and
    /// `// indirect` comments are ignored.
    #[test]
    fn go_mod_requires_parse() {
        let content = "module example.test/app\n\ngo 1.24\n\n\
                       require github.com/spf13/cobra v1.9.1\n\n\
                       require (\n\tgolang.org/x/text v0.23.0 // indirect\n\t\
                       gopkg.in/yaml.v3 v3.0.1\n)\n";
        assert_eq!(
            parse_go_mod(content),
            vec![
                Dep {
                    library: "github.com/spf13/cobra".to_owned(),
                    version: Some("1.9.1".to_owned()),
                },
                Dep {
                    library: "golang.org/x/text".to_owned(),
                    version: Some("0.23.0".to_owned()),
                },
                Dep {
                    library: "gopkg.in/yaml.v3".to_owned(),
                    version: Some("3.0.1".to_owned()),
                },
            ]
        );
    }

    /// Flags win over lockfiles; lockfiles win over query mentions.
    #[test]
    fn precedence_flag_over_lockfile_over_mention() {
        let ctx = ProjectContext {
            deps: vec![Dep {
                library: "next.js".to_owned(),
                version: Some("16.2.9".to_owned()),
            }],
            files: Vec::new(),
        };

        // Flag beats the lockfile in the field the flag fills; an
        // unrelated lockfile entry never donates its version to a flagged
        // library.
        let flag = resolve(&Query::new("routing", Some("react"), None), &ctx);
        assert_eq!(flag.library.as_deref(), Some("react"));
        assert_eq!(flag.version, None, "no lockfile entry names react");
        assert_eq!(flag.source, ResolutionSource::Flag);

        // A lockfile entry that names the flagged library still pins it.
        let matched = resolve(&Query::new("routing", Some("next.js"), None), &ctx);
        assert_eq!(matched.library.as_deref(), Some("next.js"));
        assert_eq!(matched.version.as_deref(), Some("16.2.9"));
        assert_eq!(matched.source, ResolutionSource::Flag);

        // Lockfile wins over a query mention.
        let lockfile = resolve(&Query::new("react 18 routing", None, None), &ctx);
        assert_eq!(lockfile.library.as_deref(), Some("next.js"));
        assert_eq!(lockfile.source, ResolutionSource::Lockfile);

        // The query mention only fires when nothing above it resolved.
        let empty = ProjectContext::empty();
        let mention = resolve(&Query::new("next.js 16 routing", None, None), &empty);
        assert_eq!(mention.library.as_deref(), Some("next.js"));
        assert_eq!(mention.version.as_deref(), Some("16"));
        assert_eq!(mention.source, ResolutionSource::QueryMention);

        // Nothing resolved: unfiltered.
        let none = resolve(&Query::new("routing middleware", None, None), &empty);
        assert_eq!(none.library, None);
        assert_eq!(none.version, None);
        assert_eq!(none.source, ResolutionSource::Unfiltered);
    }

    /// A `name@version` mention resolves the same way as the spaced form.
    #[test]
    fn at_mentions_resolve() {
        let empty = ProjectContext::empty();
        let resolved = resolve(&Query::new("zod@4.6.7 parse", None, None), &empty);
        assert_eq!(resolved.library.as_deref(), Some("zod"));
        assert_eq!(resolved.version.as_deref(), Some("4.6.7"));
    }

    /// A flag version pairs with a lockfile library when the flag leaves
    /// the library open.
    #[test]
    fn flag_version_pairs_with_lockfile_library() {
        let ctx = ProjectContext {
            deps: vec![Dep {
                library: "next.js".to_owned(),
                version: Some("16.2.9".to_owned()),
            }],
            files: Vec::new(),
        };
        let resolved = resolve(&Query::new("routing", None, Some("15.0.0")), &ctx);
        assert_eq!(resolved.library.as_deref(), Some("next.js"));
        assert_eq!(resolved.version.as_deref(), Some("15.0.0"));
        assert_eq!(resolved.source, ResolutionSource::Flag);
    }
}
