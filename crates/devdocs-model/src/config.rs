//! The layered configuration (docs/implementation/05 §Configuration).
//!
//! Precedence, highest first: flags > `DEVDOCS_*` environment > project
//! `devdocs.toml` (cwd or a parent) > user `config.toml`
//! (`$XDG_CONFIG_HOME/devdocs/config.toml`, macOS
//! `~/Library/Application Support/devdocs/config.toml`) > built-in defaults.
//!
//! Each layer parses into a partial view whose fields are `Option`. The
//! partial view uses `deny_unknown_fields`, so an unknown key fails
//! validation with code `CONFIG_INVALID` and exit 1.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::Error;

/// The project file name searched for in the cwd and its parents.
pub const PROJECT_FILE: &str = "devdocs.toml";

/// Command-line flags, the highest precedence layer.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    /// `--config <path>`: replaces the project-file layer.
    pub config_path: Option<PathBuf>,
    /// `--db <path>`: overrides the index database path.
    pub db_path: Option<PathBuf>,
    /// `--max-tokens <n>`: overrides `search.max_tokens_default`.
    pub max_tokens: Option<u64>,
    /// `--timeout-ms <n>`: overrides `fetch.timeout_ms`.
    pub timeout_ms: Option<u64>,
}

/// `DEVDOCS_*` variables this module reads. Names follow the `DEVDOCS_*`
/// prefix rule in docs/implementation/05 §Configuration.
pub mod env_vars {
    /// Overrides the data root on every platform; also overrides
    /// `[index] path` in the config files.
    pub const HOME: &str = "DEVDOCS_HOME";
    /// Maps to `fetch.concurrency`.
    pub const FETCH_CONCURRENCY: &str = "DEVDOCS_FETCH_CONCURRENCY";
    /// Maps to `fetch.timeout_ms`.
    pub const FETCH_TIMEOUT_MS: &str = "DEVDOCS_FETCH_TIMEOUT_MS";
    /// Maps to `fetch.max_bytes`.
    pub const FETCH_MAX_BYTES: &str = "DEVDOCS_FETCH_MAX_BYTES";
    /// Maps to `fetch.max_redirects`.
    pub const FETCH_MAX_REDIRECTS: &str = "DEVDOCS_FETCH_MAX_REDIRECTS";
    /// Maps to `fetch.user_agent`.
    pub const FETCH_USER_AGENT: &str = "DEVDOCS_FETCH_USER_AGENT";
    /// Maps to `index.path`.
    pub const INDEX_PATH: &str = "DEVDOCS_INDEX_PATH";
    /// Maps to `index.embeddings`.
    pub const INDEX_EMBEDDINGS: &str = "DEVDOCS_INDEX_EMBEDDINGS";
    /// Maps to `search.max_tokens_default`.
    pub const SEARCH_MAX_TOKENS: &str = "DEVDOCS_SEARCH_MAX_TOKENS_DEFAULT";
    /// Maps to `search.limit_default`.
    pub const SEARCH_LIMIT: &str = "DEVDOCS_SEARCH_LIMIT_DEFAULT";
    /// Maps to `search.rrf_k`.
    pub const SEARCH_RRF_K: &str = "DEVDOCS_SEARCH_RRF_K";
    /// Maps to `ingest.git_extensions` as a comma-separated list.
    pub const INGEST_GIT_EXTENSIONS: &str = "DEVDOCS_INGEST_GIT_EXTENSIONS";
}

/// The built-in default for `ingest.git_extensions`.
pub const DEFAULT_GIT_EXTENSIONS: &[&str] =
    &["md", "mdx", "rst", "adoc", "txt", "json", "yaml", "yml"];

/// The resolved paths [`Config::load`] reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Paths {
    /// The user config file. May not exist.
    pub user_config: PathBuf,
    /// The project config file, when one was found. May not exist.
    pub project_config: Option<PathBuf>,
    /// The data root (`$DEVDOCS_HOME`, else the platform data dir).
    pub data_root: PathBuf,
    /// The index database file.
    pub index_db: PathBuf,
}

/// The resolved, merged configuration
/// (docs/implementation/05 §Configuration schema).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Config {
    /// Fetch behavior.
    pub fetch: FetchConfig,
    /// Index and storage behavior.
    pub index: IndexConfig,
    /// Search behavior.
    pub search: SearchConfig,
    /// Ingest behavior.
    pub ingest: IngestConfig,
    /// The license registry.
    pub sources: BTreeMap<String, SourceConfig>,
}

/// Fetch behavior.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FetchConfig {
    /// Concurrent fetches. Default 4.
    pub concurrency: u32,
    /// Per-response timeout in milliseconds. Default 15000.
    pub timeout_ms: u64,
    /// Max bytes per response. Default 20 MiB.
    pub max_bytes: u64,
    /// Max redirects per response. Default 3.
    pub max_redirects: u32,
    /// The User-Agent string.
    pub user_agent: String,
}

