//! What the server's log file last said about the server itself.
//!
//! `status` and `service status` can tell that nothing answers on the port.
//! They could not tell why, and the why was sitting in `logs/`: a start that
//! failed, a panic, a console that was closed — or nothing at all after a line
//! saying the server was running, which is how a killed process and a machine
//! that went down look from the inside. On the morning this was written the
//! scheduled task had been starting a server every minute for an hour, each
//! start refused, and the only thing either command said was "not running".
//!
//! Only the server writes this file, so nothing here guesses whose line it is.
//! Nothing here writes anything either: it reads the newest files and phrases.

use std::path::Path;

use jiff::Timestamp;

use crate::format::describe_age;

/// The last thing the log recorded about the server's own life.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LastWord {
    /// A start was attempted and nothing about it followed.
    Starting(Timestamp),
    /// It was listening, and the log went on — or ended — without it stopping.
    /// The time is that of the last line it wrote, of any kind.
    Serving(Timestamp),
    /// It stopped cleanly, for the reason it gave.
    Stopped(Timestamp, String),
    /// It stopped with an error, before or after it had started serving.
    Failed(Timestamp, String),
    /// It panicked.
    Panicked(Timestamp, String),
}

/// How many of the newest log files are read for a last word.
///
/// One day's file can hold only lines about hook payloads; two is enough to
/// reach across midnight, and reading every file for a status line is not.
const FILES_READ: usize = 2;

/// The last word in the newest log files under `logs`, if there is one.
pub fn last_word(logs: &Path) -> Option<LastWord> {
    let mut files: Vec<_> = std::fs::read_dir(logs)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("anamnesis.") && name.ends_with(".log"))
        })
        .collect();
    // The date is in the name, so the name sorts them; modification times are
    // what a copy or a restore changes.
    files.sort();

    // A server that has been up since yesterday writes today's file with
    // nothing in it but lines about hooks. Its word is in yesterday's file, and
    // the time it was last heard from is in today's.
    let mut last_heard = None;
    for file in files.iter().rev().take(FILES_READ) {
        let Ok(bytes) = std::fs::read(file) else {
            continue;
        };
        let (word, heard) = read_last_word(&bytes);
        last_heard = last_heard.or(heard);
        match (word, last_heard) {
            (Some(LastWord::Serving(_)), Some(heard)) => return Some(LastWord::Serving(heard)),
            (Some(word), _) => return Some(word),
            (None, _) => {}
        }
    }
    None
}

/// The last word in one file's bytes, and the time of its last line of any
/// kind.
///
/// Lossy, because a line cut short by a crash can end inside a character, and
/// the lines before it are the ones this exists to read.
fn read_last_word(bytes: &[u8]) -> (Option<LastWord>, Option<Timestamp>) {
    let text = String::from_utf8_lossy(bytes);
    let mut word = None;
    let mut heard = None;
    for line in text.lines() {
        let Some((at, message)) = parse_line(line) else {
            continue;
        };
        heard = Some(at);
        word = match message {
            _ if message.starts_with("anamnesis server starting") => Some(LastWord::Starting(at)),
            _ if message.starts_with("anamnesis listening") => Some(LastWord::Serving(at)),
            _ if message.starts_with("anamnesis stopped") => Some(LastWord::Stopped(
                at,
                field(message, "cause").unwrap_or_else(|| "no reason given".to_owned()),
            )),
            _ if message.starts_with("serve stopped") => Some(LastWord::Failed(
                at,
                field(message, "error").unwrap_or_else(|| "no error recorded".to_owned()),
            )),
            _ if message.starts_with("settings file refused a line") => Some(LastWord::Failed(
                at,
                format!(
                    "settings.env refused a line: {}",
                    field(message, "problem").unwrap_or_default()
                ),
            )),
            _ if message.starts_with("panicked") => Some(LastWord::Panicked(
                at,
                field(message, "message").unwrap_or_else(|| "no message".to_owned()),
            )),
            // Any other line after it was listening says it was still there to
            // write it. One between starting and listening — an embedder that
            // did not answer, say — says only that the start went on.
            _ => match word {
                Some(LastWord::Serving(_)) => Some(LastWord::Serving(at)),
                other => other,
            },
        };
    }
    (word, heard)
}

