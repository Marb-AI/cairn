//! `cairn flow`, against a recorded provider answer.
//!
//! The provider is a Go program that runs in the indexer image. CI has no Go toolchain
//! and no Docker — the same reason the fixture's SCIP indexes are committed rather than
//! produced — so what is committed here is the provider's *output* for one fixture
//! handler, and these tests exercise everything cairn does with it: the cache it is
//! served from, the call targets it resolves, the declared facts it annotates them with,
//! and the calls it admits it could not resolve.
//!
//! The provider itself is tested by running it, which happens on a workstation. What is
//! tested here is the half that can be wrong without anyone noticing: a handle attached
//! to the wrong call, or a call quietly dropped.

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;
use common::{build_fixture_index, build_fixture_index_in};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

fn binary() -> PathBuf {
    let mut p = std::env::current_exe().expect("test binary");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("cairn")
}

fn corpus() -> PathBuf {
    workspace_root().join("crates/cairn-cli/tests/fixtures/corpus")
}

fn run(db: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(binary())
        .arg("--db")
        .arg(db)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("running cairn {args:?}: {e}"));
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

/// The handle of a symbol, by its qualified name.
fn handle_of(db: &Path, qualified: &str) -> String {
    let bare = qualified.rsplit('.').next().unwrap_or(qualified);
    let (out, _) = run(db, &["symbol", bare, "--limit", "20"]);
    out.lines()
        // Result lines only: the header says `1 matches for "classify"`, which contains
        // the name and is not a row.
        .filter_map(|l| l.trim().strip_prefix('['))
        .find(|l| l.contains(qualified))
        .and_then(|l| l.split(']').next())
        .unwrap_or_else(|| panic!("no handle for {qualified} in:\n{out}"))
        .to_string()
}

/// Put the recorded provider answer in the cache, keyed exactly as a real run would key
/// it: the content of the file the function lives in.
///
/// This is what lets the test run with no Go and no Docker. It is also the cache path a
/// second `cairn flow` takes in real use, so it is not a test-only road.
fn seed_recorded_flow(db: &Path, handle: &str) {
    let root = workspace_root();
    let recorded = std::fs::read_to_string(
        root.join("crates/cairn-cli/tests/fixtures/flow/upload-readings.json"),
    )
    .expect("the recorded provider answer");
    let file = corpus().join("srcgo/internal/ingest/service.go");
    let bytes = std::fs::read(&file).expect("the fixture source");
    let blob = blake3::hash(&bytes).as_bytes()[..16].to_vec();

    let conn = rusqlite::Connection::open(db).expect("opening the index");
    let symbol_id: i64 = conn
        .query_row(
            "SELECT symbol_id FROM handles WHERE handle = ?1",
            [handle],
            |r| r.get(0),
        )
        .expect("the handle resolves");
    conn.execute(
        "INSERT INTO flows(symbol_id, blob, provider, json) VALUES (?1, ?2, 'cairnflow 1', ?3)
         ON CONFLICT(symbol_id) DO UPDATE SET blob = excluded.blob, json = excluded.json",
        rusqlite::params![symbol_id, blob, recorded],
    )
    .expect("seeding the cache");
}

#[test]
fn a_recorded_flow_resolves_its_calls_and_admits_the_ones_it_cannot() {
    let root = workspace_root();
    let bin = binary();
    let Some(db) = build_fixture_index(&root, &bin, "flow") else {
        eprintln!("SKIP: no fixture SCIP to build an index from");
        return;
    };
    let handle = handle_of(&db, "Service.UploadReadings");
    seed_recorded_flow(&db, &handle);

    let (out, code) = run(
        &db,
        &["flow", &handle, "--repo", &corpus().to_string_lossy()],
    );
    assert_eq!(code, 0, "{out}");

    // The shape a reviewer came for: two early error returns, a loop, the branches, and
    // the call that leaves the function.
    for expected in [
        "flow of [",
        "Service.UploadReadings",
        "branch",
        "batch == nil",
        "return_error",
        "loop",
        "range batch.Readings",
        "s.accept",
    ] {
        assert!(
            out.contains(expected),
            "the flow does not mention {expected:?}:\n{out}"
        );
    }

    // The call to `accept` resolves to a handle, so the next command needs no lookup.
    // This is the half a provider cannot do and the half that can be wrong silently.
    let accept = handle_of(&db, "Service.accept");
    assert!(
        out.contains(&format!("-> [{accept}]")),
        "the call to accept did not resolve to its handle ({accept}):\n{out}"
    );

    // `len` is a builtin and resolves to nothing. Reported by name, because a reader
    // deciding whether this map is complete needs to know which call went unresolved.
    assert!(
        out.contains("could not be resolved to a symbol") && out.contains("len"),
        "an unresolved call is not reported:\n{out}"
    );

    // Served from the cache, which is what the recorded answer is standing in for.
    assert!(out.contains("cached"), "{out}");
}

