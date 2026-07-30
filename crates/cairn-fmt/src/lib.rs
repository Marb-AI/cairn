//! Compact response rendering.
//!
//! This is the product surface (architecture 6.3). Two rules drive everything here:
//!
//! * **Not JSON.** Braces, quotes and repeated keys cost several times more tokens than
//!   an aligned text table carrying the same information. When the whole pitch is
//!   cheaper context, the response format *is* the pitch.
//! * **`unknown:` and `suppressed:` are mandatory** (D8). A missing section reads as
//!   "this is everything", and that is the silent error the design exists to avoid.

use cairn_store::{Occurrence, SymbolRow};
use std::fmt::Write;

/// Every response carries the same envelope, so an agent can rely on it being there.
pub struct Envelope {
    pub body: String,
    pub unknown: Vec<String>,
    pub suppressed: Vec<String>,
    pub stale: Vec<String>,
}

impl Envelope {
    pub fn new(body: String) -> Envelope {
        Envelope { body, unknown: Vec::new(), suppressed: Vec::new(), stale: Vec::new() }
    }

    pub fn unknown(mut self, msg: impl Into<String>) -> Self {
        self.unknown.push(msg.into());
        self
    }

    pub fn suppressed(mut self, msg: impl Into<String>) -> Self {
        self.suppressed.push(msg.into());
        self
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

/// One result line: `[handle] Qualified.name  kind  lang  path:line`
pub fn symbol_line(s: &SymbolRow) -> String {
    let loc = s
        .def
        .as_ref()
        .map(|d| d.location())
        .unwrap_or_else(|| "<no definition indexed>".to_string());
    format!(
        "[{}] {:<44} {:<7} {}  {}",
        s.handle,
        truncate(&s.qualified(), 44),
        s.kind.as_str(),
        s.lang.tag(),
        loc
    )
}

pub fn symbols(rows: &[SymbolRow], query: &str) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(body, "{} matches for \"{}\"", rows.len(), query);
    for s in rows {
        let _ = writeln!(body, "{}", symbol_line(s));
    }
    if rows.is_empty() {
        let _ = writeln!(body, "(nothing matched)");
    }
    Envelope::new(body)
}

pub fn references(sym: &SymbolRow, refs: &[Occurrence], suppressed_generated: i64) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(body, "references to [{}] {}", sym.handle, sym.qualified());
    if let Some(def) = &sym.def {
        let _ = writeln!(body, "  defined at {}", def.location());
    }
    let _ = writeln!(body, "{} references                    [L0, exact]", refs.len());
    for r in refs {
        let _ = writeln!(body, "  {}", r.location());
    }
    if refs.is_empty() {
        let _ = writeln!(body, "  (none outside generated code)");
    }

    let mut env = Envelope::new(body);
    if suppressed_generated > 0 {
        env = env.suppressed(format!(
            "{suppressed_generated} references in generated code (rerun with --include-generated)"
        ));
    }
    env
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{keep}~")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_always_has_all_sections() {
        let out = Envelope::new("body".into()).render();
        assert!(out.contains("suppressed: none"));
        assert!(out.contains("unknown: none"));
        assert!(out.contains("stale: none"));
    }

    #[test]
    fn truncation_marks_itself() {
        assert_eq!(truncate("abcdef", 4), "abc~");
        assert_eq!(truncate("abc", 4), "abc");
    }
}
