//! Rendering.
//!
//! Deliberately split in two: **selection** happens in cairn-store (which symbols, how
//! deep, which edges), **presentation** happens here. Otherwise every new view would
//! have to be re-implemented for every query, and the number of both is going to grow.
//!
//! Two rules hold across every view (architecture 6.3, D8):
//!
//! * **Not JSON by default.** Braces, quotes and repeated keys cost several times more
//!   tokens than an aligned text table carrying the same information. When the whole
//!   pitch is cheaper context, the response format *is* the pitch.
//! * **`unknown:`, `suppressed:` and `stale:` are always printed.** A missing section
//!   reads as "this is everything", and that is the silent error the design avoids.

use cairn_store::{EdgeSource, Lang, Occurrence, PathHop, SymbolRow, Walk};
use std::collections::HashSet;
use std::fmt::Write;

pub mod budget;
pub mod source;
pub use budget::Budget;
pub use source::{Detail, Excerpt, SiteContext, Source};

/// How a result set is laid out. Orthogonal to *what* was selected, so any view can
/// render any walk (architecture 18.1 — this is the fifth axis alongside detail,
/// breadth x depth, aspect and budget).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// One node per line, flat. Cheapest, good for "give me the list".
    List,
    /// Indented by depth, showing how each node was reached.
    Tree,
}

impl View {
    pub fn parse(s: &str) -> Option<View> {
        match s {
            "list" => Some(View::List),
            "tree" => Some(View::Tree),
            _ => None,
        }
    }
}

/// The version of the JSON answer model, carried on every JSON answer.
///
/// Versioned because a consumer has to be able to tell that the shape changed without
/// diffing it against what it remembers. Text output is not versioned and does not need
/// to be: its reader is an agent that reads the page.
pub const ANSWER_SCHEMA: &str = "cairn.answer/1";

/// One answer, as `--format json` prints it.
///
/// Borrowed from the envelope rather than owning a copy: this is built once, on the way
/// out, and an answer can carry a megabyte of body.
///
/// Generic in the payload for one reason: printing uses `serde_json::Value`, because the
/// envelope is built where the command is no longer known, while the published schema
/// has to name the command's own data type. One declaration, instantiated twice, so the
/// schema in `docs/json-schema/v1/` describes the object this actually prints.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct Answer<'a, T = serde_json::Value> {
    pub schema: &'static str,
    /// The command that produced this, as it would be typed: `refs`, `for change`.
    pub command: &'a str,
    pub rows: Option<usize>,
    pub truncated: bool,
    pub unknown: &'a [String],
    pub suppressed: &'a [String],
    pub stale: Stale<'a>,
    /// The text answer, exactly as text mode prints it above the envelope lines. The
    /// three sections are fields here, so repeating them inside the body would be the
    /// same claim twice and two places for it to drift.
    pub body: &'a str,
    /// The answer as fields. `null` until the command has a data builder.
    pub data: Option<&'a T>,
    /// The declared container this answer was scoped to, or `null` for the whole index.
    pub within: Option<&'a str>,
}

/// The JSON Schema of one command's answer: the envelope, with `data` as that command's
/// own shape.
///
/// Generated from the same structs that serialise the answer, so the published contract
/// cannot drift from what the binary prints. A hand-written schema would be a second
/// declaration of the same thing, and the second one is always the stale one.
pub fn answer_schema<T: schemars::JsonSchema + 'static>() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(Answer<'static, T>))
        .unwrap_or(serde_json::Value::Null)
}

/// Every command whose answer carries data, with the schema of that answer.
///
/// The list the published schemas are generated from and the list a test validates real
/// output against — one definition, so a command cannot be added to one and forgotten in
/// the other. Commands absent from it print `data: null`, which the envelope's own
/// schema already describes.
pub fn answer_schemas() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("symbol", answer_schema::<SymbolsData>()),
        ("refs", answer_schema::<RefsData>()),
        ("graph", answer_schema::<WalkData>()),
        ("path", answer_schema::<PathData>()),
        ("affects", answer_schema::<AffectsData>()),
        ("runs", answer_schema::<RunsData>()),
        ("reaches", answer_schema::<ReachesData>()),
        ("expand", answer_schema::<ExpandData>()),
        ("status", answer_schema::<StatusData>()),
        ("verify", answer_schema::<cairn_store::Report>()),
        ("concept-show", answer_schema::<ConceptData>()),
        ("concept-list", answer_schema::<ConceptListData>()),
        ("links", answer_schema::<LinksData>()),
        ("flow", answer_schema::<cairn_store::Flow>()),
    ]
}

/// The staleness section of a JSON answer: `StaleFiles` plus the sentences text prints.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct Stale<'a> {
    pub tracked: Option<bool>,
    pub dirty: &'a [String],
    pub mentioned: &'a [String],
    pub notes: &'a [String],
}

/// Which files an answer rests on, and what is known about their freshness.
///
/// The sentences in `Envelope::stale` are what a reader sees; this is the same fact for a
/// program. Kept beside them rather than parsed back out of them: a consumer that had to
/// read "3 of the files in this answer changed since indexing: a, b, c" with a regular
/// expression would be relying on wording nobody promised to keep.
///
/// `tracked` carries the distinction D8 exists for. `None` means this answer never asked
/// — most reports do not —, `Some(false)` that the watcher could not say, and
/// `Some(true)` that it did. An empty `dirty` with `tracked: Some(false)` is not clean,
/// and that is precisely the case a bare empty list would hide.
#[derive(Debug, Clone, Default, serde::Serialize, schemars::JsonSchema)]
pub struct StaleFiles {
    pub tracked: Option<bool>,
    /// Files this answer mentions that have changed since the index was built.
    pub dirty: Vec<String>,
    /// Every file the answer mentions — the set `dirty` was drawn from.
    pub mentioned: Vec<String>,
}

/// Every response carries the same envelope, so an agent can rely on it being there.
pub struct Envelope {
    pub body: String,
    pub unknown: Vec<String>,
    pub suppressed: Vec<String>,
    pub stale: Vec<String>,
    /// What `stale` says, as data. See `StaleFiles`.
    pub stale_files: StaleFiles,
    /// The answer as structured data, for `--format json`. `None` while a command still
    /// has only a text renderer; the envelope around it is there either way.
    pub data: Option<serde_json::Value>,
    /// The declared container this answer was scoped to, when `--within` was given.
    pub within: Option<String>,
    /// How many rows the body carried, where the answer is a list.
    ///
    /// What was *printed*, not what matched. The two differ whenever a limit or the
    /// budget cut the list, and the part that did not arrive is already described by
    /// `suppressed` — counting it here as well would make one number mean both things.
    ///
    /// `None` is for answers that are not lists (`status`, `verify`), and is not the
    /// same as `Some(0)`: one says the question does not produce rows, the other says
    /// it does and none came back.
    pub rows: Option<usize>,
}

impl Envelope {
    pub fn new(body: String) -> Envelope {
        Envelope {
            body,
            unknown: Vec::new(),
            suppressed: Vec::new(),
            stale: Vec::new(),
            stale_files: StaleFiles::default(),
            data: None,
            within: None,
            rows: None,
        }
    }

    /// Note that this answer was scoped to a declared container, and what that cut.
    ///
    /// On the envelope rather than in the body: `--within` is a filter, and a filtered
    /// answer that does not say so reads as the whole set — the same silence `suppressed`
    /// exists for. The container is named because "12 rows were dropped" is not something
    /// a reader can act on without knowing which boundary dropped them.
    pub fn within(mut self, container: String, dropped: usize) -> Self {
        self.body = format!("within {container}\n{}", self.body);
        if dropped > 0 {
            self.suppressed.push(format!(
                "{dropped} row(s) outside {container} (--within). Drop the flag for the \
                 unscoped answer"
            ));
        }
        self.within = Some(container);
        self
    }

    /// Attach the answer as data. Text rendering is unaffected: the renderer reads
    /// `body`, so a command gains JSON without its text output moving (I2).
    pub fn data(mut self, data: impl serde::Serialize) -> Self {
        self.data = serde_json::to_value(data).ok();
        self
    }

    pub fn unknown(mut self, msg: impl Into<String>) -> Self {
        self.unknown.push(msg.into());
        self
    }

    /// Does any `unknown` line say a layer was not checked?
    ///
    /// The exit code and the text have to agree. They did not: `for understand` printed
    /// two UNCHECKED lines and exited 0, and a caller that branches on the code rather
    /// than reading the page saw a clean answer. One wording, one test for it.
    pub fn has_unchecked(&self) -> bool {
        self.unknown.iter().any(|u| u.contains("UNCHECKED"))
    }

    pub fn suppressed(mut self, msg: impl Into<String>) -> Self {
        self.suppressed.push(msg.into());
        self
    }

    /// Record how many rows reached the caller.
    pub fn rows(mut self, n: usize) -> Self {
        self.rows = Some(n);
        self
    }

    /// True when the answer told the caller it had left something out.
    ///
    /// Read from `suppressed` rather than tracked beside it, so a record of the answer
    /// can never disagree with the answer. Whatever made a caller widen `--budget` and
    /// ask again is by definition something the `suppressed:` line said, and that is
    /// the correlation the session log exists to support.
    pub fn truncated(&self) -> bool {
        !self.suppressed.is_empty()
    }

    /// Mark the answer stale where it touches files that have changed since indexing.
    ///
    /// `dirty` is what the daemon has observed; `None` means no daemon was running, and
    /// that is reported as *unknown* rather than as clean. An empty dirty set and an
    /// unknown one look identical in an answer, and treating the second as the first is
    /// exactly the silent staleness the contract forbids (D8).
    pub fn mark_stale(mut self, dirty: Option<&[String]>, mentioned: &[String]) -> Self {
        // The same facts the sentences below are built from, kept for `--format json`.
        self.stale_files.mentioned = {
            let mut m: Vec<String> = mentioned.to_vec();
            m.sort();
            m.dedup();
            m
        };
        self.stale_files.tracked = Some(dirty.is_some());
        let Some(dirty) = dirty else {
            // Deliberately not an instruction any more: the watcher starts itself on the
            // first command in a repository, so being told to run one would be telling
            // someone to do a thing that is already happening. What has to be said is
            // only that this answer cannot see edits made since the index was built.
            self.stale.push(
                "not tracked yet - the file watcher is still starting, so edits made \
                 since the index was built are not visible in this answer"
                    .to_string(),
            );
            return self;
        };
        let mut hit: Vec<&String> = mentioned
            .iter()
            .filter(|p| dirty.iter().any(|d| d == *p))
            .collect();
        hit.sort();
        hit.dedup();
        self.stale_files.dirty = hit.iter().map(|p| p.to_string()).collect();
        if !hit.is_empty() {
            self.stale.push(format!(
                "{} of the files in this answer changed since indexing: {}",
                hit.len(),
                hit.iter()
                    .take(5)
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        self
    }

    /// The same answer as JSON, for a program rather than a reader.
    ///
    /// Beside the text, never instead of it (I2). Text stays the primary format: braces
    /// and repeated keys cost several times the tokens of an aligned table, and that
    /// trade is the whole pitch. This is for the consumer that wants fields rather than
    /// a page — a wrapper, a check in CI, an editor.
    ///
    /// Pretty-printed. The reader of this is a program, so the newlines cost nothing it
    /// cares about, and they make the output of a failing command something a person can
    /// still read in a terminal.
    pub fn json(&self, command: &str) -> String {
        let answer = Answer {
            schema: ANSWER_SCHEMA,
            command,
            rows: self.rows,
            truncated: self.truncated(),
            unknown: &self.unknown,
            suppressed: &self.suppressed,
            stale: Stale {
                tracked: self.stale_files.tracked,
                dirty: &self.stale_files.dirty,
                mentioned: &self.stale_files.mentioned,
                notes: &self.stale,
            },
            body: &self.body,
            data: self.data.as_ref(),
            within: self.within.as_deref(),
        };
        // Infallible in practice: every field is a string, a number or a `Value` that
        // was built by `serde_json` in the first place. A command printing nothing at
        // all would be a worse failure than printing an envelope saying so.
        serde_json::to_string_pretty(&answer).unwrap_or_else(|e| {
            format!(
                "{{\n  \"schema\": \"{ANSWER_SCHEMA}\",\n  \"command\": \"{command}\",\n  \
                 \"unknown\": [\"this answer could not be serialised: {e}\"]\n}}"
            )
        })
    }

    pub fn render(&self) -> String {
        let mut out = self.body.clone();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        for (label, items) in [
            ("suppressed", &self.suppressed),
            ("unknown", &self.unknown),
            ("stale", &self.stale),
        ] {
            if items.is_empty() {
                let _ = writeln!(out, "{label}: none");
            } else if items.len() == 1 {
                let _ = writeln!(out, "{label}: {}", items[0]);
            } else {
                let _ = writeln!(out, "{label} ({}):", items.len());
                for i in items {
                    let _ = writeln!(out, "  {i}");
                }
            }
        }
        out
    }
}

/// Which declared containers the rows of one answer belong to, keyed by symbol id.
///
/// Built by the caller, because containment is a store question and a renderer has no
/// store. Empty whenever no connector declared any - which is every index without one,
/// and is why an answer with no containers prints exactly as it did before.
#[derive(Debug, Default)]
pub struct Containers(pub std::collections::BTreeMap<i64, Vec<String>>);

impl Containers {
    pub fn of(&self, symbol_id: i64) -> &[String] {
        self.0.get(&symbol_id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn is_empty(&self) -> bool {
        self.0.values().all(|v| v.is_empty())
    }

    /// The heading a row sits under: its first container, or `None` for a row in none.
    ///
    /// First rather than all: a row in two containers is listed under the first by id,
    /// and `containers` in the data carries the full set. Repeating a row under every
    /// container it belongs to would make the counts in the header wrong.
    fn heading(&self, symbol_id: i64) -> Option<&str> {
        self.of(symbol_id).first().map(|s| s.as_str())
    }
}

/// A symbol as data: what every JSON answer says about one.
///
/// One shape for every command, so a consumer learns it once. The fields the plan
/// requires of it — handle, the SCIP symbol string, file, line, the generated flag, the
/// edge source label — plus what the text line already shows, which is free here and
/// would otherwise have to be parsed back out of `body`.
///
/// Lines are 1-based, as the text line prints them and as every editor counts them. The
/// store keeps them 0-based; a JSON answer that quietly used the storage convention
/// would put every consumer one line out.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct SymbolJson {
    pub handle: String,
    /// The full SCIP symbol string. `null` on an index built before schema v20, which
    /// cannot print one — not an absent symbol, an index that did not record the name.
    pub symbol: Option<String>,
    pub name: String,
    /// `Type.method` where the container is a type, else the bare name — what the text
    /// line shows.
    pub qualified: String,
    pub kind: &'static str,
    pub lang: &'static str,
    pub file: Option<String>,
    pub line: Option<i64>,
    /// Last line of the definition's body, when the indexer emitted one.
    pub end_line: Option<i64>,
    /// Definition sits in generated code.
    pub generated: bool,
    pub refs: i64,
    /// Declared containers this symbol is part of, e.g. `w17:domain/orders`. Empty
    /// unless a connector declared containment.
    pub containers: Vec<String>,
    /// How the row was reached, where it arrived through an edge: `L1, exact`,
    /// `L2, agent-asserted`, `L1, declared by w17`. `null` when the row is the answer's
    /// own subject.
    ///
    /// A string rather than a tier name because a declared edge carries the connector
    /// that stated it, and a reader who cannot tell which tool said so cannot go and
    /// check.
    pub source: Option<String>,
}

impl SymbolJson {
    pub fn of(s: &SymbolRow) -> SymbolJson {
        SymbolJson {
            handle: s.handle.clone(),
            symbol: s.scip.clone(),
            name: s.name.clone(),
            qualified: s.qualified(),
            kind: s.kind.as_str(),
            lang: s.lang.tag(),
            file: s.def.as_ref().map(|d| d.path.clone()),
            line: s.def.as_ref().map(|d| d.line + 1),
            end_line: s.def_end_line.map(|l| l + 1),
            generated: s.def.as_ref().map(|d| d.generated).unwrap_or(false),
            refs: s.ref_count,
            containers: Vec::new(),
            source: None,
        }
    }

    /// The same, with the containers the caller looked up.
    pub fn in_containers(s: &SymbolRow, containers: &Containers) -> SymbolJson {
        SymbolJson {
            containers: containers.of(s.id).to_vec(),
            ..SymbolJson::of(s)
        }
    }

    pub fn from_source(
        s: &SymbolRow,
        source: EdgeSource,
        origin: Option<&str>,
        containers: &Containers,
    ) -> SymbolJson {
        SymbolJson {
            source: Some(source.label_with_origin(origin)),
            ..SymbolJson::in_containers(s, containers)
        }
    }
}

/// One result line: `[handle] Qualified.name  kind  lang  path:start-end`
///
/// The line range, not just the start, is deliberate: an agent that wants to audit on
/// its own terms - or build its own graph rather than trusting ours - needs to know
/// where the symbol ends without opening the file to find out.
pub fn symbol_line(s: &SymbolRow) -> String {
    symbol_line_of(&SymbolJson::of(s))
}

/// The same line, rendered from the data rather than from the row.
///
/// Both entry points exist so there is one implementation: a command that attaches data
/// renders its text from exactly the fields it published, and a command that has not
/// been converted yet keeps the signature it had. Two formatters for one line is how
/// text and JSON start disagreeing about what the answer was.
pub fn symbol_line_of(s: &SymbolJson) -> String {
    let loc = match (s.file.as_deref(), s.line, s.end_line) {
        (Some(path), Some(line), Some(end)) if end > line => format!("{path}:{line}-{end}"),
        (Some(path), Some(line), _) => format!("{path}:{line}"),
        _ => "<no definition indexed>".to_string(),
    };
    format!(
        "[{}] {:<40} {:<6} {}  {}",
        s.handle,
        truncate(&s.qualified, 40),
        s.kind,
        s.lang,
        loc
    )
}

/// `complete` is false when `--limit` may have cut the list. See `concentration_note`:
/// without it, a truncated set that happens to share a file is announced as all of them.
/// What a text search of the working tree found, when one was made.
///
/// `None` means no search was possible — not that it came up empty. The difference is the
/// whole point of carrying it: an absence nobody looked for is not an absence.
#[derive(Clone, Copy)]
pub struct TreeProbe {
    pub hits: usize,
    /// Files actually read, so a miss can say how wide it was.
    pub files: usize,
    /// The search stopped at its limit, so `hits` is a floor and not a count.
    pub truncated: bool,
}

/// `cairn symbol` as data.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct SymbolsData {
    pub query: String,
    /// False when `--limit` may have cut the list — there may be more matches than these.
    pub complete: bool,
    pub matches: Vec<SymbolJson>,
}

pub fn symbols(
    rows: &[SymbolRow],
    query: &str,
    coverage: &str,
    complete: bool,
    tree: Option<TreeProbe>,
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    if complete {
        let _ = writeln!(body, "{} matches for \"{}\"", rows.len(), query);
    } else {
        // The count is the header, and a bare number reads as the whole answer. Saying it
        // is a first page costs four words.
        let _ = writeln!(
            body,
            "{} matches for \"{}\" (--limit reached, there may be more)",
            rows.len(),
            query
        );
    }
    let defined_in: Vec<&str> = rows
        .iter()
        .filter_map(|r| r.def.as_ref().map(|d| d.path.as_str()))
        .collect();
    if let Some(note) = concentration_note(&defined_in, complete, "matches") {
        let _ = writeln!(body, "{note}");
    }
    // Built before the loop, and the loop renders from it: the lines printed and the rows
    // published are then the same rows by construction rather than by agreement.
    let all: Vec<SymbolJson> = rows.iter().map(SymbolJson::of).collect();
    let mut shown = 0;
    for s in &all {
        if !budget.push(&mut body, &symbol_line_of(s)) {
            break;
        }
        shown += 1;
    }
    if rows.is_empty() {
        let _ = writeln!(body, "(nothing matched)");
    }
    let mut env = Envelope::new(body).rows(shown).data(SymbolsData {
        query: query.to_string(),
        complete,
        // What the answer carried, not what matched: the rest is described by
        // `suppressed`, and counting it in both places would make one list mean two
        // things.
        matches: all[..shown].to_vec(),
    });
    if shown < rows.len() {
        env = env.suppressed(budget.cut_note(rows.len() - shown, "matches"));
    }
    // The `--limit` cut, as opposed to the budget cut above. `rows` arrives already
    // truncated, so `shown == rows.len()` and the branch above cannot fire — the header
    // said "there may be more" while the envelope said `suppressed: none`, which is the
    // one contradiction this whole design exists to prevent, in the command an agent runs
    // first on the most crowded name in the repository.
    if !complete {
        env = env.suppressed(
            "more matches beyond --limit; raise it, or narrow the query - a first page \
             that does not say so is indistinguishable from the whole answer"
                .to_string(),
        );
    }

    // A miss, or a set of hits that are all generated, most often means the caller is
    // asking about code this index does not cover. Saying what *is* covered turns a
    // sequence of probes into one answer.
    let all_generated = !rows.is_empty()
        && rows
            .iter()
            .all(|r| r.def.as_ref().map(|d| d.generated).unwrap_or(false));
    if rows.is_empty() {
        env = match tree {
            // The escalation this command used to delegate. Telling the caller "grep is
            // the tool" spends a round trip to learn something the tool could have
            // established itself, and leaves the two cases indistinguishable: a name that
            // is nowhere, and a name the index simply does not cover. Searching the tree
            // separates them, and each branch is a fact rather than a suggestion.
            Some(TreeProbe { hits: 0, files, .. }) => env.unknown(format!(
                "nothing by that name, and nothing in the working tree either - \
                 {files} file(s) were read and none contains that text. This is a \
                 checked absence, not an unsearched one: a text search will return \
                 the same. {coverage}"
            )),
            Some(TreeProbe {
                hits,
                files,
                truncated,
            }) => env.unknown(format!(
                "no symbol by that name, but the text is in the working tree - \
                 {}{hits} line(s) across {files} file(s) read. So it exists as a string, \
                 a comment, or in a file this index does not cover; \
                 `cairn for find \"{query}\"` lists each with its enclosing function. \
                 {coverage}",
                if truncated { "at least " } else { "" }
            )),
            // No tree to read, so the old answer is the honest one: this really is the
            // limit of what can be said from here.
            None => env.unknown(format!(
                "nothing by that name. {coverage} - if the code you mean lives elsewhere, \
                 it is not in this index and grep is the tool"
            )),
        };
    } else if all_generated {
        env = env.unknown(format!(
            "every match is in generated code, which usually means the thing you meant \
             is not indexed under this name. {coverage}"
        ));
    }
    env
}

/// How each site is rendered: the source to read it from, how much of it to show, and
/// the containers to group the sites under.
///
/// A struct rather than three more parameters: they travel together, they are all about
/// presentation rather than about which sites were selected, and the function was at the
/// point where one more argument makes a call site unreadable.
pub struct SiteView<'a> {
    pub source: Option<&'a mut Source>,
    pub context: SiteContext,
    pub containers: &'a Containers,
}

/// One reference site as data.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct RefJson {
    pub file: String,
    /// 1-based, as printed.
    pub line: i64,
    pub col_start: i64,
    pub col_end: i64,
    /// Symbol whose body contains this site. `null` is a site outside any function —
    /// an import, a module-level use — which is a fact about the site, not a gap.
    pub enclosing: Option<String>,
    pub generated: bool,
    pub definition: bool,
    /// Declared containers of the function this site sits in. Empty unless a connector
    /// declared containment.
    pub containers: Vec<String>,
    /// Source lines shown at this site, when `--context` asked for any. Empty otherwise.
    pub context: Vec<SourceLine>,
}

/// A line of source carried beside an answer. 1-based, as the renderer prints it.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct SourceLine {
    pub line: usize,
    pub text: String,
}

