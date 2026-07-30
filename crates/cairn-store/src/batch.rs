//! Batched writes.
//!
//! Rows are queued and flushed as multi-row `INSERT`s when either threshold trips:
//! enough rows have accumulated, or enough time has passed. Both matter, for different
//! callers:
//!
//! * **Bulk ingest** only ever hits the size threshold — 350k occurrences on the spike
//!   corpus. One statement per row costs a prepare/bind/step cycle each time; batching
//!   amortises it.
//! * **The daemon's write-behind path** (watcher -> reindex -> store) mostly hits the
//!   time threshold: a single saved file produces a handful of rows, and they must land
//!   promptly rather than wait for a batch to fill.
//!
//! SQLite caps bound parameters per statement (`SQLITE_MAX_VARIABLE_NUMBER`, 32766 on
//! current builds). `max_rows * columns` must stay under it, which `new` asserts.

use anyhow::Result;
use rusqlite::Connection;
use std::time::{Duration, Instant};

/// Conservative default: 6 columns x 512 rows = 3072 parameters.
pub const DEFAULT_MAX_ROWS: usize = 512;
pub const DEFAULT_MAX_AGE: Duration = Duration::from_millis(250);

/// Upper bound on bound parameters in one statement. Kept well below the SQLite
/// limit so a caller adding a column does not silently break.
const PARAM_CEILING: usize = 16_000;

#[derive(Debug, Clone, Copy, Default)]
pub struct BatchStats {
    pub rows: usize,
    pub flushes: usize,
}

/// Accumulates rows of a fixed column count for one table.
pub struct BatchWriter {
    table: String,
    columns: Vec<String>,
    values: Vec<rusqlite::types::Value>,
    rows_pending: usize,
    max_rows: usize,
    max_age: Duration,
    last_flush: Instant,
    stats: BatchStats,
}

impl BatchWriter {
    pub fn new(table: &str, columns: &[&str]) -> BatchWriter {
        Self::with_thresholds(table, columns, DEFAULT_MAX_ROWS, DEFAULT_MAX_AGE)
    }

    pub fn with_thresholds(
        table: &str,
        columns: &[&str],
        max_rows: usize,
        max_age: Duration,
    ) -> BatchWriter {
        assert!(!columns.is_empty(), "batch writer needs at least one column");
        let max_rows = max_rows.min(PARAM_CEILING / columns.len()).max(1);
        BatchWriter {
            table: table.to_string(),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            values: Vec::with_capacity(max_rows * columns.len()),
            rows_pending: 0,
            max_rows,
            max_age,
            last_flush: Instant::now(),
            stats: BatchStats::default(),
        }
    }

    pub fn stats(&self) -> BatchStats {
        self.stats
    }

    pub fn pending(&self) -> usize {
        self.rows_pending
    }

    /// Queue one row. Flushes first if either threshold has tripped.
    pub fn push(&mut self, conn: &Connection, row: Vec<rusqlite::types::Value>) -> Result<()> {
        debug_assert_eq!(
            row.len(),
            self.columns.len(),
            "row width must match the column list"
        );
        self.values.extend(row);
        self.rows_pending += 1;
        if self.should_flush() {
            self.flush(conn)?;
        }
        Ok(())
    }

    fn should_flush(&self) -> bool {
        self.rows_pending >= self.max_rows
            || (self.rows_pending > 0 && self.last_flush.elapsed() >= self.max_age)
    }

    pub fn flush(&mut self, conn: &Connection) -> Result<()> {
        if self.rows_pending == 0 {
            self.last_flush = Instant::now();
            return Ok(());
        }
        let sql = self.build_sql(self.rows_pending);
        let mut stmt = conn.prepare_cached(&sql)?;
        stmt.execute(rusqlite::params_from_iter(self.values.iter()))?;

        self.stats.rows += self.rows_pending;
        self.stats.flushes += 1;
        self.values.clear();
        self.rows_pending = 0;
        self.last_flush = Instant::now();
        Ok(())
    }

    /// Flush whatever is left. Callers must not drop a writer with rows pending —
    /// `Drop` cannot report an error, so this is explicit.
    pub fn finish(mut self, conn: &Connection) -> Result<BatchStats> {
        self.flush(conn)?;
        Ok(self.stats)
    }

    /// `INSERT INTO t (a,b) VALUES (?,?),(?,?)...` — one statement shape per row count,
    /// and `prepare_cached` keeps the full-size one hot, which is the common case.
    fn build_sql(&self, rows: usize) -> String {
        let ncols = self.columns.len();
        let mut sql = String::with_capacity(64 + rows * (ncols * 3 + 4));
        sql.push_str("INSERT INTO ");
        sql.push_str(&self.table);
        sql.push_str(" (");
        sql.push_str(&self.columns.join(", "));
        sql.push_str(") VALUES ");
        for r in 0..rows {
            if r > 0 {
                sql.push(',');
            }
            sql.push('(');
            for c in 0..ncols {
                if c > 0 {
                    sql.push(',');
                }
                sql.push('?');
            }
            sql.push(')');
        }
        sql
    }
}

impl Drop for BatchWriter {
    fn drop(&mut self) {
        debug_assert_eq!(
            self.rows_pending, 0,
            "BatchWriter dropped with {} rows pending - call finish()",
            self.rows_pending
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::types::Value;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t (a INTEGER, b INTEGER)")
            .unwrap();
        c
    }

    fn count(c: &Connection) -> i64 {
        c.query_row("SELECT count(*) FROM t", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn flushes_on_size_threshold() {
        let c = conn();
        let mut w = BatchWriter::with_thresholds("t", &["a", "b"], 4, Duration::from_secs(3600));
        for i in 0..4 {
            w.push(&c, vec![Value::from(i), Value::from(i * 2)]).unwrap();
        }
        // Threshold reached on the fourth push, so the rows are already durable.
        assert_eq!(count(&c), 4);
        assert_eq!(w.pending(), 0);
        let s = w.finish(&c).unwrap();
        assert_eq!(s.rows, 4);
        assert_eq!(s.flushes, 1);
    }

    #[test]
    fn flushes_on_time_threshold() {
        let c = conn();
        let mut w = BatchWriter::with_thresholds("t", &["a", "b"], 1_000_000, Duration::ZERO);
        w.push(&c, vec![Value::from(1), Value::from(2)]).unwrap();
        // A zero max_age means the next push flushes what came before it.
        w.push(&c, vec![Value::from(3), Value::from(4)]).unwrap();
        assert!(count(&c) >= 1);
        w.finish(&c).unwrap();
        assert_eq!(count(&c), 2);
    }

    #[test]
    fn finish_drains_the_remainder() {
        let c = conn();
        let mut w = BatchWriter::with_thresholds("t", &["a", "b"], 100, Duration::from_secs(3600));
        for i in 0..7 {
            w.push(&c, vec![Value::from(i), Value::from(i)]).unwrap();
        }
        assert_eq!(count(&c), 0, "below both thresholds, nothing written yet");
        let s = w.finish(&c).unwrap();
        assert_eq!(count(&c), 7);
        assert_eq!(s.rows, 7);
    }

    #[test]
    fn row_cap_respects_the_parameter_ceiling() {
        let w = BatchWriter::with_thresholds("t", &["a", "b"], 1_000_000, DEFAULT_MAX_AGE);
        assert!(w.max_rows * 2 <= PARAM_CEILING);
    }
}
