//! The raw spool: an append-only transcript of every observation, on disk.
//!
//! The SQLite index is rebuildable from the wiki — but only the *pages* are.
//! The observations a page was compiled from live nowhere else, so deleting
//! `db/` today loses every session's raw material even though the compiled
//! summaries survive in git. That asymmetry is what this module closes:
//! `<data_dir>/raw/` holds the same observations as durable, human-readable
//! JSONL, written at capture time.
//!
//! Three properties matter and are enforced here rather than left to callers:
//!
//! * **Append-only.** A line is written once and never rewritten. Nothing in
//!   this module opens a file for anything but appending, so a corrupted or
//!   half-written line can only ever be the last one in a file.
//! * **Already sanitized.** Only observations that have been through
//!   redaction are accepted. The spool outlives the database and is meant to
//!   be readable by a person, so an unredacted secret landing here would be
//!   the most durable copy of that secret in the system.
//! * **Never fatal.** Spooling failures are reported to the caller, but the
//!   capture path treats them as non-fatal: losing the durable copy of an
//!   event is bad, losing the event itself because the disk was full is
//!   worse.
//!
//! One file per session, under `raw/<workspace>/<project>/<date>/<id>.jsonl`.
//! The date directory keeps any single directory from growing without bound;
//! the session id is in the filename because two sessions in one day is
//! ordinary.

use std::io::Write;
use std::path::{Path, PathBuf};

use anamnesis_core::ids::{ProjectId, SessionId};
use anamnesis_core::observation::Observation;
use anamnesis_core::scope::Scope;
use anamnesis_core::session::Session;
use jiff::Timestamp;

/// Errors produced while spooling.
#[derive(Debug, thiserror::Error)]
pub enum RawError {
    /// A filesystem operation failed.
    #[error("raw spool io error at {path}: {source}")]
    Io {
        /// Path the operation was attempted on.
        path: PathBuf,
        /// Underlying cause.
        #[source]
        source: std::io::Error,
    },

    /// A record could not be encoded.
    #[error("raw spool could not encode a record: {0}")]
    Encode(#[from] serde_json::Error),

    /// An observation that had not been through redaction was offered.
    ///
    /// Refused rather than written: see the module docs on why the spool is
    /// the worst possible place for an unredacted secret to land.
    #[error("refusing to spool an unsanitized observation")]
    Unsanitized,
}

/// One line of a spool file.
///
/// Self-describing so a reader can tell the header from the body without
/// depending on line position — a file whose first line was lost is still
/// readable as a sequence of observations.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum RawRecord {
    /// Written once, when a session's file is first created.
    Session(Box<Session>),
    /// One captured event.
    Observation(Box<Observation>),
}

/// The file in a scope's transcript directory naming the projects its
/// transcripts were recorded under before the scope was renamed.
///
/// Not `.jsonl`, so no walk of the spool ever mistakes it for a transcript.
const PREVIOUS_PROJECTS: &str = "previous-projects";

/// An append-only transcript store rooted at one directory.
#[derive(Debug, Clone)]
pub struct RawSpool {
    root: PathBuf,
}

impl RawSpool {
    /// Treat `root` as the spool directory. Nothing is created until a write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Root directory of the spool.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory every transcript recorded under `scope` is filed in.
    pub fn scope_dir(&self, scope: &Scope) -> PathBuf {
        self.root
            .join(scope.workspace.as_str())
            .join(scope.project.as_str())
    }