/// The control flow inside one function, as a page.
///
/// One line per node, indented by what it is rather than by the graph: a reader following
/// a handler wants the branches and the early returns to stand out, and the edges are in
/// the data for anything that wants the graph itself.
pub fn flow(f: &cairn_store::Flow, budget: &mut Budget) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(
        body,
        "flow of [{}] {}   {} node(s)   [{}{}]",
        f.handle,
        f.function,
        f.nodes.len(),
        f.provider,
        if f.cached { ", cached" } else { "" }
    );
    let mut shown = 0usize;
    for n in &f.nodes {
        // The kind first and in a column, because that is what a reader scans for. A
        // branch and a return read differently at a glance and should.
        let what = n
            .condition
            .as_deref()
            .or(n.name.as_deref())
            .unwrap_or("")
            .trim();
        let target = match (&n.handle, &n.target) {
            (Some(h), _) => format!("  -> [{h}]"),
            (None, Some(_)) => "  -> (unresolved)".to_string(),
            _ => String::new(),
        };
        // What a connector declared about the callee: the reason this command is worth
        // more than reading the function. `writes w17:entity/Order` is not in the source.
        let declared = if n.declared.is_empty() {
            String::new()
        } else {
            format!(
                "   {}",
                n.declared
                    .iter()
                    .map(|a| format!("{} {} (declared by {})", a.kind, a.target, a.connector))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let line = format!(
            "  {:<13} {:<42} {}{target}{declared}",
            n.kind,
            truncate(what, 42),
            n.span
        );
        if !budget.push(&mut body, &line) {
            break;
        }
        shown += 1;
    }
    if f.nodes.is_empty() {
        let _ = writeln!(
            body,
            "  (no nodes - the provider found nothing to describe)"
        );
    }
    let mut env = Envelope::new(body).rows(shown).data(f);
    if shown < f.nodes.len() {
        env = env.suppressed(budget.cut_note(f.nodes.len() - shown, "nodes"));
    }
    for u in &f.unknown {
        env = env.unknown(u.clone());
    }
    env
}

/// `cairn status` as data.
///
/// The one answer whose text prints its envelope in the middle rather than at the end,
/// so the CLI arranges the parts twice; this is the shape the JSON arrangement publishes.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct StatusData {
    pub index: String,
    pub files: i64,
    pub symbols: i64,
    pub occurrences: i64,
    pub generated_files: i64,
    /// What each mechanism produced, in the order `coverage` prints them.
    pub coverage: Vec<AreaJson>,
    /// `null` when no daemon is watching this index.
    pub daemon: Option<DaemonJson>,
    /// The connectors this index was built with, and what each contributed.
    pub connectors: Vec<cairn_store::connect::ConnectorRun>,
}

/// One mechanism's coverage: what it produced, and how far it has been checked.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct AreaJson {
    pub name: String,
    /// `indexed`, `verified`, `stale` — the rung this mechanism has reached.
    pub state: String,
    /// What it produced, in its own words.
    pub detail: String,
}

/// The watcher behind an answer, when there is one.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct DaemonJson {
    pub repo: String,
    pub files_tracked: usize,
    pub uptime_secs: u64,
    /// False while the initial scan is still running: the dirty set below is partial.
    pub complete: bool,
    pub modified: Vec<String>,
    /// Files nothing in the index has ever seen. Not stale — absent.
    pub created: Vec<String>,
    pub removed: Vec<String>,
    pub generation: u64,
    /// Why a reindex is due, when it is.
    pub reindex_due: Option<String>,
}

/// `cairn expand` as data.
///
/// Rendered in the CLI rather than here — it is three lines of formatting over one
/// symbol — so this is the one data struct whose builder is its caller.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct ExpandData {
    pub symbol: SymbolJson,
    /// What was asked for: `skeleton`, `doc` or `body`.
    pub detail: String,
    /// The source that was printed. Empty for `skeleton`, which prints none.
    pub source: Vec<SourceLine>,
}

/// `cairn refs` as data.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct RefsData {
    pub symbol: SymbolJson,
    /// How many references the filters matched before `--limit` and the budget cut.
    pub total: i64,
    /// True when every match is in `references` and nothing was hidden.
    pub complete: bool,
    pub references: Vec<RefJson>,
}

impl RefJson {
    fn of(r: &Occurrence, context: Vec<SourceLine>, containers: &Containers) -> RefJson {
        RefJson {
            file: r.path.clone(),
            line: r.line + 1,
            col_start: r.col_start,
            col_end: r.col_end,
            enclosing: r.enclosing.clone(),
            generated: r.generated,
            definition: r.is_definition(),
            containers: r
                .enclosing_id
                .map(|id| containers.of(id).to_vec())
                .unwrap_or_default(),
            context,
        }
    }
}

/// Reference list, optionally with the source line at each site.
///
/// A bare `path:line` is a location, not information: every measured task spent most of
/// its budget turning locations into understanding by opening files. Naming the
/// enclosing function and showing the line itself usually settles whether a site
/// matters, at a fraction of the cost of reading the file.
pub fn references_with_context(
    sym: &SymbolRow,
    refs: &[Occurrence],
    suppressed_generated: i64,
    // How many the filters matched before --limit. Without it a truncated list looked
    // complete, which is the failure the envelope exists to prevent.
    total: i64,
    view: SiteView<'_>,
    budget: &mut Budget,
) -> Envelope {
    let SiteView {
        source,
        context: ctx,
        containers,
    } = view;
    let mut body = String::new();
    // Leading, not trailing: a recommendation the caller reads after paying for the
    // whole answer has not saved them anything.
    if let Some(note) = routing_note(sym) {
        let _ = writeln!(body, "{note}\n");
    }
    let _ = writeln!(body, "references to [{}] {}", sym.handle, sym.qualified());
    if let Some(def) = &sym.def {
        let _ = writeln!(body, "  defined at {}", def.location());
    }
    // Everything the filters matched is here, and nothing was hidden: only then is this
    // list the answer rather than a page of it.
    let complete = refs.len() as i64 == total && suppressed_generated == 0;
    if complete {
        let _ = writeln!(
            body,
            "{} references                    [L0, exact]",
            refs.len()
        );
    } else {
        let _ = writeln!(
            body,
            "{} of {total} references              [L0, exact, partial]",
            refs.len()
        );
    }

    let paths: Vec<&str> = refs.iter().map(|r| r.path.as_str()).collect();
    if let Some(note) = concentration_note(&paths, complete, "references") {
        let _ = writeln!(body, "{note}");
    }

    let mut source = source;
    let mut shown = 0usize;
    let mut sites: Vec<RefJson> = Vec::new();
    // Grouped by the declared container of the function each site sits in, where there
    // is one. A use list is already flat, so the grouping costs nothing but a heading -
    // and "which domains use this" is a question the ungrouped list cannot answer at a
    // glance.
    let grouped = !containers.is_empty();
    let order: Vec<usize> = if grouped {
        let mut idx: Vec<usize> = (0..refs.len()).collect();
        idx.sort_by(|a, b| {
            let key = |i: &usize| {
                (
                    refs[*i]
                        .enclosing_id
                        .and_then(|id| containers.heading(id))
                        .unwrap_or("\u{7f}")
                        .to_string(),
                    *i,
                )
            };
            key(a).cmp(&key(b))
        });
        idx
    } else {
        (0..refs.len()).collect()
    };
    let mut heading: Option<String> = None;
    for (i, r) in order.iter().map(|i| (*i, &refs[*i])) {
        if grouped {
            let within = r
                .enclosing_id
                .and_then(|id| containers.heading(id))
                .map(|c| c.to_string())
                .unwrap_or_else(|| "(no declared container)".to_string());
            if heading.as_deref() != Some(within.as_str()) {
                if !budget.push(&mut body, &format!("  within {within}")) {
                    break;
                }
                heading = Some(within);
            }
        }
        let inside = r
            .enclosing
            .as_deref()
            .map(|e| format!("  in {e}"))
            .unwrap_or_else(|| "  (module level)".to_string());
        let lines = source
            .as_deref_mut()
            .map(|src| src.site(&r.path, r.line, ctx))
            .unwrap_or_default();
        let as_data = RefJson::of(
            r,
            lines
                .iter()
                .map(|(n, text)| SourceLine {
                    line: *n,
                    text: text.clone(),
                })
                .collect(),
            containers,
        );
        let inline = if lines.len() == 1 {
            format!("   |  {}", lines[0].1.trim())
        } else {
            String::new()
        };
        if !budget.push(
            &mut body,
            &format!("  {:<46}{inside}{inline}", r.location()),
        ) {
            break;
        }
        // After the budget check, not before: a site the budget cut is in neither the
        // text nor the data, and `suppressed` is what says it existed.
        sites.push(as_data);
        // Counted, not derived from the index: with rows reordered by container, `i + 1`
        // is a position in the walk rather than a number of rows printed.
        shown += 1;
        if lines.len() > 1 {
            for (n, text) in &lines {
                let marker = if *n as i64 == r.line + 1 { ">" } else { " " };
                if !budget.push(&mut body, &format!("      {marker}{n:>5} | {text}")) {
                    break;
                }
            }
        }
        let _ = i;
    }
    if refs.is_empty() {
        let _ = writeln!(body, "  (none outside generated code)");
    }

    let mut env = Envelope::new(body).rows(shown).data(RefsData {
        symbol: SymbolJson::of(sym),
        total,
        complete,
        references: sites,
    });
    let dropped = total - refs.len() as i64;
    if dropped > 0 {
        env = env.suppressed(format!(
            "{dropped} more references beyond --limit ({total} in total). Raise --limit, \
             or use `cairn usage` for the same answer grouped by file"
        ));
    }
    if shown < refs.len() {
        env = env.suppressed(budget.cut_note(refs.len() - shown, "references"));
    }
    if suppressed_generated > 0 {
        env = env.suppressed(format!(
            "{suppressed_generated} references in generated code (rerun with --include-generated)"
        ));
    }
    if let Some(note) = attribute_caveat(sym) {
        env = env.unknown(note);
    }
    env
}

pub fn references(
    sym: &SymbolRow,
    refs: &[Occurrence],
    suppressed_generated: i64,
    total: i64,
    containers: &Containers,
) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(body, "references to [{}] {}", sym.handle, sym.qualified());
    if let Some(def) = &sym.def {
        let _ = writeln!(body, "  defined at {}", def.location());
    }
    let _ = writeln!(
        body,
        "{} references                    [L0, exact]",
        refs.len()
    );
    for r in refs {
        let _ = writeln!(body, "  {}", r.location());
    }
    if refs.is_empty() {
        let _ = writeln!(body, "  (none outside generated code)");
    }

    let mut env = Envelope::new(body).rows(refs.len()).data(RefsData {
        symbol: SymbolJson::of(sym),
        total,
        complete: refs.len() as i64 == total && suppressed_generated == 0,
        references: refs
            .iter()
            .map(|r| RefJson::of(r, Vec::new(), containers))
            .collect(),
    });
    let dropped = total - refs.len() as i64;
    if dropped > 0 {
        env = env.suppressed(format!(
            "{dropped} more references beyond --limit ({total} in total). Raise --limit or \
             use `cairn usage` for the same answer grouped by file"
        ));
    }
    if suppressed_generated > 0 {
        env = env.suppressed(format!(
            "{suppressed_generated} references in generated code (rerun with --include-generated)"
        ));
    }
    if let Some(note) = attribute_caveat(sym) {
        env = env.unknown(note);
    }
    env
}

/// Render a bounded walk over the call graph.
///
/// `detail` decides how much of each node is printed. Anything above `Skeleton` needs
/// `source`, and is the audit shape: walk the callers and show me their code.
/// One node of a walk as data.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct WalkNodeJson {
    pub symbol: SymbolJson,
    /// Hops from the root. 0 is the root itself.
    pub depth: usize,
    /// Index into `nodes` of the node this was reached from. `null` for the root — which
    /// is what makes the flat list a tree without repeating any symbol.
    pub parent: Option<usize>,
    /// Where the edge was observed, for call edges: `file:line`.
    pub site: Option<String>,
    /// Children this node had that `--fanout` cut.
    pub truncated_children: usize,
    /// Source printed for this node, when `--detail` asked for any.
    pub excerpt: Vec<SourceLine>,
}

