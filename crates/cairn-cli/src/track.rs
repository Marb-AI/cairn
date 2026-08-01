//! One line per command, so a session can be read back afterwards.
//!
//! The question this exists to answer is not "what did it ask" but "how did the asking
//! go". A single query says little; the shape of a session says a lot — did the agent
//! reach an answer in one call or circle for twelve, did it ask the same thing twice, did
//! the tool decline and did that help, was the budget too small.
//!
//! Deliberately not recorded: the answer. Names, handles, counts and timings only. Logging
//! results would make the file enormous within a week and would put source-derived content
//! somewhere nobody is watching. Nothing here is worth keeping that a `ps` listing would
//! not already show.
//!
//! Off unless switched on. A tool that starts recording what you searched for without
//! being asked is one people stop trusting, and being internal is not a reason to skip
//! asking.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What a session is, when nobody tells us.
///
/// An agent harness usually has an id and can pass it in `CAIRN_SESSION`. Without one, the
/// parent process is the best available proxy: every command an agent runs shares a parent,
/// and a different agent run has a different one. Imperfect — a long-lived shell makes one
/// session of a whole day — which is why the explicit variable exists.
fn session_id() -> String {
    if let Ok(id) = std::env::var("CAIRN_SESSION") {
        if !id.trim().is_empty() {
            return sanitise(&id);
        }
    }
    match parent_id() {
        Some(ppid) => format!("ppid-{ppid}"),
        None => "unknown".to_string(),
    }
}

/// The pid of the process that started us.
#[cfg(unix)]
fn parent_id() -> Option<u32> {
    Some(std::os::unix::process::parent_id())
}

/// Windows has no `getppid`, so the parent has to be found by walking the process table.
///
/// Worth the walk rather than falling back to "unknown": every command would otherwise
/// share one session id, which turns the whole session file into a single undifferentiated
/// stream and defeats the point of recording it.
#[cfg(windows)]
fn parent_id() -> Option<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;

    let me = unsafe { GetCurrentProcessId() };
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

    let mut found = None;
    if unsafe { Process32FirstW(snapshot, &mut entry) } != 0 {
        loop {
            if entry.th32ProcessID == me {
                found = Some(entry.th32ParentProcessID);
                break;
            }
            if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
                break;
            }
        }
    }
    unsafe { CloseHandle(snapshot) };
    found
}

#[cfg(not(any(unix, windows)))]
fn parent_id() -> Option<u32> {
    None
}

/// Keep the id usable as a file name: it comes from the environment, so it is not ours.
fn sanitise(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

/// Everything one command is worth remembering.
pub struct Record<'a> {
    pub command: &'a str,
    /// The handle or query the command was given, when it took one.
    pub subject: Option<&'a str>,
    /// Flags, without values that could carry content.
    pub flags: Vec<String>,
    pub exit: u8,
    /// Rows the answer carried, where the command produces a list.
    pub rows: Option<usize>,
    /// True when the answer said it had left something out.
    pub truncated: bool,
    pub elapsed: Duration,
}

/// Append the record, or give up quietly.
///
/// Tracking must never be able to fail a query. A full disk, a read-only checkout, a
/// directory someone removed mid-session — all of them cost a log line and nothing else.
pub fn append(db: &Path, rec: &Record) {
    let Some(dir) = db.parent().map(|d| d.join("sessions")) else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let id = session_id();
    let path: PathBuf = dir.join(format!("{id}.jsonl"));

    // Sequence within the session, so a reader can reconstruct the order without trusting
    // clocks. Counting the lines already there is cheap at this scale and needs no state.
    let seq = std::fs::read_to_string(&path)
        .map(|s| s.lines().count())
        .unwrap_or(0);

    let line = format!(
        "{{\"ts\":\"{}\",\"session\":\"{}\",\"seq\":{},\"cmd\":\"{}\",\"subject\":{},\
         \"flags\":[{}],\"exit\":{},\"rows\":{},\"truncated\":{},\"ms\":{}}}\n",
        now_iso8601(),
        id,
        seq,
        json_escape(rec.command),
        match rec.subject {
            Some(s) => format!("\"{}\"", json_escape(s)),
            None => "null".to_string(),
        },
        rec.flags
            .iter()
            .map(|f| format!("\"{}\"", json_escape(f)))
            .collect::<Vec<_>>()
            .join(","),
        rec.exit,
        match rec.rows {
            Some(n) => n.to_string(),
            None => "null".to_string(),
        },
        rec.truncated,
        rec.elapsed.as_millis(),
    );

    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

fn json_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '"' => vec!['\\', '"'],
            '\\' => vec!['\\', '\\'],
            '\n' | '\r' | '\t' => vec![' '],
            c if (c as u32) < 0x20 => vec![' '],
            c => vec![c],
        })
        .collect()
}

