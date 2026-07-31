//! When to reindex.
//!
//! A full reindex costs 30 s for Go and 2.5 minutes for Python on the target repo
//! (docs/spike-0-results.md), so the trigger has to be chosen carefully. The model here
//! is **debt, not events**: the dirty set is debt the overlay covers up to a point, and
//! a reindex pays it down when it gets expensive *and* the tree has gone quiet.
//!
//! Four rules, each earned:
//!
//! * **Never on a file event.** A single keystroke must not be able to schedule
//!   anything. This is the failure that would make the daemon worse than useless.
//! * **One slot, never a queue.** A trigger arriving during a run sets "needed again"
//!   rather than appending, so N triggers cost one extra run, not N.
//! * **Quiescence gate.** A `git checkout` touches thousands of files; starting
//!   mid-storm would index a torn tree and immediately be invalidated.
//! * **More than one trigger.** A commit is the cleanest moment, but someone editing
//!   for an hour without committing would otherwise be querying a badly stale index.
//!
//! What this module does *not* do is run anything by itself. Deciding a reindex is due
//! is free and always safe; performing one spawns heavy external indexers, so it stays
//! opt-in behind a configured command (architecture 4.6 draws the same line for
//! codegen).

use std::time::{Duration, Instant};

/// Number of dirty files past which per-file overlay stops being the cheaper option.
pub const DEBT_THRESHOLD: usize = 50;

/// How long the tree must be still before a reindex may start.
pub const QUIESCENCE: Duration = Duration::from_secs(20);

/// Debt below the threshold is still paid down eventually, just lazily.
pub const IDLE_REINDEX_AFTER: Duration = Duration::from_secs(15 * 60);

/// Minimum gap between runs, so a repo that changes constantly cannot spin.
pub const MIN_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// `HEAD` moved: commit, checkout, merge, rebase. The cleanest moment there is,
    /// because the tree is by definition in a state someone chose.
    HeadMoved,
    /// Enough files differ that the overlay is no longer the cheap path.
    DebtThreshold,
    /// Quiet for a long time with outstanding debt.
    Idle,
    /// Asked for explicitly.
    Manual,
}