/// `cairn graph` as data — and every other answer built from a walk.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct WalkData {
    /// The header line, which names what was walked and in which direction.
    pub title: String,
    pub view: &'static str,
    pub nodes: Vec<WalkNodeJson>,
    /// Neighbours dropped by `--fanout`, across all nodes.
    pub truncated: usize,
    /// Nodes reachable by more than one path. Counted, not repeated.
    pub revisited: usize,
}

pub fn walk(
    w: &Walk,
    title: &str,
    view: View,
    detail: Detail,
    source: Option<&mut Source>,
    budget: &mut Budget,
    containers: &Containers,
) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(body, "{title}");

    let mut notes: Vec<String> = Vec::new();
    let mut as_data: Vec<WalkNodeJson> = Vec::new();
    let mut source = source;
    let mut shown = 0usize;
    // Grouped by declared container where there is one to group by, and only in the flat
    // view: a tree already carries structure in its indentation, and a second structure
    // fighting it would make both unreadable. In a tree the container rides on the node
    // instead, which is the same fact in the place a reader is already looking.
    let grouped = view == View::List && !containers.is_empty();
    let order: Vec<usize> = if grouped {
        let mut idx: Vec<usize> = (0..w.nodes.len()).collect();
        // The root first, then by container, then in walk order: a stable total order, so
        // the same walk prints the same page twice.
        idx.sort_by(|a, b| {
            let key = |i: &usize| {
                (
                    *i != 0,
                    containers
                        .heading(w.nodes[*i].symbol.id)
                        .unwrap_or("\u{7f}")
                        .to_string(),
                    *i,
                )
            };
            key(a).cmp(&key(b))
        });
        idx
    } else {
        (0..w.nodes.len()).collect()
    };
    let mut heading: Option<String> = None;
    for (i, node) in order.iter().map(|i| (*i, &w.nodes[*i])) {
        if grouped && i != 0 {
            // One heading per container, printed when the group starts. A row in none
            // gets the last group, named so it is not mistaken for part of the previous.
            let within = containers
                .heading(node.symbol.id)
                .map(|c| c.to_string())
                .unwrap_or_else(|| "(no declared container)".to_string());
            if heading.as_deref() != Some(within.as_str()) {
                if !budget.push(&mut body, &format!("  within {within}")) {
                    break;
                }
                heading = Some(within);
            }
        }
        let line = match view {
            View::Tree => {
                let indent = "  ".repeat(node.depth);
                let site = node
                    .site
                    .as_deref()
                    .map(|s| format!("  @ {s}"))
                    .unwrap_or_default();
                let tag = if node.source == EdgeSource::Scip {
                    String::new()
                } else {
                    // A declared edge names its connector here, because "L1, declared"
                    // on its own leaves the reader without the one thing that makes it
                    // checkable.
                    format!(
                        "  [{}]",
                        node.source.label_with_origin(node.origin.as_deref())
                    )
                };
                let within = containers
                    .heading(node.symbol.id)
                    .map(|c| format!("  ({c})"))
                    .unwrap_or_default();
                format!("{indent}{}{site}{tag}{within}", symbol_line(&node.symbol))
            }
            // Grouped under a heading above, so the row itself stays narrow.
            View::List => symbol_line(&node.symbol),
        };
        if !budget.push(&mut body, &line) {
            break;
        }
        shown = i + 1;
        as_data.push(WalkNodeJson {
            symbol: SymbolJson::from_source(
                &node.symbol,
                node.source,
                node.origin.as_deref(),
                containers,
            ),
            depth: node.depth,
            parent: node.parent,
            site: node.site.clone(),
            truncated_children: node.truncated_children,
            excerpt: Vec::new(),
        });

        if detail.needs_source() {
            let indent = if view == View::Tree {
                "  ".repeat(node.depth + 1)
            } else {
                "  ".into()
            };
            let mut excerpt: Vec<SourceLine> = Vec::new();
            let fits = emit_excerpt(
                &mut body,
                &indent,
                &node.symbol,
                detail,
                source.as_deref_mut(),
                budget,
                ExcerptSink {
                    notes: &mut notes,
                    lines: &mut excerpt,
                },
            );
            if let Some(last) = as_data.last_mut() {
                last.excerpt = excerpt;
            }
            if !fits {
                break;
            }
        }
    }

    let mut env = Envelope::new(body).rows(shown).data(WalkData {
        title: title.to_string(),
        view: match view {
            View::Tree => "tree",
            View::List => "list",
        },
        nodes: as_data,
        truncated: w.truncated,
        revisited: w.revisited,
    });
    for n in notes {
        env = env.suppressed(n);
    }
    if shown < w.nodes.len() {
        env = env.suppressed(budget.cut_note_narrowable(
            w.nodes.len() - shown,
            "nodes",
            "--depth, --fanout or --exclude-tests",
        ));
    }
    if w.truncated > 0 {
        env = env.suppressed(format!("{} neighbours beyond --fanout", w.truncated));
    }
    if w.revisited > 0 {
        env = env.suppressed(format!(
            "{} nodes reachable by more than one path, shown once",
            w.revisited
        ));
    }
    env
}

/// One hop of a call path as data.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct HopJson {
    pub symbol: SymbolJson,
    /// Call site inside the *previous* symbol that leads here. `null` on the first hop.
    pub site: Option<String>,
    pub excerpt: Vec<SourceLine>,
}

/// `cairn path` as data.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct PathData {
    /// Edges between the hops, which is one fewer than the hops themselves.
    pub hops: usize,
    pub chain: Vec<HopJson>,
}

/// A resolved call chain, one hop per line with the call site that leads onward.
pub fn path(
    hops: &[PathHop],
    detail: Detail,
    source: Option<&mut Source>,
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let mut as_data: Vec<HopJson> = Vec::new();
    let _ = writeln!(
        body,
        "call path, {} hops                              [L1, exact]",
        hops.len().saturating_sub(1)
    );
    let mut notes: Vec<String> = Vec::new();
    let mut source = source;
    let mut shown = 0usize;
    for (i, hop) in hops.iter().enumerate() {
        let arrow = if i == 0 { "   " } else { "-> " };
        let site = hop
            .site
            .as_deref()
            .map(|s| format!("  @ {s}"))
            .unwrap_or_default();
        let line = format!("{arrow}{}{site}", symbol_line(&hop.symbol));
        if !budget.push(&mut body, &line) {
            break;
        }
        shown = i + 1;
        as_data.push(HopJson {
            symbol: SymbolJson::of(&hop.symbol),
            site: hop.site.clone(),
            excerpt: Vec::new(),
        });
        if detail.needs_source() {
            let mut excerpt: Vec<SourceLine> = Vec::new();
            let fits = emit_excerpt(
                &mut body,
                "   ",
                &hop.symbol,
                detail,
                source.as_deref_mut(),
                budget,
                ExcerptSink {
                    notes: &mut notes,
                    lines: &mut excerpt,
                },
            );
            if let Some(last) = as_data.last_mut() {
                last.excerpt = excerpt;
            }
            if !fits {
                break;
            }
        }
    }
    let mut env = Envelope::new(body).rows(shown).data(PathData {
        hops: hops.len().saturating_sub(1),
        chain: as_data,
    });
    for n in notes {
        env = env.suppressed(n);
    }
    if shown < hops.len() {
        env = env.suppressed(budget.cut_note_narrowable(hops.len() - shown, "hops", "--max-depth"));
    }
    // Measured (task F): a correct three-hop path was returned as skeletons, and the agent
    // then opened every hop by hand — 37 tool calls where the same command with
    // `--detail body` would have delivered the chain and its source together. The option
    // existed and was in the guide; the guide is not where an agent is looking when it has
    // an answer in front of it.
    if detail == Detail::Skeleton && hops.len() > 1 {
        env = env.unknown(
            "this is the shape of the path, not what it does. If the question is what              happens along it, re-run with `--detail body --repo <dir>`: same call, every              hop's source, and no need to open the files one at a time",
        );
    }
    env
}

/// Tests that reach a symbol through the call graph.
pub fn tests(sym: &SymbolRow, rows: &[SymbolRow], budget: &mut Budget) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(
        body,
        "tests reaching [{}] {}                          [L1, exact]",
        sym.handle,
        sym.qualified()
    );
    let mut shown = 0usize;
    for (i, t) in rows.iter().enumerate() {
        if !budget.push(&mut body, &format!("  {}", symbol_line(t))) {
            break;
        }
        shown = i + 1;
    }
    if rows.is_empty() {
        let _ = writeln!(
            body,
            "  (no test reaches this symbol through the call graph)"
        );
    }
    let mut env = Envelope::new(body).rows(shown);
    if shown < rows.len() {
        env = env.suppressed(budget.cut_note_narrowable(rows.len() - shown, "tests", "--depth"));
    }
    env
}

/// Candidate dynamic references. Always labelled, never merged with exact results.
pub fn weak_links(sym: &SymbolRow, sites: &[(String, f64)], budget: &mut Budget) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(
        body,
        "string literals naming [{}] {}          [L1-W, unverified]",
        sym.handle,
        sym.qualified()
    );
    let mut shown = 0usize;
    for (i, (site, conf)) in sites.iter().enumerate() {
        if !budget.push(&mut body, &format!("  {site}   confidence {conf:.1}")) {
            break;
        }
        shown = i + 1;
    }
    if sites.is_empty() {
        let _ = writeln!(body, "  (no literal in the repo spells this name)");
    }
    let mut env = Envelope::new(body).rows(shown);
    if shown < sites.len() {
        env = env.suppressed(budget.cut_note(sites.len() - shown, "sites"));
    }
    if !sites.is_empty() {
        env = env.unknown(
            "these are lexical matches, not resolved references - read the site to \
             confirm before relying on it",
        );
    }
    env
}

/// Print one symbol's source at the requested detail. Returns false when the budget
/// ran out mid-excerpt, so the caller stops the whole walk rather than emitting
/// half-symbols to the end of the list.
/// Where an excerpt reports what it did: the notes that become `suppressed` entries, and
/// the lines it printed.
///
/// `lines` exists so that an answer carrying source in its text carries the same source
/// in its data. Without it `--detail body` was a page of code in one format and an empty
/// field in the other, which is the sort of difference a consumer discovers by reaching
/// for something that is not there.
struct ExcerptSink<'a> {
    notes: &'a mut Vec<String>,
    lines: &'a mut Vec<SourceLine>,
}

fn emit_excerpt(
    out: &mut String,
    indent: &str,
    sym: &SymbolRow,
    detail: Detail,
    source: Option<&mut Source>,
    budget: &mut Budget,
    sink: ExcerptSink<'_>,
) -> bool {
    let ExcerptSink {
        notes,
        lines: collected,
    } = sink;
    let (Some(src), Some(def)) = (source, sym.def.as_ref()) else {
        return true;
    };
    let ex = src.excerpt(&def.path, def.line, sym.def_end_line, detail);
    if let Some(note) = ex.note {
        notes.push(format!("[{}] {note}", sym.handle));
    }
    for (n, text) in ex.lines {
        if !budget.push(out, &format!("{indent}{n:>5} | {text}")) {
            notes.push(format!("[{}] excerpt cut by the budget", sym.handle));
            return false;
        }
        collected.push(SourceLine { line: n, text });
    }
    true
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{keep}~")
}
/// The known-unknowns report.
///
/// Written to be read by an agent deciding whether to trust the rest: the shape is
/// "here is what is solid, here is what is not, here is what to do about it".
pub fn verify(r: &cairn_store::Report) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(body, "index coverage");
    let _ = writeln!(body, "  files                    {:>8}", r.files);
    let _ = writeln!(body, "  symbols                  {:>8}", r.symbols);
    let _ = writeln!(
        body,
        "  references               {:>8}   {} with a caller",
        r.references,
        r.references - r.references_without_caller
    );

    // The report itself is the data. Every counter the text turns into a sentence is a
    // field here, which is the difference between a consumer reading "2 weak links are
    // candidates" and reading `weak_edges: 2`.
    let mut env = Envelope::new(body).data(r);

    // Everything below is a limit on what answers can mean. It is stated even when
    // benign, because an agent cannot infer a gap it was never told about.
    if r.symbols_without_definition > 0 {
        env = env.unknown(format!(
            "{} symbols are referenced but defined nowhere in the index (third-party \
             or unresolved); `expand` cannot show their source",
            r.symbols_without_definition
        ));
    }
    if r.definitions_without_body_span > 0 {
        env = env.unknown(format!(
            "{} definitions have no body extent from the indexer; `--detail body` \
             shows only their definition line",
            r.definitions_without_body_span
        ));
    }
    if r.references_without_caller > 0 {
        env = env.unknown(format!(
            "{} references sit outside any function body (imports, module-level use) \
             and therefore have no caller in the call graph",
            r.references_without_caller
        ));
    }
    if r.ambiguous_definitions > 0 {
        env = env.unknown(format!(
            "{} symbols are defined in more than one file; ranking shows one",
            r.ambiguous_definitions
        ));
    }
    if r.generated_by_path_only > 0 {
        env = env.unknown(format!(
            "{} files were called generated on a filename pattern alone, which is the \
             unreliable signal - reindex with --repo so headers can be read",
            r.generated_by_path_only
        ));
    }
    if r.files_without_content_hash > 0 {
        env = env.unknown(format!(
            "{} files have no recorded content hash, so their staleness cannot be checked",
            r.files_without_content_hash
        ));
    }
    if r.weak_edges > 0 {
        env = env.unknown(format!(
            "{} weak links are candidates, not facts; confirm before relying on one",
            r.weak_edges
        ));
    }

    if !r.staleness_checked {
        env = env.unknown(
            "staleness was not checked: pass --repo to compare the index against the \
             working tree",
        );
    } else {
        if !r.stale_files.is_empty() {
            let sample: Vec<&str> = r.stale_files.iter().take(5).map(|s| s.as_str()).collect();
            env.stale.push(format!(
                "{} files changed since indexing: {}{}",
                r.stale_files.len(),
                sample.join(", "),
                if r.stale_files.len() > 5 { ", ..." } else { "" }
            ));
        }
        if !r.missing_files.is_empty() {
            env.stale.push(format!(
                "{} indexed files no longer exist on disk",
                r.missing_files.len()
            ));
        }
    }
    if r.concept_links_dangling > 0 {
        env = env.unknown(format!(
            "{} authored links point at symbols no longer in the index; the code was              renamed or removed after the claim was made",
            r.concept_links_dangling
        ));
    }
    if r.concept_links_stale > 0 {
        env = env.unknown(format!(
            "{} authored links are anchored in code that has since changed and need a              fresh judgement (`cairn concept show`)",
            r.concept_links_stale
        ));
    }
    if r.manual_edges_stale > 0 {
        env = env.unknown(format!(
            "{} hand-authored links are anchored in code that has since changed. The \
             static pass cannot re-derive them - they need a fresh judgement (rerun \
             `cairn verify --flag-stale`, then review with `cairn links`)",
            r.manual_edges_stale
        ));
    }
    env
}

/// Hand-authored links touching a symbol.
/// One hand-authored link as data.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct LinkJson {
    /// Which way it runs, read from the subject: `out` is subject -> other.
    pub direction: &'static str,
    pub other: Option<SymbolJson>,
    /// The source label, e.g. `L2, agent-asserted`.
    pub source: &'static str,
    /// Where in the tree the claim was anchored when it was made.
    pub anchor: Option<String>,
    /// Why the link was asserted. Required when recording one, so never empty.
    pub note: String,
    /// The anchor file changed after the link was recorded: a claim to re-check.
    pub needs_review: bool,
}

/// `cairn links` as data.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct LinksData {
    pub symbol: SymbolJson,
    pub links: Vec<LinkJson>,
}

pub fn asserted(
    store: &cairn_store::Store,
    sym: &SymbolRow,
    links: &[cairn_store::verify::AssertedLink],
    budget: &mut Budget,
) -> anyhow::Result<Envelope> {
    let mut body = String::new();
    let mut as_data: Vec<LinkJson> = Vec::new();
    let _ = writeln!(
        body,
        "asserted links for [{}] {}",
        sym.handle,
        sym.qualified()
    );
    let mut needs_review = 0;
    let mut shown = 0usize;
    for (i, l) in links.iter().enumerate() {
        let other = if l.src == sym.id { l.dst } else { l.src };
        let other_row = store.symbol(other)?;
        let other_name = other_row
            .as_ref()
            .map(|s| format!("[{}] {}", s.handle, s.qualified()))
            .unwrap_or_else(|| format!("symbol {other}"));
        let arrow = if l.src == sym.id { "->" } else { "<-" };
        let flag = if l.needs_review {
            needs_review += 1;
            "  !! anchor changed, needs review"
        } else {
            ""
        };
        let anchor = l.anchor.as_deref().unwrap_or("<no anchor>");
        let line = format!(
            "  {arrow} {other_name}   [{}]  {anchor}{flag}\n     why: {}",
            l.source.label(),
            l.note
        );
        if !budget.push(&mut body, &line) {
            break;
        }
        as_data.push(LinkJson {
            direction: if l.src == sym.id { "out" } else { "in" },
            other: other_row.as_ref().map(SymbolJson::of),
            source: l.source.label(),
            anchor: l.anchor.clone(),
            note: l.note.clone(),
            needs_review: l.needs_review,
        });
        shown = i + 1;
    }
    if links.is_empty() {
        let _ = writeln!(body, "  (none)");
    }
    let mut env = Envelope::new(body).rows(shown).data(LinksData {
        symbol: SymbolJson::of(sym),
        links: as_data,
    });
    if shown < links.len() {
        env = env.suppressed(budget.cut_note(links.len() - shown, "links"));
    }
    if needs_review > 0 {
        env = env.unknown(format!(
            "{needs_review} of these are anchored in code that changed after they were \
             recorded; treat them as claims to re-check, not as facts"
        ));
    }
    Ok(env)
}

