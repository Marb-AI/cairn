//! SCIP index loading and symbol parsing.
//!
//! SCIP is the fact schema for layer L0 (architecture section 4). Types are generated
//! from the vendored `proto/scip.proto`; on top of that this crate provides a parser
//! for SCIP symbol strings, which are the stable, position-independent identifiers the
//! whole design depends on (D4).

use anyhow::{Context, Result};
use prost::Message;
use std::path::Path;

pub mod proto {
    // prost's output is not ours to lint. The doc comments come straight from the
    // vendored `scip.proto`, and their indentation is upstream's business.
    #![allow(clippy::doc_overindented_list_items)]

    include!(concat!(env!("OUT_DIR"), "/scip.rs"));
}

pub use proto::{Document, Index, Occurrence, SymbolInformation};

/// Occurrence role bit for a definition (`SymbolRole::Definition`).
pub const ROLE_DEFINITION: i32 = 0x1;

pub fn load(path: &Path) -> Result<Index> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading SCIP index {}", path.display()))?;
    Index::decode(&*bytes).with_context(|| format!("decoding SCIP index {}", path.display()))
}

/// What a descriptor's suffix says the symbol is.
///
/// Kept deliberately coarse: enough to render `class`/`method`/`field` in output and
/// to rank results, not a full type system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Namespace,
    Type,
    Term,
    Method,
    TypeParameter,
    Parameter,
    Meta,
    Local,
    Unknown,
}

impl SymbolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SymbolKind::Namespace => "module",
            SymbolKind::Type => "type",
            SymbolKind::Term => "value",
            SymbolKind::Method => "fn",
            SymbolKind::TypeParameter => "typaram",
            SymbolKind::Parameter => "param",
            SymbolKind::Meta => "meta",
            SymbolKind::Local => "local",
            SymbolKind::Unknown => "?",
        }
    }
}

/// A parsed SCIP symbol string.
///
/// Grammar (scip.proto, `Symbol`):
///   `<scheme> ' ' <manager> ' ' <package> ' ' <version> ' ' <descriptor>+`
/// Names may be backtick-quoted when they contain spaces or punctuation, which is
/// common for Python module paths (`` `domains.orders.grpc` ``).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSymbol<'a> {
    pub scheme: &'a str,
    pub manager: &'a str,
    pub package: &'a str,
    pub version: &'a str,
    /// Raw descriptor text, still containing suffixes.
    pub descriptors: &'a str,
    pub is_local: bool,
}

impl<'a> ParsedSymbol<'a> {
    pub fn parse(symbol: &'a str) -> Option<Self> {
        if let Some(rest) = symbol.strip_prefix("local ") {
            return Some(ParsedSymbol {
                scheme: "local",
                manager: "",
                package: "",
                version: "",
                descriptors: rest,
                is_local: true,
            });
        }
        let mut parts = SpaceSplit::new(symbol);
        let scheme = parts.next()?;
        let manager = parts.next()?;
        let package = parts.next()?;
        let version = parts.next()?;
        let descriptors = parts.rest();
        Some(ParsedSymbol {
            scheme,
            manager,
            package,
            version,
            descriptors,
            is_local: false,
        })
    }

    /// Human-facing name: the last descriptor, without its suffix.
    ///
    /// `` `domains.orders.grpc.handlers.auth`/AuthServiceHandler#Verify(). `` -> `Verify`
    pub fn display_name(&self) -> &'a str {
        match self.last_descriptor() {
            Some((name, _)) => name,
            None => self.descriptors,
        }
    }

    pub fn kind(&self) -> SymbolKind {
        if self.is_local {
            return SymbolKind::Local;
        }
        self.last_descriptor()
            .map(|(_, k)| k)
            .unwrap_or(SymbolKind::Unknown)
    }

    /// The container chain without the final element, e.g. the class of a method.
    /// Returns the raw descriptor prefix so callers can render `Class.method`.
    pub fn container(&self) -> Option<&'a str> {
        let (start, _) = self.last_descriptor_span()?;
        if start == 0 {
            return None;
        }
        Some(&self.descriptors[..start])
    }

    /// Module path of the symbol: the leading namespace descriptors, unquoted.
    pub fn module(&self) -> &'a str {
        match self.descriptors.find('/') {
            Some(i) => self.descriptors[..i].trim_matches('`'),
            None => "",
        }
    }

    fn last_descriptor(&self) -> Option<(&'a str, SymbolKind)> {
        let (start, kind) = self.last_descriptor_span()?;
        let raw = &self.descriptors[start..];
        Some((strip_suffix_and_quotes(raw, kind), kind))
    }

    /// Byte offset where the final descriptor begins, plus its kind.
    fn last_descriptor_span(&self) -> Option<(usize, SymbolKind)> {
        let d = self.descriptors;
        if d.is_empty() {
            return None;
        }
        let kind = classify_suffix(d)?;
        // Walk backwards to the separator that ends the previous descriptor,
        // skipping anything inside backticks or a method's parameter list.
        let bytes = d.as_bytes();
        let mut i = trim_end_of_suffix(d, kind);
        let mut in_quotes = false;
        let mut depth = 0usize;
        while i > 0 {
            let c = bytes[i - 1];
            match c {
                b'`' => in_quotes = !in_quotes,
                b')' if !in_quotes => depth += 1,
                b'(' if !in_quotes => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                b'/' | b'#' | b'.' if !in_quotes && depth == 0 => return Some((i, kind)),
                _ => {}
            }
            i -= 1;
        }
        Some((i, kind))
    }
}