/// Index and storage behavior.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IndexConfig {
    /// The SQLite database file.
    pub path: PathBuf,
    /// Whether embeddings are computed at ingest. Default false.
    pub embeddings: bool,
}

/// Search behavior.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchConfig {
    /// Default `--max-tokens` for read paths. Default 2000.
    pub max_tokens_default: u64,
    /// Default `--limit`. Default 8.
    pub limit_default: u32,
    /// The RRF fusion constant. Default 60.
    pub rrf_k: u32,
}

/// Ingest behavior.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IngestConfig {
    /// The file extensions the git adapter may ingest. Default
    /// [`DEFAULT_GIT_EXTENSIONS`].
    pub git_extensions: Vec<String>,
}

/// Partial `[ingest]`.
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PartialIngest {
    /// The file extensions the git adapter may ingest.
    pub git_extensions: Option<Vec<String>>,
}

/// One license-registry entry under `[sources.<name>]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SourceConfig {
    /// The source kind, for example `llms_txt`.
    pub kind: Option<String>,
    /// The source URL.
    pub url: Option<String>,
    /// The license identifier, for example `MIT`.
    pub license: Option<String>,
    /// The license status, for example `approved_snippet`.
    pub status: Option<String>,
    /// Refresh cadence in days.
    pub refresh_days: Option<u32>,
}

impl SourceConfig {
    /// The parsed source URL, when set.
    ///
    /// # Errors
    /// When `url` is set but does not parse as a URL.
    pub fn parsed_url(&self, key_path: &str) -> Result<Option<url::Url>, Error> {
        self.url.as_deref().map_or(Ok(None), |raw| {
            url::Url::parse(raw)
                .map(Some)
                .map_err(|e| Error::ConfigInvalid {
                    key_path: format!("{key_path}.url"),
                    reason: e.to_string(),
                })
        })
    }
}

/// A config-file layer: the partial view that every layer parses into.
///
/// Unknown keys fail validation through `deny_unknown_fields`.
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PartialConfig {
    /// Fetch section.
    pub fetch: PartialFetch,
    /// Index section.
    pub index: PartialIndex,
    /// Search section.
    pub search: PartialSearch,
    /// Ingest section.
    pub ingest: PartialIngest,
    /// License-registry entries.
    pub sources: BTreeMap<String, SourceConfig>,
}

/// Partial `[fetch]`.
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PartialFetch {
    /// Concurrent fetches.
    pub concurrency: Option<u32>,
    /// Per-response timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Max bytes per response.
    pub max_bytes: Option<u64>,
    /// Max redirects per response.
    pub max_redirects: Option<u32>,
    /// The User-Agent string.
    pub user_agent: Option<String>,
}

/// Partial `[index]`.
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PartialIndex {
    /// The SQLite database file.
    pub path: Option<String>,
    /// Whether embeddings are computed at ingest.
    pub embeddings: Option<bool>,
}

/// Partial `[search]`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PartialSearch {
    /// Default `--max-tokens` for read paths.
    pub max_tokens_default: Option<u64>,
    /// Default `--limit`.
    pub limit_default: Option<u32>,
    /// The RRF fusion constant.
    pub rrf_k: Option<u32>,
}

impl PartialFetch {
    /// Fold a higher-precedence layer over this one.
    fn merge(&mut self, over: Self) {
        let Self {
            concurrency,
            timeout_ms,
            max_bytes,
            max_redirects,
            user_agent,
        } = over;
        self.concurrency = concurrency.or(self.concurrency);
        self.timeout_ms = timeout_ms.or(self.timeout_ms);
        self.max_bytes = max_bytes.or(self.max_bytes);
        self.max_redirects = max_redirects.or(self.max_redirects);
        self.user_agent = user_agent.or(self.user_agent.take());
    }
}

impl PartialIndex {
    /// Fold a higher-precedence layer over this one.
    fn merge(&mut self, over: Self) {
        let Self { path, embeddings } = over;
        self.path = path.or(self.path.take());
        self.embeddings = embeddings.or(self.embeddings);
    }
}

impl PartialSearch {
    /// Fold a higher-precedence layer over this one.
    fn merge(&mut self, over: Self) {
        let Self {
            max_tokens_default,
            limit_default,
            rrf_k,
        } = over;
        self.max_tokens_default = max_tokens_default.or(self.max_tokens_default.take());
        self.limit_default = limit_default.or(self.limit_default.take());
        self.rrf_k = rrf_k.or(self.rrf_k.take());
    }
}

impl PartialIngest {
    /// Fold a higher-precedence layer over this one.
    fn merge(&mut self, over: Self) {
        let Self { git_extensions } = over;
        self.git_extensions = git_extensions.or(self.git_extensions.take());
    }
}