/// A concept and everything attached to it.
/// One symbol attached to a concept.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct ConceptLinkJson {
    /// Free text, the caller's own vocabulary: `part-of`, `entry-point`, `owns`.
    pub rel: String,
    /// `null` when the symbol this link names is no longer in the index — the code was
    /// renamed or deleted after the claim was made, which `resolved` reports.
    pub symbol: Option<SymbolJson>,
    pub note: String,
    /// Where in the tree the claim was anchored. `null` can never be invalidated.
    pub anchor: Option<String>,
    pub needs_review: bool,
    pub resolved: bool,
}

/// `cairn concept show` as data.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct ConceptData {
    pub namespace: String,
    pub name: String,
    pub note: String,
    /// Who asserted it, e.g. `L2, agent-asserted`.
    pub author: &'static str,
    pub links: Vec<ConceptLinkJson>,
}

/// `cairn concept list` as data.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct ConceptListData {
    pub concepts: Vec<ConceptSummaryJson>,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct ConceptSummaryJson {
    pub namespace: String,
    pub name: String,
    pub note: String,
    pub author: &'static str,
    pub links: i64,
}

pub fn concept(
    store: &cairn_store::Store,
    c: &cairn_store::Concept,
    links: &[cairn_store::ConceptLink],
    budget: &mut Budget,
) -> anyhow::Result<Envelope> {
    let mut body = String::new();
    let mut as_data: Vec<ConceptLinkJson> = Vec::new();
    let _ = writeln!(body, "{}/{}   [{}]", c.ns, c.name, c.author.label());
    if !c.note.is_empty() {
        let _ = writeln!(body, "  {}", c.note);
    }
    let _ = writeln!(body, "{} linked symbols", links.len());

    let mut needs_review = 0;
    let mut unanchored = 0;
    let mut shown = 0usize;
    for (i, l) in links.iter().enumerate() {
        let row = if l.resolved {
            store.symbol(l.symbol_id)?
        } else {
            None
        };
        let label = if l.resolved {
            row.as_ref()
                .map(symbol_line)
                .unwrap_or_else(|| "<gone from index>".to_string())
        } else {
            "<symbol no longer in the index>".to_string()
        };
        if l.needs_review {
            needs_review += 1;
        }
        if l.anchor.is_none() {
            unanchored += 1;
        }
        let flag = if l.needs_review {
            "  !! anchor changed"
        } else if !l.resolved {
            "  !! symbol gone"
        } else {
            ""
        };
        let note = if l.note.is_empty() {
            String::new()
        } else {
            format!("  — {}", l.note)
        };
        if !budget.push(&mut body, &format!("  {:<12} {label}{note}{flag}", l.rel)) {
            break;
        }
        as_data.push(ConceptLinkJson {
            rel: l.rel.clone(),
            symbol: row.as_ref().map(SymbolJson::of),
            note: l.note.clone(),
            anchor: l.anchor.clone(),
            needs_review: l.needs_review,
            resolved: l.resolved,
        });
        shown = i + 1;
    }
    if links.is_empty() {
        let _ = writeln!(body, "  (nothing linked yet)");
    }

    let mut env = Envelope::new(body).rows(shown).data(ConceptData {
        namespace: c.ns.clone(),
        name: c.name.clone(),
        note: c.note.clone(),
        author: c.author.label(),
        links: as_data,
    });
    // The header states the full link count, so a cut list contradicts it in silence.
    if shown < links.len() {
        env = env.suppressed(budget.cut_note(links.len() - shown, "links"));
    }
    if needs_review > 0 {
        env = env.unknown(format!(
            "{needs_review} links are anchored in code that changed after they were \
             recorded; the static pass cannot re-derive them, so they need a fresh look"
        ));
    }
    if unanchored > 0 {
        env = env.unknown(format!(
            "{unanchored} links have no anchor and can never be checked against the code"
        ));
    }
    env = env.unknown("concepts are asserted knowledge, not derived facts");
    Ok(env)
}

pub fn concept_list(list: &[cairn_store::Concept], budget: &mut Budget) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(body, "{} concepts", list.len());
    let mut shown = 0usize;
    let mut as_data: Vec<ConceptSummaryJson> = Vec::new();
    for (i, c) in list.iter().enumerate() {
        let note = if c.note.is_empty() {
            String::new()
        } else {
            format!("  — {}", c.note)
        };
        if !budget.push(
            &mut body,
            &format!(
                "  {:<28} {:>3} links{note}",
                format!("{}/{}", c.ns, c.name),
                c.link_count
            ),
        ) {
            break;
        }
        as_data.push(ConceptSummaryJson {
            namespace: c.ns.clone(),
            name: c.name.clone(),
            note: c.note.clone(),
            author: c.author.label(),
            links: c.link_count,
        });
        shown = i + 1;
    }
    if list.is_empty() {
        let _ = writeln!(body, "  (none)");
    }
    let mut env = Envelope::new(body)
        .rows(shown)
        .data(ConceptListData { concepts: as_data });
    if shown < list.len() {
        env = env.suppressed(budget.cut_note(list.len() - shown, "concepts"));
    }
    env
}

/// The dirty overlay for one file: what the language server sees now, against what the
/// index recorded.
///
/// The comparison is the product, not the listing. An agent asking about a file it just
/// edited needs to know which symbols the index no longer describes correctly - showing
/// only the live list would leave it to diff two outputs by eye.
pub fn live_overlay(
    path: &str,
    live: &[LiveSymbolView],
    indexed: &[(String, i64, Option<i64>)],
    budget: &mut Budget,
) -> Envelope {
    use std::collections::HashSet;
    // Compare qualified names on both sides. Two classes in a file can share a method
    // name, and matching bare names pairs them and invents a move.
    let live_q: Vec<(String, &LiveSymbolView)> = live.iter().map(|s| (s.qualified(), s)).collect();
    let live_names: HashSet<&str> = live_q.iter().map(|(q, _)| q.as_str()).collect();
    let indexed_names: HashSet<&str> = indexed.iter().map(|(n, _, _)| n.as_str()).collect();

    let mut body = String::new();
    let _ = writeln!(
        body,
        "{path}   {} symbols live, {} in the index        [L0, live]",
        live.len(),
        indexed.len()
    );

    let mut added = 0;
    let mut moved = 0;
    let mut shown = 0usize;
    for (i, (qualified, s)) in live_q.iter().enumerate() {
        let known = indexed.iter().find(|(n, _, _)| n == qualified);
        let (mark, note) = match known {
            None => {
                added += 1;
                ("+", String::new())
            }
            Some((_, line, _)) if *line != s.start_line => {
                moved += 1;
                ("~", format!("  (index says line {})", line + 1))
            }
            Some(_) => (" ", String::new()),
        };
        if !budget.push(
            &mut body,
            &format!(
                "{mark} {}:{}-{}  {qualified}{note}",
                path,
                s.start_line + 1,
                s.end_line + 1
            ),
        ) {
            break;
        }
        shown = i + 1;
    }

    let gone: Vec<&str> = indexed_names.difference(&live_names).copied().collect();
    // Counted rather than stopped at, matching what this loop already did: each line is
    // independent, so one that does not fit does not mean the next cannot.
    let mut gone_shown = 0usize;
    for name in &gone {
        if budget.push(
            &mut body,
            &format!("- {name}   (in the index, not in the file now)"),
        ) {
            gone_shown += 1;
        }
    }

    // Both halves of the comparison count: a symbol the index still lists and the file
    // no longer has is as much a row of this answer as one that is present.
    let mut env = Envelope::new(body).rows(shown + gone_shown);
    let dropped = (live_q.len() - shown) + (gone.len() - gone_shown);
    if dropped > 0 {
        env = env.suppressed(budget.cut_note(dropped, "symbols"));
    }
    if added + moved + gone.len() > 0 {
        env.stale.push(format!(
            "the index is behind for this file: {added} new, {moved} moved, {} gone",
            gone.len()
        ));
    }
    env = env.unknown(
        "this view is `documentSymbol` only: it shows what is defined in the file now,          not what references it. Reference answers still come from the index.",
    );
    env
}

/// Mirror of the daemon's live symbol shape, so cairn-fmt does not depend on the
/// daemon crate for one struct.
pub struct LiveSymbolView {
    pub name: String,
    pub kind: i64,
    pub start_line: i64,
    pub end_line: i64,
    pub container: Option<String>,
}

impl LiveSymbolView {
    /// `Class.method`, matching how the index qualifies its own names.
    pub fn qualified(&self) -> String {
        match &self.container {
            Some(c) if !c.is_empty() => format!("{c}.{}", self.name),
            _ => self.name.clone(),
        }
    }

    /// LSP `SymbolKind::Variable`. Locals and parameters arrive as variables nested in
    /// a function, and they are not file structure.
    pub fn is_local_variable(&self) -> bool {
        self.kind == 13 && self.container.is_some()
    }
}

// Entry point by concept: seeds, why each one is here, and how far to trust it.

/// Say so when the seeds carry no navigational signal.
///
/// Measured (task A, lost three times): asked to list the MCP tools and their required
/// scopes, an agent opened with `context` and got back two test files and a module
/// `__init__` — no symbol that answers anything, and no indication that this was the
/// tool's way of saying "I have nothing for this question". It kept querying. The answer
/// was a dictionary literal in one file the whole time.
///
/// A seed that is a module `__init__` says only "this module exists", and a seed in a test
/// file says only "something here mentions your words". When that is all there is, the
/// question is about what a file *contains* rather than how code connects, and the honest
/// move is to name the file and stop.
fn weak_seeds_note(seeds: &[cairn_store::Seed]) -> Option<String> {
    if seeds.is_empty() {
        return None;
    }
    let useless = |s: &cairn_store::Seed| -> bool {
        let is_init = s.symbol.name == "__init__";
        let is_test = s
            .symbol
            .def
            .as_ref()
            .map(|d| d.path.contains("/tests/") || d.path.contains("test_"))
            .unwrap_or(false);
        is_init || is_test
    };
    if !seeds.iter().all(useless) {
        return None;
    }
    // The best file to point at: a non-test seed if there is one, else the first.
    let best = seeds
        .iter()
        .find(|s| {
            s.symbol
                .def
                .as_ref()
                .map(|d| !d.path.contains("/tests/") && !d.path.contains("test_"))
                .unwrap_or(false)
        })
        .or_else(|| seeds.first())?;
    let path = best.symbol.def.as_ref()?.path.clone();
    Some(format!(
        "  STOP - every seed is a module __init__ or a test file, which means nothing here \n         \x20 answers your question. This looks like a question about what a file contains, \n         \x20 not about how code connects. Read {path} and do not keep querying."
    ))
}

pub fn context(
    query: &str,
    r: &cairn_store::ContextResult,
    coverage: &str,
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(body, "context for \"{query}\"   {} seeds", r.seeds.len());
    if !r.concepts_matched.is_empty() {
        let _ = writeln!(body, "matched concepts: {}", r.concepts_matched.join(", "));
    }
    let mut shown = 0usize;
    for (i, s) in r.seeds.iter().enumerate() {
        let why = s
            .sources
            .iter()
            .map(|x| x.label())
            .collect::<Vec<_>>()
            .join("+");
        if !budget.push(&mut body, &format!("{}  [{why}]", symbol_line(&s.symbol))) {
            break;
        }
        shown = i + 1;
    }
    if r.seeds.is_empty() {
        let _ = writeln!(body, "  (nothing matched)");
    }
    if let Some(note) = weak_seeds_note(&r.seeds) {
        let _ = writeln!(body, "{note}");
    }

    let mut env = Envelope::new(body).rows(shown);
    if shown < r.seeds.len() {
        env = env.suppressed(budget.cut_note(r.seeds.len() - shown, "seeds"));
    }
    if r.low_confidence {
        // Handing over weak guesses dressed as answers is how a tool teaches an agent
        // to stop trusting it. Say the seed is thin, say what is covered, name the
        // fallback.
        env = env.unknown(format!(
            "low confidence: no concept, name or docstring matched strongly. {coverage} \
             - if what you want is outside that, it is not indexed and grep is the tool"
        ));
    }
    // Not when the seeds were already declared useless: telling the caller to expand a
    // seed set that answers nothing is the advice that cost task A three losses.
    if weak_seeds_note(&r.seeds).is_none() {
        env = env.unknown(
            "seeds are a starting point, not an answer - expand with `cairn graph <handle>`",
        );
    }
    env
}

/// Symbols in a subtree that production code never calls.
pub fn unreached(
    prefix: &str,
    rows: &[cairn_store::UnreachedSymbol],
    // Symbols the caller withheld because reachability cannot speak about them. The header
    // must know: with rows removed, "0 symbols … everything here has a production caller"
    // is a claim about symbols nobody looked at, and it contradicts the envelope line two
    // rows below that says exactly the opposite.
    withheld: usize,
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(
        body,
        "{} symbols under {prefix} with no production caller{}       [L1, exact]",
        rows.len(),
        if withheld > 0 {
            format!(", and {withheld} that could not be checked")
        } else {
            String::new()
        }
    );
    let mut shown = 0usize;
    for (i, r) in rows.iter().enumerate() {
        // The reference count is part of the claim, not a detail. "no callers" on a symbol
        // with references reads as "delete me" and is a trap: the enum that surfaced this
        // had ten, every one of them the table built from its own members. Saying the
        // number keeps the row — it is still a deletion candidate — while telling the
        // reader it costs more than one line.
        let why = match r.why {
            cairn_store::Unreached::TestsOnly => format!("tests only ({})", r.test_callers),
            cairn_store::Unreached::Never if r.prod_refs > 0 => {
                format!("no calls, {} ref(s)", r.prod_refs)
            }
            cairn_store::Unreached::Never => "no callers".to_string(),
        };
        if !budget.push(
            &mut body,
            &format!("  {:<22} {}", why, symbol_line(&r.symbol)),
        ) {
            break;
        }
        shown = i + 1;
    }
    if rows.is_empty() {
        let _ = writeln!(
            body,
            "  {}",
            if withheld > 0 {
                "(nothing listed here is uncalled; see `unknown` for what was not checked)"
            } else {
                "(everything here has a production caller)"
            }
        );
    }
    let mut env = Envelope::new(body).rows(shown).unknown(
        "reachability is static: a symbol invoked by name at runtime looks unreached. \
         Check `cairn weaklinks <handle>` before deleting anything",
    );
    // The header counts every unreached symbol, so a budget cut leaves a list shorter
    // than the number above it. Unsaid, that reads as a miscount rather than as a page.
    if shown < rows.len() {
        env = env.suppressed(budget.cut_note(rows.len() - shown, "symbols"));
    }
    env
}

/// What a module contains, and how used each thing is.
pub fn outline(
    prefix: &str,
    rows: &[cairn_store::OutlineEntry],
    total: i64,
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(body, "{prefix}   {} of {total} definitions", rows.len());
    let mut shown = 0usize;
    for (i, r) in rows.iter().enumerate() {
        let use_note = if r.production_callers > 0 {
            format!("{} prod", r.production_callers)
        } else if r.dispatched {
            // Not "test-only" and not "unused": a service binding reaches it, and the
            // static caller count is simply not evidence either way.
            "dispatched".to_string()
        } else if r.caller_count > 0 {
            format!("{} test-only", r.caller_count)
        } else {
            "unused".to_string()
        };
        if !budget.push(
            &mut body,
            &format!("  {:<12} {}", use_note, symbol_line(&r.symbol)),
        ) {
            break;
        }
        shown = i + 1;
    }
    if rows.is_empty() {
        // "Nothing indexed" and "everything here is generated, and I exclude that" are
        // different facts, and the first is a lie when the second is true — a caller
        // reasonably concludes the path is outside the index and stops looking.
        let _ = writeln!(
            body,
            "  (nothing here, or everything here is generated code, which this view \
             excludes - `cairn symbol <name>` still finds generated definitions)"
        );
    }
    {
        let mut env = Envelope::new(body).rows(shown);
        let dropped = total - rows.len() as i64;
        if dropped > 0 {
            env = env.suppressed(format!(
                "{dropped} definitions beyond --limit. Whole files can be missing from \
                 this list; raise --limit or narrow the path"
            ));
        }
        // A second, independent cut: `--limit` decides what the store returned, the
        // budget decides how much of that got printed. Reporting only the first left
        // the header promising rows the body never contained.
        if shown < rows.len() {
            env = env.suppressed(budget.cut_note(rows.len() - shown, "definitions"));
        }
        env
    }
}

/// Say when every use of something sits in one file — but only when that is *known*.
///
/// The measured losses were all tasks whose answer lived in a single file: the agent paid
/// for the skill, then for a query, then read the file anyway. The tool can see this, so
/// it should say it rather than let the caller discover it after paying.
///
/// `complete` is the whole of the contract. This sentence begins with ALL and ends by
/// telling the reader to stop querying, so it is only allowed to exist when the paths it
/// was given are every path there is. Reported from a real audit: a symbol with 52
/// references across several files was cut to 40 by `--limit`, the 40 that survived
/// happened to share a file, and cairn announced that ALL of them were in it and advised
/// against looking further. The references it had dropped were the production callers.
///
/// A caller that cannot prove completeness must pass `false` and get silence. Silence
/// costs a caller one more query; this sentence, when wrong, costs them the answer.
pub fn concentration_note(paths: &[&str], complete: bool, noun: &str) -> Option<String> {
    if !complete {
        return None;
    }
    // Was three. Measured (task A, three losses): a symbol whose one or two use sites sit
    // in the file that defines it is the exact case SKILL.md's "the answer is in one file
    // you already know" describes, and the threshold meant the tool never said so for the
    // smallest and clearest instances of it.
    if paths.is_empty() {
        return None;
    }
    let first = paths.first()?;
    if !paths.iter().all(|p| p == first) {
        return None;
    }
    Some(format!(
        "ALL {} {noun} ARE IN ONE FILE: {first}. Reading it is probably cheaper than \
         further queries.",
        paths.len()
    ))
}