impl Trigger {
    pub fn reason(self) -> &'static str {
        match self {
            Trigger::HeadMoved => "HEAD moved (commit, checkout, merge or rebase)",
            Trigger::DebtThreshold => "too many files differ for the overlay to stay cheap",
            Trigger::Idle => "the tree has been quiet with changes outstanding",
            Trigger::Manual => "requested",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Nothing to do.
    Idle,
    /// A reindex is due for this reason.
    Due(Trigger),
    /// Due, but the tree is still moving; wait for it to settle.
    WaitingForQuiet(Trigger),
    /// Due, but the last run was too recent.
    Cooling(Trigger),
}

/// Tracks the signals and decides. Deliberately a pure state machine with time passed
/// in, so the policy is testable without sleeping.
pub struct Scheduler {
    last_change: Option<Instant>,
    last_run: Option<Instant>,
    pending: Option<Trigger>,
    running: bool,
    /// Set when a trigger arrives mid-run: one extra run afterwards, not a queue.
    rerun_needed: bool,
    debt: usize,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scheduler {
    pub fn new() -> Scheduler {
        Scheduler {
            last_change: None,
            last_run: None,
            pending: None,
            running: false,
            rerun_needed: false,
            debt: 0,
        }
    }

    /// A file changed. Note what this does *not* do: it never makes a reindex due on
    /// its own. It only resets the quiescence clock and updates the debt.
    pub fn on_change(&mut self, now: Instant, debt: usize) {
        self.last_change = Some(now);
        self.debt = debt;
        if debt >= DEBT_THRESHOLD {
            self.raise(Trigger::DebtThreshold);
        }
    }

    pub fn on_head_moved(&mut self, now: Instant) {
        self.last_change = Some(now);
        self.raise(Trigger::HeadMoved);
    }

    pub fn on_manual(&mut self) {
        self.raise(Trigger::Manual);
    }

    /// A trigger is raised at most once until it is consumed. HEAD movement outranks
    /// the others, because it is the one that says the tree is in a chosen state.
    fn raise(&mut self, t: Trigger) {
        if self.running {
            self.rerun_needed = true;
            return;
        }
        self.pending = match self.pending {
            Some(Trigger::HeadMoved) => Some(Trigger::HeadMoved),
            _ => Some(t),
        };
    }

    pub fn decide(&mut self, now: Instant) -> Decision {
        if self.running {
            return Decision::Idle;
        }
        // Lazy debt: quiet for long enough with something outstanding.
        if self.pending.is_none() && self.debt > 0 {
            if let Some(last) = self.last_change {
                if now.duration_since(last) >= IDLE_REINDEX_AFTER {
                    self.pending = Some(Trigger::Idle);
                }
            }
        }
        let Some(trigger) = self.pending else {
            return Decision::Idle;
        };
        if let Some(last) = self.last_change {
            if now.duration_since(last) < QUIESCENCE {
                return Decision::WaitingForQuiet(trigger);
            }
        }
        if let Some(last) = self.last_run {
            if now.duration_since(last) < MIN_INTERVAL {
                return Decision::Cooling(trigger);
            }
        }
        Decision::Due(trigger)
    }

    pub fn start(&mut self, now: Instant) -> Option<Trigger> {
        let t = self.pending.take()?;
        self.running = true;
        self.rerun_needed = false;
        self.last_run = Some(now);
        Some(t)
    }

    /// Finish a run. Any triggers that arrived meanwhile collapse into one rerun.
    pub fn finish(&mut self, debt: usize) {
        self.running = false;
        self.debt = debt;
        if self.rerun_needed {
            self.rerun_needed = false;
            self.pending = Some(Trigger::Manual);
        }
    }

    pub fn is_running(&self) -> bool {
        self.running
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn a_single_edit_never_schedules_anything() {
        let mut s = Scheduler::new();
        let now = t0();
        s.on_change(now, 1);
        // Even long after the tree went quiet, one changed file is not worth 2.5
        // minutes of reindexing.
        assert_eq!(s.decide(now + QUIESCENCE * 2), Decision::Idle);
    }

    #[test]
    fn head_movement_is_the_clean_trigger() {
        let mut s = Scheduler::new();
        let now = t0();
        s.on_head_moved(now);
        assert_eq!(
            s.decide(now + Duration::from_secs(1)),
            Decision::WaitingForQuiet(Trigger::HeadMoved)
        );
        assert_eq!(
            s.decide(now + QUIESCENCE + Duration::from_secs(1)),
            Decision::Due(Trigger::HeadMoved)
        );
    }

    #[test]
    fn a_storm_of_changes_defers_until_it_settles() {
        let mut s = Scheduler::new();
        let start = t0();
        s.on_head_moved(start);
        // A checkout keeps touching files; each one pushes the gate out.
        for i in 1..50 {
            s.on_change(start + Duration::from_millis(i * 100), 200);
            assert!(matches!(
                s.decide(start + Duration::from_millis(i * 100)),
                Decision::WaitingForQuiet(_)
            ));
        }
        let settled = start + Duration::from_secs(5) + QUIESCENCE;
        assert!(matches!(s.decide(settled), Decision::Due(Trigger::HeadMoved)));
    }

    #[test]
    fn triggers_during_a_run_collapse_into_one_rerun() {
        let mut s = Scheduler::new();
        let now = t0();
        s.on_head_moved(now);
        let quiet = now + QUIESCENCE + Duration::from_secs(1);
        assert!(s.start(quiet).is_some());
        for _ in 0..100 {
            s.on_change(quiet, 5);
            s.on_head_moved(quiet);
        }
        s.finish(5);
        // A hundred triggers during the run buy exactly one more run.
        assert!(s.pending.is_some());
        s.start(quiet + MIN_INTERVAL * 2);
        s.finish(0);
        assert!(s.pending.is_none());
    }

    #[test]
    fn debt_alone_is_enough_when_it_is_large() {
        let mut s = Scheduler::new();
        let now = t0();
        s.on_change(now, DEBT_THRESHOLD);
        assert!(matches!(
            s.decide(now + QUIESCENCE + Duration::from_secs(1)),
            Decision::Due(Trigger::DebtThreshold)
        ));
    }

    #[test]
    fn back_to_back_runs_are_rate_limited() {
        let mut s = Scheduler::new();
        let now = t0();
        s.on_head_moved(now);
        let quiet = now + QUIESCENCE + Duration::from_secs(1);
        s.start(quiet);
        s.finish(0);
        s.on_head_moved(quiet);
        assert!(matches!(
            s.decide(quiet + QUIESCENCE + Duration::from_secs(1)),
            Decision::Cooling(_)
        ));
    }

    #[test]
    fn small_debt_is_paid_down_eventually() {
        let mut s = Scheduler::new();
        let now = t0();
        s.on_change(now, 3);
        assert_eq!(s.decide(now + Duration::from_secs(60)), Decision::Idle);
        // Someone editing for an hour without committing should not be querying an
        // hour-old index forever.
        assert!(matches!(
            s.decide(now + IDLE_REINDEX_AFTER + Duration::from_secs(1)),
            Decision::Due(Trigger::Idle)
        ));
    }
}
