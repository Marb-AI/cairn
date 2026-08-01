//! Output budget.
//!
//! The caller states a ceiling and the tool fills it with the most valuable rows it
//! has, then says what it dropped (architecture 18.2). This inverts the usual
//! arrangement, where the caller guesses a `--limit` and either overspends or has to
//! ask again — and asking again is the exploration loop the whole product removes.
//!
//! Accounting is against the *rendered* text, not an estimate of the input, so the
//! ceiling holds for what actually reaches the agent.

/// Characters per token. Rough on purpose: pulling in a real tokenizer would cost more
/// than the accuracy is worth, and the number only has to be stable and slightly
/// conservative. Measured against code-like English this sits around 3.5-4.
const CHARS_PER_TOKEN: f64 = 3.7;

pub struct Budget {
    /// `None` means unbounded.
    max_tokens: Option<usize>,
    spent_chars: usize,
}

impl Budget {
    pub fn unlimited() -> Budget {
        Budget {
            max_tokens: None,
            spent_chars: 0,
        }
    }

    pub fn tokens(max: usize) -> Budget {
        Budget {
            max_tokens: Some(max),
            spent_chars: 0,
        }
    }

    pub fn from_opt(max: Option<usize>) -> Budget {
        match max {
            Some(t) => Budget::tokens(t),
            None => Budget::unlimited(),
        }
    }

    pub fn spent_tokens(&self) -> usize {
        (self.spent_chars as f64 / CHARS_PER_TOKEN).ceil() as usize
    }

    pub fn remaining_tokens(&self) -> Option<usize> {
        self.max_tokens
            .map(|m| m.saturating_sub(self.spent_tokens()))
    }

    /// Append a line if it fits. Returns false when the budget is exhausted, so the
    /// caller stops and reports the remainder rather than silently truncating.
    pub fn push(&mut self, out: &mut String, line: &str) -> bool {
        let cost = line.len() + 1;
        if let Some(max) = self.max_tokens {
            let would_be = ((self.spent_chars + cost) as f64 / CHARS_PER_TOKEN).ceil() as usize;
            // Always allow the first line: returning a header and nothing else is
            // less useful than overshooting a tiny budget by one row.
            if would_be > max && self.spent_chars > 0 {
                return false;
            }
        }
        out.push_str(line);
        out.push('\n');
        self.spent_chars += cost;
        true
    }

    /// Note describing what the ceiling cut, and how to get it.
    pub fn cut_note(&self, dropped: usize, unit: &str) -> String {
        match self.max_tokens {
            Some(max) => format!(
                "{dropped} more {unit} beyond the {max}-token budget \
                 (raise with --budget, or narrow with --depth/--fanout/--aspect)"
            ),
            None => format!("{dropped} more {unit}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_never_refuses() {
        let mut b = Budget::unlimited();
        let mut s = String::new();
        for _ in 0..1000 {
            assert!(b.push(&mut s, "a fairly long line of output text"));
        }
    }

    #[test]
    fn stops_at_the_ceiling() {
        let mut b = Budget::tokens(10);
        let mut s = String::new();
        let line = "0123456789012345678901234567890123456789"; // ~11 tokens
        assert!(b.push(&mut s, line), "first line always goes through");
        assert!(!b.push(&mut s, line), "second must be refused");
        assert_eq!(s.lines().count(), 1);
    }

    #[test]
    fn reports_how_to_get_more() {
        let b = Budget::tokens(500);
        let note = b.cut_note(7, "nodes");
        assert!(note.contains("7 more nodes"));
        assert!(note.contains("--budget"));
    }
}