/// When the index cannot answer a question well, say so *first* and hand over the tool
/// that can.
///
/// Measured cost of not doing this: asked for the blast radius of a Django model field,
/// an agent read the partial list, correctly distrusted it because of the caveat, and
/// then did the full grep pass anyway - paying for both. Honesty buried at the bottom of
/// a long partial answer is the worst of both worlds.
///
/// So for symbols the resolver is known to under-report, the answer leads with the
/// recommendation and includes the command, and the partial list is kept short. One
/// cheap call that routes correctly beats an expensive one that has to be redone.
/// Python only, and the restriction is the honest part rather than an oversight.
///
/// The measurement behind this note is a Django model field: the resolver reported a
/// fraction of the uses because the holder is an ORM instance whose type is not known at
/// the access. Nothing of the kind has been measured for TypeScript, where the holder's
/// type usually *is* known, and a `USE GREP FOR THIS ONE` printed over a list that happens
/// to be complete buys the agent a grep it did not need. That is the same overstatement
/// this note exists to prevent, pointing the other way.
///
/// It was language-blind before, and on a TypeScript property it printed the ORM
/// explanation with `--include='*.py'` attached — a command that cannot match the file the
/// symbol is defined in.
pub fn routing_note(sym: &SymbolRow) -> Option<String> {
    if !cairn_store::query::is_under_resolved_attribute(sym.kind, sym.container.as_deref())
        || sym.lang != Lang::Python
    {
        return None;
    }
    Some(format!(
        "USE GREP FOR THIS ONE. `{}` is an attribute on a type, and attribute access \
         resolves only where the holder's type is known - for ORM instances and \
         dicts-as-records it usually is not, so the list below is a lower bound and \
         not a blast radius. Run instead:  grep -rn '{}' <src> --include='*.py'",
        sym.qualified(),
        sym.name
    ))
}

/// Note added when the index is likely to under-report a symbol's uses.
///
/// Unlike the routing note this stays for every language, because the mechanism does: a use
/// is resolved where the holder's type is known and not where it is not. What differs is
/// which shapes lose the type, so the examples follow the language rather than naming
/// Python's for all of them.
fn attribute_caveat(sym: &SymbolRow) -> Option<String> {
    cairn_store::query::is_under_resolved_attribute(sym.kind, sym.container.as_deref()).then(|| {
        let loses_the_type = match sym.lang {
            Lang::TypeScript => {
                "values typed `any`, index signatures and objects built dynamically"
            }
            Lang::Go => "values reached through an interface or `any`",
            _ => "ORM models, dicts-as-records and dynamically built objects",
        };
        format!(
            "`{}` is an attribute on a type. Uses are only resolved where the holder's \
             type is known, which for {loses_the_type} it often is not - so this list is a \
             lower bound. Cross-check with grep before treating it as the blast radius.",
            sym.qualified()
        )
    })
}

/// `complete` is false when `--limit` may have cut the file list.
pub fn usage(
    sym: &SymbolRow,
    rows: &[(String, i64, bool)],
    complete: bool,
    // Sites in test files the query filtered out, when tests were not asked for. Zero
    // when `--include-tests` was given, because then nothing was filtered.
    tests_filtered: (i64, usize),
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    if let Some(note) = routing_note(sym) {
        let _ = writeln!(body, "{note}\n");
    }
    let total: i64 = rows.iter().map(|(_, n, _)| n).sum();
    let _ = writeln!(
        body,
        "[{}] {} used at {total} sites in {} files      [L0, exact]",
        sym.handle,
        sym.qualified(),
        rows.len()
    );
    // One file in a list that may have been cut is one file *so far*, which is not what
    // this sentence says.
    if complete && rows.len() == 1 && !rows[0].1.eq(&0) {
        let _ = writeln!(
            body,
            "ALL USES ARE IN ONE FILE: {}. Reading it is probably cheaper than further \
             queries.",
            rows[0].0
        );
    }
    let mut shown = 0usize;
    for (i, (path, n, is_test)) in rows.iter().enumerate() {
        let tag = if *is_test { "  [test]" } else { "" };
        if !budget.push(&mut body, &format!("  {n:>4}x  {path}{tag}")) {
            break;
        }
        shown = i + 1;
    }
    if rows.is_empty() {
        let _ = writeln!(body, "  (no uses outside its own definition)");
    }
    let mut env = Envelope::new(body).rows(shown);
    // `complete` covers `--limit`; the budget can cut the printed list independently of
    // it, and a file list that stops early is exactly the case the ALL-IN-ONE-FILE
    // sentence above must never be read against.
    if shown < rows.len() {
        env = env.suppressed(budget.cut_note(rows.len() - shown, "files"));
    }
    // Measured: an agent asked which call sites a signature change would break was told
    // "2 sites in 2 files", with `suppressed: none` and `unknown: none`, while `graph
    // --aspect callers` on the same symbol returned four call sites - the two missing
    // ones were in a test file. The filter is the right default (the question is usually
    // about production), but an answer that drops rows and then states it dropped none is
    // the failure this envelope exists to prevent. The agent caught it, cross-checked
    // with `graph`, and said so in its answer; the cost was the round trips it took to
    // stop trusting the first number.
    let (test_sites, test_files) = tests_filtered;
    if test_sites > 0 {
        env = env.unknown(format!(
            "{test_sites} more site(s) in {test_files} test file(s) are not counted above. \
             `cairn usage {} --include-tests` includes them",
            sym.handle
        ));
    }
    if let Some(note) = attribute_caveat(sym) {
        env = env.unknown(note);
    }
    env
}

/// Reachability across a gRPC service boundary.
///
/// The header names the service, because the answer is only trustworthy if the caller
/// can see the route: these edges are recovered from a generator's naming convention,
/// not resolved by a compiler, and a reader who cannot check the hop should not be
/// asked to believe it.
/// Callers of one RPC, which is a stronger claim than callers of its handler: these are
/// real call sites, not a naming convention, so they are labelled exact.
/// What this symbol calls across a service boundary — the answer to "where does this land".
///
/// Separate from `rpc_reaches` because the sentence is different: one names who arrives
/// here, the other where you go next. The chain question is asked with the *start* in
/// hand and the end unknown, so this is the row an agent walking a chain needs, and the
/// row it kept rebuilding by hand when the command returned nothing.
pub fn rpc_targets(
    sym: &SymbolRow,
    targets: &[cairn_store::RpcCaller],
    // Services with no generated client in the index to name their RPCs. Their rows are
    // unfiltered, so the envelope has to say which they are.
    unchecked: &[String],
    // True when the rows come from real call edges, false when they come from a client
    // binding. Same rows either way — the strength is said, not left to the shape.
    from_call_sites: bool,
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(
        body,
        "[{}] {} — {} {} handler(s) across gRPC        [L1, {}]",
        sym.handle,
        sym.qualified(),
        if from_call_sites {
            "calls"
        } else {
            "can reach"
        },
        targets.len(),
        if from_call_sites {
            "convention"
        } else {
            "convention, by client binding"
        }
    );
    let _ = writeln!(
        body,
        "  {}",
        if from_call_sites {
            // Not "in the other language" any more. That wording described a filter that
            // was hiding every Python service calling another Python one, and a header
            // that promises a narrower answer than the command gives is the same defect
            // as one that promises a wider one.
            "where this lands, in the process that serves it. Each row is the handler \
             that serves an RPC this code was seen to call"
        } else {
            "this code holds a generated client for these services; each row is a handler \
             they serve. No call site was resolved, so a row here is what it *can* reach, \
             not what it was seen to reach"
        }
    );
    let mut shown = 0usize;
    for t in targets {
        let def = t
            .symbol
            .def
            .as_ref()
            // `location()`, not `path:line`. SCIP lines are 0-based and every other
            // command in this binary prints the 1-based number an editor uses; this one
            // did not, so `reaches X --outgoing` and `reaches X` named the same
            // definition one line apart. Found by building `for understand` on top of it.
            .map(|d| d.location())
            .unwrap_or_default();
        if !budget.push(
            &mut body,
            &format!(
                "  [{}] {:<40} {:<3} {}  [{}.{}.{}]",
                t.symbol.handle,
                t.symbol.qualified(),
                t.symbol.lang.tag(),
                def,
                t.pkg,
                t.service,
                t.rpc
            ),
        ) {
            break;
        }
        shown += 1;
    }
    let mut env = Envelope::new(body).rows(shown);
    if shown < targets.len() {
        env = env.suppressed(budget.cut_note(targets.len() - shown, "targets"));
    }
    if !unchecked.is_empty() {
        env = env.unknown(format!(
            "{} service(s) have no generated client in this index to list their RPCs \
             ({}), so every member of their handler is shown - private helpers included. \
             Rows for the other services are filtered to real RPCs",
            unchecked.len(),
            unchecked.join(", ")
        ));
    }
    env.unknown(
        "the handler is matched to the RPC by the generator's naming convention \
         (`GetFolder` <-> `get_folder`), so this is exact where the convention holds. A \
         call made through a hand-written transport or a queue is not here, and a branch \
         the caller only takes conditionally is still listed",
    )
}

/// One caller on the other side of a gRPC boundary.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct CrossCallerJson {
    pub symbol: SymbolJson,
    pub package: String,
    pub service: String,
    /// The RPC, where the answer is per-RPC rather than per-service.
    pub rpc: Option<String>,
}

/// `cairn reaches` as data, in either direction.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct ReachesData {
    pub symbol: SymbolJson,
    /// `incoming` is who reaches this; `outgoing` is what this reaches.
    pub direction: &'static str,
    /// How the edges were derived: `rpc` is the per-RPC call graph through the generated
    /// client, `convention` is the protobuf generator's naming convention.
    pub basis: &'static str,
    /// The enclosing type the answer was given for, when a binding names the handler
    /// rather than each of its RPC methods.
    pub via_symbol: Option<SymbolJson>,
    /// Services the symbol takes part in, as `(package, service, role)`.
    pub services: Vec<ServiceRoleJson>,
    pub callers: Vec<CrossCallerJson>,
}

/// A service the symbol takes part in, and on which side.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct ServiceRoleJson {
    pub package: String,
    pub service: String,
    /// `serves` or `calls`.
    pub role: String,
}

pub fn rpc_reaches(
    sym: &SymbolRow,
    callers: &[cairn_store::RpcCaller],
    whole_type: bool,
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let mut as_data: Vec<CrossCallerJson> = Vec::new();
    let _ = writeln!(
        body,
        "[{}] {} — {} caller(s) across gRPC, by RPC        [L1, exact]",
        sym.handle,
        sym.qualified(),
        callers.len()
    );
    let _ = writeln!(
        body,
        "  {}. The package is part of the answer: two packages can carry the same \
         service name to different processes",
        if whole_type {
            "every RPC this handler serves, with the callers of each - one call, not one per method"
        } else {
            "this RPC only, not everything its handler serves"
        }
    );
    let mut shown = 0usize;
    for (i, c) in callers.iter().enumerate() {
        if !budget.push(
            &mut body,
            &format!(
                "  {}  [{}.{}.{}]",
                symbol_line(&c.symbol),
                c.pkg,
                c.service,
                c.rpc
            ),
        ) {
            break;
        }
        as_data.push(CrossCallerJson {
            symbol: SymbolJson::of(&c.symbol),
            package: c.pkg.clone(),
            service: c.service.clone(),
            rpc: Some(c.rpc.clone()),
        });
        shown = i + 1;
    }
    let mut env = Envelope::new(body).rows(shown).data(ReachesData {
        symbol: SymbolJson::of(sym),
        direction: "incoming",
        basis: "rpc",
        via_symbol: None,
        services: Vec::new(),
        callers: as_data,
    });
    if shown < callers.len() {
        env = env.suppressed(budget.cut_note(callers.len() - shown, "callers"));
    }
    env = env.unknown(
        "these are calls to the generated client. A service that reaches this some other \
         way - a hand-written transport, a REST gateway, a queue - is not here",
    );
    env
}

pub fn cross_language(
    sym: &SymbolRow,
    services: &[(String, String, cairn_store::ServiceRole)],
    links: &[cairn_store::CrossLink],
    outgoing: bool,
    via: Option<&SymbolRow>,
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let direction = if outgoing { "reached by" } else { "reaches" };
    let _ = writeln!(
        body,
        "[{}] {} — {} {} across gRPC        [L1, convention]",
        sym.handle,
        sym.qualified(),
        links.len(),
        if outgoing { "targets" } else { "callers" }
    );
    if let Some(owner) = via {
        let _ = writeln!(
            body,
            "  answered for the enclosing type [{}] {}: a service binding names the \
             handler, not each of its RPC methods",
            owner.handle,
            owner.qualified()
        );
    }
    if !services.is_empty() {
        let list: Vec<String> = services
            .iter()
            .map(|(p, n, r)| format!("{} {p}.{n}", r.label()))
            .collect();
        let _ = writeln!(body, "  via: {}", list.join(", "));
    }
    let _ = writeln!(body, "  ({direction} it, in the other language)");

    let mut shown = 0usize;
    let mut as_data: Vec<CrossCallerJson> = Vec::new();
    for (i, l) in links.iter().enumerate() {
        if !budget.push(
            &mut body,
            &format!("  {}  [{}.{}]", symbol_line(&l.symbol), l.pkg, l.service),
        ) {
            break;
        }
        as_data.push(CrossCallerJson {
            symbol: SymbolJson::of(&l.symbol),
            package: l.pkg.clone(),
            service: l.service.clone(),
            rpc: None,
        });
        shown = i + 1;
    }
    if links.is_empty() {
        let _ = writeln!(body, "  (nothing on the other side of a service boundary)");
    }

    let mut env = Envelope::new(body).rows(shown).data(ReachesData {
        symbol: SymbolJson::of(sym),
        direction: if outgoing { "outgoing" } else { "incoming" },
        basis: "convention",
        via_symbol: via.map(SymbolJson::of),
        services: services
            .iter()
            .map(|(p, n, r)| ServiceRoleJson {
                package: p.clone(),
                service: n.clone(),
                role: r.label().to_string(),
            })
            .collect(),
        callers: as_data,
    });
    if shown < links.len() {
        env = env.suppressed(budget.cut_note(links.len() - shown, "links"));
    }
    env = env.unknown(
        "these edges come from the protobuf generator's naming convention, not from a \
         compiler. They are exact where the convention holds, and blind to any service \
         wired up by hand or reached over a transport other than the generated client.",
    );
    env
}

/// The deployment map: services, what starts each one, and where that lands in the code.
pub fn topology(rows: &[cairn_store::DeployServiceRow], budget: &mut Budget) -> Envelope {
    let mut body = String::new();
    let resolved = rows.iter().filter(|r| r.4).count();
    let _ = writeln!(
        body,
        "{} services, {resolved} with a resolved entrypoint        [L0-D, exact]",
        rows.len()
    );
    let mut unresolved = Vec::new();
    let mut shown = 0usize;
    for (i, (name, command, entry_path, ports, ok)) in rows.iter().enumerate() {
        let where_ =
            entry_path
                .clone()
                .unwrap_or_else(|| if *ok { String::new() } else { "—".into() });
        let p = if ports.is_empty() {
            String::new()
        } else {
            format!("  :{ports}")
        };
        if !*ok && command.is_some() {
            unresolved.push(name.clone());
        }
        if !budget.push(
            &mut body,
            &format!(
                "  {:<24} {:<44} {}{p}",
                name,
                truncate(command.as_deref().unwrap_or("(image default)"), 44),
                where_
            ),
        ) {
            break;
        }
        shown = i + 1;
    }
    let mut env = Envelope::new(body).rows(shown);
    if shown < rows.len() {
        env = env.suppressed(budget.cut_note(rows.len() - shown, "services"));
    }
    if !unresolved.is_empty() {
        // An unresolved entrypoint makes everything that service runs look unreachable.
        // That silently reclassifies live code as dead, so it is stated, not counted.
        env = env.unknown(format!(
            "{} services have a start command that could not be resolved to code ({}). \
             Anything only those run will look unreachable",
            unresolved.len(),
            unresolved.join(", ")
        ));
    }
    env
}

/// String literals, with the code around each and whose code it is.
///
/// The source comes by default rather than on request. A location alone forces a second
/// call to decide anything, and a second call costs an inference — seconds — against the
/// milliseconds this query takes. Cheap to send, expensive to make someone ask for.
pub fn literals(
    rows: &[cairn_store::LiteralSite],
    needle: &str,
    source: Option<&mut Source>,
    ctx: SiteContext,
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let attributed = rows.iter().filter(|l| l.enclosing.is_some()).count();
    let _ = writeln!(
        body,
        "{} literals containing \"{needle}\", {attributed} inside a function   [L0, exact]",
        rows.len()
    );

    let mut source = source;
    let mut shown = 0usize;
    for (i, l) in rows.iter().enumerate() {
        let whose = match &l.enclosing {
            // The handle is the product: it turns "found it here" into a question you can
            // ask next, without going back to `symbol` first.
            Some(s) => format!("  in {} [{}]", s.qualified(), s.handle),
            None => "  (module level)".to_string(),
        };
        let lines = source
            .as_deref_mut()
            .map(|src| src.site(&l.path, l.line, ctx))
            .unwrap_or_default();
        let inline = if lines.len() == 1 {
            format!("   |  {}", lines[0].1.trim())
        } else {
            String::new()
        };
        let head = format!("  {}:{}{whose}{inline}", l.path, l.line + 1);
        if !budget.push(&mut body, &head) {
            break;
        }
        if lines.len() > 1 {
            for (n, text) in &lines {
                let marker = if *n as i64 == l.line + 1 { ">" } else { " " };
                if !budget.push(&mut body, &format!("      {marker}{n:>5} | {text}")) {
                    break;
                }
            }
        }
        shown = i + 1;
    }
    if rows.is_empty() {
        let _ = writeln!(
            body,
            "  (no literal contains that. Only Python and Go are scanned, comments are \
             skipped, and nothing outside a string is here - for those, grep)"
        );
    }

    let mut env = Envelope::new(body).rows(shown);
    if shown < rows.len() {
        env = env.suppressed(budget.cut_note(rows.len() - shown, "literals"));
    }
    env = env.unknown(
        "string literals only, from the languages that are indexed. A name built at run \
         time - concatenated, formatted, or read from config - is not a literal and is \
         not here",
    );
    env
}

