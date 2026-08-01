//! The live overlay: what the index cannot know because the file changed since it was built.
//!
//! When a file is dirty, the index is describing a version of it that no longer exists.
//! Rather than answer from stale data or refuse, cairn asks a language server what the file
//! looks like *now*. That is the only path in the tool that depends on a process it does
//! not control, which makes it the one most likely to fail in someone else's environment.
//!
//! Two properties matter, and the second matters more.
//!
//! It has to work: a real server, a real file, symbols back. And it has to fail *quietly*
//! when the server is not installed — which is the normal state for most people who will
//! ever run this. A missing `pyright` must degrade to "no live view, the index is what
//! there is", never to a crash, and never to a hang that looks like the tool being slow.

use cairn_daemon::lsp::{Pool, ServerSpec};
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("cairn-hot-path").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn have(binary: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(binary).exists()))
        .unwrap_or(false)
}

#[test]
fn a_language_with_no_server_is_none_rather_than_a_guess() {
    let root = scratch("unknown-lang");
    assert!(ServerSpec::for_lang("py", root.clone()).is_some());
    assert!(ServerSpec::for_lang("go", root.clone()).is_some());
    // Not "try something and see": a language cairn has no server for has no live view,
    // and saying so is the difference between a stale answer and a wrong one.
    assert!(ServerSpec::for_lang("cobol", root.clone()).is_none());
    assert!(ServerSpec::for_lang("", root).is_none());
}

#[test]
fn a_server_that_is_not_installed_is_recorded_rather_than_fatal() {
    // The normal state for most people who install cairn. It must cost them a missing
    // live view and nothing else.
    let root = scratch("missing-server");
    let mut pool = Pool::new(
        &root,
        &[("nosuchlang".to_string(), "src".to_string())],
        None,
    );
    let langs = pool.languages();
    assert!(
        !langs.contains(&"nosuchlang".to_string()),
        "a language with no server must not be offered as available"
    );
    // Asking anyway must not panic, and must not hang: this is inside a test with no
    // timeout, so a hang here fails the suite by wall clock rather than silently.
    let out = pool.document_symbols("src/whatever.xyz");
    assert!(
        out.is_err() || out.as_ref().is_ok_and(|v| v.is_empty()),
        "an unavailable server produced symbols from nowhere"
    );
}

#[test]
fn a_real_server_answers_with_the_file_as_it_is_now() {
    if !have("pyright-langserver") {
        eprintln!("SKIP: pyright-langserver not on PATH");
        return;
    }
    let root = scratch("live-python");
    let file = root.join("sample.py");
    std::fs::write(
        &file,
        "class Alpha:\n    def beta(self):\n        pass\n\ndef gamma():\n    pass\n",
    )
    .expect("writing the sample");

    let spec = ServerSpec::for_lang("py", root.clone()).expect("a python server spec");
    let mut server = match cairn_daemon::lsp::Server::start(spec) {
        Ok(s) => s,
        Err(e) => {
            // Starting a language server is the one thing here that can fail for
            // environmental reasons. Reported, not asserted away.
            eprintln!("SKIP: pyright would not start: {e:#}");
            return;
        }
    };
    let text = std::fs::read_to_string(&file).expect("reading it back");
    let symbols = server
        .document_symbols(&file, &text)
        .expect("asking for document symbols");
    server.shutdown();

    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"Alpha") && names.contains(&"gamma"),
        "the live view missed top-level definitions: {names:?}"
    );
    assert!(
        names.contains(&"beta"),
        "the live view did not flatten members: {names:?}"
    );
}
