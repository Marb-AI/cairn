//! Per-connector refresh: the right one reruns, and nothing waits for it.
//!
//! A connector's inputs are usually not code — a `.proto` file, a contract, a schema —
//! so none of the machinery that watches indexed files has anything to say about them. A
//! change there leaves declared facts describing the previous version of a contract while
//! every answer reports a clean tree, which is the quiet staleness this whole design is
//! built to refuse.
//!
//! Two properties, and the second is the one that keeps the daemon usable:
//!
//!   * **Only the matching connector reruns.** Re-running every connector on every save
//!     would make the watcher the most expensive thing in the repository.
//!   * **A query never waits for a rerun** (D7). While a refresh is in flight, the
//!     answer is the previous facts with the connector named — not a pause.
//!
//! The refresh callback is the caller's, so this needs no store and no real connector:
//! what is tested is the daemon's decision, which is the part that lives here.

use cairn_daemon::watch::{ConnectorWatch, DirtyTracker};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("cairn-connector-refresh")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A tracker over a scratch tree, with two connectors reading different inputs.
fn tracker_with_two(
    dir: &std::path::Path,
    refresh: cairn_daemon::RefreshConnector,
) -> Arc<DirtyTracker> {
    let tracker = Arc::new(DirtyTracker::new(dir, HashMap::new()));
    tracker.set_connectors(
        vec![
            ConnectorWatch {
                name: "protos".to_string(),
                matches: Box::new(|rel: &str| rel.ends_with(".proto")),
            },
            ConnectorWatch {
                name: "compose".to_string(),
                matches: Box::new(|rel: &str| rel.starts_with("compose.")),
            },
        ],
        refresh,
    );
    tracker
}

/// Wait for a condition, or give up. Polling rather than sleeping a fixed time: a fixed
/// sleep is either flaky or slow, and on a loaded machine it is both.
fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("{what} did not happen within five seconds");
}

#[test]
fn only_the_connector_whose_inputs_changed_reruns() {
    let dir = scratch("one-of-two");
    std::fs::write(dir.join("orders.proto"), "service Orders {}\n").unwrap();
    let asked: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&asked);
    let tracker = tracker_with_two(
        &dir,
        Box::new(move |name: &str| {
            log.lock().unwrap().push(name.to_string());
            Ok(())
        }),
    );

    let (_stop_tx, stop_rx) = std::sync::mpsc::channel();
    let watcher = Arc::clone(&tracker);
    std::thread::spawn(move || {
        let _ = watcher.watch_forever(stop_rx);
    });
    // The watcher needs to be watching before the write, or the event is missed and the
    // test passes or fails on timing rather than on behaviour.
    std::thread::sleep(Duration::from_millis(200));

    std::fs::write(
        dir.join("orders.proto"),
        "service Orders { rpc Create(); }\n",
    )
    .unwrap();
    eventually("the proto connector was asked to refresh", || {
        !asked.lock().unwrap().is_empty()
    });
    // Settle, so a second connector being asked late would still be caught.
    std::thread::sleep(Duration::from_millis(300));

    let asked = asked.lock().unwrap().clone();
    assert_eq!(
        asked,
        vec!["protos"],
        "a .proto change is nothing to a connector that reads compose files"
    );
}

#[test]
fn a_query_during_a_refresh_answers_from_the_previous_facts() {
    let dir = scratch("never-blocks");
    std::fs::write(dir.join("orders.proto"), "service Orders {}\n").unwrap();
    // A refresh that takes its time, which is what a real connector does: a subprocess,
    // a project model, a few hundred milliseconds at best.
    let started = Arc::new(Mutex::new(false));
    let flag = Arc::clone(&started);
    let tracker = tracker_with_two(
        &dir,
        Box::new(move |_name: &str| {
            *flag.lock().unwrap() = true;
            std::thread::sleep(Duration::from_millis(800));
            Ok(())
        }),
    );

    let (_stop_tx, stop_rx) = std::sync::mpsc::channel();
    let watcher = Arc::clone(&tracker);
    std::thread::spawn(move || {
        let _ = watcher.watch_forever(stop_rx);
    });
    std::thread::sleep(Duration::from_millis(200));
    std::fs::write(
        dir.join("orders.proto"),
        "service Orders { rpc Create(); }\n",
    )
    .unwrap();
    eventually("the refresh started", || *started.lock().unwrap());

    // The query, while the refresh is still running. Both halves matter: that it returns
    // at once, and that it says which connector the answer is behind.
    let asked_at = Instant::now();
    let snapshot = tracker.snapshot();
    let took = asked_at.elapsed();
    assert!(
        took < Duration::from_millis(200),
        "a query waited {took:?} for a connector - D7 says it must never wait"
    );
    assert_eq!(
        snapshot.stale_connectors,
        vec!["protos".to_string()],
        "the answer does not say whose facts are the previous run's"
    );

    // And once the refresh finishes, the answer stops claiming staleness that is over.
    eventually("the refresh finished and cleared the flag", || {
        tracker.snapshot().stale_connectors.is_empty()
    });
}

#[test]
fn a_failed_refresh_leaves_the_connector_stale() {
    // The facts in the index are the previous run's and nothing has replaced them, so
    // every answer has to go on saying so. Clearing the flag on failure would be the
    // worst of both: stale facts, presented as fresh.
    let dir = scratch("failed");
    std::fs::write(dir.join("orders.proto"), "service Orders {}\n").unwrap();
    let tracker = tracker_with_two(
        &dir,
        Box::new(|_name: &str| anyhow::bail!("the project model could not be read")),
    );

    let (_stop_tx, stop_rx) = std::sync::mpsc::channel();
    let watcher = Arc::clone(&tracker);
    std::thread::spawn(move || {
        let _ = watcher.watch_forever(stop_rx);
    });
    std::thread::sleep(Duration::from_millis(200));
    std::fs::write(
        dir.join("orders.proto"),
        "service Orders { rpc Create(); }\n",
    )
    .unwrap();

    eventually("the connector was marked stale", || {
        !tracker.snapshot().stale_connectors.is_empty()
    });
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        tracker.snapshot().stale_connectors,
        vec!["protos".to_string()],
        "a refresh that failed cleared the staleness it did not fix"
    );
}