/// The documentation map: what exists, and roughly what each one covers.
///
/// The question this answers is which document to open, and the numbers are there for
/// exactly that. A reader deciding between four files needs to know what each would cost
/// before paying it — which is the decision that goes wrong today, silently, every time.
pub fn documents(rows: &[cairn_store::DocumentRow], budget: &mut Budget) -> Envelope {
    let mut body = String::new();
    let words: usize = rows.iter().map(|d| d.words).sum();
    let _ = writeln!(
        body,
        "{} documents, {} sections, ~{words} words        [L0-M, exact]",
        rows.len(),
        rows.iter().map(|d| d.sections).sum::<usize>()
    );
    let mut shown = 0usize;
    for (i, d) in rows.iter().enumerate() {
        let title = d.title.as_deref().unwrap_or("(no title)");
        let mut block = format!(
            "  {:<44} {:>5}w  {}",
            truncate(&d.path, 44),
            d.words,
            truncate(title, 40)
        );
        if !d.top.is_empty() {
            // The top-level headings are the summary. Written by whoever wrote the
            // document, which makes them a better description than anything derived.
            let _ = write!(block, "\n{:<46}   {}", "", truncate(&d.top.join(" · "), 90));
        }
        if !budget.push(&mut body, &block) {
            break;
        }
        shown = i + 1;
    }
    if rows.is_empty() {
        let _ = writeln!(
            body,
            "  (no markdown indexed - run `cairn index` in the repository)"
        );
    }
    let mut env = Envelope::new(body).rows(shown);
    if shown < rows.len() {
        env = env.suppressed(budget.cut_note(rows.len() - shown, "documents"));
    }
    env = env.unknown(
        "headings and spans only, never the prose. This says which document and which \
         lines; what they say is in the file",
    );
    env
}

/// One document's sections, as a skeleton to descend through.
pub fn doc_sections(rows: &[cairn_store::SectionRow], budget: &mut Budget) -> Envelope {
    let mut body = String::new();
    let words: usize = rows.iter().map(|s| s.words).sum();
    let _ = writeln!(
        body,
        "{} sections, ~{words} words        [L0-M, exact]",
        rows.len()
    );
    let mut shown = 0usize;
    for (i, s) in rows.iter().enumerate() {
        let indent = "  ".repeat(s.level.saturating_sub(1));
        // The range is the answer: read exactly this, not the file.
        let line = format!(
            "  {:<52} {:>5}w  {}:{}-{}",
            format!(
                "{indent}{}",
                truncate(&s.heading, 50 - indent.len().min(40))
            ),
            s.words,
            s.path,
            s.start_line,
            s.end_line
        );
        if !budget.push(&mut body, &line) {
            break;
        }
        shown = i + 1;
    }
    if rows.is_empty() {
        let _ = writeln!(
            body,
            "  (no such document indexed - `cairn docs` lists what there is)"
        );
    }
    let mut env = Envelope::new(body).rows(shown);
    if shown < rows.len() {
        env = env.suppressed(budget.cut_note(rows.len() - shown, "sections"));
    }
    env
}

/// Sections that answer a search, strongest claim first.
pub fn doc_search(
    rows: &[(cairn_store::SectionRow, cairn_store::Hit)],
    query: &str,
    budget: &mut Budget,
) -> Envelope {
    use cairn_store::Hit;
    let mut body = String::new();
    let named = rows.iter().filter(|(_, h)| *h == Hit::Heading).count();
    let _ = writeln!(
        body,
        "{} sections for \"{query}\": {named} are about it, {} mention it   [L0-M, exact]",
        rows.len(),
        rows.len() - named
    );
    let _ = writeln!(
        body,
        "  Each is a range to read, not a file to open. `about` means a heading names it."
    );

    let mut shown = 0usize;
    for (i, (s, hit)) in rows.iter().enumerate() {
        let mark = match hit {
            Hit::Heading => "about".to_string(),
            Hit::Body(n) => format!("{n}x"),
        };
        let line = format!(
            "  {:<8} {:<48} {:>5}w  {}:{}-{}",
            mark,
            truncate(&s.trail, 48),
            s.words,
            s.path,
            s.start_line,
            s.end_line
        );
        if !budget.push(&mut body, &line) {
            break;
        }
        shown = i + 1;
    }
    if rows.is_empty() {
        let _ = writeln!(
            body,
            "  (nothing. `cairn docs` lists what is indexed - if the documentation for \
             this lives outside the repository, nothing here can see it)"
        );
    }

    let mut env = Envelope::new(body).rows(shown);
    if shown < rows.len() {
        env = env.suppressed(budget.cut_note(rows.len() - shown, "sections"));
    }
    env = env.unknown(
        "a plain substring, case-insensitive: this finds where a subject is written \
         about, not what is said about it. Synonyms and paraphrases are not matched, and \
         nothing here reads the prose for meaning",
    );
    env
}

/// Claims put to a judgement, with the evidence and what would falsify each.
///
/// Written to be worked through rather than read: every check names where to look and how
/// to record the answer, because a plan that leaves the reader to work out the next step
/// is a plan that gets skimmed and confirmed wholesale.
pub fn verification_plan(
    checks: &[(cairn_store::Check, cairn_store::Standing)],
    head: Option<&str>,
    dirty: bool,
    budget: &mut Budget,
) -> Envelope {
    use cairn_store::Standing;
    let mut body = String::new();
    let open = checks
        .iter()
        .filter(|(_, s)| *s != Standing::Current)
        .count();
    let _ = writeln!(
        body,
        "{open} of {} claims need a judgement        [asserted, not derived]",
        checks.len()
    );
    let _ = writeln!(
        body,
        "  These are the places being indexed does not settle: a pass can produce output \
         that\n  counts correctly and means nothing. Settle one by looking, then record it."
    );
    match head {
        Some(sha) => {
            let _ = writeln!(body, "  commit    {}", &sha[..sha.len().min(12)]);
        }
        None => {
            let _ = writeln!(
                body,
                "  commit    unknown - a verdict recorded now cannot be aged, so it will \
                 read as expired"
            );
        }
    }
    if dirty {
        let _ = writeln!(
            body,
            "  tree      has uncommitted changes, so what you look at is not what the \
             commit describes"
        );
    }

    let mut shown = 0usize;
    for (i, (c, standing)) in checks.iter().enumerate() {
        if *standing == Standing::Current {
            continue;
        }
        let mut block = format!("\n  [{}] {}\n    claim:     {}", c.id, c.area, c.claim);
        for e in &c.evidence {
            let _ = write!(block, "\n    look at:   {e}");
        }
        let _ = write!(block, "\n    wrong if:  {}", c.falsifier);
        let _ = write!(
            block,
            "\n    record:    cairn llm verify --check {} --holds\n\
             \x20              cairn llm verify --check {} --broken --note \"<what is wrong>\"",
            c.id, c.id
        );
        if *standing == Standing::Broken {
            let _ = write!(
                block,
                "\n    standing:  judged wrong, and still recorded so"
            );
        } else if *standing == Standing::Expired {
            let _ = write!(
                block,
                "\n    standing:  judged against a different commit - look again"
            );
        }
        if !budget.push(&mut body, &block) {
            break;
        }
        shown = i + 1;
    }

    if open == 0 {
        let _ = writeln!(
            body,
            "\n  (every claim has been judged against this commit)"
        );
    }
    if checks.is_empty() {
        let _ = writeln!(
            body,
            "\n  (nothing here needs judging: no entrypoint resolved and no service \
             boundary was recovered)"
        );
    }

    let mut env = Envelope::new(body).rows(shown);
    if shown < checks.len() && open > shown {
        env = env.suppressed(budget.cut_note(open - shown, "claims"));
    }
    env = env.unknown(
        "a verdict recorded here is an opinion, not a derivation. Nothing acts on it, no \
         exit code changes, and it can be recorded again",
    );
    env
}

/// The coverage axis: what each mechanism produced, area by area.
///
/// Plain text rather than an envelope because `status` is not an answer about the code —
/// it is the report an agent reads to decide whether to believe the answers. Returned as
/// a block so the layout lives here with every other layout.
pub fn coverage(areas: &[cairn_store::Area]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "\ncoverage   what each mechanism produced, against the tree it was built from"
    );
    // The ladder, said once at the top. `indexed` is two counts agreeing; `verified` is
    // the rung above and needs a check that runs the query, so it can only ever apply to
    // something already indexed.
    let _ = writeln!(
        out,
        "           indexed -> verified -> verify stale -> verified. `indexed` is the pass\n\
         \x20          having run and matched the tree, which is counting; the rungs above\n\
         \x20          are judgements recorded by `cairn llm verify` and expire with the tree"
    );
    for a in areas {
        // Severity in the gutter rather than in the state's spelling. The table is
        // skimmed, so it needs a mark at row level; the footer below names the same rows
        // for anyone reading rather than scanning.
        let _ = writeln!(
            out,
            "{} {:<14} {:<13} {}",
            if a.state.is_trouble() { "!" } else { " " },
            truncate(&a.name, 14),
            a.state.label(),
            a.detail
        );
    }
    let trouble: Vec<&str> = areas
        .iter()
        .filter(|a| a.state.is_trouble())
        .map(|a| a.name.as_str())
        .collect();
    if !trouble.is_empty() {
        // Repeated at the bottom on purpose: the rows above are a table, and a table is
        // skimmed. The one line that says "do not trust answers about these" is not.
        let _ = writeln!(
            out,
            "  -> answers that rest on {} are incomplete or empty, and will not say so",
            trouble.join(", ")
        );
    }
    out
}

/// Every way into the codebase: what starts code, and where that lands.
///
/// The unit is the entrypoint, not the service, because the question this answers is how
/// code gets run and a container with three cron jobs offers four different answers to
/// it. Each row ends in a path so the next question is `cairn outline <path>` — the point
/// of listing entrypoints is to have somewhere to descend from.
pub fn entrypoints(
    rows: &[cairn_store::Entrypoint],
    reaches: Option<&SymbolRow>,
    blind: &[String],
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let services: std::collections::BTreeSet<&str> =
        rows.iter().map(|e| e.service.as_str()).collect();
    match reaches {
        Some(sym) => {
            let _ = writeln!(
                body,
                "{} entrypoint(s) can run [{}] {}, across {} service(s)   [L1 + L0-D, exact]",
                rows.len(),
                sym.handle,
                sym.qualified(),
                services.len()
            );
        }
        None => {
            let _ = writeln!(
                body,
                "{} entrypoint(s) across {} service(s)        [L0-D, exact]",
                rows.len(),
                services.len()
            );
        }
    }

    let mut shown = 0usize;
    let mut unresolved = Vec::new();
    for (i, e) in rows.iter().enumerate() {
        if e.entry_path.is_none() && !e.idle {
            unresolved.push(format!("{} ({})", e.service, e.trigger.label()));
        }
        let lands = match (&e.entry_path, e.idle) {
            (Some(p), _) => p.as_str(),
            // Held open on purpose. Saying so is the answer; calling it unresolved would
            // be reporting a working parse as a failure.
            (None, true) => "(idle - runs nothing at start)",
            // Named, not blank: an empty column reads as "nothing here" when what it
            // means is "this is where reachability stops seeing".
            (None, false) => "-> UNRESOLVED",
        };
        let line = format!(
            "  {:<18} {:<20} {:<42} {}",
            e.trigger.label(),
            truncate(&e.service, 20),
            truncate(e.command.as_deref().unwrap_or("(image default)"), 42),
            lands
        );
        if !budget.push(&mut body, &line) {
            break;
        }
        // The runner script is the evidence for an on-demand chain, and a claim about
        // what a container runs nightly is worth nothing if it cannot be checked.
        if let Some(script) = &e.script {
            if !budget.push(&mut body, &format!("  {:<18} via {script}", "")) {
                break;
            }
        }
        shown = i + 1;
    }
    if rows.is_empty() {
        let _ = writeln!(
            body,
            "  (nothing starts any code here - no compose file resolved, or no service \
             declares a command)"
        );
    }

    let mut env = Envelope::new(body).rows(shown);
    if shown < rows.len() {
        env = env.suppressed(budget.cut_note_narrowable(
            rows.len() - shown,
            "entrypoints",
            "--reaches",
        ));
    }
    if !unresolved.is_empty() {
        // The whole reason to list these: everything reached only from an entrypoint
        // that did not resolve is invisible to `runs`, `affects` and `unreached`, and
        // looks like dead code rather than like an unanswered question.
        env = env.unknown(format!(
            "{} entrypoint(s) did not resolve to code ({}). Anything only these run will \
             look unreachable - `cairn rules` shows the command shapes that are \
             recognised, and .cairn/rules.yaml adds more",
            unresolved.len(),
            unresolved.join(", ")
        ));
    }
    if !blind.is_empty() {
        env = env.unknown(format!(
            "{} service(s) start nothing and were not found in any cron entry ({}). They \
             run code on demand - a management command, `docker exec` - and nothing here \
             or in the index can see what",
            blind.len(),
            blind.join(", ")
        ));
    }
    env = env.unknown(
        "this is what the deployment declares. A process started outside it - a developer \
         shell, a CI job, an orchestrator living in another repository - is not here",
    );
    env
}

/// Which deployed services can reach a symbol.
/// `cairn runs` as data.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct RunsData {
    pub symbol: SymbolJson,
    pub services: Vec<String>,
    /// How the answer was attributed: `direct` is a call path to this symbol, `type` is
    /// the enclosing type (a dispatched method has no static caller), `file` is the
    /// module the symbol sits in (a framework route handler). The weaker two say the
    /// code is loaded there, not that a call path reaches it.
    pub via: &'static str,
    /// The enclosing type the answer was given for, when `via` is `type`.
    pub via_symbol: Option<SymbolJson>,
    /// Call hops the search was allowed.
    pub depth: usize,
    /// Services that start nothing and run code on demand instead.
    pub blind: Vec<String>,
}

pub fn runs_in(
    sym: &SymbolRow,
    services: &[String],
    depth: usize,
    via: &cairn_store::Attribution,
    blind: &[String],
) -> Envelope {
    let mut body = String::new();
    // `exact` is a claim about the call graph and only the direct answer earns it. The
    // two fallbacks are weaker attributions and say so in the header, not only in a note
    // further down that a reader skimming for the number will not reach.
    let strength = match via {
        cairn_store::Attribution::Direct => "[L1 + L0-D, exact]",
        cairn_store::Attribution::ViaType(_) => "[L1 + L0-D, via the enclosing type]",
        cairn_store::Attribution::ViaFile => "[L1 + L0-D, via the file, not a call path]",
    };
    let _ = writeln!(
        body,
        "[{}] {} runs in {} service(s)        {strength}",
        sym.handle,
        sym.qualified(),
        services.len()
    );
    match via {
        cairn_store::Attribution::ViaType(owner) => {
            let _ = writeln!(
                body,
                "  answered for the enclosing type [{}] {}: nothing calls the method \
                 statically, which for a dispatched method means the caller is a table, \
                 not that the code is dead",
                owner.handle,
                owner.qualified()
            );
        }
        cairn_store::Attribution::ViaFile => {
            let _ = writeln!(
                body,
                "  answered for the file this sits in: nothing calls it statically and it \
                 owns no dispatched method, which is the shape of a framework route \
                 handler. This says the module is loaded by that service, not that a call \
                 path reaches this symbol"
            );
        }
        cairn_store::Attribution::Direct => {}
    }
    for s in services {
        let _ = writeln!(body, "  {s}");
    }
    if services.is_empty() {
        let _ = writeln!(body, "  (no service entrypoint reaches it)");
    }
    // No budget here: a service list is short by construction and is printed whole.
    let mut env = Envelope::new(body).rows(services.len()).data(RunsData {
        symbol: SymbolJson::of(sym),
        services: services.to_vec(),
        via: match via {
            cairn_store::Attribution::Direct => "direct",
            cairn_store::Attribution::ViaType(_) => "type",
            cairn_store::Attribution::ViaFile => "file",
        },
        via_symbol: match via {
            cairn_store::Attribution::ViaType(owner) => Some(SymbolJson::of(owner)),
            _ => None,
        },
        depth,
        blind: blind.to_vec(),
    });
    if services.is_empty() {
        env = env.unknown(format!(
            "no service reaches this within {depth} call hops. That can mean dead code, \
             a deeper path, or an entrypoint the topology could not resolve - check \
             `cairn topology` before concluding it is unused"
        ));
    }
    if !blind.is_empty() {
        // Stated on every answer, not only on the empty one: a *non-empty* list is
        // where this misleads, because it looks complete.
        env = env.unknown(format!(
            "this list covers only what a service runs at start-up. {} service(s) start \
             nothing ({}) and run code on demand instead - cron, a management command, \
             `docker exec`. If one of those could invoke this, grep its cron and entrypoint \
             scripts; reachability cannot see them",
            blind.len(),
            blind.join(", ")
        ));
    }
    env
}

