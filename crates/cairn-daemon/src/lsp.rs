//! Language server pool — the hot path for files that have changed since indexing.
//!
//! This is the other half of the dirty overlay (architecture 4.2). The batch indexers
//! cannot answer about a modified file without a full run, but a warm language server
//! can, and measurement says how fast: after an edit, `documentSymbol` costs 4-5 ms for
//! pyright and 3.6-7.3 ms for gopls, `references` 94-115 ms and 23-27 ms respectively
//! (docs/spike-0-results.md 4.2c).
//!
//! Three things that measurement taught, all of which shape the code below:
//!
//! * **A client must answer server-initiated requests.** pyright asks for
//!   `workspace/configuration` during start-up and blocks until it gets a reply. The
//!   first version of the benchmark ignored those and appeared to show a 180 s timeout
//!   on every request.
//! * **The first cross-file query is a different class.** pyright's first `references`
//!   took 1.35 s even after a long warm-up, against 130 ms warm. Callers must not treat
//!   the first answer's latency as typical, and the pool warms servers in the
//!   background rather than on the first question.
//! * **The languages are not symmetric.** pyright is roughly 4x slower on the hot path,
//!   which is the reverse of the batch picture. Timeouts are per language, not shared.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Per-language request deadline. Generous relative to the measurements so that a busy
/// machine does not produce spurious failures, but bounded: the caller has a query
/// waiting, and a slow answer is worse than an honest "could not ask".
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// How a language server is launched.
#[derive(Debug, Clone)]
pub struct ServerSpec {
    pub lang: String,
    pub command: Vec<String>,
    /// Root the server is initialised at — the language's source root, not the repo
    /// root: pyright wants `srcpy/`, gopls wants `srcgo/`.
    pub root: PathBuf,
    pub language_id: String,
}

impl ServerSpec {
    /// Default launcher for a language tag as recorded by the indexer.
    pub fn for_lang(lang: &str, root: PathBuf) -> Option<ServerSpec> {
        let (command, language_id) = match lang {
            "py" => (vec!["pyright-langserver".into(), "--stdio".into()], "python"),
            "go" => (vec!["gopls".into(), "-mode=stdio".into()], "go"),
            "ts" => (
                vec!["typescript-language-server".into(), "--stdio".into()],
                "typescript",
            ),
            _ => return None,
        };
        Some(ServerSpec {
            lang: lang.to_string(),
            command,
            root,
            language_id: language_id.to_string(),
        })
    }
}

/// One symbol as the language server currently sees it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LiveSymbol {
    pub name: String,
    pub kind: i64,
    pub start_line: i64,
    pub end_line: i64,
    pub container: Option<String>,
}

struct Pending {
    tx: Sender<serde_json::Value>,
}

/// A running language server.
pub struct Server {
    spec: ServerSpec,
    child: Child,
    /// Shared with the reply pump: `ChildStdin` cannot be cloned, and both the request
    /// path and the answers to server-initiated requests have to write to it.
    stdin: Arc<Mutex<ChildStdin>>,
    next_id: AtomicI64,
    pending: Arc<Mutex<HashMap<i64, Pending>>>,
    /// Documents currently open, with their version counter.
    open: HashMap<String, i64>,
    ready_at: Option<Instant>,
}

impl Server {
    pub fn start(spec: ServerSpec) -> Result<Server> {
        let mut child = Command::new(&spec.command[0])
            .args(&spec.command[1..])
            .current_dir(&spec.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("spawning {:?}", spec.command))?;

        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let pending: Arc<Mutex<HashMap<i64, Pending>>> = Arc::default();

        let mut server = Server {
            spec,
            child,
            stdin: Arc::new(Mutex::new(stdin)),
            next_id: AtomicI64::new(1),
            pending: Arc::clone(&pending),
            open: HashMap::new(),
            ready_at: None,
        };
        server.spawn_reader(stdout, pending);
        server.initialize()?;
        Ok(server)
    }

