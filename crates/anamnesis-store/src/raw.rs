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

use anamnesis_core::observation::Observation;
use anamnesis_core::scope::Scope;
use anamnesis_core::session::Session;

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

    /// Where a session's transcript lives.
    pub fn locate(&self, scope: &Scope, session: &Session) -> PathBuf {
        let stamp = session.started_at.to_string();
        let date = stamp.split('T').next().unwrap_or("undated").to_owned();
        let short: String = session.id.to_string().chars().take(8).collect();
        self.root
            .join(scope.workspace.as_str())
            .join(scope.project.as_str())
            .join(date)
            .join(format!("{short}.jsonl"))
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
}