/// The whole answer to "what does changing this touch", shaped like the answer.
///
/// Laid out so it can be used as the deliverable rather than as material for one: the
/// in-process services with the command that makes them the runner, then the network
/// route hop by hop with the RPC that carries it. The `unknown:` section names what
/// reachability cannot see, so a caller can tell a complete answer from a partial one
/// without going to check — which is the whole point of answering in one call.
/// A service that runs this code in its own process.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct InProcessJson {
    pub service: String,
    /// The start command that makes it the runner. `null` means the image default.
    pub command: Option<String>,
    /// Attributed through the file the symbol sits in rather than through a call path —
    /// the `~` of the text answer. The module is loaded there; no call path was found.
    pub by_file: bool,
}

/// One network hop into this symbol, as the text groups them: a route, and the RPCs it
/// carries.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct AffectsHopJson {
    /// Services that start the calling side. Empty means nothing starts it.
    pub from: Vec<String>,
    /// Services that serve the called side. Empty means none was resolved.
    pub to: Vec<String>,
    pub package: String,
    pub service: String,
    pub rpcs: Vec<String>,
    /// How many call sites this route was observed at.
    pub call_sites: usize,
    /// File the calls sit in.
    pub file: String,
    pub from_by_file: bool,
}

/// A network call *out* of this symbol: a change here changes what the far side receives.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct OutgoingJson {
    pub served_by: Vec<String>,
    pub package: String,
    pub service: String,
}

/// `cairn affects` as data.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct AffectsData {
    pub symbol: SymbolJson,
    /// Distinct deployed services a change here touches.
    pub services: usize,
    pub in_process: Vec<InProcessJson>,
    pub hops: Vec<AffectsHopJson>,
    pub outgoing: Vec<OutgoingJson>,
    /// Services that start nothing and run code on demand. Where one appears above it is
    /// on a path; how it is triggered is not something reachability can see.
    pub blind: Vec<String>,
    /// The service chain continued past the hop limit.
    pub truncated_hops: bool,
}

pub fn affects(sym: &SymbolRow, a: &cairn_store::Affects, budget: &mut Budget) -> Envelope {
    let mut body = String::new();
    let mut data = AffectsData {
        symbol: SymbolJson::of(sym),
        services: 0,
        in_process: Vec::new(),
        hops: Vec::new(),
        outgoing: Vec::new(),
        blind: a.blind.clone(),
        truncated_hops: a.truncated_hops,
    };
    let services: usize = a.in_process.len()
        + a.hops
            .iter()
            .flat_map(|h| h.from.iter())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
    let _ = writeln!(
        body,
        "[{}] {} — affects {} deployed service(s)        [L1 + L0-D]",
        sym.handle,
        sym.qualified(),
        services
    );

    // Every printed route is a row of this answer, whichever of the three sections it
    // came from: the deliverable is the set of ways a change here travels, not any one
    // section of it.
    let mut rows = 0usize;
    let mut cut = 0usize;

    let _ = writeln!(body, "\nin-process");
    for p in &a.in_process {
        // An on-demand row already carries its trigger and its command in the label, and
        // the container's own start command is not what runs this code.
        let name = if p.by_file {
            format!("{} ~", p.service)
        } else {
            p.service.clone()
        };
        let line = if p.service.contains(" (cron ") || p.service.contains(" (on demand") {
            format!("  {name}")
        } else {
            format!(
                "  {:<20} {}",
                name,
                p.command.as_deref().unwrap_or("(image default)")
            )
        };
        if budget.push(&mut body, &line) {
            rows += 1;
            data.in_process.push(InProcessJson {
                service: p.service.clone(),
                command: p.command.clone(),
                by_file: p.by_file,
            });
        } else {
            cut += 1;
        }
    }
    if a.in_process.is_empty() {
        let _ = writeln!(body, "  (no service entrypoint reaches it)");
    }

    // Grouped by route and service rather than one line per RPC. Ungrouped, a real
    // question produced 70 lines carrying maybe six distinct facts, and the whole reason
    // this command exists is that the answer should cost less than assembling it.
    if !a.hops.is_empty() {
        // Spelled out because an agent read the grouped list as "calls into that service"
        // and discarded two thirds of it as unrelated. Every row is a route to the symbol
        // that was asked about; nothing is listed for context.
        let _ = writeln!(
            body,
            "\nover the network, by hop - every route below reaches this symbol"
        );
        // The file is part of the key, not a property of the group. Keyed without it, a
        // group kept whichever call site came first and printed that path for all of
        // them: ten routes into FolderService listed as one line ending `in folder.go`,
        // when one of them is the share endpoint and lives in share.go. A row of this
        // answer is a place to go and look, so a row that names a file the call is not in
        // is worse than an extra row.
        let mut groups: std::collections::BTreeMap<(String, String, String, String), Vec<String>> =
            std::collections::BTreeMap::new();
        for h in &a.hops {
            let from = if h.from.is_empty() {
                "(starts nothing)".to_string()
            } else if h.from_by_file {
                format!("{} ~", h.from.join(", "))
            } else {
                h.from.join(", ")
            };
            let to = if h.to.is_empty() {
                "?".to_string()
            } else {
                h.to.join(", ")
            };
            let site = h
                .call_site
                .def
                .as_ref()
                .map(|d| d.path.clone())
                .unwrap_or_default();
            groups
                .entry((from, to, format!("{}.{}", h.pkg, h.service), site))
                .or_default()
                .push(h.rpc.clone());
        }
        let group_count = groups.len();
        let mut hops_shown = 0usize;
        // Which groups were attributed by file, keyed the way the groups are.
        let by_file: std::collections::BTreeSet<String> = a
            .hops
            .iter()
            .filter(|h| h.from_by_file)
            .map(|h| format!("{}.{}", h.pkg, h.service))
            .collect();
        for ((from, to, service, site), mut rpcs) in groups {
            // The count is call sites; the list is which RPCs they carry. Two sites on
            // one RPC printed its name twice, which reads as two RPCs.
            let sites = rpcs.len();
            rpcs.sort();
            rpcs.dedup();
            let (package, svc) = service
                .split_once('.')
                .map(|(p, s)| (p.to_string(), s.to_string()))
                .unwrap_or_else(|| (String::new(), service.clone()));
            let as_data = AffectsHopJson {
                // Back to a list: the text joins them for one line, and a consumer that
                // had to split on ", " would be parsing a layout.
                from: from
                    .trim_end_matches(" ~")
                    .split(", ")
                    .filter(|s| !s.is_empty() && *s != "(starts nothing)")
                    .map(|s| s.to_string())
                    .collect(),
                to: to
                    .split(", ")
                    .filter(|s| !s.is_empty() && *s != "?")
                    .map(|s| s.to_string())
                    .collect(),
                package,
                service: svc,
                rpcs: rpcs.clone(),
                call_sites: sites,
                file: site.clone(),
                from_by_file: from.ends_with(" ~") || by_file.contains(&service),
            };
            if !budget.push(
                &mut body,
                &format!(
                    "  {:<18} -> {:<16} {}  ({})\n    {} in {}",
                    from,
                    to,
                    service,
                    sites,
                    rpcs.join(", "),
                    site
                ),
            ) {
                break;
            }
            data.hops.push(as_data);
            hops_shown += 1;
        }
        rows += hops_shown;
        if hops_shown < group_count {
            cut += group_count - hops_shown;
        }
    }

    if !a.outgoing.is_empty() {
        let _ = writeln!(
            body,
            "\ncalls out over the network - a change here changes what these receive"
        );
        let mut out_shown = 0usize;
        for o in &a.outgoing {
            let who = if o.served_by.is_empty() {
                "(no deployed server resolved)".to_string()
            } else {
                o.served_by.join(", ")
            };
            if !budget.push(
                &mut body,
                &format!("  -> {:<22} {}.{}", who, o.pkg, o.service),
            ) {
                break;
            }
            data.outgoing.push(OutgoingJson {
                served_by: o.served_by.clone(),
                package: o.pkg.clone(),
                service: o.service.clone(),
            });
            out_shown += 1;
        }
        rows += out_shown;
        cut += a.outgoing.len() - out_shown;
    }

    data.services = services;
    let mut env = Envelope::new(body).rows(rows).data(data);
    if cut > 0 {
        env = env.suppressed(budget.cut_note(cut, "routes"));
    }
    if !a.blind.is_empty() {
        env = env.unknown(format!(
            "{} service(s) start nothing and run code on demand instead - cron, a \
             management command, `docker exec` ({}). Where one appears above it is on a \
             path; how it is triggered is not something reachability can see",
            a.blind.len(),
            a.blind.join(", ")
        ));
    }
    if a.hops.iter().any(|h| h.from_by_file) || a.in_process.iter().any(|p| p.by_file) {
        env = env.unknown(
            "`~` marks a service attributed through the file the call sits in rather than \
             through a call path: a framework route handler has no static caller, so this \
             says the module is loaded there, not that the handler is reached from the \
             entrypoint",
        );
    }
    if a.truncated_hops {
        env = env.suppressed("the service chain continued past the hop limit".to_string());
    }
    env = env.unknown(
        "hops are calls through a generated gRPC client, and are exact. A service that \
         reaches this some other way - a hand-written transport, a queue, an HTTP call - \
         is not here",
    );
    env = env.unknown(
        "the in-process side follows static calls only. A module-level binding is \
         followed, so `af = wrap(f)` does not break the chain, but a task queue, a \
         registry of callables or a name resolved at run time does - `cairn weaklinks` \
         is where those candidates live",
    );
    env
}

/// One attributed text hit, for `cairn for find`.
pub struct FoundLine {
    pub path: String,
    pub line: usize,
    pub text: String,
    pub context: cairn_store::attribute::LineContext,
    /// Lines either side of the match, numbered.
    pub around: Vec<(usize, String)>,
}

/// A text search, with whose line each hit is.
///
/// The line alone is what `rg` returns in the same 20 ms, so the line alone would make
/// this command a slower ripgrep. Every row therefore carries what the index knows about
/// it — the enclosing function and its handle, the markdown section and its range, the
/// deployed service — and where it knows nothing, it says why rather than leaving a gap
/// the reader has to interpret.
/// The shape of the search behind a `for find` answer, as opposed to its results.
///
/// Four facts that all qualify the same claim - how wide the read was, whether it was cut
/// short, what it could not open, and whether the caller asked for everything - so they
/// travel together rather than as four positional arguments at the end of a call.
pub struct TreeSweep {
    pub files_read: usize,
    pub skipped_large: usize,
    pub truncated: bool,
    pub list_all: bool,
}

