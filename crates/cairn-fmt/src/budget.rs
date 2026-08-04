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
    ///
    /// Only `--budget` is named, because it is the only flag every command that can be
    /// cut actually has. This used to add "or narrow with --depth/--fanout/--aspect",
    /// which are `graph`'s flags and nobody else's: `refs`, `symbol`, `usage`,
    /// `outline` and the rest have none of them. Measured in the session logs — an agent
    /// ran `refs --budget` four times, was told each time to narrow with flags that do
    /// not exist on `refs`, and gave up on `--budget` for `--limit`, which is the exact
    /// loop `--budget` was built to remove. Advice that cannot be followed is worse than
    /// no advice: it costs a round trip before it can be recognised as wrong.
    pub fn cut_note(&self, dropped: usize, unit: &str) -> String {
        match self.max_tokens {
            Some(max) => {
                format!("{dropped} more {unit} beyond the {max}-token budget (raise --budget)")
            }
            None => format!("{dropped} more {unit}"),
        }
    }

    /// The same note for a command that really can narrow its question.
    ///
    /// `how` is the caller's own flags, so it is right by construction: only the command
    /// building the answer knows what it accepts.
    pub fn cut_note_narrowable(&self, dropped: usize, unit: &str, how: &str) -> String {
        match self.max_tokens {
            Some(max) => format!(
                "{dropped} more {unit} beyond the {max}-token budget \
                 (raise --budget, or narrow with {how})"
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

    #[test]
    fn the_generic_note_names_no_flag_a_command_might_not_have() {
        // `refs`, `symbol`, `usage` and `outline` have none of graph's narrowing flags.
        // Telling them to narrow with `--depth` costs a round trip to find out, and the
        // session logs show where that leads: back to `--limit`, which `--budget` exists
        // to replace.
        let note = Budget::tokens(500).cut_note(7, "references");
        for absent in ["--depth", "--fanout", "--aspect", "--limit"] {
            assert!(
                !note.contains(absent),
                "generic cut note offered {absent}, which the command may not accept: {note}"
            );
        }
    }

    #[test]
    fn a_command_that_can_narrow_says_so_in_its_own_flags() {
        let note = Budget::tokens(500).cut_note_narrowable(3, "nodes", "--depth or --fanout");
        assert!(note.contains("3 more nodes"));
        assert!(note.contains("--budget"));
        assert!(note.contains("--depth or --fanout"));
        // Without a ceiling there is nothing to raise and nothing to narrow towards.
        assert_eq!(
            Budget::unlimited().cut_note_narrowable(3, "nodes", "--depth"),
            "3 more nodes"
        );
    }
}