/// A line's time and the message after its level and target, or `None` for a
/// line that is not the start of an event — the continuation of a multi-line
/// error, say.
///
/// The shape is the one `tracing_subscriber::fmt` writes without colour:
/// `2026-09-14T02:03:06.123456Z ERROR anamnesis: serve stopped error=...`.
fn parse_line(line: &str) -> Option<(Timestamp, &str)> {
    let (stamp, rest) = line.split_once(' ')?;
    let at = stamp.parse::<Timestamp>().ok()?;
    let rest = rest.trim_start();
    let (_level, rest) = rest.split_once(' ')?;
    let rest = rest.trim_start();
    let (_target, message) = rest.split_once(": ")?;
    Some((at, message))
}

/// The value of `name=` in a message: quoted, or running to the next field.
///
/// Display values are written bare and may hold spaces, so a bare value ends
/// where the next ` key=` begins. The fields each event here carries are
/// known, and the ones that follow a bare value are plain words.
fn field(message: &str, name: &str) -> Option<String> {
    let start = message.find(&format!("{name}="))? + name.len() + 1;
    let value = &message[start..];
    if let Some(quoted) = value.strip_prefix('"') {
        return Some(quoted.split('"').next().unwrap_or_default().to_owned());
    }
    let end = [" location=", " thread="]
        .iter()
        .filter_map(|next| value.find(next))
        .min()
        .unwrap_or(value.len());
    Some(value[..end].trim().to_owned())
}