    fn spawn_reader(
        &mut self,
        stdout: std::process::ChildStdout,
        pending: Arc<Mutex<HashMap<i64, Pending>>>,
    ) {
        // Server requests are answered on a queue drained by the writer side, so the
        // reader never blocks on the process's stdin.
        let (reply_tx, reply_rx) = channel::<serde_json::Value>();
        self.start_reply_pump(reply_rx);

        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let Some(msg) = read_message(&mut reader) else {
                    return;
                };
                // Server-initiated request: must be answered or the server stalls.
                if msg.get("method").is_some() && msg.get("id").is_some() {
                    let id = msg["id"].clone();
                    let method = msg["method"].as_str().unwrap_or("");
                    let result = match method {
                        "workspace/configuration" => {
                            let n = msg["params"]["items"].as_array().map(|a| a.len()).unwrap_or(1);
                            serde_json::Value::Array(vec![serde_json::json!({}); n])
                        }
                        _ => serde_json::Value::Null,
                    };
                    let _ = reply_tx.send(serde_json::json!({
                        "jsonrpc": "2.0", "id": id, "result": result
                    }));
                    continue;
                }
                if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
                    if let Some(p) = pending.lock().unwrap().remove(&id) {
                        let _ = p.tx.send(msg);
                    }
                }
            }
        });
    }

    fn start_reply_pump(&mut self, rx: Receiver<serde_json::Value>) {
        let stdin = Arc::clone(&self.stdin);
        std::thread::spawn(move || {
            while let Ok(msg) = rx.recv() {
                let _ = write_message(&mut *stdin.lock().unwrap(), &msg);
            }
        });
    }

    fn initialize(&mut self) -> Result<()> {
        let root_uri = format!("file://{}", self.spec.root.display());
        let params = serde_json::json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "workspaceFolders": [{"uri": root_uri, "name": "w"}],
            "capabilities": {
                "textDocument": {
                    "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                    "references": {},
                    "synchronization": {"didSave": true, "dynamicRegistration": false}
                },
                "workspace": {"workspaceFolders": true, "configuration": true},
                "window": {"workDoneProgress": true}
            }
        });
        self.request("initialize", params, Duration::from_secs(60))?;
        self.notify("initialized", serde_json::json!({}))?;
        self.ready_at = Some(Instant::now());
        Ok(())
    }

    fn notify(&mut self, method: &str, params: serde_json::Value) -> Result<()> {
        write_message(
            &mut *self.stdin.lock().unwrap(),
            &serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params}),
        )
    }

    fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel();
        self.pending.lock().unwrap().insert(id, Pending { tx });
        write_message(
            &mut *self.stdin.lock().unwrap(),
            &serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )?;
        match rx.recv_timeout(timeout) {
            Ok(msg) => {
                if let Some(err) = msg.get("error") {
                    return Err(anyhow!("{method} failed: {err}"));
                }
                Ok(msg.get("result").cloned().unwrap_or(serde_json::Value::Null))
            }
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(anyhow!("{method} timed out after {timeout:?}"))
            }
        }
    }

    /// Tell the server about the file's current contents and ask what is in it.
    ///
    /// The text is passed in rather than read here: the caller already has it, and on
    /// the dirty path what matters is the buffer as it stands, not what is on disk.
    pub fn document_symbols(&mut self, abs_path: &Path, text: &str) -> Result<Vec<LiveSymbol>> {
        let uri = format!("file://{}", abs_path.display());
        let version = self.open.entry(uri.clone()).or_insert(0);
        *version += 1;
        let version = *version;

        if version == 1 {
            self.notify(
                "textDocument/didOpen",
                serde_json::json!({"textDocument": {
                    "uri": uri, "languageId": self.spec.language_id,
                    "version": version, "text": text
                }}),
            )?;
        } else {
            self.notify(
                "textDocument/didChange",
                serde_json::json!({
                    "textDocument": {"uri": uri, "version": version},
                    "contentChanges": [{"text": text}]
                }),
            )?;
        }

        let result = self.request(
            "textDocument/documentSymbol",
            serde_json::json!({"textDocument": {"uri": uri}}),
            REQUEST_TIMEOUT,
        )?;
        Ok(flatten_symbols(&result, None))
    }

    pub fn shutdown(&mut self) {
        let _ = self.request("shutdown", serde_json::Value::Null, Duration::from_secs(5));
        let _ = self.notify("exit", serde_json::Value::Null);
        let _ = self.child.kill();
    }
}

/// Servers keyed by language, started on first use.
pub struct Pool {
    repo: PathBuf,
    specs: Vec<ServerSpec>,
    servers: HashMap<String, Server>,
    /// Languages whose server failed to start, so we stop retrying and can say why.
    failed: HashMap<String, String>,
}

impl Pool {
    pub fn new(repo: &Path, roots: &[(String, String)]) -> Pool {
        let specs = roots
            .iter()
            .filter_map(|(lang, rel)| ServerSpec::for_lang(lang, repo.join(rel)))
            .collect();
        Pool {
            repo: repo.to_path_buf(),
            specs,
            servers: HashMap::new(),
            failed: HashMap::new(),
        }
    }

    pub fn languages(&self) -> Vec<String> {
        self.specs.iter().map(|s| s.lang.clone()).collect()
    }

    pub fn failures(&self) -> &HashMap<String, String> {
        &self.failed
    }

