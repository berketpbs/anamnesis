//! Events the hook could not deliver, kept until it can.
//!
//! A hook runs inside somebody's editing session and cannot wait: the budgets
//! in `cmd_hook` are a quarter of a second to connect and one to answer,
//! because the case that matters is the server being *down*, where a generous
//! timeout turns "memory is not running" into "the agent feels broken". The
//! price of those budgets used to be the event itself — a POST that failed
//! dropped what it was carrying on the floor.
//!
//! This repository has now lost capture twice that way: four days in August
//! while a server was not running, and nine hours on the day this was written.
//! Both were invisible until somebody went looking. The queue does not make
//! the server more reliable; it makes the hook's failure survivable, which is
//! the half that was missing.
//!
//! Two rules shape everything here.
//!
//! **Nothing unredacted is written.** The queue outlives the process that
//! wrote it, exactly like the raw spool on the server, so the same rule
//! applies: a secret that reaches this directory would be the most durable
//! copy in the system. Payloads are redacted *before* they are written, by
//! the same rules the server applies, which is safe to do twice because
//! redaction is idempotent and there is a test in `anamnesis-core` that says
//! so.
//!
//! **Order is kept.** A session is a sequence — it starts, does things, ends —
//! and replaying its middle before its beginning would produce a session the
//! index cannot make sense of. Files sort by the instant they were queued, and
//! a replay that fails stops rather than skipping ahead. The one event it does
//! step over is the one the server has already read and refused: waiting
//! cannot change that answer, and holding it would cost every event behind it
//! for as long as the queue exists.

use std::path::{Path, PathBuf};

use anamnesis_core::datadir::DataDir;
use anamnesis_core::sanitize::Redactor;
use serde::{Deserialize, Serialize};

/// Directory under the data dir where undelivered events wait.
const DIRECTORY: &str = "pending";

/// How many events the queue will hold before it stops accepting.
///
/// Reached only after a long outage: at roughly one event per tool call, this
/// is days of work. When it is reached the queue stops taking *new* events
/// rather than dropping old ones, because the old ones are the ones that make
/// a session coherent — a queue holding the end of every session and the
/// start of none is worse than a queue that is honestly full.
const CAPACITY: usize = 10_000;

/// One event, as it was when the hook could not deliver it.
#[derive(Debug, Serialize, Deserialize)]
pub struct Queued {
    /// Harness the event came from; the replay needs it for `?agent=`.
    pub agent: String,
    /// The payload, already redacted.
    pub body: serde_json::Value,
    /// The name this event was offered under the first time.
    ///
    /// Carried so the replay can offer it under the same one. A hook that
    /// gave up after a second may have given up on a server that recorded the
    /// event anyway, and the copy that goes out later has to be recognisable
    /// as the same event rather than land as a second prompt in the session.
    ///
    /// Optional because a queue written by an earlier version has entries
    /// without one, and they are still worth delivering.
    #[serde(default)]
    pub event: Option<String>,
}

/// The queue of events waiting to be delivered.
pub struct Queue {
    root: PathBuf,
}

impl Queue {
    /// The queue belonging to a data directory.
    ///
    /// Nothing is created until something is written: a hook whose server is
    /// up must not pay for a directory it never uses.
    pub fn new(data: &DataDir) -> Self {
        Self {
            root: data.root().join(DIRECTORY),
        }
    }

    /// Where the queue lives, for a report someone reads.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// How many events are waiting.
    pub fn len(&self) -> usize {
        self.files().len()
    }

    /// Whether nothing is waiting.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Keep an event that could not be delivered.
    ///
    /// The payload is redacted first and parsed as JSON, so what lands on disk
    /// is both safe to keep and certain to be replayable — a queue entry that
    /// turns out not to be JSON when it is read back would be a failure with
    /// no owner left to report it to.
    pub fn push(&self, agent: &str, event: &str, payload: &str) -> anyhow::Result<PathBuf> {
        self.push_within(agent, event, payload, CAPACITY)
    }

