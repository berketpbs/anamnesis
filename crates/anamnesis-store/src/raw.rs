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
//!
//! **Compacted once it goes quiet.** The spool is the one part of memory that
//! only grows: nothing is ever dropped from it, and on the machine this was
//! written on it held 25 MB after a month, all of it JSON that gzip takes to
//! 4.5. So a transcript nothing has been added to for [`COMPACT_AFTER`] is
//! folded into `<id>.jsonl.gz` beside it — every line kept byte for byte, only
//! the container changes, and still readable by any gzip tool. A session that
//! is resumed afterwards starts a fresh `<id>.jsonl` with its own header, as
//! any new file does, and every reader here reads the two together.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use parking_lot::Mutex;

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

/// How long a transcript has to go without a line before it is compacted.
///
/// A week, because a session can be resumed: `claude --resume` picks up a
/// conversation days later, and a transcript compacted the evening it went
/// quiet would split the next morning into a compressed half and a plain one.
/// That is handled — see the module docs — but it is not what the ordinary
/// case should look like.
pub const COMPACT_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// What a compaction pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Compaction {
    /// Transcripts folded into their compressed sibling.
    pub files: usize,
    /// Their plain size before.
    pub bytes_before: u64,
    /// The size of the compressed files they were folded into, after.
    pub bytes_after: u64,
}

/// An append-only transcript store rooted at one directory.
///
/// Clones share one lock, which is what keeps [`RawSpool::append`] and
/// [`RawSpool::compact`] from interleaving inside a server: a line appended
/// to a transcript between being read for compaction and being removed would
/// otherwise be lost.
#[derive(Debug, Clone)]
pub struct RawSpool {
    root: PathBuf,
    writing: Arc<Mutex<()>>,
}