    /// Which language a repo-relative path belongs to, by its extension.
    pub fn lang_of(path: &str) -> Option<&'static str> {
        match path.rsplit('.').next()? {
            "py" | "pyi" => Some("py"),
            "go" => Some("go"),
            "ts" | "tsx" | "js" | "jsx" => Some("ts"),
            _ => None,
        }
    }

    fn server_for(&mut self, lang: &str) -> Result<&mut Server> {
        if let Some(why) = self.failed.get(lang) {
            return Err(anyhow!("{lang} server unavailable: {why}"));
        }
        if !self.servers.contains_key(lang) {
            let spec = self
                .specs
                .iter()
                .find(|s| s.lang == lang)
                .cloned()
                .ok_or_else(|| anyhow!("no server configured for {lang}"))?;
            match Server::start(spec) {
                Ok(s) => {
                    self.servers.insert(lang.to_string(), s);
                }
                Err(e) => {
                    let why = format!("{e:#}");
                    self.failed.insert(lang.to_string(), why.clone());
                    return Err(anyhow!("{lang} server unavailable: {why}"));
                }
            }
        }
        Ok(self.servers.get_mut(lang).unwrap())
    }

    /// Current symbols in a file, straight from the language server.
    pub fn document_symbols(&mut self, rel_path: &str) -> Result<Vec<LiveSymbol>> {
        let lang = Pool::lang_of(rel_path)
            .ok_or_else(|| anyhow!("no language server handles {rel_path}"))?;
        let abs = self.repo.join(rel_path);
        let text = std::fs::read_to_string(&abs)
            .with_context(|| format!("reading {}", abs.display()))?;
        self.server_for(lang)?.document_symbols(&abs, &text)
    }

    /// Start every configured server so the first real query is not the one paying
    /// warm-up. Failures are recorded, not raised: a missing server degrades the
    /// overlay, it does not stop the daemon.
    pub fn warm(&mut self) {
        let langs = self.languages();
        for lang in langs {
            let _ = self.server_for(&lang);
        }
    }

    pub fn shutdown(&mut self) {
        for (_, mut s) in self.servers.drain() {
            s.shutdown();
        }
    }
}

/// `documentSymbol` returns either a flat `SymbolInformation[]` or a nested
/// `DocumentSymbol[]`, depending on the server. Both are flattened, keeping the
/// container name so nesting is not lost.
fn flatten_symbols(v: &serde_json::Value, container: Option<&str>) -> Vec<LiveSymbol> {
    let mut out = Vec::new();
    let Some(items) = v.as_array() else {
        return out;
    };
    for item in items {
        let name = item["name"].as_str().unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        let kind = item["kind"].as_i64().unwrap_or(0);
        let range = if item.get("location").is_some() {
            &item["location"]["range"]
        } else {
            &item["range"]
        };
        let start = range["start"]["line"].as_i64().unwrap_or(0);
        let end = range["end"]["line"].as_i64().unwrap_or(start);
        let container = item["containerName"]
            .as_str()
            .map(|s| s.to_string())
            .or_else(|| container.map(|s| s.to_string()));
        out.push(LiveSymbol {
            name: name.clone(),
            kind,
            start_line: start,
            end_line: end,
            container,
        });
        if let Some(children) = item.get("children") {
            out.extend(flatten_symbols(children, Some(&name)));
        }
    }
    out
}

fn write_message<W: Write>(w: &mut W, msg: &serde_json::Value) -> Result<()> {
    let body = serde_json::to_vec(msg)?;
    write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
    w.write_all(&body)?;
    w.flush()?;
    Ok(())
}

fn read_message<R: BufRead>(reader: &mut R) -> Option<serde_json::Value> {
    let mut len = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            len = rest.trim().parse().ok()?;
        }
    }
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_files_to_the_right_server() {
        assert_eq!(Pool::lang_of("srcpy/a/b.py"), Some("py"));
        assert_eq!(Pool::lang_of("srcgo/a/b.go"), Some("go"));
        assert_eq!(Pool::lang_of("web/a.tsx"), Some("ts"));
        assert_eq!(Pool::lang_of("proto/a.proto"), None);
        assert_eq!(Pool::lang_of("Makefile"), None);
    }

    #[test]
    fn flattens_the_nested_shape() {
        let v = serde_json::json!([{
            "name": "Klass", "kind": 5,
            "range": {"start": {"line": 10}, "end": {"line": 40}},
            "children": [{
                "name": "method", "kind": 6,
                "range": {"start": {"line": 12}, "end": {"line": 20}}
            }]
        }]);
        let out = flatten_symbols(&v, None);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "Klass");
        assert_eq!(out[1].name, "method");
        assert_eq!(out[1].container.as_deref(), Some("Klass"));
        assert_eq!(out[1].start_line, 12);
        assert_eq!(out[1].end_line, 20);
    }

    #[test]
    fn flattens_the_flat_shape() {
        // The other half of the protocol: SymbolInformation carries `location`.
        let v = serde_json::json!([{
            "name": "top", "kind": 12,
            "location": {"range": {"start": {"line": 3}, "end": {"line": 9}}},
            "containerName": "mod"
        }]);
        let out = flatten_symbols(&v, None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].start_line, 3);
        assert_eq!(out[0].container.as_deref(), Some("mod"));
    }

    #[test]
    fn framing_round_trips() {
        let msg = serde_json::json!({"jsonrpc": "2.0", "id": 7, "result": {"ok": true}});
        let mut buf: Vec<u8> = Vec::new();
        write_message(&mut buf, &msg).unwrap();
        assert!(String::from_utf8_lossy(&buf).starts_with("Content-Length: "));
        let mut reader = BufReader::new(&buf[..]);
        assert_eq!(read_message(&mut reader).unwrap(), msg);
    }
}