fn classify_suffix(d: &str) -> Option<SymbolKind> {
    let b = d.as_bytes();
    let last = *b.last()?;
    Some(match last {
        b'/' => SymbolKind::Namespace,
        b'#' => SymbolKind::Type,
        b')' => SymbolKind::Parameter,
        b']' => SymbolKind::TypeParameter,
        b'!' => SymbolKind::Meta,
        b'.' => {
            if d.ends_with(").") {
                SymbolKind::Method
            } else {
                SymbolKind::Term
            }
        }
        _ => SymbolKind::Unknown,
    })
}

/// Offset just past the descriptor's *name*, i.e. where its suffix starts.
fn trim_end_of_suffix(d: &str, kind: SymbolKind) -> usize {
    let n = d.len();
    match kind {
        SymbolKind::Method => {
            // `name(disambiguator).` - walk back over the parenthesised part.
            let bytes = d.as_bytes();
            let mut i = n.saturating_sub(2); // skip ")."
            while i > 0 && bytes[i - 1] != b'(' {
                i -= 1;
            }
            i.saturating_sub(1)
        }
        SymbolKind::Parameter | SymbolKind::TypeParameter => n.saturating_sub(1),
        _ => n.saturating_sub(1),
    }
}

fn strip_suffix_and_quotes(raw: &str, kind: SymbolKind) -> &str {
    let end = trim_end_of_suffix(raw, kind);
    let name = &raw[..end.min(raw.len())];
    let name = name.trim_start_matches(['/', '#', '.']);
    name.trim_matches('`')
}

/// Splits on spaces, but treats backtick-quoted runs as opaque.
struct SpaceSplit<'a> {
    s: &'a str,
    pos: usize,
}

impl<'a> SpaceSplit<'a> {
    fn new(s: &'a str) -> Self {
        SpaceSplit { s, pos: 0 }
    }
    fn rest(&self) -> &'a str {
        &self.s[self.pos.min(self.s.len())..]
    }
}

impl<'a> Iterator for SpaceSplit<'a> {
    type Item = &'a str;
    fn next(&mut self) -> Option<&'a str> {
        let bytes = self.s.as_bytes();
        if self.pos >= bytes.len() {
            return None;
        }
        let start = self.pos;
        let mut in_quotes = false;
        let mut i = start;
        while i < bytes.len() {
            match bytes[i] {
                b'`' => in_quotes = !in_quotes,
                b' ' if !in_quotes => break,
                _ => {}
            }
            i += 1;
        }
        self.pos = i + 1;
        Some(&self.s[start..i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> ParsedSymbol<'_> {
        ParsedSymbol::parse(s).expect("parses")
    }

    #[test]
    fn parses_python_method() {
        let s = p("scip-python python orders_api spike \
                   `domains.orders.grpc.handlers.auth`/AuthServiceHandler#verify().");
        assert_eq!(s.scheme, "scip-python");
        assert_eq!(s.package, "orders_api");
        assert_eq!(s.display_name(), "verify");
        assert_eq!(s.kind(), SymbolKind::Method);
        assert_eq!(s.module(), "domains.orders.grpc.handlers.auth");
        assert_eq!(
            s.container(),
            Some("`domains.orders.grpc.handlers.auth`/AuthServiceHandler#")
        );
    }

    #[test]
    fn parses_python_class() {
        let s = p("scip-python python p v `domains.orders.rds.models`/Order#");
        assert_eq!(s.display_name(), "Order");
        assert_eq!(s.kind(), SymbolKind::Type);
        assert_eq!(s.module(), "domains.orders.rds.models");
    }

    #[test]
    fn parses_go_symbol() {
        let s = p("scip-go gomod github.com/example/x/srcgo v0 \
                   `github.com/example/x/srcgo/domains/orders`/NewHandler().");
        assert_eq!(s.scheme, "scip-go");
        assert_eq!(s.package, "github.com/example/x/srcgo");
        assert_eq!(s.display_name(), "NewHandler");
        assert_eq!(s.kind(), SymbolKind::Method);
    }

    #[test]
    fn parses_local() {
        let s = p("local 42");
        assert!(s.is_local);
        assert_eq!(s.kind(), SymbolKind::Local);
    }

    #[test]
    fn parses_term_not_method() {
        let s = p("scip-python python p v `mod`/CONSTANT.");
        assert_eq!(s.display_name(), "CONSTANT");
        assert_eq!(s.kind(), SymbolKind::Term);
    }

    #[test]
    fn namespace_has_no_container() {
        let s = p("scip-python python p v `domains`/");
        assert_eq!(s.kind(), SymbolKind::Namespace);
        assert_eq!(s.container(), None);
        assert_eq!(s.display_name(), "domains");
    }
}