impl RawSpool {
    /// Treat `root` as the spool directory. Nothing is created until a write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            writing: Arc::new(Mutex::new(())),
        }
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
    ///
    /// Compacted transcripts are found with the plain ones, for the same
    /// reason: a compressed file is still somebody's prompts.
    pub fn locate_all(&self, scope: &Scope, session_id: SessionId) -> Vec<PathBuf> {
        let short: String = session_id.to_string().chars().take(8).collect();
        let name = format!("{short}.jsonl");
        let project = self.scope_dir(scope);

        let Ok(dates) = std::fs::read_dir(&project) else {
            return Vec::new();
        };
        let mut found: Vec<PathBuf> = dates
            .flatten()
            .flat_map(|entry| {
                let plain = entry.path().join(&name);
                [compressed_of(&plain), plain]
            })
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

        // Held to the end: see [`RawSpool::compact`]. A transcript that was
        // compacted has no plain file, so the line opens a new one, header
        // first — which is what keeps that file readable on its own.
        let _writing = self.writing.lock();
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

    /// Read a session's transcript back: its compacted part, if it has one,
    /// then what was appended since.
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
        let mut records = self.read_file(&compressed_of(&path))?;
        records.extend(self.read_file(&path)?);
        Ok(records)
    }

    /// Read one spool file by path, compacted or not.
    pub fn read_file(&self, path: &Path) -> Result<Vec<RawRecord>, RawError> {
        let text = match read_text(path) {
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

    /// How many lines one spool file holds, compacted or not, for a person
    /// deciding whether to remove it. `None` when it cannot be read.
    pub fn lines_in(&self, path: &Path) -> Option<usize> {
        read_text(path).ok().map(|text| text.lines().count())
    }

    /// Fold one plain transcript into its compressed sibling.
    ///
    /// The second exception to append-only, after [`RawSpool::redact_file`],
    /// and a narrower one: no line changes. The compressed file afterwards
    /// holds what it held before, then the plain file's lines, byte for byte —
    /// less any line it already held, which is how a compaction interrupted
    /// between writing the one and removing the other is finished rather than
    /// doubled. No line has a legitimate twin: every observation carries its
    /// own id.
    ///
    /// Written to a sibling that replaces the compressed file, so a crash
    /// leaves one or the other whole; and the plain file is removed last, and
    /// only if it is the size it was when it was read. The lock
    /// [`RawSpool::append`] takes is held throughout, so inside one server
    /// nothing is appended in between; the size check is for a writer that
    /// is not this one.
    ///
    /// Returns whether the plain file was folded in.
    pub fn compact(&self, plain: &Path) -> Result<bool, RawError> {
        let _writing = self.writing.lock();
        match stage(plain)? {
            Some(staged) => finish(staged),
            None => Ok(false),
        }
    }

    /// Compact every plain transcript nothing has been added to since
    /// `quiet` before `now`.
    ///
    /// Quiet is read from the file itself, not from a session row: the spool
    /// is the part of memory that outlives the index, and this has to work in
    /// a data directory whose index was lost. A transcript that fails is
    /// logged and left as it was; the pass carries on.
    pub fn compact_quiet(&self, quiet: Duration, now: SystemTime) -> Result<Compaction, RawError> {
        let mut done = Compaction::default();
        for path in self.files()? {
            if is_compressed(&path) {
                continue;
            }
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            let idle = metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|idle| idle >= quiet);
            if !idle {
                continue;
            }
            match self.compact(&path) {
                Ok(true) => {
                    done.files += 1;
                    done.bytes_before += metadata.len();
                    done.bytes_after += std::fs::metadata(compressed_of(&path))
                        .map(|packed| packed.len())
                        .unwrap_or(0);
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, "could not compact a transcript; left as it was");
                }
            }
        }
        Ok(done)
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
    ///
    /// A compacted transcript is rewritten compacted. A rule added after a
    /// secret was captured has to reach the copy that has been sitting
    /// compressed for a month as much as today's.
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
        let compressed = is_compressed(path);
        for _ in 0..3 {
            let read_at = std::fs::metadata(path).map_err(io)?.len();
            let before = read_text(path).map_err(io)?;
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

            let staged = if compressed {
                path.with_extension("gz.redacting")
            } else {
                path.with_extension("jsonl.redacting")
            };
            if compressed {
                let file = std::fs::File::create(&staged).map_err(io)?;
                let mut encoder =
                    flate2::write::GzEncoder::new(file, flate2::Compression::default());
                encoder.write_all(rewritten.as_bytes()).map_err(io)?;
                encoder.finish().map_err(io)?.sync_all().map_err(io)?;
            } else {
                std::fs::write(&staged, rewritten.as_bytes()).map_err(io)?;
            }
            let now = std::fs::metadata(path).map_err(io)?.len();
            if now != read_at {
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

/// Where a plain transcript's compacted lines are kept: `<id>.jsonl.gz`.
fn compressed_of(plain: &Path) -> PathBuf {
    plain.with_extension("jsonl.gz")
}

/// A compaction written to its staging file and not yet put in place.
#[derive(Debug)]
struct Staged {
    plain: PathBuf,
    packed: PathBuf,
    staged: PathBuf,
    /// The plain file's length when it was read.
    read_at: u64,
    /// The compressed file as it was when it was read, if there was one.
    packed_at: Option<(u64, Option<SystemTime>)>,
}

/// The first half of [`RawSpool::compact`]: read both files and write what
/// the compressed one will hold to a staging file beside it. `None` when there
/// is no plain file to fold in.
fn stage(plain: &Path) -> Result<Option<Staged>, RawError> {
    let io = |source| RawError::Io {
        path: plain.to_path_buf(),
        source,
    };
    let read_at = match std::fs::metadata(plain) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io(source)),
    };
    let tail = std::fs::read(plain).map_err(io)?;
    let packed = compressed_of(plain);
    let packed_at = fingerprint(&packed);
    let head = match std::fs::File::open(&packed) {
        Ok(file) => {
            let mut head = Vec::new();
            flate2::read::MultiGzDecoder::new(file)
                .read_to_end(&mut head)
                .map_err(io)?;
            head
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(source) => return Err(io(source)),
    };

    let merged = merge_lines(&head, &tail);
    let staged = packed.with_extension("gz.compacting");
    let file = std::fs::File::create(&staged).map_err(io)?;
    let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    encoder.write_all(&merged).map_err(io)?;
    encoder.finish().map_err(io)?.sync_all().map_err(io)?;
    Ok(Some(Staged {
        plain: plain.to_path_buf(),
        packed,
        staged,
        read_at,
        packed_at,
    }))
}

/// The second half: put the staged file in place and remove the plain one —
/// unless either has been written to since [`stage`] read it.
///
/// Whoever else touched either file wins, and the transcript waits for the
/// next pass. Another process can: `redact` rewrites a compressed transcript
/// in place, and writing back what it held before would undo the redaction;
/// `forget-session` removes both, and writing the compressed one back would
/// return a session somebody asked to have forgotten.
fn finish(staged: Staged) -> Result<bool, RawError> {
    let io = |source| RawError::Io {
        path: staged.plain.clone(),
        source,
    };
    let plain_now = std::fs::metadata(&staged.plain)
        .ok()
        .map(|metadata| metadata.len());
    if plain_now != Some(staged.read_at) || fingerprint(&staged.packed) != staged.packed_at {
        let _ = std::fs::remove_file(&staged.staged);
        return Ok(false);
    }
    std::fs::rename(&staged.staged, &staged.packed).map_err(io)?;

    let plain_now = std::fs::metadata(&staged.plain)
        .ok()
        .map(|metadata| metadata.len());
    if plain_now == Some(staged.read_at) {
        std::fs::remove_file(&staged.plain).map_err(io)?;
    }
    Ok(true)
}

/// What a file looked like, to tell afterwards whether anything wrote to it:
/// its length and when it was last changed. `None` for no file.
fn fingerprint(path: &Path) -> Option<(u64, Option<SystemTime>)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.len(), metadata.modified().ok()))
}