    /// [`Queue::push`], with the ceiling named.
    ///
    /// Separate only so the full-queue rule can be tested: reaching the real
    /// capacity in a test would mean writing ten thousand files to assert one
    /// sentence.
    fn push_within(
        &self,
        agent: &str,
        event: &str,
        payload: &str,
        capacity: usize,
    ) -> anyhow::Result<PathBuf> {
        let body: serde_json::Value = serde_json::from_str(payload)?;
        let body = redacted(body);

        if self.len() >= capacity {
            anyhow::bail!(
                "{} events are already waiting in {}; not keeping any more until they are delivered",
                capacity,
                self.root.display()
            );
        }

        std::fs::create_dir_all(&self.root)?;
        let entry = Queued {
            agent: agent.to_owned(),
            body,
            event: Some(event.to_owned()),
        };
        let text = serde_json::to_string(&entry)?;

        // Two events can be queued in the same millisecond — a tool call and
        // its result — so the name carries a counter as well. Written to a
        // temporary name and renamed, because a reader walking this directory
        // must never find half a file.
        let name = format!(
            "{}-{:04}.json",
            jiff::Timestamp::now().as_millisecond(),
            unique()
        );
        let path = self.root.join(&name);
        let temporary = self.root.join(format!("{name}.part"));
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, &path)?;
        Ok(path)
    }

    /// The oldest waiting events, up to `limit`.
    ///
    /// Sorted by name, which is the instant they were queued: the replay has
    /// to put a session's start back before its end.
    pub fn take(&self, limit: usize) -> Vec<(PathBuf, Queued)> {
        let mut out = Vec::new();
        for path in self.files().into_iter().take(limit) {
            match std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<Queued>(&text).ok())
            {
                Some(entry) => out.push((path, entry)),
                // A file this cannot read is a file nothing can replay.
                // Removing it is the only way the queue ever empties, and it
                // is named on the way out.
                None => {
                    eprintln!(
                        "anamnesis: discarding an unreadable queued event: {}",
                        path.display()
                    );
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        out
    }

    /// Forget an event that has been delivered.
    pub fn remove(&self, path: &Path) {
        let _ = std::fs::remove_file(path);
    }

    /// Queue files, oldest first.
    fn files(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        files.sort();
        files
    }
}

/// A counter that makes two names in the same millisecond differ.
fn unique() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static NEXT: AtomicU16 = AtomicU16::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Redact every string in a payload, leaving its shape alone.
///
/// String by string rather than over the serialised text: replacing inside the
/// JSON source could cross a quote or an escape and leave something that is no
/// longer parseable, and an unparseable queue entry is an event lost twice.
fn redacted(value: serde_json::Value) -> serde_json::Value {
    let redactor = Redactor::new();
    walk(value, &redactor)
}

fn walk(value: serde_json::Value, redactor: &Redactor) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => {
            serde_json::Value::String(redactor.redact(&text).into_text())
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(|item| walk(item, redactor)).collect())
        }
        serde_json::Value::Object(fields) => serde_json::Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key, walk(value, redactor)))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue() -> (tempfile::TempDir, Queue) {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = DataDir::resolve(Some(dir.path().to_path_buf())).expect("data dir");
        let queue = Queue::new(&data);
        (dir, queue)
    }

    #[test]
    fn an_event_that_could_not_be_delivered_comes_back_out() {
        let (_dir, queue) = queue();
        queue
            .push(
                "claude-code",
                "test-event-1",
                r#"{"hook_event_name":"SessionStart"}"#,
            )
            .expect("queued");

        let waiting = queue.take(10);

        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].1.agent, "claude-code");
        assert_eq!(waiting[0].1.body["hook_event_name"], "SessionStart");
    }

    /// The name the event was first offered under survives the file, because
    /// the attempt that failed may not have failed at the server: a hook that
    /// gave up after a second can have given up on a server that recorded the
    /// event anyway. Offered again under the same name it is one event; under
    /// a new one it is a prompt the session never had.
    #[test]
    fn a_waiting_event_is_offered_again_under_the_name_it_had() {
        let (_dir, queue) = queue();
        queue
            .push(
                "claude-code",
                "01998f3a-0000-7000-8000-00000000c0de",
                r#"{"hook_event_name":"UserPromptSubmit"}"#,
            )
            .expect("queued");

        let waiting = queue.take(10);

        assert_eq!(
            waiting[0].1.event.as_deref(),
            Some("01998f3a-0000-7000-8000-00000000c0de")
        );
    }

    /// A queue written before events had names still drains. Refusing to
    /// replay those entries would throw away exactly the events an upgrade
    /// found waiting.
    #[test]
    fn an_entry_from_before_events_had_names_is_still_readable() {
        let entry: Queued = serde_json::from_str(
            r#"{"agent":"claude-code","body":{"hook_event_name":"SessionStart"}}"#,
        )
        .expect("an older entry is still an entry");

        assert_eq!(entry.agent, "claude-code");
        assert_eq!(entry.event, None);
    }

    /// A session is a sequence, and replaying its middle before its beginning
    /// would leave the index with a session it cannot make sense of.
    #[test]
    fn the_oldest_event_comes_back_first() {
        let (_dir, queue) = queue();
        for n in 0..5 {
            queue
                .push("claude-code", "test-event-2", &format!(r#"{{"n":{n}}}"#))
                .expect("queued");
        }

        let waiting = queue.take(10);

        let order: Vec<i64> = waiting
            .iter()
            .map(|(_, entry)| entry.body["n"].as_i64().expect("n"))
            .collect();
        assert_eq!(order, [0, 1, 2, 3, 4]);
    }

    /// The whole reason this can exist at all: the queue outlives the process
    /// that wrote it, so it is held to the rule the server's raw spool is.
    #[test]
    fn a_secret_never_reaches_the_queue() {
        let (_dir, queue) = queue();
        let secret = "sk-proj-QUEUETESTSECRET0123456789abcd";

        let path = queue
            .push(
                "claude-code",
                "test-event-6",
                &serde_json::json!({ "prompt": format!("deploy with {secret}") }).to_string(),
            )
            .expect("queued");

        let written = std::fs::read_to_string(&path).expect("read");
        assert!(!written.contains(secret), "{written}");
        assert!(written.contains("[redacted:openai-key]"), "{written}");
    }

    #[test]
    fn a_delivered_event_is_forgotten() {
        let (_dir, queue) = queue();
        queue
            .push("claude-code", "test-event-3", r#"{"a":1}"#)
            .expect("queued");
        let waiting = queue.take(10);

        queue.remove(&waiting[0].0);

        assert!(queue.is_empty());
    }

    #[test]
    fn a_payload_that_is_not_json_is_refused_rather_than_kept() {
        let (_dir, queue) = queue();

        assert!(
            queue
                .push("claude-code", "test-event-4", "not json at all")
                .is_err()
        );
        assert!(queue.is_empty());
    }

    /// Nothing is created until something needs keeping: a hook whose server
    /// is up should not leave a directory behind for the trouble.
    #[test]
    fn an_unused_queue_leaves_nothing_on_disk() {
        let (_dir, queue) = queue();

        assert!(queue.is_empty());
        assert!(!queue.root().exists());
    }

    /// The direction the ceiling refuses in is the whole decision. Dropping
    /// the oldest to make room would leave a queue holding the end of every
    /// session and the start of none — coherent-looking and useless. Refusing
    /// the newest keeps what is already there replayable.
    #[test]
    fn a_full_queue_refuses_the_new_event_rather_than_dropping_the_old() {
        let (_dir, queue) = queue();
        queue
            .push_within("claude-code", "test-event-x", r#"{"n":0}"#, 2)
            .expect("queued");
        queue
            .push_within("claude-code", "test-event-x", r#"{"n":1}"#, 2)
            .expect("queued");

        let refused = queue.push_within("claude-code", "test-event-x", r#"{"n":2}"#, 2);

        assert!(refused.is_err());
        let waiting = queue.take(10);
        let order: Vec<i64> = waiting
            .iter()
            .map(|(_, entry)| entry.body["n"].as_i64().expect("n"))
            .collect();
        assert_eq!(order, [0, 1], "the two already waiting are untouched");
    }

    #[test]
    fn an_unreadable_entry_is_dropped_so_the_queue_can_empty() {
        let (_dir, queue) = queue();
        queue
            .push("claude-code", "test-event-5", r#"{"a":1}"#)
            .expect("queued");
        std::fs::create_dir_all(queue.root()).expect("dir");
        std::fs::write(queue.root().join("0000000000-9999.json"), "{ not json").expect("write");

        let waiting = queue.take(10);

        assert_eq!(waiting.len(), 1, "the good one is still delivered");
        assert_eq!(queue.len(), 1, "the unreadable one is gone");
    }
}
