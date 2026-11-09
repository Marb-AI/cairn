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

/// Where a Rust impl method's descriptor names the type, the trait if there is one, and
/// the method's own descriptor with its suffix.
///
/// Every method inside an `impl` block gets this `impl#[..]` wrapper - checked against
/// ripgrep's real `Cargo.toml` workspace, not just a hand-written fixture: inherent
/// methods (`impl Match { fn new() {} }`) are `impl#[Match]new().`, one bracket, same as
/// `impl#[Square][Shape]area().` is a trait impl's, two. Only a method's own declaration
/// inside the trait definition (`Matcher#find_at().`) skips the wrapper entirely - there
/// is no impl block to disambiguate there.
///
/// `impl#[Square][Shape]area().` -> (`"Square"`, `Some("Shape")`, `"area()."`)
/// `impl#[Match]new().` -> (`"Match"`, `None`, `"new()."`)
fn rust_impl_parts(descriptors: &str) -> Option<(usize, &str, Option<&str>, &str)> {
    let marker = "impl#[";
    let start = descriptors.rfind(marker)?;
    let rest = &descriptors[start + marker.len()..];
    let (type_name, rest) = rest.split_once(']')?;
    if type_name.is_empty() {
        return None;
    }
    match rest.strip_prefix('[') {
        Some(rest) => {
            let (trait_name, method_part) = rest.split_once(']')?;
            if trait_name.is_empty() || method_part.is_empty() {
                return None;
            }
            Some((start, type_name, Some(trait_name), method_part))
        }
        None if !rest.is_empty() => Some((start, type_name, None, rest)),
        None => None,
    }
}

/// The trait method a Rust trait-impl method symbol satisfies, derived from the symbol
/// string itself rather than from a SCIP relationship. `None` for an inherent impl -
/// there is no trait for it to satisfy, so there is nothing to synthesise.
///
/// `rust-analyzer scip` emits no relationships at all: every `SymbolInformation.
/// relationships` in its output is empty, trait impls included - checked against a
/// `dyn Trait`-dispatched fixture on both the 1.82 stable release and the 2026-11-07
/// nightly, at the raw protobuf level, not just the JSON rendering. Go and Python get
/// `EdgeKind::Implements` from a real relationship (`ingest.rs`); Rust has to get the
/// same fact a different way, and the symbol string already carries it via
/// `rust_impl_parts`. This turns that naming convention back into the same fact, so the
/// one survey query that excludes dispatched methods from `unreached`/`outline`
/// (`survey.rs`) needs no Rust-specific case at all.
///
/// `impl#[Square][Shape]area().` -> `Shape#area().`, in the same package and module.
pub fn rust_trait_impl_target(symbol: &str) -> Option<String> {
    let parsed = ParsedSymbol::parse(symbol)?;
    if parsed.is_local {
        return None;
    }
    let d = parsed.descriptors;
    let (start, _type_name, trait_name, method_part) = rust_impl_parts(d)?;
    let trait_name = trait_name?;
    let prefix = &d[..start];
    Some(format!(
        "{} {} {} {} {prefix}{trait_name}#{method_part}",
        parsed.scheme, parsed.manager, parsed.package, parsed.version,
    ))
}

