//! Tiny logging shim + ring buffer for operational messages.
//! Plugin warnings, MCP stderr, provider quirks, hook errors —
//! anything that used to be a bare `eprintln!` — routes through
//! [`push`], which both prints to stderr (subject to `RUST_LOG`)
//! and stashes the line in a process-wide ring buffer.
//!
//! The ring is capped at ~8 KB so a chatty plugin can't blow up
//! memory. `/diagnostics` paginates the tail; `/diagnostics
//! clear` wipes it.
//!
//! Filtering: `RUST_LOG=info` (default) | `warn` | `error` |
//! `debug` | `trace` (debug/trace currently unused but reserved).
//! The threshold gates *stderr printing only* — the ring stores
//! everything regardless so debugging from `/diagnostics` works
//! even when the user didn't set `RUST_LOG=debug` upfront.
//!
//! Kept under 150 LOC of internal code; no `tracing`/`log` dep.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiagnosticEntry {
    pub ts: SystemTime,
    pub level: Level,
    pub body: String,
}

/// Soft cap on total body bytes retained. The oldest entries are
/// evicted FIFO once the cap is exceeded, so a chatty plugin
/// can't blow up the process.
const MAX_BYTES: usize = 8 * 1024;

struct Ring {
    entries: VecDeque<DiagnosticEntry>,
    bytes: usize,
}

static RING: OnceLock<Mutex<Ring>> = OnceLock::new();

fn ring() -> &'static Mutex<Ring> {
    RING.get_or_init(|| {
        Mutex::new(Ring {
            entries: VecDeque::new(),
            bytes: 0,
        })
    })
}

/// Append an entry. Stderr-prints if `level` is at or above the
/// `RUST_LOG`-derived threshold; stashes it in the ring buffer
/// either way.
pub fn push(level: Level, body: String) {
    if level >= threshold_from_env() {
        eprintln!("{}", body);
    }
    let bytes = body.len();
    let entry = DiagnosticEntry {
        ts: SystemTime::now(),
        level,
        body,
    };
    let mut r = ring().lock().unwrap();
    r.entries.push_back(entry);
    r.bytes = r.bytes.saturating_add(bytes);
    while r.bytes > MAX_BYTES {
        match r.entries.pop_front() {
            Some(e) => r.bytes = r.bytes.saturating_sub(e.body.len()),
            None => break,
        }
    }
}

/// Snapshot the most-recent `n` entries (or all of them if
/// `n` is greater than the buffer). Cheap clone — the buffer is
/// small by construction.
pub fn tail(n: usize) -> Vec<DiagnosticEntry> {
    let r = ring().lock().unwrap();
    let total = r.entries.len();
    let skip = total.saturating_sub(n);
    r.entries.iter().skip(skip).cloned().collect()
}

/// Drop every entry. `/diagnostics clear` calls this; tests do
/// too so they don't bleed state between runs.
pub fn clear() {
    let mut r = ring().lock().unwrap();
    r.entries.clear();
    r.bytes = 0;
}

/// Serializes test cases that interact with the global ring.
/// The ring is process-wide; running them in parallel would have
/// them stomp on each other's pushes and `clear`s. Tests that
/// touch the buffer should hold this mutex for the duration of
/// their body — keep `_g = TEST_SERIAL.lock().unwrap()` as the
/// first line.
#[cfg(test)]
pub static TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn threshold_from_env() -> Level {
    match std::env::var("RUST_LOG").as_deref() {
        Ok("trace") => Level::Trace,
        Ok("debug") => Level::Debug,
        Ok("info") | Err(_) => Level::Info,
        Ok("warn") => Level::Warn,
        Ok("error") => Level::Error,
        _ => Level::Info,
    }
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::diagnostics::push(
            $crate::diagnostics::Level::Info,
            format!($($arg)*),
        )
    };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::diagnostics::push(
            $crate::diagnostics::Level::Warn,
            format!($($arg)*),
        )
    };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::diagnostics::push(
            $crate::diagnostics::Level::Error,
            format!($($arg)*),
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_appends_to_ring_and_tail_returns_newest_last() {
        let _g = TEST_SERIAL.lock().unwrap();
        clear();
        push(Level::Info, "first".into());
        push(Level::Warn, "second".into());
        let entries = tail(10);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].body, "first");
        assert_eq!(entries[1].body, "second");
        assert_eq!(entries[1].level, Level::Warn);
    }

    #[test]
    fn ring_evicts_oldest_when_byte_cap_exceeded() {
        let _g = TEST_SERIAL.lock().unwrap();
        clear();
        // Push roughly 12 KB across many entries; expect the
        // ring to evict down to ≤ MAX_BYTES.
        let chunk = "x".repeat(256);
        for _ in 0..50 {
            push(Level::Info, chunk.clone());
        }
        let entries = tail(usize::MAX);
        let total: usize = entries.iter().map(|e| e.body.len()).sum();
        assert!(
            total <= MAX_BYTES,
            "expected ≤ {} bytes after eviction, got {}",
            MAX_BYTES,
            total
        );
    }

    #[test]
    fn clear_drops_every_entry() {
        let _g = TEST_SERIAL.lock().unwrap();
        clear();
        push(Level::Info, "a".into());
        push(Level::Info, "b".into());
        clear();
        assert!(tail(usize::MAX).is_empty());
    }

    #[test]
    fn tail_n_returns_at_most_n_newest_entries() {
        let _g = TEST_SERIAL.lock().unwrap();
        clear();
        push(Level::Info, "1".into());
        push(Level::Info, "2".into());
        push(Level::Info, "3".into());
        let entries = tail(2);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].body, "2");
        assert_eq!(entries[1].body, "3");
    }

    #[test]
    fn level_ordering_works_for_threshold_comparisons() {
        // The threshold check leans on PartialOrd. Verify the
        // intuitive ordering — error highest, trace lowest.
        assert!(Level::Error > Level::Warn);
        assert!(Level::Warn > Level::Info);
        assert!(Level::Info > Level::Debug);
        assert!(Level::Debug > Level::Trace);
    }
}