/// One line for a server that did not answer, from what its log last said.
pub fn describe(word: &LastWord, now: Timestamp) -> String {
    match word {
        LastWord::Failed(at, error) => {
            format!(
                "the last start, {}, failed — {error}",
                describe_age(*at, now)
            )
        }
        LastWord::Panicked(at, message) => {
            format!("it panicked {} — {message}", describe_age(*at, now))
        }
        LastWord::Stopped(at, cause) => {
            format!("it stopped {} — {cause}", describe_age(*at, now))
        }
        LastWord::Serving(at) => format!(
            "its log ends {} with it running, and no start since: it was ended without a \
             word — killed, or the machine went down",
            describe_age(*at, now)
        ),
        LastWord::Starting(at) => format!(
            "a start {} got no further than starting, and left no reason",
            describe_age(*at, now)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(raw: &str) -> Timestamp {
        raw.parse().expect("timestamp")
    }

    /// Lines as the server writes them, copied from this machine's log.
    const STARTED: &str = "2026-09-13T21:16:10.692098Z  INFO anamnesis::serve: anamnesis server starting version=\"1.1.0\" address=127.0.0.1:8080 data_dir=C:\\data";
    const LISTENING: &str =
        "2026-09-13T21:16:10.693273Z  INFO anamnesis_web: anamnesis listening bind=127.0.0.1:8080";
    const HOOK: &str = "2026-09-13T21:16:16.480455Z  INFO anamnesis_web: redacted secrets from hook payload rules=[\"assignment\"]";

    fn word(lines: &[&str]) -> Option<LastWord> {
        read_last_word(lines.join("\n").as_bytes()).0
    }

    /// The morning this was written: running, a line about a hook, and then
    /// nothing — the machine rebooted.
    #[test]
    fn a_log_that_ends_while_serving_says_the_server_was_ended_without_a_word() {
        let last = word(&[STARTED, LISTENING, HOOK]).expect("a word");
        assert_eq!(last, LastWord::Serving(at("2026-09-13T21:16:16.480455Z")));

        let line = describe(&last, at("2026-09-13T22:17:00Z"));
        assert!(line.contains("1h ago"), "{line}");
        assert!(line.contains("killed, or the machine went down"), "{line}");
    }

    /// And what the same morning looks like with a server that logs a failed
    /// start: the reason, in the words the server used.
    #[test]
    fn a_start_that_failed_is_reported_with_its_error() {
        let failed = "2026-09-14T02:03:06.000000Z ERROR anamnesis: serve stopped error=127.0.0.1:8080 is already taken — something is listening there. (os error 10048)";
        let last = word(&[STARTED, LISTENING, HOOK, STARTED, failed]).expect("a word");

        let LastWord::Failed(when, error) = &last else {
            panic!("{last:?}");
        };
        assert_eq!(*when, at("2026-09-14T02:03:06Z"));
        assert!(
            error.starts_with("127.0.0.1:8080 is already taken"),
            "{error}"
        );
        assert!(error.ends_with("(os error 10048)"), "{error}");
        assert!(
            describe(&last, at("2026-09-14T02:05:06Z")).contains("the last start, 2m ago, failed")
        );
    }

    /// A quoted cause is read without its quotes, and a clean stop is not a
    /// failure: it is a different fix, usually "start it again".
    #[test]
    fn a_clean_stop_names_its_cause() {
        let stopped = "2026-08-30T10:00:00.000000Z  INFO anamnesis_web: anamnesis stopped cause=\"the console was closed\"";
        assert_eq!(
            word(&[STARTED, LISTENING, stopped]),
            Some(LastWord::Stopped(
                at("2026-08-30T10:00:00Z"),
                "the console was closed".to_owned()
            ))
        );
    }

    /// A panic's message is bare and has spaces; it ends where its location
    /// begins.
    #[test]
    fn a_panic_is_read_up_to_where_it_happened() {
        let panicked = "2026-09-10T08:00:00.000000Z ERROR anamnesis: panicked message=index out of range for slice location=crates/x.rs:10:5 thread=\"anamnesis\"";
        assert_eq!(
            word(&[STARTED, LISTENING, panicked]),
            Some(LastWord::Panicked(
                at("2026-09-10T08:00:00Z"),
                "index out of range for slice".to_owned()
            ))
        );
    }

    /// A start is attempted after a failure: the failure is the past, and the
    /// start with nothing after it is what is happening now.
    #[test]
    fn a_start_after_a_failure_is_the_last_word() {
        let failed = "2026-09-14T02:03:06.000000Z ERROR anamnesis: serve stopped error=boom";
        let again = STARTED.replace("2026-09-13T21:16:10", "2026-09-14T02:04:06");
        assert_eq!(
            word(&[failed, &again]),
            Some(LastWord::Starting(at("2026-09-14T02:04:06.692098Z")))
        );
    }

    /// The continuation of a multi-line error is not an event, and the refusal
    /// to serve a network address without a token has one.
    #[test]
    fn lines_that_do_not_start_an_event_are_skipped() {
        let failed = "2026-09-14T02:03:06.000000Z ERROR anamnesis: serve stopped error=refusing to serve 0.0.0.0:8080 with no token configured.";
        let last = word(&[STARTED, failed, "", "Everything this server holds —"]).expect("a word");
        assert_eq!(
            last,
            LastWord::Failed(
                at("2026-09-14T02:03:06Z"),
                "refusing to serve 0.0.0.0:8080 with no token configured.".to_owned()
            )
        );
    }

    /// A file with only hook lines in it says nothing about the server, and
    /// the file before it is read instead — but the server was last heard from
    /// in the newer one.
    #[test]
    fn a_server_up_since_yesterday_was_last_heard_from_today() {
        let logs = tempfile::tempdir().expect("logs");
        std::fs::write(
            logs.path().join("anamnesis.2026-09-13.log"),
            [STARTED, LISTENING].join("\n"),
        )
        .expect("write");
        let today = HOOK.replace("2026-09-13T21:16:16", "2026-09-14T09:00:00");
        std::fs::write(
            logs.path().join("anamnesis.2026-09-14.log"),
            format!("not a line\n{today}\n"),
        )
        .expect("write");
        std::fs::write(logs.path().join("serve-stderr.log"), STARTED).expect("write");

        assert_eq!(
            last_word(logs.path()),
            Some(LastWord::Serving(at("2026-09-14T09:00:00.480455Z")))
        );
    }

    /// A line between starting and listening is the start going on, not the
    /// server serving: killed there, it never took the port.
    #[test]
    fn a_warning_during_a_start_is_not_serving() {
        let warned = "2026-09-13T21:16:10.692500Z  WARN anamnesis::serve: http://127.0.0.1:11434/v1/embeddings did not answer";
        assert_eq!(
            word(&[STARTED, warned]),
            Some(LastWord::Starting(at("2026-09-13T21:16:10.692098Z")))
        );
    }

    #[test]
    fn no_log_directory_is_no_word() {
        let logs = tempfile::tempdir().expect("logs");
        assert_eq!(last_word(&logs.path().join("missing")), None);
        assert_eq!(last_word(logs.path()), None);
    }
}
