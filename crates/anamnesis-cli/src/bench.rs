//! How many events a second this machine can record.
//!
//! Every other measurement in this repository is about whether retrieval finds
//! the right page. This one is about the other end: a hook runs before every
//! tool call an agent makes, it gives up after one second, and on a server
//! more than one person points at, every one of those events arrives at the
//! same index. "Will that hold" has been answered by argument until now.
//!
//! What is measured is the path an event actually takes — parsed and redacted
//! the way a payload from a harness is, then recorded the way `POST /hook`
//! records it, scope resolution and all. Not `INSERT` in a loop: the number
//! that matters includes the marker file being read and the body being scanned
//! for secrets, because those happen per event in production too.
//!
//! It runs against a temporary data directory. A benchmark that wrote two
//! thousand invented sessions into somebody's memory would be a strange thing
//! to ship, and the first person to run it twice would have a wiki full of
//! them.

use std::time::{Duration, Instant};

use anamnesis_core::datadir::DataDir;
use anamnesis_core::session::AgentKind;
use anamnesis_store::{RawSpool, Store};
use jiff::Timestamp;

/// How many events to record when nobody says.
pub const DEFAULT_EVENTS: usize = 2_000;

/// Roughly the size of a real tool result, in bytes.
///
/// The body is what redaction walks and what the index stores, so measuring
/// with an empty one would measure something nobody runs.
const BODY_BYTES: usize = 2_048;

/// What one pass measured.
#[derive(Debug, Clone, PartialEq)]
pub struct Measured {
    /// How many events went through it.
    pub events: usize,
    /// How long the whole pass took.
    pub elapsed: Duration,
    /// Per-event durations, sorted, for the percentiles.
    pub sorted: Vec<Duration>,
}

impl Measured {
    /// Events per second, as a rate rather than a total.
    pub fn per_second(&self) -> f64 {
        if self.elapsed.is_zero() {
            return 0.0;
        }
        self.events as f64 / self.elapsed.as_secs_f64()
    }

    /// The duration `fraction` of events finished within.
    ///
    /// Percentiles rather than an average, because the average of a capture
    /// path is not the thing a hook waits on: one event in a hundred taking
    /// twenty times the mean is what a person feels, and a mean hides it.
    pub fn percentile(&self, fraction: f64) -> Duration {
        if self.sorted.is_empty() {
            return Duration::ZERO;
        }
        let index = ((self.sorted.len() - 1) as f64 * fraction).round() as usize;
        self.sorted[index.min(self.sorted.len() - 1)]
    }
}

/// The three passes one run makes.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// Parsing and redacting the payload, with nothing written.
    pub parsed: Measured,
    /// The whole capture path, index only.
    pub recorded: Measured,
    /// The same, with the durable transcript written as well.
    pub spooled: Measured,
}

/// Run the benchmark against a temporary data directory.
pub fn measure(events: usize) -> anyhow::Result<Report> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join(".anamnesis.toml"),
        "[scope]\nworkspace = \"bench\"\nproject = \"bench\"\n",
    )?;

    let parsed = time_parse(repo.path(), events)?;
    let recorded = time_record(repo.path(), events, false)?;
    let spooled = time_record(repo.path(), events, true)?;

    Ok(Report {
        parsed,
        recorded,
        spooled,
    })
}

/// One payload of the kind a harness sends, numbered so no two are identical.
///
/// Alternating prompts and tool calls, because they cost different things: a
/// prompt is a body to redact, a tool call is that plus a path to read and an
/// outcome to classify.
fn payload(cwd: &std::path::Path, session: &str, n: usize) -> serde_json::Value {
    let filler = "x".repeat(BODY_BYTES);
    if n.is_multiple_of(2) {
        serde_json::json!({
            "session_id": session,
            "hook_event_name": "UserPromptSubmit",
            "cwd": cwd.to_string_lossy(),
            "prompt": format!("event {n}: {filler}"),
        })
    } else {
        serde_json::json!({
            "session_id": session,
            "hook_event_name": "PostToolUse",
            "cwd": cwd.to_string_lossy(),
            "tool_name": "Read",
            "tool_input": {"file_path": format!("src/module_{n}.rs")},
            "tool_response": {"output": filler, "success": true},
        })
    }
}

/// Parsing and redacting, with nothing written.
fn time_parse(repo: &std::path::Path, events: usize) -> anyhow::Result<Measured> {
    let agent = AgentKind::ClaudeCode;
    let payloads: Vec<serde_json::Value> = (0..events)
        .map(|n| payload(repo, "bench-session", n))
        .collect();

    let mut sorted = Vec::with_capacity(events);
    let started = Instant::now();
    for payload in &payloads {
        let at = Instant::now();
        let parsed = anamnesis_hooks::parse(&agent, payload);
        sorted.push(at.elapsed());
        // Read, so the compiler cannot decide the work was pointless.
        anyhow::ensure!(parsed.is_ok(), "a generated payload did not parse");
    }
    let elapsed = started.elapsed();
    sorted.sort_unstable();

    Ok(Measured {
        events,
        elapsed,
        sorted,
    })
}

