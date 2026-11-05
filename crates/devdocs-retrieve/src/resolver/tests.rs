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

/// package-lock.json v3: the name is the segment after the last
/// `node_modules/`, the root entry is skipped, and a nested key
/// yields to its top-level twin.
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
                library: "react".to_owned(),
                version: Some("18.3.1".to_owned()),
            },
        ]
    );
}

/// A nested key and its top-level twin name the same package; the
/// top-level pin wins even when the nested key sorts first.
#[test]
fn package_lock_nested_duplicate_keeps_top_level() {
    let content = r#"{
        "packages": {
            "node_modules/app/node_modules/react": {"version": "19.0.0"},
            "node_modules/react": {"version": "18.3.1"}
        }
    }"#;
    assert_eq!(
        parse_package_lock(content),
        vec![Dep {
            library: "react".to_owned(),
            version: Some("18.3.1".to_owned()),
        }]
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
/// peer suffixes, scoped names, and the v6 leading `/` handled.
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

    // v6 keys carry a leading `/`; it trims away so the names match
    // the v9 shape.
    let v6 = "packages:\n\
              \x20 '/next@16.2.9': {}\n\
              \x20 '/@babel/core@7.26.0': {}\n";
    // BTreeMap key order: `/@babel/core` sorts before `/next`.
    assert_eq!(
        parse_pnpm_lock(v6),
        vec![
            Dep {
                library: "@babel/core".to_owned(),
                version: Some("7.26.0".to_owned()),
            },
            Dep {
                library: "next".to_owned(),
                version: Some("16.2.9".to_owned()),
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

/// A one-line `require ( mod vX )` form parses, and a require whose
/// version lacks the leading `v` yields no entry.
#[test]
fn go_mod_one_line_block_and_unversioned_require() {
    let content = "module example.test/app\n\n\
                   require (github.com/spf13/cobra v1.9.1)\n\n\
                   require golang.org/x/mod 0.17.0\n";
    assert_eq!(
        parse_go_mod(content),
        vec![Dep {
            library: "github.com/spf13/cobra".to_owned(),
            version: Some("1.9.1".to_owned()),
        }]
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

/// A `name@version` mention resolves the same way as the spaced form;
/// a `v` prefix on the version keeps only the bare spelling.
#[test]
fn at_mentions_resolve() {
    let empty = ProjectContext::empty();
    let resolved = resolve(&Query::new("zod@4.6.7 parse", None, None), &empty);
    assert_eq!(resolved.library.as_deref(), Some("zod"));
    assert_eq!(resolved.version.as_deref(), Some("4.6.7"));

    let at_v = resolve(&Query::new("next@v16 routing", None, None), &empty);
    assert_eq!(at_v.library.as_deref(), Some("next"));
    assert_eq!(at_v.version.as_deref(), Some("16"));

    let spaced_v = resolve(&Query::new("next v16 routing", None, None), &empty);
    assert_eq!(spaced_v.library.as_deref(), Some("next"));
    assert_eq!(spaced_v.version.as_deref(), Some("16"));
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

/// Writes one lockfile into a temp project and checks what `detect`
/// parses from it.
fn assert_lockfile_detected(name: &str, content: &str, expected: Dep) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(name), content).unwrap();
    let ctx = ProjectContext::detect(dir.path());
    assert_eq!(ctx.deps, vec![expected], "lockfile: {name}");
    assert_eq!(ctx.files, vec![dir.path().join(name)]);
}

/// `detect` reads the JS lockfiles (npm, pnpm) and parses their
/// entries.
#[test]
fn detect_reads_js_lockfiles() {
    assert_lockfile_detected(
        "package-lock.json",
        r#"{"packages": {"node_modules/next": {"version": "16.2.9"}}}"#,
        Dep {
            library: "next".to_owned(),
            version: Some("16.2.9".to_owned()),
        },
    );
    assert_lockfile_detected(
        "pnpm-lock.yaml",
        "packages:\n  'next@16.2.9': {}\n",
        Dep {
            library: "next".to_owned(),
            version: Some("16.2.9".to_owned()),
        },
    );
}

/// `detect` reads the tool lockfiles (Cargo, uv, pyproject, Go) and
/// parses their entries.
#[test]
fn detect_reads_tool_lockfiles() {
    assert_lockfile_detected(
        "Cargo.lock",
        "[[package]]\nname = \"serde\"\nversion = \"1.0.229\"\n",
        Dep {
            library: "serde".to_owned(),
            version: Some("1.0.229".to_owned()),
        },
    );
    assert_lockfile_detected(
        "uv.lock",
        "[[package]]\nname = \"click\"\nversion = \"8.1.8\"\n",
        Dep {
            library: "click".to_owned(),
            version: Some("8.1.8".to_owned()),
        },
    );
    assert_lockfile_detected(
        "pyproject.toml",
        "[project]\nname = \"app\"\ndependencies = [\"uvicorn\"]\n",
        Dep {
            library: "uvicorn".to_owned(),
            version: None,
        },
    );
    assert_lockfile_detected(
        "go.mod",
        "module example.test/app\n\nrequire github.com/spf13/cobra v1.9.1\n",
        Dep {
            library: "github.com/spf13/cobra".to_owned(),
            version: Some("1.9.1".to_owned()),
        },
    );
}

/// `detect` keeps walking when the present lockfiles parse to
/// nothing — a garbage Cargo.lock is a weak signal, not a hit.
#[test]
fn detect_continues_when_lockfiles_parse_to_nothing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.lock"), "garbage {").unwrap();
    assert_eq!(ProjectContext::detect(dir.path()), ProjectContext::empty());
}

/// A mention needs a version-shaped token; tokens that exist but
/// match nothing resolve unfiltered.
#[test]
fn mentions_that_match_nothing_stay_unfiltered() {
    let empty = ProjectContext::empty();
    let resolved = resolve(&Query::new("2x 4y", None, None), &empty);
    assert_eq!(resolved.source, ResolutionSource::Unfiltered);
    // No tokens at all: the mention scan runs off the end of its loop.
    let blank = resolve(&Query::new("   ", None, None), &empty);
    assert_eq!(blank.source, ResolutionSource::Unfiltered);
}

/// An English word followed by a digit token is a mention — "use 3"
/// resolves to the library `use`. The lexical rule cannot separate this
/// from a real `next.js 16` mention without a dictionary; the serve
/// layer guards the false positive instead: an absent weak-hint library
/// falls back to the unfiltered search.
#[test]
fn an_english_word_before_a_digit_is_a_weak_mention() {
    let empty = ProjectContext::empty();
    let resolved = resolve(&Query::new("how to use 3 hooks", None, None), &empty);
    assert_eq!(resolved.source, ResolutionSource::QueryMention);
    assert_eq!(resolved.library.as_deref(), Some("use"));
    assert_eq!(resolved.version.as_deref(), Some("3"));
}

/// An empty crate name in a Cargo.lock row is skipped.
#[test]
fn cargo_lock_rows_with_empty_names_are_skipped() {
    assert_eq!(
        parse_cargo_lock("[[package]]\nname = \"\"\nversion = \"1.0\"\n"),
        Vec::new()
    );
}

/// package-lock.json content that is not JSON parses to nothing.
#[test]
fn package_lock_json_errors_parse_to_nothing() {
    assert_eq!(parse_package_lock("not json"), Vec::new());
}

/// pnpm-lock.yaml content that is not YAML parses to nothing.
#[test]
fn pnpm_lock_yaml_errors_parse_to_nothing() {
    assert_eq!(parse_pnpm_lock("packages: {"), Vec::new());
    // A key without a version yields no entry.
    assert_eq!(parse_pnpm_lock("packages:\n  'next@': {}\n"), Vec::new());
}

/// pyproject.toml content that is not TOML parses to nothing.
#[test]
fn pyproject_toml_errors_parse_to_nothing() {
    assert_eq!(parse_pyproject("garbage ["), Vec::new());
}

/// Requirement names that trim to nothing and require entries that
/// fail the name or version shape yield no dependency.
#[test]
fn malformed_requirements_parse_to_nothing() {
    // A requirement string that starts with a specifier character has
    // an empty name.
    assert_eq!(
        parse_pyproject("[project]\ndependencies = [\">=1.0\"]\n"),
        Vec::new()
    );
    // A module whose name fails `is_name` and a version with nothing
    // after the `v` both reject the require entry.
    assert_eq!(
        parse_go_mod("module example.test/app\n\nrequire (bad~name v1.0.0)\n"),
        Vec::new()
    );
    assert_eq!(
        parse_go_mod("module example.test/app\n\nrequire (mod v)\n"),
        Vec::new()
    );
}