    /// Note, in `scope`'s directory, that its transcripts were recorded under
    /// `previous`, which was called `was`.
    ///
    /// A rename moves a project's transcripts to the directory of its new
    /// name, and there they stay exactly as written: a transcript is
    /// append-only, and a header rewritten to claim the new project would be
    /// the one line in the file nobody recorded. So the headers keep naming
    /// the old project, and this note is what lets a rebuild of the new one
    /// recognise them. Written before the directory moves, so it travels
    /// with the transcripts it speaks for.
    pub fn record_previous(
        &self,
        scope: &Scope,
        previous: ProjectId,
        was: &Scope,
    ) -> Result<(), RawError> {
        let dir = self.scope_dir(scope);
        let io = |source| RawError::Io {
            path: dir.join(PREVIOUS_PROJECTS),
            source,
        };
        std::fs::create_dir_all(&dir).map_err(io)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(PREVIOUS_PROJECTS))
            .map_err(io)?;
        // The id is what a rebuild reads; the name is for whoever opens the
        // file to find out what it is.
        writeln!(file, "{previous} {was}").map_err(io)?;
        Ok(())
    }

    /// The projects `scope`'s transcripts were recorded under before a
    /// rename, oldest first. Empty for a scope that was never renamed into.
    pub fn previous_projects(&self, scope: &Scope) -> Result<Vec<ProjectId>, RawError> {
        let path = self.scope_dir(scope).join(PREVIOUS_PROJECTS);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(RawError::Io { path, source }),
        };
        Ok(text
            .lines()
            .filter_map(|line| line.split_whitespace().next()?.parse().ok())
            .collect())
    }

    /// Where a session's transcript lives.
    pub fn locate(&self, scope: &Scope, session: &Session) -> PathBuf {
        self.locate_parts(scope, session.id, session.started_at)
    }

    /// Where a session's transcript lives, for a caller holding the two
    /// fields the path is built from rather than the whole session.
    ///
    /// Split out for the paths that read a session as a summary row — a
    /// listing, or a command that removes one — so that finding the
    /// transcript never costs a second query for columns the layout does not
    /// use.
    pub fn locate_parts(
        &self,
        scope: &Scope,
        session_id: SessionId,
        started_at: Timestamp,
    ) -> PathBuf {
        let stamp = started_at.to_string();
        let date = stamp.split('T').next().unwrap_or("undated").to_owned();
        let short: String = session_id.to_string().chars().take(8).collect();
        self.scope_dir(scope)
            .join(date)
            .join(format!("{short}.jsonl"))
    }

    /// Every file on disk that belongs to `session_id`, in any date directory.
    ///
    /// There should only ever be one, and [`RawSpool::locate`] names it. This
    /// exists for the ones that are already on disk: until the capture path
    /// wrote its header from the *stored* session, a session that ran past
    /// midnight was filed twice — once under the day it started and once under
    /// the day the next event arrived — and only the first was reachable by
    /// name. A `forget-session` that removed the file it could name left the
    /// other one, with somebody's prompts in it, after they had asked for it
    /// to be gone.
    pub fn locate_all(&self, scope: &Scope, session_id: SessionId) -> Vec<PathBuf> {
        let short: String = session_id.to_string().chars().take(8).collect();
        let name = format!("{short}.jsonl");
        let project = self.scope_dir(scope);

        let Ok(dates) = std::fs::read_dir(&project) else {
            return Vec::new();
        };
        let mut found: Vec<PathBuf> = dates
            .flatten()
            .map(|entry| entry.path().join(&name))
            .filter(|path| path.is_file())
            .collect();
        found.sort();
        found
    }

    /// Append one observation, writing the session header first if this is
    /// the session's first line.
    ///
    /// The header is written on file creation rather than on session start
    /// because hooks arrive out of order: the first event of a session is not
    /// reliably `SessionStart`, so "when the file does not exist yet" is the
    /// only moment guaranteed to happen exactly once.
    pub fn append(
        &self,
        scope: &Scope,
        session: &Session,
        observation: &Observation,
    ) -> Result<(), RawError> {
        if !observation.sanitized {
            return Err(RawError::Unsanitized);
        }

        let path = self.locate(scope, session);
        let parent = path
            .parent()
            .expect("a spool path always has a parent directory");
        std::fs::create_dir_all(parent).map_err(|source| RawError::Io {
            path: parent.to_path_buf(),
            source,
        })?;

        let is_new = !path.exists();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| RawError::Io {
                path: path.clone(),
                source,
            })?;

        let mut buffer = String::new();
        if is_new {
            buffer.push_str(&serde_json::to_string(&RawRecord::Session(Box::new(
                session.clone(),
            )))?);
            buffer.push('\n');
        }
        buffer.push_str(&serde_json::to_string(&RawRecord::Observation(Box::new(
            observation.clone(),
        )))?);
        buffer.push('\n');

        // One write call for both lines: a header and its first observation
        // cannot end up separated by a crash between them.
        file.write_all(buffer.as_bytes())
            .map_err(|source| RawError::Io {
                path: path.clone(),
                source,
            })
    }

    /// Read a session's transcript back.
    ///
    /// A line that will not parse is skipped rather than failing the read:
    /// the only line that can be malformed is a partially written last one,
    /// and losing it should not cost the reader every line before it.
    pub fn read_session(
        &self,
        scope: &Scope,
        session: &Session,
    ) -> Result<Vec<RawRecord>, RawError> {
        let path = self.locate(scope, session);
        self.read_file(&path)
    }

    /// Read one spool file by path.
    pub fn read_file(&self, path: &Path) -> Result<Vec<RawRecord>, RawError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(RawError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };

        Ok(text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect())
    }

    /// Run `redactor` over every string in one spool file, rewriting it with
    /// `apply`.
    ///
    /// The one exception to append-only, and the reason it is allowed: a rule
    /// added after a secret was captured is otherwise never applied to it, and
    /// the spool is the most durable copy there is. Everything else about the
    /// file is kept. Lines are decoded and the rules run over their string
    /// values, never over the encoded line — a rule like `authorization: bearer
    /// \S+` run across JSON would eat the closing quote and brace. A line that
    /// does not parse, which can only be a half-written last one, is kept
    /// byte for byte. Keys and structure are untouched.
    ///
    /// The rewrite goes to a sibling file that replaces the original, so a
    /// crash leaves one or the other whole. The server may be appending to
    /// today's files while this runs, so the length is checked again just
    /// before the replace: a file that grew is read again, up to three times,
    /// rather than losing the line that arrived.
    pub fn redact_file(
        &self,
        path: &Path,
        redactor: &anamnesis_core::sanitize::Redactor,
        apply: bool,
    ) -> Result<crate::redact::Redaction, RawError> {
        let io = |source| RawError::Io {
            path: path.to_path_buf(),
            source,
        };
        for _ in 0..3 {
            let before = std::fs::read_to_string(path).map_err(io)?;
            let mut found = crate::redact::Redaction::default();
            let mut rewritten = String::with_capacity(before.len());

            for line in before.split_inclusive('\n') {
                let content = line.trim_end_matches(['\n', '\r']);
                let ending = &line[content.len()..];
                if content.trim().is_empty() {
                    rewritten.push_str(line);
                    continue;
                }
                let Ok(mut value) = serde_json::from_str::<serde_json::Value>(content) else {
                    rewritten.push_str(line);
                    continue;
                };
                found.examined += 1;
                let mut hits = Vec::new();
                redact_strings(&mut value, redactor, &mut hits);
                if hits.is_empty() {
                    rewritten.push_str(line);
                } else {
                    found.count(&hits);
                    rewritten.push_str(&serde_json::to_string(&value)?);
                    rewritten.push_str(ending);
                }
            }

            if !apply || found.changed == 0 {
                return Ok(found);
            }

            let staged = path.with_extension("jsonl.redacting");
            std::fs::write(&staged, rewritten.as_bytes()).map_err(io)?;
            let now = std::fs::metadata(path).map_err(io)?.len();
            if now != before.len() as u64 {
                let _ = std::fs::remove_file(&staged);
                continue;
            }
            std::fs::rename(&staged, path).map_err(io)?;
            return Ok(found);
        }
        Err(RawError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(
                "the file kept growing while it was being rewritten; try again when it is quiet",
            ),
        })
    }

    /// Every spool file under the root, oldest path first.
    ///
    /// Used by a rebuild, which has no database to ask what sessions exist.
    pub fn files(&self) -> Result<Vec<PathBuf>, RawError> {
        let mut found = Vec::new();
        collect_jsonl(&self.root, &mut found)?;
        found.sort();
        Ok(found)
    }
}