/// The whole capture path, with or without the durable transcript.
fn time_record(repo: &std::path::Path, events: usize, spool: bool) -> anyhow::Result<Measured> {
    let data = tempfile::tempdir()?;
    let dir = DataDir::resolve(Some(data.path().to_path_buf()))?;
    dir.ensure_layout()?;
    let store = Store::open(dir.db_file())?;
    store.migrate()?;
    let raw = spool.then(|| RawSpool::new(dir.raw()));

    let agent = AgentKind::ClaudeCode;
    let session = format!("bench-{}", if spool { "spooled" } else { "indexed" });
    let hooks: Vec<_> = (0..events)
        .map(|n| anamnesis_hooks::parse(&agent, &payload(repo, &session, n)))
        .collect::<Result<Vec<_>, _>>()?;

    let mut sorted = Vec::with_capacity(events);
    let started = Instant::now();
    for hook in &hooks {
        let at = Instant::now();
        anamnesis_web::record(&store, raw.as_ref(), hook, Timestamp::now(), None)?;
        sorted.push(at.elapsed());
    }
    let elapsed = started.elapsed();
    sorted.sort_unstable();

    Ok(Measured {
        events,
        elapsed,
        sorted,
    })
}

/// Print what a run measured.
pub fn cmd_bench(events: usize) -> anyhow::Result<()> {
    anyhow::ensure!(events > 0, "there is nothing to measure in zero events");

    println!("⏱  Recording {events} events into a temporary index");
    println!();

    let report = measure(events)?;

    println!(
        "  {:<22} {:>10}  {:>9}  {:>9}  {:>9}",
        "", "events/s", "p50", "p95", "p99"
    );
    for (label, measured) in [
        ("parse + redact", &report.parsed),
        ("record (index)", &report.recorded),
        ("record + transcript", &report.spooled),
    ] {
        println!(
            "  {label:<22} {:>10}  {:>9}  {:>9}  {:>9}",
            thousands(measured.per_second()),
            millis(measured.percentile(0.50)),
            millis(measured.percentile(0.95)),
            millis(measured.percentile(0.99)),
        );
    }

    println!();
    // What durability costs, said as a ratio rather than left to be worked
    // out from two rows: the transcript is the copy that outlives the index,
    // and this is its price.
    let index_rate = report.recorded.per_second();
    let spooled_rate = report.spooled.per_second();
    if spooled_rate > 0.0 && index_rate > spooled_rate {
        println!(
            "  The durable transcript costs {:.1}× — it is the copy that survives",
            index_rate / spooled_rate
        );
        println!("  losing the index, and it is written on the same thread.");
        println!();
    }
    // The number is only useful against the budget it has to fit in, and the
    // budget is the one the hook already keeps: a quarter of a second to
    // connect, one second to answer.
    let p95 = report.spooled.percentile(0.95);
    let in_a_second = if p95.is_zero() {
        f64::INFINITY
    } else {
        1.0 / p95.as_secs_f64()
    };
    println!(
        "  A hook gives up after one second. At p95 that is room for {} events,",
        thousands(in_a_second)
    );
    println!("  so on this machine recording is not what a session waits on.");
    println!();
    println!("  Measured against a temporary data directory: nothing was written to");
    println!("  this project's memory, and nothing here touches the server.");
    Ok(())
}

/// A rate, grouped so it can be read at a glance.
fn thousands(value: f64) -> String {
    if !value.is_finite() {
        return "—".to_owned();
    }
    let whole = value.round() as u64;
    let digits = whole.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// A duration, in the unit a per-event cost is read in.
fn millis(value: Duration) -> String {
    format!("{:.2}ms", value.as_secs_f64() * 1_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The benchmark has to record what it says it recorded. A timing loop
    /// that quietly wrote nothing would report a very good number.
    #[test]
    fn every_event_asked_for_is_recorded() {
        let report = measure(12).expect("measure");

        assert_eq!(report.parsed.events, 12);
        assert_eq!(report.recorded.events, 12);
        assert_eq!(report.spooled.events, 12);
        assert_eq!(report.recorded.sorted.len(), 12);
        assert!(
            report.recorded.per_second() > 0.0,
            "the recording pass measured no rate at all"
        );
    }

    /// And it has to leave nothing behind. A benchmark that filled somebody's
    /// memory with invented sessions would be worse than no benchmark.
    #[test]
    fn nothing_is_written_outside_the_temporary_directory() {
        let before = std::env::var_os(anamnesis_core::datadir::DATA_DIR_ENV);
        measure(4).expect("measure");
        let after = std::env::var_os(anamnesis_core::datadir::DATA_DIR_ENV);

        assert_eq!(before, after, "the benchmark changed where memory is kept");
    }

    #[test]
    fn percentiles_come_out_of_the_sorted_run() {
        let measured = Measured {
            events: 5,
            elapsed: Duration::from_millis(10),
            sorted: (1..=5).map(Duration::from_millis).collect(),
        };

        assert_eq!(measured.percentile(0.0), Duration::from_millis(1));
        assert_eq!(measured.percentile(0.5), Duration::from_millis(3));
        assert_eq!(measured.percentile(1.0), Duration::from_millis(5));
        assert_eq!(measured.per_second(), 500.0);
    }

    #[test]
    fn a_rate_is_grouped_so_it_can_be_read() {
        assert_eq!(thousands(7.0), "7");
        assert_eq!(thousands(942.0), "942");
        assert_eq!(thousands(3412.0), "3 412");
        assert_eq!(thousands(1234567.0), "1 234 567");
        assert_eq!(thousands(f64::INFINITY), "—");
    }

    #[test]
    fn zero_events_is_refused_rather_than_divided_by() {
        assert!(cmd_bench(0).is_err());
    }
}