impl PartialConfig {
    /// Fold a higher-precedence layer over this one. Source entries merge
    /// per name: the higher layer wins whole entries.
    fn merge(&mut self, over: Self) {
        let Self {
            fetch,
            index,
            search,
            ingest,
            sources,
        } = over;
        self.fetch.merge(fetch);
        self.index.merge(index);
        self.search.merge(search);
        self.ingest.merge(ingest);
        self.sources.extend(sources);
    }

    /// Parse a config file into the partial view.
    ///
    /// The parse walks the top-level table by name, so an unknown section
    /// or key fails with a structural key path (`fetch.conkurrency`,
    /// `sources.nextjs.urll`).
    ///
    /// # Errors
    /// [`Error::ConfigInvalid`] when the file is not valid TOML or carries
    /// an unknown key; the error names the failing key path.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let table: toml::Table = toml::from_str(text).map_err(|e| syntax_error(e.to_string()))?;
        let mut partial = Self::default();
        for (key, value) in table {
            match key.as_str() {
                "fetch" => partial.fetch = parse_section("fetch", value)?,
                "index" => partial.index = parse_section("index", value)?,
                "search" => partial.search = parse_section("search", value)?,
                "ingest" => partial.ingest = parse_section("ingest", value)?,
                "sources" => {
                    let toml::Value::Table(entries) = value else {
                        return Err(leaf_error(
                            "sources",
                            "expected a table of named sources".to_owned(),
                        ));
                    };
                    for (name, entry) in entries {
                        let source = parse_section(&format!("sources.{name}"), entry)?;
                        partial.sources.insert(name, source);
                    }
                }
                other => {
                    return Err(Error::ConfigInvalid {
                        key_path: other.to_owned(),
                        reason: format!(
                            "unknown section `{other}`; expected fetch, index, search, ingest, or sources"
                        ),
                    });
                }
            }
        }
        Ok(partial)
    }

    /// Build the environment layer by reading `DEVDOCS_*` variables.
    ///
    /// # Errors
    /// [`Error::ConfigInvalid`] when a set variable does not parse into its
    /// target type; the error names the variable.
    pub fn from_env() -> Result<Self, Error> {
        use env_vars::{
            FETCH_CONCURRENCY, FETCH_MAX_BYTES, FETCH_MAX_REDIRECTS, FETCH_TIMEOUT_MS,
            FETCH_USER_AGENT, INDEX_EMBEDDINGS, INDEX_PATH, INGEST_GIT_EXTENSIONS, SEARCH_LIMIT,
            SEARCH_MAX_TOKENS, SEARCH_RRF_K,
        };
        let mut layer = Self::default();
        let mut fetch = PartialFetch::default();
        let mut index = PartialIndex::default();
        let mut search = PartialSearch::default();
        let mut ingest = PartialIngest::default();

        for (var, target) in [
            (FETCH_CONCURRENCY, EnvTarget::U32(&mut fetch.concurrency)),
            (FETCH_TIMEOUT_MS, EnvTarget::U64(&mut fetch.timeout_ms)),
            (FETCH_MAX_BYTES, EnvTarget::U64(&mut fetch.max_bytes)),
            (
                FETCH_MAX_REDIRECTS,
                EnvTarget::U32(&mut fetch.max_redirects),
            ),
        ] {
            apply_num(var, target)?;
        }
        if let Some(agent) = env_string(FETCH_USER_AGENT)? {
            fetch.user_agent = Some(agent);
        }
        if let Some(path) = env_string(INDEX_PATH)? {
            index.path = Some(path);
        }
        if let Some(embeddings) = env_string(INDEX_EMBEDDINGS)? {
            index.embeddings = Some(embeddings.parse().map_err(|_| Error::ConfigInvalid {
                key_path: INDEX_EMBEDDINGS.to_owned(),
                reason: format!("expected `true` or `false`, got `{embeddings}`"),
            })?);
        }
        for (var, target) in [
            (
                SEARCH_MAX_TOKENS,
                EnvTarget::U64(&mut search.max_tokens_default),
            ),
            (SEARCH_LIMIT, EnvTarget::U32(&mut search.limit_default)),
            (SEARCH_RRF_K, EnvTarget::U32(&mut search.rrf_k)),
        ] {
            apply_num(var, target)?;
        }

        if let Some(raw) = env_string(INGEST_GIT_EXTENSIONS)? {
            ingest.git_extensions = Some(parse_git_extensions(&raw));
        }

        layer.fetch = fetch;
        layer.index = index;
        layer.search = search;
        layer.ingest = ingest;
        Ok(layer)
    }

    /// Build the flag layer from the command-line overrides.
    #[must_use]
    pub fn from_overrides(over: &Overrides) -> Self {
        let mut layer = Self::default();
        layer.fetch.timeout_ms = over.timeout_ms;
        layer.index.path = over
            .db_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());
        layer.search.max_tokens_default = over.max_tokens;
        layer
    }
}

