//! The published JSON schemas, and real answers measured against them.
//!
//! `docs/json-schema/v1/` is a contract with whoever wraps this tool, and a contract
//! nothing checks is a comment. Two things are checked here, and they fail for different
//! reasons:
//!
//!   * **The committed schema is what the code generates.** Add a field to a data struct
//!     and the schema file is stale until it is regenerated —
//!     `CAIRN_BLESS=1 cargo test --test schema` does that, the same gesture the golden
//!     recordings use.
//!   * **Real output validates against it.** The schema comes from the same structs that
//!     serialise the answer, so this cannot catch a mismatched field name; what it does
//!     catch is the gap between a type and its instances — a field `serde` skips, a value
//!     that is null where the schema says it never is, an enum printing a word the schema
//!     does not list.
//!
//! The validator is deliberately small and only understands the subset schemars emits
//! for these types. A validator that quietly passes everything would be worse than none,
//! so `a_broken_document_is_rejected` feeds it documents that must fail.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;
use common::build_fixture_index;

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

fn schema_dir() -> PathBuf {
    workspace_root().join("docs/json-schema/v1")
}

/// Where a validation failure is, in the document rather than in the schema.
type Problems = Vec<String>;

/// Does `doc` satisfy `schema`? Problems are collected rather than returned one at a
/// time: a document with four wrong fields is one fix, not four runs.
fn validate(
    doc: &serde_json::Value,
    schema: &serde_json::Value,
    defs: &serde_json::Value,
    at: &str,
    problems: &mut Problems,
) {
    use serde_json::Value;

    // `$ref` into `$defs`, which is how schemars names a nested struct.
    if let Some(r) = schema.get("$ref").and_then(|r| r.as_str()) {
        let name = r.rsplit('/').next().unwrap_or_default();
        match defs.get(name) {
            Some(target) => validate(doc, target, defs, at, problems),
            None => problems.push(format!(
                "{at}: schema refers to $defs/{name}, which is not there"
            )),
        }
        return;
    }

    // A union: `Option<T>` and an untagged enum both arrive this way. Valid if any
    // branch is, and the branch errors are dropped — reporting all of them for a
    // nullable field would bury the one that matters.
    for key in ["anyOf", "oneOf"] {
        if let Some(branches) = schema.get(key).and_then(|b| b.as_array()) {
            let ok = branches.iter().any(|b| {
                let mut ignored = Problems::new();
                validate(doc, b, defs, at, &mut ignored);
                ignored.is_empty()
            });
            if !ok {
                problems.push(format!("{at}: no {key} branch accepts {}", brief(doc)));
            }
            return;
        }
    }

    // `type` is either a word or a list of them; a list is how a nullable scalar is
    // spelled.
    let types: Vec<&str> = match schema.get("type") {
        None => Vec::new(),
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(|t| t.as_str()).collect(),
        Some(other) => {
            problems.push(format!(
                "{at}: schema has a type of {other}, which this validator does not read"
            ));
            return;
        }
    };
    if !types.is_empty() && !types.iter().any(|t| matches_type(doc, t)) {
        problems.push(format!(
            "{at}: expected {}, found {}",
            types.join(" or "),
            brief(doc)
        ));
        return;
    }

    if let Some(allowed) = schema.get("enum").and_then(|e| e.as_array()) {
        if !allowed.contains(doc) {
            problems.push(format!(
                "{at}: {} is not one of the allowed values",
                brief(doc)
            ));
        }
    }

    if let Some(items) = schema.get("items") {
        if let Some(array) = doc.as_array() {
            for (i, v) in array.iter().enumerate() {
                validate(v, items, defs, &format!("{at}[{i}]"), problems);
            }
        }
    }

    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        let Some(object) = doc.as_object() else {
            return;
        };
        for (name, sub) in props {
            if let Some(v) = object.get(name) {
                validate(v, sub, defs, &format!("{at}.{name}"), problems);
            }
        }
        // A required field that is absent, and a field the schema never declared. The
        // second is the direction that matters: it is what a new field looks like before
        // the schema is regenerated.
        if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
            for name in required.iter().filter_map(|n| n.as_str()) {
                if !object.contains_key(name) {
                    problems.push(format!("{at}: required field `{name}` is missing"));
                }
            }
        }
        // Strict unless the schema says otherwise. JSON Schema's default is the
        // opposite — anything not declared is allowed — and schemars only writes
        // `additionalProperties: false` for a type that asked for it. That default is
        // wrong for this test: a field printed and not published is precisely the
        // mismatch between a type and its instances that this exists to catch.
        if schema.get("additionalProperties") != Some(&Value::Bool(true)) {
            for name in object.keys() {
                if !props.contains_key(name) {
                    problems.push(format!("{at}: field `{name}` is not in the schema"));
                }
            }
        }
    }
}

