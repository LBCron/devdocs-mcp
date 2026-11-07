//! Shared fixtures for the harness integration tests.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rusqlite::Connection;

/// The `devdocs` binary the subprocess pass needs.
///
/// The helper derives the path from `CARGO_BIN_EXE_devdocs-eval`. Cargo
/// embeds that absolute path at compile time, and it points at this
/// package's own binary beside the test binary, so its parent is the active
/// profile directory. The helper expects `devdocs` in the same directory,
/// with the platform `EXE_SUFFIX`. `cargo test -p devdocs-eval` does not
/// build `devdocs` as a side effect, so on a clean target the helper builds
/// it once and caches the path.
///
/// The fallback build is deterministic. It receives the absolute parent of
/// the profile directory as `--target-dir`, so cargo writes the default
/// profile layout there and the output lands exactly where the helper
/// looks, even when `CARGO_TARGET_DIR` is relative. The build also removes
/// `CARGO_BUILD_TARGET`, so cargo does not nest the target triple twice
/// under that directory.
pub fn devdocs_bin() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let (bin, profile_dir) = binary_beside_the_test();
        if !bin.is_file() {
            build_devdocs(&profile_dir);
        }
        assert!(
            bin.is_file(),
            "`cargo build --locked -p devdocs` did not produce {}",
            bin.display()
        );
        bin
    })
    .clone()
}

/// The `devdocs` path that matches the test binary's target profile, with
/// the profile directory that holds it.
fn binary_beside_the_test() -> (PathBuf, PathBuf) {
    let eval = PathBuf::from(env!("CARGO_BIN_EXE_devdocs-eval"));
    let profile_dir = eval
        .parent()
        .expect("the devdocs-eval binary has a parent")
        .to_path_buf();
    let bin = profile_dir.join(format!("devdocs{}", std::env::consts::EXE_SUFFIX));
    (bin, profile_dir)
}

/// Builds `devdocs` into the profile directory the helper expects.
fn build_devdocs(profile_dir: &Path) {
    let profile = profile_dir
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .expect("the profile directory has a UTF-8 name");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = std::process::Command::new(cargo);
    command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env_remove("CARGO_BUILD_TARGET")
        .args(["build", "--locked", "-p", "devdocs"])
        .arg("--target-dir")
        .arg(
            profile_dir
                .parent()
                .expect("the profile directory has a parent"),
        );
    match profile {
        "debug" => {}
        "release" => {
            command.arg("--release");
        }
        other => {
            command.args(["--profile", other]);
        }
    }
    let status = command.status().expect("cargo runs the devdocs build");
    assert!(
        status.success(),
        "`cargo build --locked -p devdocs` failed: {status}"
    );
}

/// Builds a fixture database and a three-question suite: one answered, one
/// unanswerable that abstains, and one whose only expected ref is absent.
pub fn fixture(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let db_path = dir.join("fixture.db");
    let store = devdocs_testkit::open_migrated(&db_path).unwrap();
    drop(store);
    let conn = Connection::open(&db_path).unwrap();
    let source = devdocs_testkit::seed_source(&conn, "test-lib", true).unwrap();
    devdocs_testkit::seed_chunk(
        &conn,
        source,
        0,
        r#"["proxy"]"#,
        Some("rewrite"),
        "middleware rewrite the incoming request path",
    )
    .unwrap();
    let chunk_id: i64 = conn
        .query_row("SELECT chunk_id FROM chunks LIMIT 1", [], |row| row.get(0))
        .unwrap();
    conn.execute(
        "INSERT INTO symbols(symbol, chunk_id, kind) VALUES ('rewrite', ?1, 'function')",
        [chunk_id],
    )
    .unwrap();
    drop(conn);

    let suite_path = dir.join("fixture.jsonl");
    let lines = [
        r#"{"id":"answered-001","library":"test-lib","version":"1.0.0","task_type":"exact_api","question":"rewrite()","expect_refs":["docs://test-lib/v1.0.0/proxy#rewrite"],"expect_symbols":["rewrite"],"grader":"ref_match","notes":"the seeded chunk"}"#,
        r#"{"id":"unanswerable-001","library":"test-lib","version":"1.0.0","task_type":"unanswerable","question":"quantum flux capacitor wiring","expect_refs":[],"grader":"ref_match","notes":"nothing matches"}"#,
        r#"{"id":"missed-001","library":"test-lib","version":"1.0.0","task_type":"exact_api","question":"rewrite the request path","expect_refs":["docs://test-lib/v1.0.0/absent"],"grader":"ref_match","notes":"never seeded"}"#,
    ];
    std::fs::write(&suite_path, lines.join("\n") + "\n").unwrap();
    (db_path, suite_path, dir.join("runs"))
}