enum EnvTarget<'a> {
    U32(&'a mut Option<u32>),
    U64(&'a mut Option<u64>),
}

fn apply_num(var: &str, target: EnvTarget<'_>) -> Result<(), Error> {
    let Some(raw) = env_string(var)? else {
        return Ok(());
    };
    let parsed = match target {
        EnvTarget::U32(slot) => raw.parse::<u32>().map(|v| *slot = Some(v)),
        EnvTarget::U64(slot) => raw.parse::<u64>().map(|v| *slot = Some(v)),
    };
    parsed.map_err(|reason| Error::ConfigInvalid {
        key_path: var.to_owned(),
        reason: reason.to_string(),
    })
}

/// Read one environment variable. A set but non-UTF-8 value is a config
/// error, not silence: a dropped `DEVDOCS_INDEX_PATH` would make the CLI
/// report the wrong database.
///
/// # Errors
/// [`Error::ConfigInvalid`] when the value is not valid UTF-8.
fn env_string(var: &str) -> Result<Option<String>, Error> {
    match std::env::var_os(var) {
        None => Ok(None),
        Some(raw) => coerce_utf8(var, raw),
    }
}

/// Split a raw `DEVDOCS_INGEST_GIT_EXTENSIONS` value into entries:
/// comma-separated, trimmed, empty entries dropped.
fn parse_git_extensions(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|ext| !ext.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Convert a set value to UTF-8; empty values count as unset.
///
/// # Errors
/// [`Error::ConfigInvalid`] when the value is not valid UTF-8.
fn coerce_utf8(var: &str, raw: std::ffi::OsString) -> Result<Option<String>, Error> {
    if raw.is_empty() {
        return Ok(None);
    }
    raw.into_string()
        .map(Some)
        .map_err(|_| Error::ConfigInvalid {
            key_path: var.to_owned(),
            reason: "expected a valid UTF-8 value".to_owned(),
        })
}

/// Read and parse one config layer file.
fn read_layer(path: &Path) -> Result<PartialConfig, Error> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::ConfigInvalid {
        key_path: path.display().to_string(),
        reason: e.to_string(),
    })?;
    PartialConfig::parse(&text)
}

/// Parse one known section from its raw TOML value.
fn parse_section<T: DeserializeOwned>(name: &str, value: toml::Value) -> Result<T, Error> {
    T::deserialize(value).map_err(|e| leaf_error(name, e.to_string()))
}

/// A config error inside a known section. The leaf field comes from serde's
/// "unknown field" message, else from the final "in `x`" tail that
/// wrong-type and missing-field messages carry. The final tail wins, so
/// backticks inside a value cannot masquerade as the field name. Without
/// either the path stays at the section and the message carries the rest.
fn leaf_error(section: &str, message: String) -> Error {
    let unknown = message
        .split("unknown field `")
        .nth(1)
        .and_then(|rest| rest.split('`').next());
    let typed = message
        .rsplit_once("in `")
        .and_then(|(_, rest)| rest.split('`').next());
    let key_path = unknown
        .or(typed)
        .map_or_else(|| section.to_owned(), |f| format!("{section}.{f}"));
    Error::ConfigInvalid {
        key_path,
        reason: message,
    }
}

/// Normalizes the extension allowlist: a leading `.` on an entry is
/// dropped (`.md` names the same extension as `md`, which is what the
/// walk matches against), blank entries go, and an empty result — an
/// empty `DEVDOCS_INGEST_GIT_EXTENSIONS` value or config array — falls
/// back to the defaults instead of a silent zero-document ingest
/// (docs/implementation/07 §Git adapter).
fn normalize_git_extensions(provided: Option<Vec<String>>) -> Vec<String> {
    let trimmed: Vec<String> = provided
        .unwrap_or_default()
        .into_iter()
        .map(|ext| ext.trim().trim_start_matches('.').to_owned())
        .filter(|ext| !ext.is_empty())
        .collect();
    if trimmed.is_empty() {
        DEFAULT_GIT_EXTENSIONS
            .iter()
            .map(|ext| (*ext).to_owned())
            .collect()
    } else {
        trimmed
    }
}

/// A config error for malformed TOML syntax; no key path applies.
fn syntax_error(message: String) -> Error {
    Error::ConfigInvalid {
        key_path: "config".to_owned(),
        reason: message,
    }
}

/// Expand a leading `~` to the user home directory.
fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

/// The user config file path: `$XDG_CONFIG_HOME/devdocs/config.toml` when
/// the variable is set, else the platform config dir
/// (`~/Library/Application Support/devdocs/config.toml` on macOS). `None`
/// means no home directory could be resolved.
///
/// # Errors
/// [`Error::ConfigInvalid`] when a set `XDG_CONFIG_HOME` is not valid
/// UTF-8.
pub fn user_config_path() -> Result<Option<PathBuf>, Error> {
    if let Some(dir) = env_string("XDG_CONFIG_HOME")? {
        return Ok(Some(Path::new(&dir).join("devdocs").join("config.toml")));
    }
    Ok(dirs::config_dir().map(|dir| dir.join("devdocs").join("config.toml")))
}