/// Whether a spool file is a compacted one.
fn is_compressed(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".jsonl.gz"))
}

/// A spool file's text, decompressed if it was compacted.
///
/// Every member of a gzip stream, not only the first: a file written by some
/// other tool may hold several, and a reader that stopped at the first would
/// quietly return part of a transcript.
fn read_text(path: &Path) -> std::io::Result<String> {
    if !is_compressed(path) {
        return std::fs::read_to_string(path);
    }
    let mut text = String::new();
    flate2::read::MultiGzDecoder::new(std::fs::File::open(path)?).read_to_string(&mut text)?;
    Ok(text)
}

/// `head` and then every line of `tail` that `head` does not already hold,
/// byte for byte, each line ending in a newline.
///
/// Lines are compared without their line ending, so a file written on one
/// platform and continued on another is not doubled. Blank lines are dropped:
/// they carry nothing, and every reader skips them.
fn merge_lines(head: &[u8], tail: &[u8]) -> Vec<u8> {
    let trim = |line: &[u8]| -> usize {
        let mut end = line.len();
        while end > 0 && matches!(line[end - 1], b'\n' | b'\r') {
            end -= 1;
        }
        end
    };
    let mut merged = Vec::with_capacity(head.len() + tail.len() + 1);
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    for line in head
        .split_inclusive(|byte| *byte == b'\n')
        .chain(tail.split_inclusive(|byte| *byte == b'\n'))
    {
        let content = &line[..trim(line)];
        if content.iter().all(u8::is_ascii_whitespace) || !seen.insert(content.to_vec()) {
            continue;
        }
        merged.extend_from_slice(line);
        if !line.ends_with(b"\n") {
            merged.push(b'\n');
        }
    }
    merged
}