#[test]
fn the_json_answer_carries_the_same_nodes_as_fields() {
    let root = workspace_root();
    let bin = binary();
    let Some(db) = build_fixture_index(&root, &bin, "flow-json") else {
        return;
    };
    let handle = handle_of(&db, "Service.UploadReadings");
    seed_recorded_flow(&db, &handle);

    let (out, _) = run(
        &db,
        &[
            "--format",
            "json",
            "flow",
            &handle,
            "--repo",
            &corpus().to_string_lossy(),
        ],
    );
    let doc: serde_json::Value = serde_json::from_str(&out).expect("the answer is JSON");
    let data = &doc["data"];
    assert_eq!(data["function"], "Service.UploadReadings");
    let nodes = data["nodes"].as_array().expect("nodes");
    assert!(nodes.len() > 8, "{} nodes is too few", nodes.len());
    // Every node carries its span with the file in it, so a consumer can open the lines
    // without reconstructing a path.
    assert!(nodes.iter().all(|n| n["span"]
        .as_str()
        .unwrap_or_default()
        .starts_with("srcgo/internal/ingest/service.go:")));
    // The edges are in the data even though the text prints a list: anything that wants
    // the graph has it.
    assert!(!data["edges"].as_array().expect("edges").is_empty());
    let resolved = nodes
        .iter()
        .filter(|n| n["handle"].is_string())
        .collect::<Vec<_>>();
    assert!(
        !resolved.is_empty(),
        "no call resolved, so the resolution half is untested"
    );
    assert!(
        resolved.iter().all(|n| n["target"].is_string()),
        "a resolved call carries a handle but not the SCIP symbol it resolved to"
    );
}

#[test]
fn a_declared_fact_annotates_the_call_it_is_about() {
    // The reason this command is worth more than reading the function: `writes
    // w17:entity/Reading` is nowhere in the source, and a reviewer following the handler
    // is exactly the reader who needs it.
    let root = workspace_root();
    let bin = binary();
    let fixtures = root.join("crates/cairn-cli/tests/fixtures");
    let script = fixtures.join("connectors/w17-fixture.sh");
    let Ok(template) = std::fs::read_to_string(fixtures.join("connectors/w17.yaml")) else {
        return;
    };
    let dir = std::env::temp_dir().join("cairn-flow-annotated");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("connectors")).unwrap();
    std::fs::write(
        dir.join("connectors/w17.yaml"),
        template
            .replace("__SCRIPT__", &script.to_string_lossy())
            .replace("__MODE__", "annotated")
            .replace("__TIMEOUT__", "30"),
    )
    .unwrap();
    let Some(db) = build_fixture_index_in(&dir, &root, &bin) else {
        return;
    };

    let handle = handle_of(&db, "Service.UploadReadings");
    seed_recorded_flow(&db, &handle);
    let (out, _) = run(
        &db,
        &["flow", &handle, "--repo", &corpus().to_string_lossy()],
    );
    assert!(
        out.contains("writes w17:entity/Reading (declared by w17)"),
        "the declared fact about the callee is missing from the flow:\n{out}"
    );
}

#[test]
fn python_is_out_of_scope_and_says_so() {
    // Not an error and not an empty answer: the question was well formed and the honest
    // reply is that nothing here can describe it.
    let root = workspace_root();
    let bin = binary();
    let Some(db) = build_fixture_index(&root, &bin, "flow-python") else {
        return;
    };
    let handle = handle_of(&db, "classify");
    let (out, code) = run(
        &db,
        &["flow", &handle, "--repo", &corpus().to_string_lossy()],
    );
    assert_eq!(code, 1, "a missing provider is not a bad query:\n{out}");
    assert!(
        out.contains("flow is not available for python"),
        "the answer does not say why there is no flow:\n{out}"
    );
}