pub fn found(
    needle: &str,
    hits: &[FoundLine],
    services: &[String],
    sweep: &TreeSweep,
    budget: &mut Budget,
) -> Envelope {
    let TreeSweep {
        files_read: files,
        skipped_large: skipped,
        truncated,
        list_all,
    } = *sweep;
    let mut body = String::new();
    let files_hit = hits
        .iter()
        .map(|h| &h.path)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let _ = writeln!(
        body,
        "{} line(s) containing \"{needle}\" in {files_hit} file(s), from {files} searched   [L0, the tree as it is now]",
        hits.len()
    );
    if !services.is_empty() {
        // A fact about the search term, so it is said once. Per row it claimed that a
        // README line mentioning the variable belonged to the service, which is not what
        // the lookup established.
        let _ = writeln!(
            body,
            "this text appears in the start command or published ports of: {}",
            services.join(", ")
        );
    }
    if hits.is_empty() {
        let _ = writeln!(
            body,
            "  (nothing in the working tree contains that, in any file type)"
        );
    }

    // A long listing is not an answer; it is the work of reading one, handed back. Measured
    // on the target repository, 8 of 50 name searches return 20 lines or more, and in the
    // five largest, hand-written non-test code was 5 lines of 355 — the rest was tests and
    // generated code. Both are things the index already distinguishes, so the count can be
    // stated and the lines left out, which is a smaller answer and not a weaker one: the
    // header still counts every hit and `suppressed:` names what it held back.
    const CLASSIFY_AT: usize = 20;
    let tests = hits.iter().filter(|h| h.context.is_test).count();
    let generated = hits.iter().filter(|h| h.context.generated).count();
    let docs = hits.iter().filter(|h| h.context.section.is_some()).count();
    let derived = |h: &FoundLine| h.context.is_test || h.context.generated;
    // The last clause is not a refinement, it is the fix for a case that reduced a real
    // answer to nothing: `chatWithFilter` is *defined* in a test file, so every one of its
    // 28 hits was derived and the collapsed body was empty. A count with an empty body is
    // not a smaller answer, it is a missing one — so a collapse that would leave nothing
    // to read does not happen, and the caller gets the lines.
    let collapse = !list_all
        && hits.len() >= CLASSIFY_AT
        && hits.iter().any(derived)
        && hits.iter().any(|h| !derived(h));
    let listed: Vec<&FoundLine> = if collapse {
        hits.iter().filter(|h| !derived(h)).collect()
    } else {
        hits.iter().collect()
    };
    if collapse {
        let _ = writeln!(
            body,
            "of these: {} in hand-written code or prose (below), {tests} in tests, \
             {generated} in generated code{}",
            listed.len(),
            if docs > 0 {
                format!("; {docs} of the listed lines are markdown prose, not code")
            } else {
                String::new()
            }
        );
    }

    let mut shown = 0usize;
    let mut last_path = String::new();
    // Every line some hit will print as its own match, so context never steals one, and
    // the match lines of each file in order so a window can be clamped to its neighbours.
    let matched: HashSet<(&str, usize)> =
        listed.iter().map(|h| (h.path.as_str(), h.line)).collect();
    let mut match_lines: std::collections::HashMap<&str, Vec<usize>> =
        std::collections::HashMap::new();
    for h in listed.iter().copied() {
        match_lines.entry(h.path.as_str()).or_default().push(h.line);
    }
    for v in match_lines.values_mut() {
        v.sort_unstable();
    }
    let mut printed: HashSet<(String, usize)> = HashSet::new();
    for h in listed.iter().copied() {
        if h.path != last_path {
            // The file header carries the flags, because "this hit is in generated code"
            // changes what the hit means and is cheaper said once per file.
            let mut tags = Vec::new();
            if h.context.generated {
                tags.push("generated");
            }
            if h.context.is_test {
                tags.push("test");
            }
            // Only worth a tag where the absence is news: a code file the index has not
            // got is a gap, a `.yaml` never could be. Said per file it was ten identical
            // lines of noise; the general case belongs in the envelope.
            //
            // From the shared mapping, not two extensions written here. It was `.py` or
            // `.go`, so on the frontend every hit in the one app that fails to index was
            // shown untagged - identical to a hit in a file the index knows - and the
            // reader had no way to learn that no symbol command can see that code.
            // Three states, not two. "Missed" tells the reader to go and look; "outside
            // the indexed projects" tells them there is nothing to look at, which is the
            // ordinary case for a retired app a workspace excludes on purpose. Saying the
            // first about the second was measured in a run: it sends a reader after code
            // that was never meant to be there.
            let code = Lang::is_code_path(&h.path);
            if code && !h.context.indexed {
                tags.push(match h.context.inside_indexed_project {
                    Some(true) => "not in the index - a code file the last index run missed",
                    Some(false) => "not in the index - outside the indexed projects",
                    None => "not in the index",
                });
            }
            let tag = if tags.is_empty() {
                String::new()
            } else {
                format!("   [{}]", tags.join(", "))
            };
            if !budget.push(&mut body, &format!("{}{tag}", h.path)) {
                break;
            }
            last_path = h.path.clone();
        }
        // The attribution, then the line. Whose line it is is the reason to prefer this
        // over grep, so it goes first.
        let mut whose = Vec::new();
        if let Some((name, handle)) = &h.context.symbol {
            whose.push(format!("in {name} [{handle}]"));
        }
        if let Some((trail, start, end)) = &h.context.section {
            whose.push(format!("in section {trail} ({start}-{end})"));
        }
        let whose = if whose.is_empty() {
            String::new()
        } else {
            format!("  <- {}", whose.join(", "))
        };
        // Context above, the match, context below — the match marked so the eye finds it
        // without counting lines.
        //
        // Each hit used to print its own window with no memory of the last one, so hits a
        // line or two apart repeated everything between them. Measured on the scenario
        // this command wins: `for find "openLabel"` printed 70 context lines carrying 33
        // distinct ones — **52 % of the answer was the same lines again**, and one line of
        // a translation file appeared five times. A translation file is precisely where
        // matches cluster, so the waste is worst exactly where this command is used.
        //
        // A line already printed is not printed again, and a line that is some other hit's
        // match is left for that hit, which prints it with `>` and its attribution. Every
        // hit still appears; what goes is only the repetition between them.
        // Clamped to the neighbouring matches, or the output stops being in line order:
        // skipping a line because some other hit will print it lets the line *after* it
        // arrive first, and `6` printed before `5` reads as a rendering bug even though
        // every line is present exactly once.
        let (prev, next) = match match_lines.get(h.path.as_str()) {
            Some(v) => (
                v.iter().copied().rfind(|m| *m < h.line),
                v.iter().copied().find(|m| *m > h.line),
            ),
            None => (None, None),
        };
        for (n, line) in h
            .around
            .iter()
            .filter(|(n, _)| *n < h.line && prev.is_none_or(|p| *n > p))
        {
            if matched.contains(&(h.path.as_str(), *n)) || !printed.insert((h.path.clone(), *n)) {
                continue;
            }
            let _ = budget.push(&mut body, &format!("  {n:>5} | {}", line.trim_end()));
        }
        if !budget.push(
            &mut body,
            &format!("  {:>5} > {}{whose}", h.line, h.text.trim()),
        ) {
            break;
        }
        printed.insert((h.path.clone(), h.line));
        for (n, line) in h
            .around
            .iter()
            .filter(|(n, _)| *n > h.line && next.is_none_or(|x| *n < x))
        {
            if matched.contains(&(h.path.as_str(), *n)) || !printed.insert((h.path.clone(), *n)) {
                continue;
            }
            let _ = budget.push(&mut body, &format!("  {n:>5} | {}", line.trim_end()));
        }
        shown += 1;
    }

    let mut env = Envelope::new(body).rows(shown);
    // Two different reasons for a line not to be here, and they must not be conflated: the
    // budget ran out, or the line was classified as derived and deliberately withheld. The
    // second is recoverable by a flag and the first is not, so the caller is told which.
    if collapse {
        env = env.suppressed(format!(
            "{} line(s) in test or generated files, counted in the header but not listed \
             - `cairn for find \"{needle}\" --all` lists every hit",
            hits.len() - listed.len()
        ));
    }
    if shown < listed.len() {
        env = env.suppressed(budget.cut_note(listed.len() - shown, "lines"));
    }
    if truncated {
        env = env.unknown(
            "the search stopped at its limit, so this is not the whole tree. Narrow the \
             text or raise --limit",
        );
    }
    if skipped > 0 {
        env = env.unknown(format!(
            "{skipped} file(s) over 2 MB were not read. A match inside one would not be here"
        ));
    }
    env = env.unknown(
        "substring, case-insensitive, over the working tree - so it is never stale, and \
         it is not a regex. Symbol context is attached only where the file is indexed \
         (Python and Go); section context only for markdown",
    );
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_truncated_set_is_never_announced_as_all_of_them() {
        // Reported from a real audit: 52 references cut to 40 by --limit, the survivors
        // shared a file, and cairn said ALL of them were in it and advised against looking
        // further. The ones it dropped were the production callers.
        let same_file = ["a/b.py", "a/b.py", "a/b.py"];
        assert!(
            concentration_note(&same_file, false, "references").is_none(),
            "claimed to know where every reference is, from a partial list"
        );
        assert!(
            concentration_note(&same_file, true, "references").is_some(),
            "a complete set that really is in one file should still say so"
        );
    }

    #[test]
    fn the_claim_needs_one_file_as_well_as_completeness() {
        let spread = ["a/b.py", "a/c.py"];
        assert!(concentration_note(&spread, true, "references").is_none());
        assert!(concentration_note(&[], true, "references").is_none());
    }

    #[test]
    fn envelope_always_has_all_sections() {
        let out = Envelope::new("body".into()).render();
        assert!(out.contains("suppressed: none"));
        assert!(out.contains("unknown: none"));
        assert!(out.contains("stale: none"));
    }

    /// A property on a type, in each language, so the note that fires can be read.
    fn attribute_on_a_type(lang: cairn_store::Lang) -> SymbolRow {
        SymbolRow {
            id: 1,
            handle: "abc".into(),
            scip: None,
            name: "email".into(),
            container: Some("Credentials#".into()),
            module: None,
            kind: cairn_store::SymbolKind::Term,
            lang,
            def: None,
            def_end_line: None,
            ref_count: 1,
        }
    }

    #[test]
    fn the_grep_that_is_recommended_can_match_the_language_it_is_recommended_for() {
        // It could not. The note was language-blind, so a TypeScript property was handed
        // the Django explanation and `--include='*.py'` - a command that cannot match the
        // file the symbol is defined in, printed in capitals above a list that was
        // probably complete.
        let py = routing_note(&attribute_on_a_type(cairn_store::Lang::Python))
            .expect("the measured case still routes");
        assert!(py.contains("--include='*.py'"));

        assert!(
            routing_note(&attribute_on_a_type(cairn_store::Lang::TypeScript)).is_none(),
            "nothing has been measured about TypeScript under-reporting, and USE GREP FOR \
             THIS ONE over a complete list is the same overstatement pointing the other way"
        );
        assert!(routing_note(&attribute_on_a_type(cairn_store::Lang::Go)).is_none());
    }

    #[test]
    fn the_caveat_names_the_shapes_that_lose_the_type_in_this_language() {
        // The caveat stays for every language - the mechanism is the same - but naming
        // ORM models and dicts-as-records to a TypeScript reader describes a language
        // they are not in.
        let ts = attribute_caveat(&attribute_on_a_type(cairn_store::Lang::TypeScript))
            .expect("a property on a type is still under-resolved sometimes");
        assert!(ts.contains("any"), "{ts}");
        assert!(
            !ts.contains("ORM"),
            "Python's shapes, in a TypeScript answer: {ts}"
        );

        let py = attribute_caveat(&attribute_on_a_type(cairn_store::Lang::Python)).unwrap();
        assert!(py.contains("ORM"), "{py}");
    }

    #[test]
    fn a_definition_is_printed_where_an_editor_would_open_it() {
        // SCIP counts lines from 0 and this binary prints from 1 everywhere else, so a
        // renderer that formats `path:line` by hand is off by one against every other
        // command. `reaches X --outgoing` was: it named a handler one line above its own
        // `func` keyword, while `reaches X` on the same symbol named it correctly. The
        // two directions of one command disagreed about one fact.
        let sym = cairn_store::SymbolRow {
            id: 1,
            handle: "abc".into(),
            scip: None,
            name: "GetReading".into(),
            container: Some("stationGateway#".into()),
            module: None,
            kind: cairn_store::SymbolKind::Method,
            lang: cairn_store::Lang::Go,
            def: Some(cairn_store::Occurrence {
                path: "srcgo/gateway/station.go".into(),
                line: 32,
                col_start: 0,
                col_end: 0,
                role: 1,
                generated: false,
                gen_via: cairn_store::GeneratedVia::No,
                enclosing: None,
                enclosing_id: None,
            }),
            def_end_line: None,
            ref_count: 0,
        };
        let target = cairn_store::RpcCaller {
            pkg: "station_core".into(),
            service: "StationService".into(),
            rpc: "GetReading".into(),
            symbol: sym.clone(),
        };
        let mut b = Budget::unlimited();
        let out = rpc_targets(&sym, std::slice::from_ref(&target), &[], true, &mut b).render();
        assert!(
            out.contains("srcgo/gateway/station.go:33"),
            "the 0-based SCIP line reached the page: {out}"
        );
    }

    #[test]
    fn truncation_marks_itself() {
        assert_eq!(truncate("abcdef", 4), "abc~");
        assert_eq!(truncate("abc", 4), "abc");
    }

    #[test]
    fn what_the_answer_left_out_and_what_it_reports_leaving_out_are_the_same_thing() {
        let clean = Envelope::new("body".into());
        assert!(!clean.truncated(), "nothing suppressed is nothing cut");
        let cut = Envelope::new("body".into()).suppressed("3 more nodes");
        assert!(
            cut.truncated(),
            "an answer that printed a suppressed: line has to record itself as truncated, \
             or the session log and the answer disagree about the same query"
        );
    }

    #[test]
    fn rows_counts_what_arrived_not_what_matched() {
        // The distinction is the whole reason the field exists: a log saying 40 rows for
        // an answer that carried 2 cannot explain why the caller immediately asked again.
        let services: Vec<cairn_store::DeployServiceRow> = (0..12)
            .map(|i| {
                (
                    format!("service-{i}"),
                    Some(format!("python -m service_{i}.main --serve")),
                    Some(format!("srcpy/service_{i}/main.py")),
                    String::new(),
                    true,
                )
            })
            .collect();

        let mut unbounded = Budget::unlimited();
        let whole = topology(&services, &mut unbounded);
        assert_eq!(whole.rows, Some(12));
        assert!(!whole.truncated(), "nothing was left out of this one");

        let mut tight = Budget::tokens(30);
        let cut = topology(&services, &mut tight);
        let carried = cut.rows.expect("a list answer reports its length");
        assert!(
            carried < services.len(),
            "the budget was too small for twelve rows, so fewer should be reported"
        );
        assert_eq!(
            carried,
            cut.body
                .lines()
                .filter(|l| l.trim_start().starts_with("service-"))
                .count(),
            "the count has to match the rows actually in the body"
        );
        assert!(cut.truncated(), "a cut list says so");
    }

    #[test]
    fn a_list_with_nothing_in_it_is_not_the_same_as_no_list() {
        let mut budget = Budget::unlimited();
        let empty = topology(&[], &mut budget);
        assert_eq!(
            empty.rows,
            Some(0),
            "`topology` produces rows; none found is a fact worth logging"
        );
        // A report is not a list and has no honest row count to give. `verify` builds
        // its envelope this way and leaves the field alone.
        assert_eq!(Envelope::new("report".into()).rows, None);
    }
}

#[cfg(test)]
mod guarantees {
    //! The guarantees a mutation audit found nothing defending.
    //!
    //! Ten realistic regressions were applied to the source and the whole suite re-run;
    //! three were caught. These pin the ones that were not. Each is a pure function of its
    //! input, so it is checked here rather than through a corpus case that would need the
    //! fixture to grow a file for every branch.
    use super::*;
    use cairn_store::attribute::LineContext;

    fn line(path: &str, generated: bool, is_test: bool) -> FoundLine {
        FoundLine {
            path: path.into(),
            line: 1,
            text: "needle".into(),
            context: LineContext {
                symbol: None,
                section: None,
                generated,
                is_test,
                indexed: true,
                inside_indexed_project: None,
            },
            around: Vec::new(),
        }
    }

    /// A hit with its own line number and a window of context around it.
    fn hit_at(path: &str, n: usize, text: &str, around: &[(usize, &str)]) -> FoundLine {
        FoundLine {
            path: path.into(),
            line: n,
            text: text.into(),
            context: LineContext {
                symbol: None,
                section: None,
                generated: false,
                is_test: false,
                indexed: true,
                inside_indexed_project: None,
            },
            around: around.iter().map(|(i, l)| (*i, l.to_string())).collect(),
        }
    }

    #[test]
    fn clustered_hits_do_not_print_the_lines_between_them_twice() {
        // Every hit printed its own window with no memory of the last, so hits a line or
        // two apart repeated everything between them. Measured on the scenario this
        // command wins - a translation key, where matches cluster by construction -
        // 70 context lines carried 33 distinct ones and one line appeared five times.
        let around = |n: usize| {
            vec![
                (n - 1, "line above"),
                (n + 1, "line below"),
                (n + 2, "two below"),
            ]
        };
        let hits = vec![
            hit_at("locales/cs.json", 3, "\"openLabel\": \"a\"", &around(3)),
            hit_at(
                "locales/cs.json",
                4,
                "\"openLabelUnread\": \"b\"",
                &around(4),
            ),
            hit_at(
                "locales/cs.json",
                5,
                "\"openLabelUnread_one\": \"c\"",
                &around(5),
            ),
        ];
        let out = render(&hits, false);

        // The body rows only: the header also starts with a count, and counting it as a
        // line number made the first version of this test fail for the wrong reason.
        let numbered: Vec<&str> = out
            .lines()
            .filter(|l| l.starts_with("  ") && (l.contains(" | ") || l.contains(" > ")))
            .collect();
        let mut seen = std::collections::HashSet::new();
        let mut last = 0usize;
        for l in &numbered {
            let n: usize = l.split_whitespace().next().unwrap().parse().unwrap();
            assert!(seen.insert(n), "line {n} printed twice:\n{out}");
            // In order, too. Dropping a line because another hit will print it is only
            // correct if what remains still reads top to bottom.
            assert!(n > last, "line {n} came after {last}:\n{out}");
            last = n;
        }

        // And every hit is still there, marked as a match rather than swallowed into
        // some neighbour's context - which is the way a dedup could quietly lose one.
        for n in ["3", "4", "5"] {
            assert!(
                numbered
                    .iter()
                    .any(|l| l.split_whitespace().next() == Some(n) && l.contains('>')),
                "the hit on line {n} must still be marked:\n{out}"
            );
        }
    }

    fn sweep() -> TreeSweep {
        TreeSweep {
            files_read: 10,
            skipped_large: 0,
            truncated: false,
            list_all: false,
        }
    }

    fn render(hits: &[FoundLine], list_all: bool) -> String {
        let mut b = Budget::unlimited();
        found(
            "needle",
            hits,
            &[],
            &TreeSweep {
                list_all,
                ..sweep()
            },
            &mut b,
        )
        .render()
    }

    #[test]
    fn lines_withheld_from_a_large_answer_are_counted_in_the_envelope() {
        // Withholding without declaring it is the failure the envelope exists to prevent:
        // a body that reads as the whole answer while a third of it is missing.
        let mut hits: Vec<_> = (0..15)
            .map(|_| line("srcgo/a_test.go", false, true))
            .collect();
        hits.extend((0..10).map(|_| line("srcpy/a.py", false, false)));
        let out = render(&hits, false);
        assert!(out.contains("of these:"), "no classification was printed");
        assert!(
            out.contains("15 line(s) in test or generated files"),
            "held back 15 lines without saying how many:\n{out}"
        );
        assert!(
            !out.contains("suppressed: none"),
            "withheld silently:\n{out}"
        );
    }

    #[test]
    fn a_small_answer_is_listed_rather_than_classified() {
        // The threshold is the whole reason this is an improvement and not noise: the
        // median text search on the target repository returns two lines, and a two-line
        // answer summarised into buckets is strictly worse than the two lines.
        let mut hits: Vec<_> = (0..3)
            .map(|_| line("srcgo/a_test.go", false, true))
            .collect();
        hits.extend((0..2).map(|_| line("srcpy/a.py", false, false)));
        let out = render(&hits, false);
        assert!(
            !out.contains("of these:"),
            "classified a 5-line answer:\n{out}"
        );
    }

    #[test]
    fn a_symbol_that_lives_in_tests_is_still_answered() {
        // `chatWithFilter` is *defined* in a test file, so every one of its 28 hits is
        // derived and the classified body had nothing in it at all. A count with nothing
        // to read is a missing answer, not a smaller one.
        let hits: Vec<_> = (0..25)
            .map(|_| line("srcgo/a_test.go", false, true))
            .collect();
        let out = render(&hits, false);
        assert!(
            out.contains("srcgo/a_test.go"),
            "every hit was withheld and the body is empty:\n{out}"
        );
    }

    #[test]
    fn asking_for_everything_lists_everything() {
        let mut hits: Vec<_> = (0..15)
            .map(|_| line("srcgo/a_test.go", false, true))
            .collect();
        hits.extend((0..10).map(|_| line("srcpy/a.py", false, false)));
        let all = render(&hits, true);
        assert!(!all.contains("of these:"));
        assert!(
            all.contains("srcgo/a_test.go"),
            "--all dropped the test hits"
        );
    }

    #[test]
    fn an_answer_about_edited_files_says_so() {
        // `mark_stale` going quiet is invisible: every field still prints, `stale: none` is
        // a valid-looking answer, and the caller reads an index older than their own edit
        // as current. Nothing in the suite noticed when it was turned into a no-op.
        let dirty = ["srcpy/a.py".to_string()];
        let fresh = Envelope::new("body".into())
            .mark_stale(Some(&dirty), &["srcpy/b.py".to_string()])
            .render();
        assert!(fresh.contains("stale: none"), "unedited file marked stale");
        let edited = Envelope::new("body".into())
            .mark_stale(Some(&dirty), &["srcpy/a.py".to_string()])
            .render();
        assert!(
            !edited.contains("stale: none"),
            "an answer quoting an edited file reported itself current:\n{edited}"
        );
    }

    #[test]
    fn a_result_set_that_is_entirely_generated_says_which_kind_of_nothing_that_is() {
        // Matches that are all generated mean the caller's name exists only in code nobody
        // wrote, which is a different answer from "here it is" and usually means the thing
        // they meant is not indexed under that name.
        let gen = |name: &str| SymbolRow {
            id: 1,
            handle: "h".into(),
            scip: None,
            name: name.into(),
            container: None,
            module: None,
            kind: cairn_store::SymbolKind::Type,
            lang: cairn_store::Lang::Go,
            def: Some(cairn_store::Occurrence {
                path: "srcgo/x.pb.go".into(),
                line: 1,
                col_start: 0,
                col_end: 5,
                role: 0,
                generated: true,
                gen_via: cairn_store::GeneratedVia::PathPattern,
                enclosing: None,
                enclosing_id: None,
            }),
            def_end_line: None,
            ref_count: 0,
        };
        let mut b = Budget::unlimited();
        let out = symbols(
            &[gen("Alert")],
            "Alert",
            "indexed: srcgo/",
            true,
            None,
            &mut b,
        )
        .render();
        assert!(
            out.contains("every match is in generated code"),
            "a wholly generated result set read as an ordinary one:\n{out}"
        );
    }
}

#[cfg(test)]
mod unreached_header {
    use super::*;

    #[test]
    fn a_header_with_rows_withheld_does_not_claim_everything_is_called() {
        // The fix for one false claim produced another: with the platform-variant rows
        // removed the body said "(everything here has a production caller)" while the
        // envelope two lines below said one symbol had not been checked. A tool that
        // contradicts itself inside one answer is worse than one that overclaims once.
        let mut b = Budget::unlimited();
        let clean = unreached("hooks", &[], 0, &mut b).render();
        assert!(clean.contains("everything here has a production caller"));
        let mut b = Budget::unlimited();
        let withheld = unreached("hooks", &[], 1, &mut b).render();
        assert!(
            !withheld.contains("everything here has a production caller"),
            "claimed full coverage while withholding a row:\n{withheld}"
        );
        assert!(withheld.contains("could not be checked"));
    }
}

#[cfg(test)]
mod exit_agrees_with_text {
    use super::*;

    #[test]
    fn an_envelope_that_says_unchecked_reports_itself_as_such() {
        // The page and the exit code are two answers to one question and they disagreed:
        // `for understand` printed two UNCHECKED lines and exited 0, so a caller branching
        // on the code rather than reading the text saw a clean answer. Every command that
        // degrades now asks this, so the wording and the code cannot drift apart.
        assert!(!Envelope::new("b".into()).has_unchecked());
        assert!(!Envelope::new("b".into())
            .unknown("the search stopped at its limit")
            .has_unchecked());
        let gap = cairn_store::LayerCounts::unchecked(0, "service links", "Reindex.")
            .expect("an empty layer yields a note");
        assert!(Envelope::new("b".into()).unknown(gap).has_unchecked());
    }
}