/// The display name and container a Rust impl method would have if it were named
/// `Type#method().` directly, the way every other impl method here is - rather than the
/// `impl#[Type]method().` or `impl#[Type][Trait]method().` form SCIP needs internally.
///
/// Without this, `ParsedSymbol::display_name`/`container` read the wrapper literally:
/// an inherent method outlined as `impl.[Match]new` and a trait impl as
/// `impl.[Terse][Reporter]report`, where Go's equivalent of either prints `Type.method`.
/// Applies to both shapes - an inherent method has no trait to hide, so nothing is lost
/// by unwrapping it; a trait impl's trait is not lost either, it is exactly what
/// `rust_trait_impl_target` already turns into `EdgeKind::Implements`. This is purely
/// cosmetic, one more reader of the same symbol string.
pub fn rust_impl_display(symbol: &str) -> Option<(String, String)> {
    let parsed = ParsedSymbol::parse(symbol)?;
    if parsed.is_local {
        return None;
    }
    let d = parsed.descriptors;
    let (start, type_name, _trait_name, method_part) = rust_impl_parts(d)?;
    let prefix = &d[..start];
    let kind = classify_suffix(method_part)?;
    Some((
        strip_suffix_and_quotes(method_part, kind).to_string(),
        format!("{prefix}{type_name}#"),
    ))
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

    #[test]
    fn rust_trait_impl_method_points_at_the_trait_method() {
        // Captured from rust-analyzer 1.82.0 and confirmed unchanged on the
        // 2026-11-07 nightly, for a `impl Shape for Square { fn area(&self) {..} }`.
        let target = rust_trait_impl_target(
            "rust-analyzer cargo spike 0.1.0 bin/spike/impl#[Square][Shape]area().",
        )
        .expect("a trait impl method names its trait");
        assert_eq!(
            target,
            "rust-analyzer cargo spike 0.1.0 bin/spike/Shape#area()."
        );
    }

    #[test]
    fn rust_trait_impl_target_handles_a_second_implementor_the_same_way() {
        let target = rust_trait_impl_target(
            "rust-analyzer cargo spike 0.1.0 bin/spike/impl#[Circle][Shape]area().",
        )
        .expect("a trait impl method names its trait");
        assert_eq!(
            target,
            "rust-analyzer cargo spike 0.1.0 bin/spike/Shape#area()."
        );
    }

    #[test]
    fn an_inherent_impl_method_names_no_trait() {
        // `impl#[Match]new().`, one bracket - captured from indexing ripgrep's own
        // `grep-matcher` crate, not invented: an inherent `impl` method gets the same
        // `impl#[..]` wrapper a trait impl does, just with no trait to name. An earlier
        // version of this test asserted `Square#new().` with no wrapper at all, which
        // was never checked against a real rust-analyzer run and was wrong.
        assert_eq!(
            rust_trait_impl_target("rust-analyzer cargo grep-matcher 0.1.9 impl#[Match]new()."),
            None
        );
    }

    #[test]
    fn a_non_rust_symbol_names_no_trait() {
        assert_eq!(
            rust_trait_impl_target(
                "scip-go gomod github.com/example/x v0 `github.com/example/x`/Handle()."
            ),
            None
        );
    }

    #[test]
    fn rust_impl_display_reads_like_an_ordinary_method() {
        let (name, container) = rust_impl_display(
            "rust-analyzer cargo spike 0.1.0 bin/spike/impl#[Square][Shape]area().",
        )
        .expect("a trait impl method displays as Type.method");
        assert_eq!(name, "area");
        assert_eq!(container, "bin/spike/Square#");
    }

    #[test]
    fn an_inherent_impl_method_also_displays_as_type_dot_method() {
        // Same real symbol as `an_inherent_impl_method_names_no_trait`: no trait to
        // synthesise an edge from, but still wrapped for SCIP's own disambiguation, so
        // display needs unwrapping just as much as a trait impl's does.
        let (name, container) =
            rust_impl_display("rust-analyzer cargo grep-matcher 0.1.9 impl#[Match]new().")
                .expect("an inherent impl method displays as Type.method too");
        assert_eq!(name, "new");
        assert_eq!(container, "Match#");
    }

    #[test]
    fn a_trait_methods_own_declaration_has_no_display_override() {
        // `Matcher#find_at().` - the method's declaration inside the trait definition
        // itself, not an impl of it. Already in the shape every other language's method
        // is, so there is nothing for `rust_impl_display` to do here.
        assert_eq!(
            rust_impl_display("rust-analyzer cargo grep-pcre2 0.1.10 matcher/Matcher#find_at()."),
            None
        );
    }
}