/// Seconds since the epoch, rendered as a timestamp without pulling in a date library.
fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Civil-from-days, the standard algorithm. Enough to sort and to read.
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Peak resident memory, where the platform will say.
///
/// Each platform reports a different unit, so the conversion is part of the answer rather
/// than something the caller is expected to know.
#[cfg(target_os = "linux")]
pub fn peak_rss_kb() -> Option<u64> {
    // VmHWM is already in kB.
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|n| n.parse().ok())
}

#[cfg(target_os = "macos")]
pub fn peak_rss_kb() -> Option<u64> {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return None;
    }
    // Darwin reports `ru_maxrss` in bytes; Linux reports the same field in kilobytes.
    // Reading it as kB here would overstate the peak by a factor of 1024.
    Some((usage.ru_maxrss as u64) / 1024)
}

#[cfg(windows)]
pub fn peak_rss_kb() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    // Peak working set is the closest Windows equivalent of high-water resident size.
    (ok != 0).then_some(counters.PeakWorkingSetSize as u64 / 1024)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn peak_rss_kb() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CAIRN_SESSION` is process-wide and these tests each set it, so they cannot run at
    /// the same time. Found by them racing, which is the same class of bug the tool itself
    /// had: shared state that only misbehaves under concurrency.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn an_id_from_the_environment_cannot_escape_its_directory() {
        assert_eq!(sanitise("../../etc/passwd"), "------etc-passwd");
        assert_eq!(sanitise("ok-id_9"), "ok-id_9");
        assert_eq!(sanitise(""), "unknown");
        assert!(sanitise(&"x".repeat(200)).len() <= 64);
    }

    #[test]
    fn a_record_is_one_json_line_and_carries_no_answer() {
        let dir = std::env::temp_dir().join("cairn-track-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CAIRN_SESSION", "t1");
        let db = dir.join("index.sqlite");
        append(
            &db,
            &Record {
                command: "affects",
                subject: Some("fba"),
                flags: vec!["--depth".into()],
                exit: 0,
                rows: Some(4),
                truncated: false,
                elapsed: Duration::from_millis(290),
            },
        );
        let text = std::fs::read_to_string(dir.join("sessions/t1.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 1);
        let line = text.lines().next().unwrap();
        assert!(line.contains("\"cmd\":\"affects\""));
        assert!(line.contains("\"subject\":\"fba\""));
        assert!(line.contains("\"seq\":0"));
        assert!(line.ends_with('}'));
        std::env::remove_var("CAIRN_SESSION");
    }

    #[test]
    fn the_sequence_advances_within_a_session() {
        let dir = std::env::temp_dir().join("cairn-track-seq");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CAIRN_SESSION", "t2");
        let db = dir.join("index.sqlite");
        for _ in 0..3 {
            append(
                &db,
                &Record {
                    command: "refs",
                    subject: Some("wb2"),
                    flags: vec![],
                    exit: 0,
                    rows: Some(5),
                    truncated: true,
                    elapsed: Duration::from_millis(3),
                },
            );
        }
        let text = std::fs::read_to_string(dir.join("sessions/t2.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert!(text.lines().nth(2).unwrap().contains("\"seq\":2"));
        // The same subject three times in one session is exactly the pattern this file
        // exists to make visible.
        assert_eq!(text.matches("\"subject\":\"wb2\"").count(), 3);
        std::env::remove_var("CAIRN_SESSION");
    }

    #[test]
    fn a_directory_that_cannot_be_written_costs_a_line_and_nothing_else() {
        // Tracking must never fail a query.
        append(
            Path::new("/proc/definitely/not/writable/index.sqlite"),
            &Record {
                command: "symbol",
                subject: None,
                flags: vec![],
                exit: 1,
                rows: None,
                truncated: false,
                elapsed: Duration::from_millis(1),
            },
        );
    }
}