/// The user config file path when a home directory exists. This is the
/// form every command that must write or print the path needs.
///
/// # Errors
/// [`Error::ConfigInvalid`] when a set `XDG_CONFIG_HOME` is not valid
/// UTF-8 or when no home directory can be resolved.
pub fn require_user_config_path() -> Result<PathBuf, Error> {
    user_config_path()?.ok_or_else(|| Error::ConfigInvalid {
        key_path: "$HOME".to_owned(),
        reason: "cannot resolve a user config directory; is HOME set?".to_owned(),
    })
}

/// The data root: `$DEVDOCS_HOME` when set, else `$XDG_DATA_HOME/devdocs`
/// when that is set, else the platform data dir.
///
/// # Errors
/// [`Error::ConfigInvalid`] when no home directory can be resolved.
pub fn data_root() -> Result<PathBuf, Error> {
    if let Some(home) = env_string(env_vars::HOME)? {
        return Ok(PathBuf::from(home));
    }
    if let Some(dir) = env_string("XDG_DATA_HOME")? {
        return Ok(Path::new(&dir).join("devdocs"));
    }
    dirs::data_dir()
        .map(|dir| dir.join("devdocs"))
        .ok_or_else(|| Error::ConfigInvalid {
            key_path: "$HOME".to_owned(),
            reason: "cannot resolve a data root; is HOME set?".to_owned(),
        })
}

/// Search the cwd and its parents for the project `devdocs.toml`.
#[must_use]
pub fn find_project_config(start: &Path) -> Option<PathBuf> {
    start.ancestors().find_map(|dir| {
        let candidate = dir.join(PROJECT_FILE);
        candidate.is_file().then_some(candidate)
    })
}

impl Config {
    /// Load the merged configuration through the full precedence chain.
    ///
    /// # Errors
    /// [`Error::ConfigInvalid`] when any layer fails validation.
    pub fn load(over: &Overrides) -> Result<Self, Error> {
        let mut merged = PartialConfig::default();

        if let Some(user) = user_config_path()?
            && user.is_file()
        {
            merged.merge(read_layer(&user)?);
        }

        match &over.config_path {
            // An explicit --config file is required to be a readable file;
            // silently skipping it would run on defaults without telling
            // the user.
            Some(path) => {
                if !path.is_file() {
                    let reason = if path.exists() {
                        "the --config path is not a regular file"
                    } else {
                        "the --config file does not exist"
                    };
                    return Err(Error::ConfigInvalid {
                        key_path: path.display().to_string(),
                        reason: reason.to_owned(),
                    });
                }
                merged.merge(read_layer(path)?);
            }
            // A discovered devdocs.toml stays optional.
            None => {
                if let Some(path) =
                    find_project_config(&std::env::current_dir().unwrap_or_default())
                    && path.is_file()
                {
                    merged.merge(read_layer(&path)?);
                }
            }
        }

        merged.merge(PartialConfig::from_env()?);
        merged.merge(PartialConfig::from_overrides(over));

        Self::resolve(&merged, over)
    }

    /// Validate one config file against the schema, including every
    /// `[sources.*]` URL, so `config validate` agrees with the load path.
    ///
    /// # Errors
    /// [`Error::ConfigInvalid`] when the file is unreadable or fails
    /// validation.
    pub fn validate_file(path: &Path) -> Result<(), Error> {
        let partial = read_layer(path)?;
        for (name, source) in &partial.sources {
            source.parsed_url(&format!("sources.{name}"))?;
        }
        Ok(())
    }