fn matches_type(v: &serde_json::Value, t: &str) -> bool {
    use serde_json::Value;
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "integer" => matches!(v, Value::Number(n) if n.is_i64() || n.is_u64()),
        "number" => v.is_number(),
        _ => true,
    }
}

/// Enough of a value to recognise it, never the whole thing: an answer's body is a page.
fn brief(v: &serde_json::Value) -> String {
    let s = match v {
        serde_json::Value::Object(_) => "an object".to_string(),
        serde_json::Value::Array(a) => format!("an array of {}", a.len()),
        other => other.to_string(),
    };
    if s.chars().count() > 60 {
        format!("{}…", s.chars().take(60).collect::<String>())
    } else {
        s
    }
}

/// One invocation per command with data, against the fixture index.
///
/// The arguments are the same shape the golden recordings use, because the point is the
/// answers a caller actually gets rather than a hand-built document that happens to fit.
fn invocations(handle: &str, other: &str, repo: &Path) -> Vec<(&'static str, Vec<String>)> {
    let repo = repo.to_string_lossy().to_string();
    let arg = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<String>>();
    vec![
        ("symbol", arg(&["symbol", "Registry"])),
        ("refs", arg(&["refs", handle])),
        ("graph", arg(&["graph", handle, "--aspect", "callers"])),
        ("path", arg(&["path", handle, other])),
        ("affects", arg(&["affects", handle])),
        ("runs", arg(&["runs", handle])),
        ("reaches", arg(&["reaches", handle])),
        (
            "expand",
            arg(&["expand", handle, "--detail", "body", "--repo", &repo]),
        ),
        ("status", arg(&["status"])),
        ("verify", arg(&["verify", "--repo", &repo])),
        ("concept-list", arg(&["concept", "list"])),
        ("links", arg(&["links", handle])),
    ]
}

