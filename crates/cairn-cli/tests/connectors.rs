//! An external connector, end to end: the manifest, the protocol, the cache, and every
//! way a connector can let the core down.
//!
//! The fixture connector is a shell script printing fixed JSONL (see
//! `tests/fixtures/connectors/`). That is deliberate: what this repository owns is the
//! protocol and the validation, not anybody's project model. A real connector is tested
//! by its own repository; what has to be tested here is that cairn believes the right
//! things and says so when it cannot.
//!
//! The failure modes are half the file, because they are the half that decides whether a
//! connector can be trusted at all. A connector that half-worked and said nothing would
//! leave answers quietly short of facts — and an answer that is quietly short is worse
//! than no answer, which is the whole of D8.

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

/// Build an index with the fixture connector configured in `mode`.
///
/// The manifest goes beside the index in a scratch directory, never into the fixture
/// corpus: a manifest committed there would change what every other test sees, which is
/// exactly the invariant this phase is supposed to protect.
fn index_with_connector(label: &str, mode: &str, timeout_s: u64) -> Option<(PathBuf, String)> {
    let root = workspace_root();
    let bin = binary();
    let fixtures = root.join("crates/cairn-cli/tests/fixtures");
    let script = fixtures.join("connectors/w17-fixture.sh");
    let template = std::fs::read_to_string(fixtures.join("connectors/w17.yaml")).ok()?;
    let dir = std::env::temp_dir().join(format!("cairn-connector-{label}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("connectors")).ok()?;
    std::fs::write(
        dir.join("connectors/w17.yaml"),
        template
            .replace("__SCRIPT__", &script.to_string_lossy())
            .replace("__MODE__", mode)
            .replace("__TIMEOUT__", &timeout_s.to_string()),
    )
    .ok()?;
    let db = build_fixture_index_in(&dir, &root, &bin)?;
    // The index output is where a connector's progress lines appear, and the second run
    // in a test needs them too, so the caller gets them back.
    let out = Command::new(&bin)
        .arg("--db")
        .arg(&db)
        .arg("index")
        .arg(fixtures.join("index/go.scip"))
        .arg(fixtures.join("index/py.scip"))
        .arg("--repo")
        .arg(fixtures.join("corpus"))
        .arg("--without-skill")
        .output()
        .ok()?;
    Some((db, String::from_utf8_lossy(&out.stdout).to_string()))
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

/// The handle of a symbol, found by its qualified name.
///
/// `cairn symbol` searches names, and the name of `Registry.Add` is `Add` — the
/// qualification is derived for display. So the search is on the bare name and the row
/// is picked by what it prints.
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

#[test]
fn declared_facts_reach_every_answer_that_rests_on_them() {
    let Some((db, index_log)) = index_with_connector("ok", "ok", 30) else {
        eprintln!("SKIP: no fixture SCIP to build an index from");
        return;
    };

    // The index says what the connector contributed, in its own line.
    assert!(
        index_log.contains("w17:") || index_log.contains("w17: "),
        "the index run said nothing about the connector:\n{index_log}"
    );

    // The node is addressable by the id the connector gave it — not by a handle the
    // caller would have to look up first.
    let (symbol, code) = run(&db, &["expand", "w17:rpc/Station.Register"]);
    assert_eq!(code, 0, "{symbol}");
    assert!(
        symbol.contains("rpc/Station.Register"),
        "the declared node is not addressable by its id:\n{symbol}"
    );
    // Its anchor is a definition, so the answer points at the declaration rather than
    // saying it has none.
    assert!(
        symbol.contains("proto/telemetry.proto:7"),
        "the declaration's anchor is missing:\n{symbol}"
    );

    // The blast radius crosses the declared dependency edge: walking callers of the Go
    // method finds the rpc that declares it, labelled with the connector that said so.
    let handle = handle_of(&db, "Registry.Add");
    let (callers, _) = run(&db, &["graph", &handle, "--aspect", "callers"]);
    assert!(
        callers.contains("rpc/Station.Register"),
        "the declared caller is not in the call graph:\n{callers}"
    );
    assert!(
        callers.contains("L1, declared by w17"),
        "a declared edge has to name the connector that stated it:\n{callers}"
    );

    // What the connector could not determine is on the answer, not only in the index log.
    let (refs, _) = run(&db, &["refs", &handle]);
    assert!(
        refs.contains("Station.Retire"),
        "the connector's own gap is missing from an ordinary answer:\n{refs}"
    );

    // And `status` names the connector, with its version as the connector reported it.
    let (status, _) = run(&db, &["status"]);
    assert!(
        status.contains("connector  w17") && status.contains("1.0.0"),
        "status does not name the connector:\n{status}"
    );
}

#[test]
fn within_scopes_an_answer_to_a_declared_container() {
    let Some((db, _)) = index_with_connector("within", "ok", 30) else {
        return;
    };
    // The rpc is part of the domain; the Go method is not. Walking the rpc's callers
    // inside the domain keeps nothing, and that is the honest answer — containment
    // groups, it does not propagate.
    let handle = handle_of(&db, "Registry.Add");

    let (unscoped, _) = run(&db, &["graph", &handle, "--aspect", "callers"]);
    let (scoped, _) = run(
        &db,
        &[
            "graph",
            &handle,
            "--aspect",
            "callers",
            "--within",
            "w17:domain/telemetry",
        ],
    );
    assert!(scoped.contains("within w17:domain/telemetry"), "{scoped}");
    assert!(
        scoped.contains("rpc/Station.Register"),
        "the rpc is inside the domain and has to survive the filter:\n{scoped}"
    );
    assert!(
        scoped.lines().count() < unscoped.lines().count(),
        "the filter dropped nothing, so it is not filtering:\nscoped:\n{scoped}\nunscoped:\n{unscoped}"
    );
    // A filtered answer that does not say what it dropped reads as the whole set.
    assert!(
        scoped.contains("--within"),
        "the envelope does not report what the scope cut:\n{scoped}"
    );
}

#[test]
fn an_answer_is_grouped_by_the_container_its_rows_are_in() {
    // The other half of containment, and the one that is useful without a filter: a flat
    // answer says which declared container each row belongs to, so "which domain uses
    // this" is readable at a glance rather than one query per domain.
    let Some((db, _)) = index_with_connector("grouped", "ok", 30) else {
        return;
    };
    let handle = handle_of(&db, "Registry.Add");
    let (out, _) = run(
        &db,
        &["graph", &handle, "--aspect", "callers", "--view", "list"],
    );
    assert!(
        out.contains("within w17:domain/telemetry"),
        "the rows are not grouped under the container they are in:\n{out}"
    );
    // The heading sits above the row it describes, not after it.
    let heading = out
        .lines()
        .position(|l| l.contains("within w17:domain/telemetry"));
    let row = out.lines().position(|l| l.contains("rpc/Station.Register"));
    assert!(
        heading < row,
        "the heading has to come before its rows:\n{out}"
    );
    // And a row in no container is not filed under the previous heading.
    assert!(
        out.contains("(no declared container)"),
        "rows outside every container need a heading of their own:\n{out}"
    );

    // In JSON the container is on the row itself, so a consumer can group it any way it
    // likes rather than parsing a layout.
    let (json, _) = run(
        &db,
        &["--format", "json", "graph", &handle, "--aspect", "callers"],
    );
    let doc: serde_json::Value = serde_json::from_str(&json).expect("the answer is JSON");
    let nodes = doc["data"]["nodes"].as_array().expect("nodes");
    assert!(
        nodes.iter().any(|n| n["symbol"]["containers"]
            .as_array()
            .map(|c| c.iter().any(|x| x == "w17:domain/telemetry"))
            .unwrap_or(false)),
        "no row carries its container in the data:\n{json}"
    );
}

#[test]
fn an_unresolved_code_reference_is_expected_rather_than_dropped() {
    let Some((db, _)) = index_with_connector("expected", "expected", 30) else {
        return;
    };
    // The contract exists even though the implementation is not in the index
    // (architecture 7.2). The edge is kept, the target is unresolved, and the gap is
    // reported — dropping it would make a contract that exists look like one nobody
    // implements.
    let (status, _) = run(&db, &["status"]);
    assert!(status.contains("connector  w17"), "{status}");
    let (answer, _) = run(&db, &["expand", "w17:rpc/Station.Retire"]);
    assert!(
        answer.contains("expected rather than resolved"),
        "the unresolved reference is not reported on an answer:\n{answer}"
    );
}

#[test]
fn every_way_a_connector_can_fail_lands_on_the_answer() {
    // Three failures, three reports. None of them may be silent and none of them may
    // fail the index: the rest of the answer is still worth having.
    let cases: &[(&str, &str, u64, &str)] = &[
        ("broken", "broken", 30, "is not a fact this protocol knows"),
        ("fail", "fail", 30, "failed (exited 1"),
        // One second, so the hanging connector is killed rather than waited for.
        ("hang", "hang", 1, "timed out"),
    ];
    for (label, mode, timeout, expected) in cases {
        let Some((db, _)) = index_with_connector(label, mode, *timeout) else {
            return;
        };
        let (answer, code) = run(&db, &["status"]);
        assert!(
            (0..=3).contains(&code),
            "a failing connector broke the exit contract: {code}"
        );
        assert!(
            answer.contains(expected),
            "a {mode} connector did not report {expected:?} on an answer:\n{answer}"
        );
        if *mode == "broken" {
            // The line number is the point: "a kind was wrong" is not actionable.
            assert!(
                answer.contains("line 2") || answer.contains("line 3"),
                "a rejected line has to be named by number:\n{answer}"
            );
        }
    }
}

#[test]
fn two_runs_over_unchanged_inputs_produce_the_same_key_and_the_same_facts() {
    let Some((db, second_log)) = index_with_connector("cache", "ok", 30) else {
        return;
    };
    // `index_with_connector` runs the index twice, so the second run is the cached one.
    assert!(
        second_log.contains("from cache"),
        "the second run re-ran a connector whose inputs had not changed:\n{second_log}"
    );
    let key = db
        .parent()
        .map(|d| d.join("cache/connectors/w17.key"))
        .expect("a cache directory beside the index");
    let first_key = std::fs::read_to_string(&key).expect("a cache key");
    assert!(!first_key.trim().is_empty());

    // Facts unchanged across the rebuild, which is what "deterministic" has to mean for
    // a connector: the same inputs, the same answer.
    let (first, _) = run(&db, &["expand", "w17:rpc/Station.Register"]);
    let Some((db2, _)) = index_with_connector("cache2", "ok", 30) else {
        return;
    };
    let (second, _) = run(&db2, &["expand", "w17:rpc/Station.Register"]);
    let strip = |s: &str| {
        s.lines()
            .filter(|l| !l.starts_with("index "))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        strip(&first),
        strip(&second),
        "two indexes built from the same inputs describe the declared node differently"
    );
    let key2 = std::fs::read_to_string(
        db2.parent()
            .map(|d| d.join("cache/connectors/w17.key"))
            .unwrap(),
    )
    .expect("a cache key");
    assert_eq!(
        first_key.trim(),
        key2.trim(),
        "the same manifest over the same inputs produced two different cache keys"
    );
}

#[test]
fn removing_the_manifest_removes_every_trace_of_the_connector() {
    // I1, as a test rather than as a promise: with no manifest, the answer is the one
    // cairn gave before connectors existed.
    let root = workspace_root();
    let bin = binary();
    let Some((db, _)) = index_with_connector("removed", "ok", 30) else {
        return;
    };
    let (before, _) = run(&db, &["status"]);
    assert!(before.contains("connector  w17"));

    std::fs::remove_file(db.parent().unwrap().join("connectors/w17.yaml")).unwrap();
    let dir = db.parent().unwrap().to_path_buf();
    build_fixture_index_in(&dir, &root, &bin).expect("reindexing without the manifest");

    let (after, _) = run(&db, &["status"]);
    assert!(
        !after.contains("w17"),
        "the connector is gone and its name is still in the answer:\n{after}"
    );
    assert!(
        run(&db, &["expand", "w17:rpc/Station.Register"]).1 != 0,
        "a declared node outlived the connector that stated it"
    );

    // Byte for byte against an index that never had a connector at all.
    let plain = build_fixture_index(&root, &bin, "connectors-plain").expect("a plain index");
    let (plain_status, _) = run(&plain, &["status"]);
    let strip = |s: &str| {
        s.lines()
            .filter(|l| !l.starts_with("index "))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        strip(&after),
        strip(&plain_status),
        "an index that once had a connector does not answer like one that never did"
    );
}