/// Walk `dir` collecting transcripts: `.jsonl` files and the `.jsonl.gz` ones
/// they were compacted into.
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
        } else if path.extension().is_some_and(|ext| ext == "jsonl") || is_compressed(&path) {
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

    fn gunzip(path: &Path) -> Vec<u8> {
        let mut bytes = Vec::new();
        flate2::read::MultiGzDecoder::new(std::fs::File::open(path).unwrap())
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }

    /// Records compared as what they serialise to: they carry no `PartialEq`.
    fn as_json(records: &[RawRecord]) -> Vec<String> {
        records
            .iter()
            .map(|record| serde_json::to_string(record).unwrap())
            .collect()
    }

    fn bodies(records: &[RawRecord]) -> Vec<String> {
        records
            .iter()
            .filter_map(|record| match record {
                RawRecord::Observation(o) => Some(o.body.as_str().to_owned()),
                RawRecord::Session(_) => None,
            })
            .collect()
    }

    /// The claim compaction rests on: every byte the transcript held is in
    /// the compressed file, in order, and it reads back as the same records.
    #[test]
    fn a_compacted_transcript_keeps_every_line_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RawSpool::new(dir.path());
        let session = session();
        for body in ["first", "second", "third"] {
            spool
                .append(&scope(), &session, &observation(body))
                .unwrap();
        }
        let plain = spool.locate(&scope(), &session);
        let written = std::fs::read(&plain).unwrap();
        let before = spool.read_session(&scope(), &session).unwrap();

        assert!(spool.compact(&plain).unwrap());

        let packed = plain.with_extension("jsonl.gz");
        assert!(!plain.exists(), "the plain file is folded in, not kept");
        assert_eq!(gunzip(&packed), written);
        assert_eq!(
            as_json(&spool.read_session(&scope(), &session).unwrap()),
            as_json(&before)
        );
        assert_eq!(spool.files().unwrap(), vec![packed]);
    }

    /// `claude --resume` a week later: the session starts a plain file of its
    /// own, header first, the two read as one, and the next compaction folds
    /// the new lines in after the old.
    #[test]
    fn a_session_resumed_after_compaction_reads_as_one() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RawSpool::new(dir.path());
        let session = session();
        for body in ["first", "second"] {
            spool
                .append(&scope(), &session, &observation(body))
                .unwrap();
        }
        let plain = spool.locate(&scope(), &session);
        let written = std::fs::read(&plain).unwrap();
        spool.compact(&plain).unwrap();

        spool
            .append(&scope(), &session, &observation("third"))
            .unwrap();
        let resumed = std::fs::read_to_string(&plain).unwrap();
        assert!(
            matches!(
                serde_json::from_str::<RawRecord>(resumed.lines().next().unwrap()).unwrap(),
                RawRecord::Session(_)
            ),
            "a file a rebuild can read on its own:\n{resumed}"
        );
        assert_eq!(
            bodies(&spool.read_session(&scope(), &session).unwrap()),
            vec!["first", "second", "third"]
        );

        spool.compact(&plain).unwrap();
        let packed = gunzip(&plain.with_extension("jsonl.gz"));
        assert!(
            packed.starts_with(&written),
            "the old lines first, as they were"
        );
        assert_eq!(
            bodies(&spool.read_session(&scope(), &session).unwrap()),
            vec!["first", "second", "third"]
        );
        assert!(!plain.exists());
    }

    /// Stopped after the compressed file replaced the old one and before the
    /// plain file was removed: the next compaction finishes the job instead of
    /// holding every line twice.
    #[test]
    fn an_interrupted_compaction_is_finished_not_doubled() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RawSpool::new(dir.path());
        let session = session();
        for body in ["first", "second"] {
            spool
                .append(&scope(), &session, &observation(body))
                .unwrap();
        }
        let plain = spool.locate(&scope(), &session);
        let written = std::fs::read(&plain).unwrap();
        let packed = plain.with_extension("jsonl.gz");
        let mut encoder = flate2::write::GzEncoder::new(
            std::fs::File::create(&packed).unwrap(),
            flate2::Compression::default(),
        );
        encoder.write_all(&written).unwrap();
        encoder.finish().unwrap();

        spool.compact(&plain).unwrap();

        assert_eq!(gunzip(&packed), written);
        assert!(!plain.exists());
    }

    /// A transcript written to within the quiet period is left alone; only
    /// one that has gone the whole period without a line is compacted.
    #[test]
    fn only_a_transcript_quiet_for_the_whole_period_is_compacted() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RawSpool::new(dir.path());
        let quiet = session();
        let mut busy = session();
        busy.id = SessionId::derive(ProjectId::from_uuid(uuid::Uuid::nil()), "agent-session-2");
        spool
            .append(&scope(), &quiet, &observation("a week ago"))
            .unwrap();
        spool
            .append(&scope(), &busy, &observation("just now"))
            .unwrap();
        let quiet_path = spool.locate(&scope(), &quiet);
        let busy_path = spool.locate(&scope(), &busy);
        std::fs::File::options()
            .write(true)
            .open(&quiet_path)
            .unwrap()
            .set_modified(SystemTime::now() - COMPACT_AFTER - Duration::from_secs(60))
            .unwrap();

        let done = spool
            .compact_quiet(COMPACT_AFTER, SystemTime::now())
            .unwrap();

        assert_eq!(done.files, 1);
        assert!(done.bytes_after > 0 && done.bytes_before > 0);
        assert!(!quiet_path.exists());
        assert!(quiet_path.with_extension("jsonl.gz").exists());
        assert!(busy_path.exists());
        assert!(!busy_path.with_extension("jsonl.gz").exists());
    }

    /// Forgetting a session has to find the compressed file too: it is still
    /// somebody's prompts.
    #[test]
    fn a_compacted_transcript_is_found_with_its_session() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RawSpool::new(dir.path());
        let session = session();
        spool
            .append(&scope(), &session, &observation("before"))
            .unwrap();
        let plain = spool.locate(&scope(), &session);
        spool.compact(&plain).unwrap();
        spool
            .append(&scope(), &session, &observation("after"))
            .unwrap();

        assert_eq!(
            spool.locate_all(&scope(), session.id),
            vec![plain.clone(), plain.with_extension("jsonl.gz")]
        );
        assert_eq!(spool.lines_in(&plain.with_extension("jsonl.gz")), Some(2));
    }

    /// A rule added after a secret was captured reaches the compressed copy,
    /// and the file stays compressed.
    #[test]
    fn a_compacted_transcript_is_redacted_where_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let (spool, path) = spooled_before_the_rule(dir.path());
        assert!(spool.compact(&path).unwrap());
        let packed = path.with_extension("jsonl.gz");

        let found = spool
            .redact_file(&packed, &anamnesis_core::sanitize::Redactor::new(), true)
            .unwrap();

        assert_eq!(found.changed, 1);
        let text = String::from_utf8(gunzip(&packed)).unwrap();
        assert!(!text.contains(KEY));
        assert!(text.contains("[redacted:google-auth-key]"));
        assert_eq!(spool.read_file(&packed).unwrap().len(), 3);
        assert!(!packed.with_extension("gz.redacting").exists());
    }

    /// `forget-session` run while a server is halfway through compacting the
    /// same transcript: the compaction must not put back what was removed.
    #[test]
    fn a_compaction_does_not_bring_back_a_transcript_forgotten_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RawSpool::new(dir.path());
        let session = session();
        spool
            .append(&scope(), &session, &observation("forget me"))
            .unwrap();
        let plain = spool.locate(&scope(), &session);
        let staged = stage(&plain).unwrap().expect("staged");

        for doomed in spool.locate_all(&scope(), session.id) {
            std::fs::remove_file(doomed).unwrap();
        }

        assert!(!finish(staged).unwrap());
        assert!(spool.locate_all(&scope(), session.id).is_empty());
        assert!(!plain.with_extension("jsonl.gz.compacting").exists());
    }

    /// `redact` run while a server is halfway through compacting: writing
    /// back what the compressed file held before would put the secret back.
    #[test]
    fn a_compaction_does_not_undo_a_redaction_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let (spool, path) = spooled_before_the_rule(dir.path());
        assert!(spool.compact(&path).unwrap());
        let packed = path.with_extension("jsonl.gz");
        spool
            .append(&scope(), &session(), &observation("resumed"))
            .unwrap();
        let staged = stage(&path).unwrap().expect("staged");

        spool
            .redact_file(&packed, &anamnesis_core::sanitize::Redactor::new(), true)
            .unwrap();

        assert!(!finish(staged).unwrap());
        assert!(!String::from_utf8(gunzip(&packed)).unwrap().contains(KEY));
        assert!(path.exists(), "the resumed lines wait for the next pass");
    }

    /// A line arriving from a writer outside the lock while a compaction is
    /// staged: the plain file is kept, and nothing it held is lost.
    #[test]
    fn a_line_appended_during_a_compaction_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RawSpool::new(dir.path());
        let session = session();
        spool
            .append(&scope(), &session, &observation("first"))
            .unwrap();
        let plain = spool.locate(&scope(), &session);
        let staged = stage(&plain).unwrap().expect("staged");

        spool
            .append(&scope(), &session, &observation("arrived meanwhile"))
            .unwrap();

        assert!(!finish(staged).unwrap());
        assert_eq!(
            bodies(&spool.read_session(&scope(), &session).unwrap()),
            vec!["first", "arrived meanwhile"]
        );
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