/// Redact every string value inside `value`, in place, collecting rule names.
fn redact_strings(
    value: &mut serde_json::Value,
    redactor: &anamnesis_core::sanitize::Redactor,
    hits: &mut Vec<&'static str>,
) {
    match value {
        serde_json::Value::String(text) => {
            let redacted = redactor.redact(text);
            // Only when the text changes: see `Store::redact_observations`
            // on rules that match what they already masked.
            if !redacted.is_clean() && redacted.text() != text.as_str() {
                hits.extend_from_slice(redacted.hits());
                *text = redacted.into_text();
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_strings(item, redactor, hits);
            }
        }
        serde_json::Value::Object(fields) => {
            for (_, field) in fields.iter_mut() {
                redact_strings(field, redactor, hits);
            }
        }
        _ => {}
    }
}

/// Walk `dir` collecting `.jsonl` files.
///
/// A missing root is an empty spool, not an error: nothing has been captured
/// yet is the state every new installation starts in.
fn collect_jsonl(dir: &Path, found: &mut Vec<PathBuf>) -> Result<(), RawError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(RawError::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    };

    for entry in entries {
        let entry = entry.map_err(|source| RawError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, found)?;
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            found.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Files already on disk from before a session was filed under one date.
    /// The point is the stray: it is unreachable by name, and something has to
    /// be able to find it in order to delete it.
    #[test]
    fn every_file_belonging_to_a_session_is_found_across_dates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = RawSpool::new(dir.path());
        let scope = Scope {
            workspace: WorkspaceName::default(),
            project: ProjectName::sanitized("widget").expect("project"),
        };
        let id = SessionId::new();
        let short: String = id.to_string().chars().take(8).collect();

        for date in ["2026-08-31", "2026-09-01"] {
            let day = dir
                .path()
                .join(scope.workspace.as_str())
                .join(scope.project.as_str())
                .join(date);
            std::fs::create_dir_all(&day).expect("day");
            std::fs::write(day.join(format!("{short}.jsonl")), "{}\n").expect("file");
            // A different session on the same day must not be swept up with it.
            std::fs::write(day.join("deadbeef.jsonl"), "{}\n").expect("other");
        }

        let found = spool.locate_all(&scope, id);

        assert_eq!(found.len(), 2, "{found:?}");
        assert!(
            found
                .iter()
                .all(|path| path.ends_with(format!("{short}.jsonl")))
        );
    }
    use anamnesis_core::ids::{ProjectId, SessionId, WorkspaceId};
    use anamnesis_core::observation::{BoundedBody, EventKind};
    use anamnesis_core::scope::{ProjectName, WorkspaceName};
    use anamnesis_core::session::AgentKind;
    use jiff::Timestamp;

    fn scope() -> Scope {
        Scope {
            workspace: WorkspaceName::parse("default").unwrap(),
            project: ProjectName::parse("widget").unwrap(),
        }
    }

    fn now() -> Timestamp {
        "2026-08-25T09:00:00Z".parse().unwrap()
    }

    fn session() -> Session {
        let project = ProjectId::from_uuid(uuid::Uuid::nil());
        crate::new_session(
            SessionId::derive(project, "agent-session-1"),
            project,
            WorkspaceId::from_uuid(uuid::Uuid::nil()),
            AgentKind::ClaudeCode,
            "/repo".into(),
            now(),
            None,
        )
    }

    fn observation(body: &str) -> Observation {
        crate::new_observation(
            session().id,
            EventKind::UserPrompt,
            None,
            BoundedBody::truncating(body, 1024),
            now(),
        )
    }

    #[test]
    fn a_session_file_starts_with_its_header_then_its_observations() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RawSpool::new(dir.path());
        let session = session();

        spool
            .append(&scope(), &session, &observation("first"))
            .unwrap();
        spool
            .append(&scope(), &session, &observation("second"))
            .unwrap();

        let records = spool.read_session(&scope(), &session).unwrap();
        assert_eq!(records.len(), 3, "one header plus two observations");
        assert!(matches!(records[0], RawRecord::Session(_)));

        let bodies: Vec<String> = records
            .iter()
            .filter_map(|record| match record {
                RawRecord::Observation(o) => Some(o.body.as_str().to_owned()),
                RawRecord::Session(_) => None,
            })
            .collect();
        assert_eq!(bodies, vec!["first".to_owned(), "second".to_owned()]);
    }

    #[test]
    fn the_header_is_written_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RawSpool::new(dir.path());
        let session = session();

        for _ in 0..5 {
            spool.append(&scope(), &session, &observation("x")).unwrap();
        }

        let headers = spool
            .read_session(&scope(), &session)
            .unwrap()
            .into_iter()
            .filter(|record| matches!(record, RawRecord::Session(_)))
            .count();
        assert_eq!(headers, 1);
    }

    #[test]
    fn an_unsanitized_observation_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RawSpool::new(dir.path());
        let session = session();
        let mut observation = observation("AWS_SECRET_ACCESS_KEY=nope");
        observation.sanitized = false;

        let result = spool.append(&scope(), &session, &observation);
        assert!(matches!(result, Err(RawError::Unsanitized)));
        // And nothing was created on the way to refusing.
        assert!(spool.read_session(&scope(), &session).unwrap().is_empty());
    }

    #[test]
    fn a_truncated_last_line_does_not_cost_the_lines_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RawSpool::new(dir.path());
        let session = session();
        spool
            .append(&scope(), &session, &observation("kept"))
            .unwrap();

        // Simulate a crash mid-write.
        let path = spool.locate(&scope(), &session);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{\"type\":\"observation\",\"partial")
            .unwrap();
        drop(file);

        let records = spool.read_session(&scope(), &session).unwrap();
        assert_eq!(records.len(), 2, "header and the one complete observation");
    }

    #[test]
    fn reading_a_session_that_was_never_spooled_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RawSpool::new(dir.path());
        assert!(spool.read_session(&scope(), &session()).unwrap().is_empty());
    }

    #[test]
    fn files_finds_every_transcript_and_an_absent_root_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RawSpool::new(dir.path().join("never-created"));
        assert!(spool.files().unwrap().is_empty());

        let spool = RawSpool::new(dir.path());
        let mut second = session();
        second.id = SessionId::derive(ProjectId::from_uuid(uuid::Uuid::nil()), "agent-session-2");
        spool
            .append(&scope(), &session(), &observation("a"))
            .unwrap();
        spool.append(&scope(), &second, &observation("b")).unwrap();

        assert_eq!(spool.files().unwrap().len(), 2);
    }

    #[test]
    fn transcripts_land_under_their_scope_and_date() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RawSpool::new(dir.path());
        let session = session();
        spool.append(&scope(), &session, &observation("x")).unwrap();

        let path = spool.locate(&scope(), &session);
        assert!(path.starts_with(dir.path().join("default").join("widget").join("2026-08-25")));
        assert!(path.is_file());
    }

    /// Not a key; the shape the `AQ.` rule masks.
    const KEY: &str = "AQ.testonlynotarealkey0123456789abcdefghijklmn";

    /// A file as it looked before the rule existed: the observation was
    /// sanitized by the rules there were, and the key went through.
    fn spooled_before_the_rule(dir: &Path) -> (RawSpool, PathBuf) {
        let spool = RawSpool::new(dir);
        let session = session();
        spool
            .append(
                &scope(),
                &session,
                &observation(&format!("use {KEY} for gemini")),
            )
            .unwrap();
        spool
            .append(&scope(), &session, &observation("cargo test"))
            .unwrap();
        let path = spool.locate(&scope(), &session);
        // A half-written last line, which must survive byte for byte.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{\"type\":\"observ").unwrap();
        (spool, path)
    }

    #[test]
    fn a_dry_run_over_a_spool_file_counts_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (spool, path) = spooled_before_the_rule(dir.path());
        let before = std::fs::read(&path).unwrap();

        let found = spool
            .redact_file(&path, &anamnesis_core::sanitize::Redactor::new(), false)
            .unwrap();

        assert_eq!(
            found.examined, 3,
            "header and two observations, not the torn line"
        );
        assert_eq!(found.changed, 1);
        assert_eq!(found.rules.get("google-auth-key"), Some(&1));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    /// The key is gone, every line still parses as the record it was, the
    /// untouched lines are the same bytes, and the torn line is still there.
    #[test]
    fn applying_rewrites_only_the_value_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let (spool, path) = spooled_before_the_rule(dir.path());
        let before = std::fs::read_to_string(&path).unwrap();

        spool
            .redact_file(&path, &anamnesis_core::sanitize::Redactor::new(), true)
            .unwrap();
        let after = std::fs::read_to_string(&path).unwrap();

        assert!(!after.contains(KEY));
        assert!(after.contains("[redacted:google-auth-key]"));
        let before_lines: Vec<&str> = before.lines().collect();
        let after_lines: Vec<&str> = after.lines().collect();
        assert_eq!(before_lines.len(), after_lines.len());
        assert_eq!(before_lines[0], after_lines[0], "the header was untouched");
        assert_eq!(
            before_lines[2], after_lines[2],
            "the clean observation was untouched"
        );
        assert!(
            after.ends_with("{\"type\":\"observ"),
            "the torn line survived"
        );
        let records = spool.read_file(&path).unwrap();
        assert_eq!(records.len(), 3, "every whole line still reads back");
        assert!(!path.with_extension("jsonl.redacting").exists());

        let again = spool
            .redact_file(&path, &anamnesis_core::sanitize::Redactor::new(), true)
            .unwrap();
        assert_eq!(again.changed, 0);
    }

    #[test]
    fn a_scope_never_renamed_into_has_no_previous_projects() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RawSpool::new(dir.path());

        assert!(spool.previous_projects(&scope()).unwrap().is_empty());
    }

    /// Renamed twice, a directory holds transcripts from both earlier
    /// projects, and the note has to name both.
    #[test]
    fn previous_projects_accumulate_across_renames() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RawSpool::new(dir.path());
        let first = ProjectId::from_uuid(uuid::Uuid::from_u128(1));
        let second = ProjectId::from_uuid(uuid::Uuid::from_u128(2));

        spool.record_previous(&scope(), first, &scope()).unwrap();
        spool.record_previous(&scope(), second, &scope()).unwrap();

        assert_eq!(spool.previous_projects(&scope()).unwrap(), [first, second]);
    }

    /// The note sits among the transcripts and must never be read as one.
    #[test]
    fn the_note_is_not_a_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RawSpool::new(dir.path());
        spool
            .record_previous(&scope(), ProjectId::from_uuid(uuid::Uuid::nil()), &scope())
            .unwrap();

        assert!(spool.files().unwrap().is_empty());
    }
}