    /// Fill the remaining holes from built-in defaults.
    fn resolve(partial: &PartialConfig, over: &Overrides) -> Result<Self, Error> {
        let data_root = data_root()?;
        let user_agent = partial.fetch.user_agent.clone().unwrap_or_else(|| {
            format!(
                "devdocs/{} (+https://github.com/OWNER/devdocs-mcp)",
                env!("CARGO_PKG_VERSION")
            )
        });

        // `DEVDOCS_HOME` overrides the data root and any config-file index
        // path; the `--db` flag overrides everything.
        let index_db = if let Some(db) = &over.db_path {
            db.clone()
        } else if env_string(env_vars::HOME)?.is_some() {
            data_root.join("docs.db")
        } else {
            partial
                .index
                .path
                .as_deref()
                .map_or_else(|| data_root.join("docs.db"), expand_home)
        };

        let sources = partial
            .sources
            .iter()
            .map(|(name, source)| {
                source
                    .parsed_url(&format!("sources.{name}"))
                    .map(|_| (name.clone(), source.clone()))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;

        Ok(Self {
            fetch: FetchConfig {
                concurrency: partial.fetch.concurrency.unwrap_or(4),
                timeout_ms: partial.fetch.timeout_ms.unwrap_or(15_000),
                max_bytes: partial.fetch.max_bytes.unwrap_or(20 * 1024 * 1024),
                max_redirects: partial.fetch.max_redirects.unwrap_or(3),
                user_agent,
            },
            index: IndexConfig {
                path: index_db,
                embeddings: partial.index.embeddings.unwrap_or(false),
            },
            search: SearchConfig {
                max_tokens_default: partial.search.max_tokens_default.unwrap_or(2000),
                limit_default: partial.search.limit_default.unwrap_or(8),
                rrf_k: partial.search.rrf_k.unwrap_or(60),
            },
            ingest: IngestConfig {
                git_extensions: normalize_git_extensions(partial.ingest.git_extensions.clone()),
            },
            sources,
        })
    }

    /// The resolved paths for `config path`. The index database path comes
    /// from the loaded configuration, so `config path` and `config show`
    /// can never disagree.
    ///
    /// # Errors
    /// [`Error::ConfigInvalid`] when the config fails validation or a path
    /// cannot be resolved.
    pub fn paths(over: &Overrides) -> Result<Paths, Error> {
        let config = Config::load(over)?;
        let project = over
            .config_path
            .clone()
            .or_else(|| find_project_config(&std::env::current_dir().unwrap_or_default()));
        let user = require_user_config_path()?;
        Ok(Paths {
            user_config: user,
            project_config: project,
            data_root: data_root()?,
            index_db: config.index.path,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{
        Config, Overrides, PartialConfig, Paths, SourceConfig, env_vars, user_config_path,
    };
    use crate::error::Error;
    use std::path::PathBuf;

    /// An empty file parses to an all-empty partial view.
    #[test]
    fn empty_file_parses_to_defaults() {
        let partial = PartialConfig::parse("").unwrap();
        assert_eq!(partial, PartialConfig::default());
    }

    /// The env value of `DEVDOCS_INGEST_GIT_EXTENSIONS` parses as a
    /// comma-separated list: entries trim, empty entries drop
    /// (docs/implementation/07 §Git adapter).
    #[test]
    fn the_env_value_parses_the_git_extension_list() {
        assert_eq!(
            super::parse_git_extensions(" md , ,rst "),
            vec!["md".to_owned(), "rst".to_owned()]
        );
        assert_eq!(super::parse_git_extensions(","), Vec::<String>::new());
    }

    /// A full file parses every section of the docs/05 schema.
    #[test]
    fn full_schema_parses() {
        let text = r#"
[fetch]
concurrency = 4
timeout_ms = 15000
max_bytes = 20971520
max_redirects = 3
user_agent = "devdocs/0.1"

[index]
path = "~/.local/share/devdocs/docs.db"
embeddings = false

[search]
max_tokens_default = 2000
limit_default = 8
rrf_k = 60

[ingest]
git_extensions = ["md", "adoc"]

[sources.nextjs]
kind = "llms_txt"
url = "https://nextjs.org/docs/llms.txt"
license = "MIT"
status = "approved_snippet"
refresh_days = 2
"#;
        let partial = PartialConfig::parse(text).unwrap();
        assert_eq!(partial.fetch.concurrency, Some(4));
        assert_eq!(partial.search.rrf_k, Some(60));
        assert_eq!(
            partial.ingest.git_extensions,
            Some(vec!["md".to_owned(), "adoc".to_owned()])
        );
        assert_eq!(partial.sources["nextjs"].license.as_deref(), Some("MIT"));
        partial.sources["nextjs"]
            .parsed_url("sources.nextjs")
            .unwrap();
    }

    /// Merging folds the higher layer over the lower one per field.
    #[test]
    fn merge_prefers_the_higher_layer() {
        let low =
            PartialConfig::parse("[fetch]\nconcurrency = 2\nmax_bytes = 1\n[search]\nrrf_k = 30\n")
                .unwrap();
        let high = PartialConfig::parse("[fetch]\nconcurrency = 8\n").unwrap();
        let mut merged = low;
        merged.merge(high);
        assert_eq!(merged.fetch.concurrency, Some(8));
        assert_eq!(merged.fetch.max_bytes, Some(1));
        assert_eq!(merged.search.rrf_k, Some(30));
    }

    /// An unknown key fails validation and names the full structural path.
    #[test]
    fn unknown_key_fails_with_a_key_path() {
        let error = PartialConfig::parse("[fetch]\nconkurrency = 4\n").unwrap_err();
        match error {
            Error::ConfigInvalid { key_path, .. } => {
                assert_eq!(key_path, "fetch.conkurrency");
            }
            other => panic!("expected ConfigInvalid, got {other:?}"),
        }
    }

    /// An unknown key inside a source entry names the entry and the key.
    #[test]
    fn unknown_source_key_names_the_entry_path() {
        let error = PartialConfig::parse("[sources.nextjs]\nurll = \"https://x\"\n").unwrap_err();
        match error {
            Error::ConfigInvalid { key_path, .. } => {
                assert_eq!(key_path, "sources.nextjs.urll");
            }
            other => panic!("expected ConfigInvalid, got {other:?}"),
        }
    }

    /// An unknown top-level section fails validation and names the section.
    #[test]
    fn unknown_section_fails_validation() {
        let error = PartialConfig::parse("[nope]\nkey = 1\n").unwrap_err();
        match error {
            Error::ConfigInvalid { key_path, .. } => {
                assert_eq!(key_path, "nope");
            }
            other => panic!("expected ConfigInvalid, got {other:?}"),
        }
    }

    /// A wrong value type fails validation and names the full key path.
    #[test]
    fn wrong_type_fails_validation() {
        let error = PartialConfig::parse("[fetch]\nconcurrency = \"four\"\n").unwrap_err();
        match error {
            Error::ConfigInvalid { key_path, .. } => {
                assert_eq!(key_path, "fetch.concurrency");
            }
            other => panic!("expected ConfigInvalid, got {other:?}"),
        }
    }

    /// A set but non-UTF-8 environment value is a config error, not
    /// silence. Unix-only: Windows environment values are always UTF-16.
    #[cfg(unix)]
    #[test]
    fn non_utf8_env_value_is_a_config_error() {
        use std::os::unix::ffi::OsStringExt;

        let raw = std::ffi::OsString::from_vec(vec![0xff, 0xfe]);
        let error = super::coerce_utf8("DEVDOCS_INDEX_PATH", raw).unwrap_err();
        match error {
            Error::ConfigInvalid { key_path, .. } => {
                assert_eq!(key_path, "DEVDOCS_INDEX_PATH");
            }
            other => panic!("expected ConfigInvalid, got {other:?}"),
        }
        assert!(
            super::coerce_utf8("DEVDOCS_INDEX_PATH", std::ffi::OsString::new())
                .unwrap()
                .is_none()
        );
    }

    /// A malformed source URL fails validation and names the entry.
    #[test]
    fn bad_source_url_names_the_entry() {
        let error = SourceConfig {
            url: Some("not a url".to_owned()),
            ..SourceConfig::default()
        }
        .parsed_url("sources.nextjs")
        .unwrap_err();
        match error {
            Error::ConfigInvalid { key_path, .. } => {
                assert_eq!(key_path, "sources.nextjs.url");
            }
            other => panic!("expected ConfigInvalid, got {other:?}"),
        }
    }

    /// The resolved config fills unset fields from built-in defaults.
    #[test]
    fn resolve_fills_defaults() {
        let config =
            Config::load(&Overrides::default()).expect("defaults resolve without a config file");
        assert_eq!(config.fetch.concurrency, 4);
        assert_eq!(config.fetch.timeout_ms, 15_000);
        assert_eq!(config.fetch.max_bytes, 20 * 1024 * 1024);
        assert_eq!(config.fetch.max_redirects, 3);
        assert_eq!(config.search.max_tokens_default, 2000);
        assert_eq!(config.search.limit_default, 8);
        assert_eq!(config.search.rrf_k, 60);
        assert!(!config.index.embeddings);
        assert!(config.index.path.ends_with("docs.db"));
        assert!(config.fetch.user_agent.contains("devdocs/"));
        assert_eq!(config.ingest.git_extensions, super::DEFAULT_GIT_EXTENSIONS);
    }

    /// A `[ingest]` section in a config file overrides the default
    /// extensions and leaves every other section on its default.
    #[test]
    fn ingest_file_layer_overrides_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ingest.toml");
        std::fs::write(&file, "[ingest]\ngit_extensions = [\"md\"]\n").unwrap();
        let over = Overrides {
            config_path: Some(file),
            ..Overrides::default()
        };
        let config = Config::load(&over).unwrap();
        assert_eq!(config.ingest.git_extensions, ["md"]);
        assert_eq!(config.fetch.concurrency, 4);
    }

    /// An unknown key under `[ingest]` fails validation and names the key.
    #[test]
    fn unknown_ingest_key_fails_with_a_key_path() {
        let error = PartialConfig::parse("[ingest]\ngit_extensionz = [\"md\"]\n").unwrap_err();
        match error {
            Error::ConfigInvalid { key_path, .. } => {
                assert_eq!(key_path, "ingest.git_extensionz");
            }
            other => panic!("expected ConfigInvalid, got {other:?}"),
        }
    }

    /// The `--db` flag layer wins over every file layer.
    #[test]
    fn db_flag_overrides_index_path() {
        let over = Overrides {
            db_path: Some(PathBuf::from("/tmp/other.db")),
            ..Overrides::default()
        };
        let config = Config::load(&over).unwrap();
        assert_eq!(config.index.path, PathBuf::from("/tmp/other.db"));
    }

    /// A missing explicit `--config` file fails instead of silently
    /// running on defaults.
    #[test]
    fn missing_explicit_config_fails() {
        let over = Overrides {
            config_path: Some(PathBuf::from("/nonexistent/devdocs.toml")),
            ..Overrides::default()
        };
        let error = Config::load(&over).unwrap_err();
        match error {
            Error::ConfigInvalid { key_path, .. } => {
                assert_eq!(key_path, "/nonexistent/devdocs.toml");
            }
            other => panic!("expected ConfigInvalid, got {other:?}"),
        }
    }

    /// `validate_file` rejects a malformed source URL, so it agrees with
    /// the load path.
    #[test]
    fn validate_file_rejects_a_bad_source_url() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sources.toml");
        std::fs::write(&file, "[sources.nextjs]\nurl = \"not a url\"\n").unwrap();
        let error = Config::validate_file(&file).unwrap_err();
        match error {
            Error::ConfigInvalid { key_path, .. } => {
                assert_eq!(key_path, "sources.nextjs.url");
            }
            other => panic!("expected ConfigInvalid, got {other:?}"),
        }
    }

    /// The user config path sits under the platform config dir.
    #[test]
    fn user_config_path_resolves() {
        let path = user_config_path().unwrap().expect("a home exists");
        assert!(path.ends_with("devdocs/config.toml"));
    }

    /// A value that itself contains backticks cannot masquerade as the
    /// failing field: the final "in `key`" tail wins.
    #[test]
    fn backtick_value_does_not_break_the_leaf_key() {
        let error = PartialConfig::parse("[fetch]\nconcurrency = \"four in `x`\"\n").unwrap_err();
        match error {
            Error::ConfigInvalid { key_path, .. } => {
                assert_eq!(key_path, "fetch.concurrency");
            }
            other => panic!("expected ConfigInvalid, got {other:?}"),
        }
    }

    /// An existing non-file `--config` path reports a different reason than
    /// a missing one.
    #[test]
    fn non_file_explicit_config_reports_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let over = Overrides {
            config_path: Some(dir.path().to_path_buf()),
            ..Overrides::default()
        };
        let error = Config::load(&over).unwrap_err();
        match error {
            Error::ConfigInvalid { key_path, reason } => {
                assert_eq!(key_path, dir.path().display().to_string());
                assert!(reason.contains("not a regular file"), "reason: {reason}");
            }
            other => panic!("expected ConfigInvalid, got {other:?}"),
        }
    }

    /// The env-var constants follow the `DEVDOCS_*` prefix rule.
    #[test]
    fn env_var_names_follow_the_prefix_rule() {
        for name in [
            env_vars::HOME,
            env_vars::FETCH_CONCURRENCY,
            env_vars::FETCH_TIMEOUT_MS,
            env_vars::FETCH_MAX_BYTES,
            env_vars::FETCH_MAX_REDIRECTS,
            env_vars::FETCH_USER_AGENT,
            env_vars::INDEX_PATH,
            env_vars::INDEX_EMBEDDINGS,
            env_vars::SEARCH_MAX_TOKENS,
            env_vars::SEARCH_LIMIT,
            env_vars::SEARCH_RRF_K,
            env_vars::INGEST_GIT_EXTENSIONS,
        ] {
            assert!(name.starts_with("DEVDOCS_"), "{name}");
        }
    }

    /// The extension allowlist normalizes: a leading `.` drops (`.md`
    /// names the same extension the walk matches against), blank entries
    /// go, and an empty value — e.g. `DEVDOCS_INGEST_GIT_EXTENSIONS=""` —
    /// resolves to the defaults, never to a silent zero-document
    /// allowlist (docs/implementation/07 §Git adapter).
    #[test]
    fn git_extensions_normalize_to_a_usable_allowlist() {
        let defaults: Vec<String> = super::DEFAULT_GIT_EXTENSIONS
            .iter()
            .map(|ext| (*ext).to_owned())
            .collect();
        // Unset and empty resolve to the defaults.
        assert_eq!(super::normalize_git_extensions(None), defaults);
        assert_eq!(
            super::normalize_git_extensions(Some(vec![])),
            defaults,
            "an empty value counts as unset"
        );
        assert_eq!(
            super::normalize_git_extensions(Some(vec![String::new(), "  ".to_owned()])),
            defaults,
            "blank entries never survive"
        );
        // A leading `.` drops; real entries pass through.
        assert_eq!(
            super::normalize_git_extensions(Some(vec![".md".to_owned(), "rst".to_owned(),])),
            ["md".to_owned(), "rst".to_owned()]
        );
    }

    /// The paths struct serializes with the resolved locations.
    #[test]
    fn paths_serialize() {
        let paths = Paths {
            user_config: PathBuf::from("/u/config.toml"),
            project_config: None,
            data_root: PathBuf::from("/d"),
            index_db: PathBuf::from("/d/docs.db"),
        };
        let json = serde_json::to_string(&paths).unwrap();
        assert_eq!(
            json,
            r#"{"user_config":"/u/config.toml","project_config":null,"data_root":"/d","index_db":"/d/docs.db"}"#
        );
    }
}