/// `flow` needs a provider, and CI has none. The recorded answer goes into the cache the
/// way a real run would key it — on the content of the file — so the schema of a command
/// that spawns a subprocess is still checked against a real answer.
///
/// Returns the handle whose flow was seeded.
fn seed_flow(root: &Path, db: &Path) -> Option<String> {
    let recorded = std::fs::read_to_string(
        root.join("crates/cairn-cli/tests/fixtures/flow/upload-readings.json"),
    )
    .ok()?;
    let file = root.join("crates/cairn-cli/tests/fixtures/corpus/srcgo/internal/ingest/service.go");
    let blob = blake3::hash(&std::fs::read(file).ok()?).as_bytes()[..16].to_vec();
    let conn = rusqlite::Connection::open(db).ok()?;
    let (symbol_id, handle): (i64, String) = conn
        .query_row(
            // Constrained to the file the recorded answer is about: the corpus has two
            // symbols called `UploadReadings`, and the other one is the generated
            // interface method in a different file - whose contents would key the cache
            // to a blob the recorded flow was never computed from.
            "SELECT h.symbol_id, h.handle
               FROM handles h
               JOIN symbols s ON s.id = h.symbol_id
               JOIN strings n ON n.id = s.name_id
               JOIN files   f ON f.id = s.def_file_id
               JOIN strings p ON p.id = f.path_id
              WHERE n.s = 'UploadReadings'
                AND p.s = 'srcgo/internal/ingest/service.go'
              LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()?;
    conn.execute(
        "INSERT INTO flows(symbol_id, blob, provider, json) VALUES (?1, ?2, 'cairnflow 1', ?3)
         ON CONFLICT(symbol_id) DO UPDATE SET blob = excluded.blob, json = excluded.json",
        rusqlite::params![symbol_id, blob, recorded],
    )
    .ok()?;
    Some(handle)
}

/// The one command whose data needs something recorded first.
fn seed_a_concept(bin: &Path, db: &Path, handle: &str) {
    for args in [
        vec![
            "concept",
            "add",
            "ingest-path",
            "--note",
            "the ingest chain",
        ],
        vec!["concept", "link", "ingest-path", handle, "--rel", "part-of"],
    ] {
        let out = Command::new(bin)
            .arg("--db")
            .arg(db)
            .args(&args)
            .output()
            .expect("recording a concept");
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn first_handles(db: &Path) -> (String, String) {
    let conn = rusqlite::Connection::open(db).expect("opening the index");
    let mut stmt = conn
        .prepare(
            "SELECT h.handle FROM handles h
               JOIN symbols s ON s.id = h.symbol_id
              WHERE s.def_file_id IS NOT NULL AND s.ref_count > 0
              ORDER BY s.ref_count DESC, h.symbol_id
              LIMIT 2",
        )
        .expect("listing handles");
    let handles: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .expect("reading handles")
        .filter_map(|h| h.ok())
        .collect();
    assert!(handles.len() >= 2, "the fixture index has too few symbols");
    (handles[0].clone(), handles[1].clone())
}

#[test]
fn the_published_schemas_are_what_the_code_generates() {
    let dir = schema_dir();
    let bless = std::env::var_os("CAIRN_BLESS").is_some();
    let generated: BTreeMap<&str, serde_json::Value> =
        cairn_fmt::answer_schemas().into_iter().collect();

    if bless {
        std::fs::create_dir_all(&dir).expect("schema directory");
        for (command, schema) in &generated {
            let mut text = serde_json::to_string_pretty(schema).expect("rendering a schema");
            text.push('\n');
            std::fs::write(dir.join(format!("{command}.json")), text).expect("writing a schema");
        }
        eprintln!("blessed {} schemas in {}", generated.len(), dir.display());
        return;
    }

    let mut stale: Vec<String> = Vec::new();
    for (command, schema) in &generated {
        let path = dir.join(format!("{command}.json"));
        let on_disk = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => {
                stale.push(format!("{command}.json is missing"));
                continue;
            }
        };
        let parsed: serde_json::Value = match serde_json::from_str(&on_disk) {
            Ok(v) => v,
            Err(e) => {
                stale.push(format!("{command}.json is not JSON: {e}"));
                continue;
            }
        };
        if parsed != *schema {
            stale.push(format!(
                "{command}.json no longer matches the structs it was generated from"
            ));
        }
    }
    assert!(
        stale.is_empty(),
        "{} published schema(s) are out of date. Regenerate with \
         `CAIRN_BLESS=1 cargo test --test schema`:\n  {}",
        stale.len(),
        stale.join("\n  ")
    );
}

#[test]
fn every_command_with_data_validates_against_its_schema() {
    let root = workspace_root();
    let bin = binary();
    assert!(bin.exists(), "cairn not built at {}", bin.display());
    let Some(db) = build_fixture_index(&root, &bin, "schema") else {
        eprintln!("SKIP: no fixture SCIP to build an index from");
        return;
    };
    let repo = root.join("crates/cairn-cli/tests/fixtures/corpus");
    let (handle, other) = first_handles(&db);
    seed_a_concept(&bin, &db, &handle);
    let flow_handle = seed_flow(&root, &db);

    let schemas: BTreeMap<&str, serde_json::Value> =
        cairn_fmt::answer_schemas().into_iter().collect();
    let mut problems: Problems = Vec::new();
    let mut checked = 0usize;
    let mut cases = invocations(&handle, &other, &repo);
    if let Some(h) = &flow_handle {
        cases.push((
            "flow",
            vec![
                "flow".to_string(),
                h.clone(),
                "--repo".to_string(),
                repo.to_string_lossy().to_string(),
            ],
        ));
    }
    cases.push(("concept-show", {
        ["concept", "show", "ingest-path"]
            .iter()
            .map(|a| a.to_string())
            .collect()
    }));

    for (command, args) in &cases {
        let out = Command::new(&bin)
            .arg("--db")
            .arg(&db)
            .arg("--format")
            .arg("json")
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("running {args:?}: {e}"));
        let doc: serde_json::Value = match serde_json::from_slice(&out.stdout) {
            Ok(v) => v,
            Err(e) => {
                problems.push(format!(
                    "{command}: --format json did not produce JSON ({e}); stderr: {}",
                    String::from_utf8_lossy(&out.stderr)
                        .lines()
                        .next()
                        .unwrap_or("")
                ));
                continue;
            }
        };
        // A `data: null` here would make the validation below pass by saying nothing,
        // which is exactly the silence these commands were converted to remove.
        if doc.get("data").map(|d| d.is_null()).unwrap_or(true) {
            problems.push(format!(
                "{command}: answered with `data: null`, so its schema describes nothing it printed"
            ));
            continue;
        }
        let schema = schemas
            .get(*command)
            .unwrap_or_else(|| panic!("no schema listed for {command}"));
        let defs = schema
            .get("$defs")
            .cloned()
            .unwrap_or(serde_json::Value::Object(Default::default()));
        validate(&doc, schema, &defs, command, &mut problems);
        checked += 1;
    }

    assert!(
        problems.is_empty(),
        "{} problem(s) across {checked} answers:\n  {}",
        problems.len(),
        problems.join("\n  ")
    );
    assert_eq!(
        checked,
        schemas.len(),
        "{checked} of {} schemas were exercised; a schema nothing runs against is a \
         document, not a contract",
        schemas.len()
    );
}

#[test]
fn a_broken_document_is_rejected() {
    // The validator's own test. Each document below breaks one rule, and a validator
    // that accepts any of them is not checking the thing this file claims to check.
    let schema = cairn_fmt::answer_schema::<cairn_fmt::ExpandData>();
    let defs = schema.get("$defs").cloned().unwrap_or_default();
    let good = serde_json::json!({
        "schema": "cairn.answer/1",
        "command": "expand",
        "rows": null,
        "truncated": false,
        "unknown": [],
        "suppressed": [],
        "stale": {"tracked": null, "dirty": [], "mentioned": [], "notes": []},
        "body": "text",
        "data": {
            "detail": "skeleton",
            "source": [],
            "symbol": {
                "handle": "abc", "symbol": null, "name": "f", "qualified": "f",
                "kind": "fn", "lang": "go", "file": null, "line": null,
                "end_line": null, "generated": false, "refs": 0, "source": null,
                "containers": []
            }
        }
    });
    let mut problems = Problems::new();
    validate(&good, &schema, &defs, "good", &mut problems);
    assert!(
        problems.is_empty(),
        "the validator rejected a document that is correct:\n  {}",
        problems.join("\n  ")
    );

    let mutate = |f: &dyn Fn(&mut serde_json::Value)| {
        let mut doc = good.clone();
        f(&mut doc);
        let mut problems = Problems::new();
        validate(&doc, &schema, &defs, "broken", &mut problems);
        problems
    };
    /// One way to break a document, and a name for it.
    type Break = (&'static str, Box<dyn Fn(&mut serde_json::Value)>);
    let cases: Vec<Break> = vec![
        (
            "a missing required field",
            Box::new(|d: &mut serde_json::Value| {
                d["data"].as_object_mut().unwrap().remove("detail");
            }),
        ),
        (
            "a field of the wrong type",
            Box::new(|d: &mut serde_json::Value| {
                d["data"]["symbol"]["generated"] = serde_json::json!("yes");
            }),
        ),
        (
            "a field the schema does not declare",
            Box::new(|d: &mut serde_json::Value| {
                d["data"]["symbol"]["invented"] = serde_json::json!(1);
            }),
        ),
        (
            "an array element of the wrong shape",
            Box::new(|d: &mut serde_json::Value| {
                d["data"]["source"] = serde_json::json!([{"line": "x", "text": "y"}]);
            }),
        ),
    ];
    for (what, break_it) in cases {
        let problems = mutate(&*break_it);
        assert!(
            !problems.is_empty(),
            "the validator accepted {what}, so it is not validating anything"
        );
    }
}
