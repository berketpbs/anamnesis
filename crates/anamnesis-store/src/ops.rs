//! Typed reads and writes over the schema.
//!
//! Everything here is synchronous and short. The capture path runs on every
//! tool call an agent makes, so a write that takes milliseconds is a write the
//! agent never notices, and one that takes hundreds is a stutter in someone's
//! editing session.

use std::path::PathBuf;

use anamnesis_core::embedding::{Embed, page_text};
use anamnesis_core::handoff::{Handoff, HandoffState, Slot};
use anamnesis_core::ids::{HandoffId, ObservationId, PageId, ProjectId, SessionId, WorkstreamId};
use anamnesis_core::observation::{BoundedBody, EventKind, Observation, ToolRef};
use anamnesis_core::page::Page;
use anamnesis_core::session::{AgentKind, Session, SessionState};
use jiff::Timestamp;
use rusqlite::{OptionalExtension, Row, params};

use crate::convert::{parse_id, parse_operator, parse_time};
use crate::{Result, Store};

/// A session still open, and when it last said anything.
///
/// The checkout path rides along because it is what a scope resolves from,
/// and the reaper has no request to take one from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSession {
    /// The session.
    pub id: SessionId,
    /// Working copy it was recorded from.
    pub checkout_path: PathBuf,
    /// Its newest observation, or its start when it has none.
    pub last_seen: Timestamp,
}

/// One row of `anamnesis sessions`: what a session was, without its
/// observations.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    /// The session.
    pub id: SessionId,
    /// Which harness ran it.
    pub agent: String,
    /// Its lifecycle state.
    pub state: String,
    /// When it started.
    pub started_at: Timestamp,
    /// When it ended, if it has.
    pub ended_at: Option<Timestamp>,
    /// Slug of the workstream it joined, if any.
    pub workstream: Option<String>,
    /// The operator whose token it arrived under, if the server named one.
    pub operator: Option<String>,
    /// How many observations it captured.
    pub observation_count: i64,
}

impl Store {
    /// Record a session, ignoring the call if it is already known.
    ///
    /// Hooks arrive out of order and more than once; the first event of a
    /// session is not reliably `SessionStart`. Making this idempotent means the
    /// capture path never has to ask whether a session exists yet.
    pub fn ensure_session(&self, session: &Session) -> Result<()> {
        let conn = self.connection();
        conn.execute(
            "INSERT INTO sessions
                 (id, project_id, agent, checkout_path, state, started_at, ended_at,
                  workstream_id, operator)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (id) DO NOTHING",
            params![
                session.id.to_string(),
                session.project_id.to_string(),
                session.agent.as_str(),
                session.checkout_path.to_string_lossy(),
                session.state.as_str(),
                session.started_at.to_string(),
                session.ended_at.map(|t| t.to_string()),
                session.workstream_id.map(|id| id.to_string()),
                session.operator.as_ref().map(ToString::to_string),
            ],
        )?;
        Ok(())
    }

    /// Mark a session ended.
    pub fn close_session(&self, id: SessionId, ended_at: Timestamp) -> Result<()> {
        let conn = self.connection();
        conn.execute(
            "UPDATE sessions SET state = 'closed', ended_at = ?2 WHERE id = ?1",
            params![id.to_string(), ended_at.to_string()],
        )?;
        Ok(())
    }

    /// Claim an open session for consolidation.
    ///
    /// A compare-and-swap, which is the whole point: the reaper wakes on a
    /// timer and a model call can outlast the gap between two ticks, so the
    /// second tick must be able to tell "still being summarised" from "still
    /// open". `Ok(false)` means somebody else got there first.
    pub fn begin_ending(&self, id: SessionId) -> Result<bool> {
        let conn = self.connection();
        let changed = conn.execute(
            "UPDATE sessions SET state = 'ending' WHERE id = ?1 AND state = 'open'",
            params![id.to_string()],
        )?;
        Ok(changed == 1)
    }

    /// Release a claim that came to nothing.
    ///
    /// A summary that failed must be retried rather than left in `ending`
    /// forever, where no later pass would ever see it again. Guarded on the
    /// state it is undoing, so this can never resurrect a session that was
    /// properly closed.
    pub fn reopen_session(&self, id: SessionId) -> Result<()> {
        let conn = self.connection();
        conn.execute(
            "UPDATE sessions SET state = 'open', ended_at = NULL
             WHERE id = ?1 AND state = 'ending'",
            params![id.to_string()],
        )?;
        Ok(())
    }

    /// Take a closed session back up, because it just said something.
    ///
    /// The other half of summarising a session nobody closed: an agent that
    /// goes quiet for hours and then keeps working must not have the rest of
    /// its session dropped on the floor because something already wrote its
    /// page. Ending it again rewrites that same page and supersedes its
    /// handoff, so resuming costs a second pass and nothing else.
    ///
    /// Deliberately not guarded against `ending`: a session being summarised
    /// right now is somebody else's, and taking it back mid-flight would race
    /// them for the page.
    pub fn resume_session(&self, id: SessionId) -> Result<()> {
        let conn = self.connection();
        conn.execute(
            "UPDATE sessions SET state = 'open', ended_at = NULL
             WHERE id = ?1 AND state = 'closed'",
            params![id.to_string()],
        )?;
        Ok(())
    }

    /// Every session still open, with the time it was last heard from.
    ///
    /// Whether that is long enough ago to act on is the caller's decision, and
    /// it is made in Rust: timestamps are stored as text, and text ordering
    /// disagrees with time ordering when two of them carry a different number
    /// of fractional digits. Over a threshold measured in hours the difference
    /// is under a second, but the comparison is cheap to do properly and there
    /// are only ever a handful of open sessions.
    pub fn open_sessions(&self) -> Result<Vec<OpenSession>> {
        let conn = self.connection();
        let mut statement = conn.prepare(
            "SELECT s.id, s.checkout_path,
                    COALESCE(
                        (SELECT MAX(o.at) FROM observations o WHERE o.session_id = s.id),
                        s.started_at
                    )
             FROM sessions s
             WHERE s.state = 'open'",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(OpenSession {
                id: parse_id(row.get::<_, String>(0)?),
                checkout_path: PathBuf::from(row.get::<_, String>(1)?),
                last_seen: parse_time(&row.get::<_, String>(2)?),
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Load one session, with the workspace reached through its project.
    pub fn load_session(&self, id: SessionId) -> Result<Option<Session>> {
        let conn = self.connection();
        let session = conn
            .query_row(
                "SELECT s.id, s.project_id, p.workspace_id, s.agent, s.checkout_path,
                        s.state, s.started_at, s.ended_at, s.workstream_id, s.operator
                 FROM sessions s
                 JOIN projects p ON p.id = s.project_id
                 WHERE s.id = ?1",
                params![id.to_string()],
                read_session,
            )
            .optional()?;
        Ok(session)
    }

    /// Append an observation.
    pub fn insert_observation(&self, observation: &Observation) -> Result<()> {
        let conn = self.connection();
        conn.execute(
            "INSERT INTO observations
                 (id, session_id, kind, tool_name, tool_ok, at, body, truncated, sanitized)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (id) DO NOTHING",
            params![
                observation.id.to_string(),
                observation.session_id.to_string(),
                observation.kind.as_str(),
                observation.tool.as_ref().map(|t| t.name.clone()),
                observation.tool.as_ref().and_then(|t| t.ok),
                observation.at.to_string(),
                observation.body.as_str(),
                observation.body.is_truncated(),
                observation.sanitized,
            ],
        )?;
        Ok(())
    }

    /// Every observation in a session, oldest first.
    pub fn observations(&self, session_id: SessionId) -> Result<Vec<Observation>> {
        let conn = self.connection();
        let mut statement = conn.prepare(
            "SELECT id, session_id, kind, tool_name, tool_ok, at, body, truncated, sanitized
             FROM observations WHERE session_id = ?1 ORDER BY at, rowid",
        )?;
        let rows = statement.query_map(params![session_id.to_string()], read_observation)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Put everything the index knows about a page in place: the row, the
    /// names it declares, the links it makes, and its vector.
    ///
    /// **The one way to index a page.** There were seven, hand-written, in as
    /// many modules — consolidation, the wiki watcher, `reindex`, `bootstrap`,
    /// `write-page`, the MCP tool, and the auto-improve pass — and they had
    /// already drifted twice. Once when the live path wrote a page without its
    /// links, so the link-neighbour stream was blind to every page this system
    /// wrote for itself until somebody rebuilt. And again, still true when this
    /// was written: exactly one of the seven embedded anything, so a vector
    /// stream that was switched on covered the pages an agent had written by
    /// hand through MCP and nothing else — not one session summary.
    ///
    /// A caller with no embedder passes `None` and gets what it had before. The
    /// difference is that the choice is now in the signature, where the next
    /// person writing a page has to see it, instead of being four calls they
    /// might make three of.
    ///
    /// An embedding that fails is logged and stepped over. It costs the page
    /// one retrieval stream; refusing the write would cost the page.
    pub fn index_page(
        &self,
        project_id: ProjectId,
        page: &Page,
        links: &[String],
        embedder: Option<&dyn Embed>,
        now: Timestamp,
    ) -> Result<()> {
        self.upsert_page(page, now)?;
        self.set_page_entities(project_id, page.id, &page.frontmatter.entities)?;
        self.set_page_links(project_id, page.id, links)?;

        self.embed_page(page, embedder)
    }

    /// Give a page its vector, if there is an embedder to make one.
    ///
    /// Split out for the two rebuilds — `reindex` and `bootstrap` — which
    /// resolve links in a second pass and so cannot use [`Store::index_page`]
    /// as it stands. They call this instead, which keeps the one thing that
    /// must not vary between writers: what text a page is embedded as. Two
    /// call sites that disagreed would fill one index with two kinds of vector
    /// and no way to tell them apart.
    pub fn embed_page(&self, page: &Page, embedder: Option<&dyn Embed>) -> Result<()> {
        let Some(embedder) = embedder else {
            return Ok(());
        };
        let text = page_text(&page.frontmatter.title, &page.body);
        match embedder.embed(&text) {
            Ok(vector) => self.set_page_embedding(page.id, embedder.model(), &vector)?,
            Err(error) => tracing::warn!(
                %error,
                path = %page.path,
                "page embedding failed; the page is indexed without one"
            ),
        }
        Ok(())
    }

    /// Insert or refresh the index row for a page.
    ///
    /// Also resolves the page's supersession, in both directions: what this
    /// page replaces, and whether anything replaces it. Both are derived from
    /// the authored `supersedes` path rather than passed in, so writing the
    /// pages in either order produces the same index — a page can name its
    /// predecessor before the index has seen it, and a rebuild visits paths in
    /// an order nobody chose.
    pub fn upsert_page(&self, page: &Page, now: Timestamp) -> Result<()> {
        let fm = &page.frontmatter;
        let target = fm.supersedes.as_ref().map(|path| path.as_str().to_owned());

        let mut conn = self.connection();
        let tx = conn.transaction()?;

        // Read before writing: a page that stops naming a predecessor has to
        // give it back its place at the head of the chain, and after the write
        // there is nothing left to say who that was.
        let previous: Option<String> = tx
            .query_row(
                "SELECT supersedes_target FROM pages WHERE id = ?1",
                params![page.id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .unwrap_or(None)
            .flatten();

        tx.execute(
            "INSERT INTO pages
                 (id, project_id, path, title, body, tier, status, pinned, canonical,
                  salience, expires_at, git_commit, supersedes_target, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)
             ON CONFLICT (id) DO UPDATE SET
                 title             = excluded.title,
                 body              = excluded.body,
                 tier              = excluded.tier,
                 status            = excluded.status,
                 pinned            = excluded.pinned,
                 canonical         = excluded.canonical,
                 salience          = excluded.salience,
                 expires_at        = excluded.expires_at,
                 git_commit        = excluded.git_commit,
                 supersedes_target = excluded.supersedes_target,
                 updated_at        = excluded.updated_at",
            params![
                page.id.to_string(),
                page.project_id.to_string(),
                page.path.as_str(),
                fm.title,
                page.body,
                fm.tier.as_str(),
                fm.status.as_str(),
                fm.pinned,
                fm.canonical,
                fm.salience,
                fm.expires_at.map(|t| t.to_string()),
                page.git_commit.clone(),
                target.clone(),
                now.to_string(),
            ],
        )?;

        // Three paths can have changed standing: this page, the predecessor it
        // names now, and the one it named before.
        let mut affected = vec![page.path.as_str().to_owned()];
        affected.extend(target);
        affected.extend(previous);
        affected.sort();
        affected.dedup();
        resolve_supersession(&tx, page.project_id, &affected)?;

        tx.commit()?;
        Ok(())
    }

    /// The page that replaced this one, if any has.
    ///
    /// Read from the authored claim rather than from `supersedes`, so it
    /// answers the same way whether or not the replacement has been indexed
    /// yet — the question someone asks of a page they were handed is "is this
    /// still current", and "a page I have not seen yet says no" is still no.
    pub fn superseded_by(
        &self,
        project_id: ProjectId,
        path: &anamnesis_core::page::PagePath,
    ) -> Result<Option<anamnesis_core::page::PagePath>> {
        let conn = self.connection();
        let replacement: Option<String> = conn
            .query_row(
                "SELECT path FROM pages
                 WHERE project_id = ?1 AND supersedes_target = ?2 AND path <> ?2
                 ORDER BY updated_at DESC, path
                 LIMIT 1",
                params![project_id.to_string(), path.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(replacement.as_deref().map(crate::convert::parse_page_path))
    }

    /// Record a handoff, expiring any that was still waiting.
    ///
    /// Expiring first is what upholds the single-pending invariant the schema
    /// enforces: a newer summary always replaces an unread older one, because
    /// handing a session two "where you left off" notes is worse than handing
    /// it the wrong one.
    pub fn record_handoff(&self, handoff: &Handoff) -> Result<()> {
        let workstream = handoff.workstream_id.map(|id| id.to_string());
        let operator = handoff.operator.as_ref().map(ToString::to_string);
        let mut conn = self.connection();
        let transaction = conn.transaction()?;
        // Only a pending handoff in the *same* slot is superseded — a
        // workstream's handoff must not expire another workstream's, the
        // project-wide one, or one belonging to a different operator.
        transaction.execute(
            "UPDATE handoffs SET state = 'expired'
             WHERE project_id = ?1 AND state = 'pending'
               AND COALESCE(workstream_id, '') = COALESCE(?2, '')
               AND COALESCE(operator, '') = COALESCE(?3, '')",
            params![handoff.project_id.to_string(), workstream, operator],
        )?;
        transaction.execute(
            "INSERT INTO handoffs
                 (id, project_id, from_session, body, state, created_at, workstream_id, operator)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?7)",
            params![
                handoff.id.to_string(),
                handoff.project_id.to_string(),
                handoff.from_session.to_string(),
                handoff.body.as_str(),
                handoff.created_at.to_string(),
                workstream,
                operator,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Claim the pending handoff in one of a project's slots, if there is one.
    ///
    /// [`Slot::shared`] is the whole story for a project that has asked for
    /// neither workstreams nor per-operator slots.
    ///
    /// Claiming is a single statement so two sessions starting at the same
    /// moment cannot both receive it: the second finds nothing pending.
    pub fn claim_handoff(
        &self,
        project_id: ProjectId,
        claimant: SessionId,
        slot: &Slot,
        now: Timestamp,
    ) -> Result<Option<String>> {
        let conn = self.connection();
        let body = conn
            .query_row(
                "UPDATE handoffs SET state = 'accepted', to_session = ?2, accepted_at = ?3
                 WHERE id = (
                     SELECT id FROM handoffs
                     WHERE project_id = ?1 AND state = 'pending'
                       AND COALESCE(workstream_id, '') = COALESCE(?4, '')
                       AND COALESCE(operator, '') = COALESCE(?5, '')
                     ORDER BY created_at DESC LIMIT 1
                 )
                 RETURNING body",
                params![
                    project_id.to_string(),
                    claimant.to_string(),
                    now.to_string(),
                    slot.workstream_key(),
                    slot.operator_key(),
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(body)
    }

    /// Drop the pending handoff in a slot without handing it to anybody.
    ///
    /// The third thing that can happen to a note, after being claimed and
    /// being superseded by a newer one: somebody read it and decided the next
    /// session is better off without it. A summary written from a bad model
    /// reply is the case this exists for — until now the only way to get rid of
    /// one was to let a session claim it, which means it lands in that
    /// session's context, which is the thing being avoided.
    ///
    /// Marked `expired` rather than deleted, and it is the same state a newer
    /// handoff already puts the older one in. A row that says a note was
    /// written and never delivered is a more honest account than no row, and
    /// the body stays readable behind it.
    ///
    /// Returns what was dropped, so the caller can say what it was rather than
    /// reporting a silent success.
    pub fn discard_handoff(&self, project_id: ProjectId, slot: &Slot) -> Result<Option<String>> {
        let conn = self.connection();
        conn.query_row(
            "UPDATE handoffs SET state = 'expired'
             WHERE id = (
                 SELECT id FROM handoffs
                 WHERE project_id = ?1 AND state = 'pending'
                   AND COALESCE(workstream_id, '') = COALESCE(?2, '')
                   AND COALESCE(operator, '') = COALESCE(?3, '')
                 ORDER BY created_at DESC LIMIT 1
             )
             RETURNING body",
            params![
                project_id.to_string(),
                slot.workstream_key(),
                slot.operator_key()
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(Into::into)
    }

    /// Read the pending handoff without consuming it.
    ///
    /// The counterpart to [`Store::claim_handoff`], for showing someone what
    /// is waiting. Claiming is what a starting session does; looking is what
    /// a person at a terminal does, and conflating the two would mean running
    /// `anamnesis handoff` silently costs the next session its note.
    pub fn peek_handoff(&self, project_id: ProjectId, slot: &Slot) -> Result<Option<String>> {
        let conn = self.connection();
        conn.query_row(
            "SELECT body FROM handoffs
             WHERE project_id = ?1 AND state = 'pending'
               AND COALESCE(workstream_id, '') = COALESCE(?2, '')
               AND COALESCE(operator, '') = COALESCE(?3, '')
             ORDER BY created_at DESC LIMIT 1",
            params![
                project_id.to_string(),
                slot.workstream_key(),
                slot.operator_key()
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(Into::into)
    }

    /// A project's sessions, most recent first.
    pub fn recent_sessions(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> Result<Vec<SessionSummary>> {
        let conn = self.connection();
        let mut statement = conn.prepare(
            "SELECT s.id, s.agent, s.state, s.started_at, s.ended_at, w.slug, s.operator,
                    (SELECT COUNT(*) FROM observations o WHERE o.session_id = s.id)
             FROM sessions s
             LEFT JOIN workstreams w ON w.id = s.workstream_id
             WHERE s.project_id = ?1
             ORDER BY s.started_at DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![project_id.to_string(), limit as i64], |row| {
            Ok(SessionSummary {
                id: parse_id(row.get::<_, String>(0)?),
                agent: row.get(1)?,
                state: row.get(2)?,
                started_at: parse_time(&row.get::<_, String>(3)?),
                ended_at: row.get::<_, Option<String>>(4)?.map(|t| parse_time(&t)),
                workstream: row.get(5)?,
                operator: row.get(6)?,
                observation_count: row.get(7)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// How many sessions a project has recorded.
    pub fn session_count(&self, project_id: ProjectId) -> Result<i64> {
        let conn = self.connection();
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE project_id = ?1",
            params![project_id.to_string()],
            |row| row.get(0),
        )?)
    }

    /// How many pages a project has indexed.
    pub fn page_count(&self, project_id: ProjectId) -> Result<i64> {
        let conn = self.connection();
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM pages WHERE project_id = ?1",
            params![project_id.to_string()],
            |row| row.get(0),
        )?)
    }

    /// Whether the index already holds exactly this page.
    ///
    /// Written for the wiki watcher, which cannot tell its own writes from
    /// someone else's: every page this system compiles arrives as a filesystem
    /// event a moment later, as does every save an editor makes without
    /// changing anything. Re-indexing those is harmless to the access counters,
    /// which `upsert_page` leaves alone — but `updated_at` is what a sweep reads
    /// as "when the page was last written", so writing it back would renew a
    /// page nobody edited, and a wiki that watched itself would never decay.
    ///
    /// Compares every column an author can change from the markdown. `false`
    /// for a page the index has never seen.
    pub fn page_is_current(&self, page: &Page) -> Result<bool> {
        let fm = &page.frontmatter;
        let conn = self.connection();
        let found = conn
            .query_row(
                "SELECT title, body, tier, status, pinned, canonical, salience,
                        expires_at, supersedes_target
                 FROM pages WHERE id = ?1",
                params![page.id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, bool>(4)?,
                        row.get::<_, bool>(5)?,
                        row.get::<_, f64>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                    ))
                },
            )
            .optional()?;

        let Some(stored) = found else {
            return Ok(false);
        };

        Ok(stored
            == (
                fm.title.clone(),
                page.body.clone(),
                fm.tier.as_str().to_owned(),
                fm.status.as_str().to_owned(),
                fm.pinned,
                fm.canonical,
                fm.salience,
                fm.expires_at.map(|at| at.to_string()),
                fm.supersedes.as_ref().map(|p| p.as_str().to_owned()),
            ))
    }

    /// Every page the index holds for a project, by id and authored path.
    ///
    /// The question this answers is the one a rebuild has to ask in reverse:
    /// not "what is in the wiki", but "what does the index still claim exists".
    /// A page deleted from the wiki by hand leaves a row behind that no walk of
    /// the filesystem will ever visit, so nothing else can find it.
    pub fn page_paths(&self, project_id: ProjectId) -> Result<Vec<(PageId, String)>> {
        let conn = self.connection();
        let mut statement =
            conn.prepare("SELECT id, path FROM pages WHERE project_id = ?1 ORDER BY path")?;
        let rows = statement.query_map(params![project_id.to_string()], |row| {
            Ok((parse_id(row.get::<_, String>(0)?), row.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// When this project last captured anything, if it ever has.
    ///
    /// The question `anamnesis status` asks on someone's behalf is "is my work
    /// being recorded right now?", and neither half of the obvious answer
    /// settles it alone: a reachable server records nothing if no harness is
    /// calling it, and an unreachable one may have been recording until a
    /// minute ago. When the last observation landed is the only evidence that
    /// capture reached the index rather than merely being configured to.
    pub fn last_observation_at(&self, project_id: ProjectId) -> Result<Option<Timestamp>> {
        let conn = self.connection();
        let found: Option<String> = conn.query_row(
            "SELECT MAX(o.at) FROM observations o
             JOIN sessions s ON s.id = o.session_id
             WHERE s.project_id = ?1",
            params![project_id.to_string()],
            |row| row.get(0),
        )?;
        Ok(found.as_deref().map(parse_time))
    }
}

/// Recompute supersession for a set of page paths.
///
/// Two facts, both derived from the authored `supersedes_target` rather than
/// asserted by whoever happened to write last:
///
/// * `supersedes` — the row a page named, once that row exists.
/// * `is_latest` — whether anything names *this* page. Retrieval filters on
///   it, so this is what actually takes a replaced page out of circulation.
///
/// Resolution runs in both directions because either page can be written
/// first. A page naming a predecessor the index has not seen keeps the name
/// and resolves when it arrives, which is the same shape as an unresolved
/// wikilink — and the same bug avoided.
///
/// A page naming itself resolves to nothing. Left alone it would set its own
/// `is_latest` to zero and disappear from retrieval, which is a strange way to
/// punish a typo.
fn resolve_supersession(
    tx: &rusqlite::Transaction<'_>,
    project_id: ProjectId,
    paths: &[String],
) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }

    let list = crate::query::placeholders(paths.len());
    let mut values: Vec<rusqlite::types::Value> = Vec::with_capacity(paths.len() + 1);
    values.push(rusqlite::types::Value::Text(project_id.to_string()));
    values.extend(
        paths
            .iter()
            .map(|path| rusqlite::types::Value::Text(path.clone())),
    );

    // What these pages replace.
    tx.execute(
        &format!(
            "UPDATE pages SET supersedes = (
                 SELECT t.id FROM pages t
                 WHERE t.project_id = pages.project_id
                   AND t.path = pages.supersedes_target
                   AND t.id <> pages.id
             )
             WHERE project_id = ?1 AND path IN ({list})"
        ),
        rusqlite::params_from_iter(values.iter()),
    )?;

    // What replaces them — pages written earlier that named one of these and
    // could not resolve it at the time.
    tx.execute(
        &format!(
            "UPDATE pages SET supersedes = (
                 SELECT t.id FROM pages t
                 WHERE t.project_id = pages.project_id
                   AND t.path = pages.supersedes_target
                   AND t.id <> pages.id
             )
             WHERE project_id = ?1 AND supersedes_target IN ({list})"
        ),
        rusqlite::params_from_iter(values.iter()),
    )?;

    // And whether each is still the head of its chain.
    tx.execute(
        &format!(
            "UPDATE pages SET is_latest = CASE WHEN EXISTS (
                 SELECT 1 FROM pages o
                 WHERE o.project_id = pages.project_id
                   AND o.supersedes_target = pages.path
                   AND o.id <> pages.id
             ) THEN 0 ELSE 1 END
             WHERE project_id = ?1 AND path IN ({list})"
        ),
        rusqlite::params_from_iter(values.iter()),
    )?;

    Ok(())
}

/// Build a [`Session`] from a joined row.
fn read_session(row: &Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: parse_id(row.get::<_, String>(0)?),
        project_id: parse_id(row.get::<_, String>(1)?),
        workspace_id: parse_id(row.get::<_, String>(2)?),
        agent: row
            .get::<_, String>(3)?
            .parse()
            .expect("AgentKind parsing is infallible"),
        checkout_path: row.get::<_, String>(4)?.into(),
        state: SessionState::from_storage(&row.get::<_, String>(5)?),
        started_at: parse_time(&row.get::<_, String>(6)?),
        ended_at: row.get::<_, Option<String>>(7)?.map(|t| parse_time(&t)),
        workstream_id: row.get::<_, Option<String>>(8)?.map(parse_id),
        operator: row
            .get::<_, Option<String>>(9)?
            .as_deref()
            .map(parse_operator),
    })
}

/// Build an [`Observation`] from a row.
fn read_observation(row: &Row<'_>) -> rusqlite::Result<Observation> {
    let tool_name: Option<String> = row.get(3)?;
    Ok(Observation {
        id: parse_id(row.get::<_, String>(0)?),
        session_id: parse_id(row.get::<_, String>(1)?),
        kind: EventKind::from_storage(&row.get::<_, String>(2)?),
        tool: tool_name.map(|name| ToolRef {
            name,
            ok: row.get(4).unwrap_or(None),
        }),
        at: parse_time(&row.get::<_, String>(5)?),
        body: BoundedBody::from_stored(row.get::<_, String>(6)?, row.get(7)?),
        sanitized: row.get(8)?,
    })
}

/// Build a handoff ready to be recorded.
pub fn new_handoff(
    project_id: ProjectId,
    from_session: SessionId,
    slot: Slot,
    body: &str,
    created_at: Timestamp,
) -> Handoff {
    Handoff {
        id: HandoffId::new(),
        project_id,
        workstream_id: slot.workstream_id,
        operator: slot.operator,
        from_session,
        to_session: None,
        body: BoundedBody::truncating(body, BoundedBody::DEFAULT_LIMIT),
        created_at,
        accepted_at: None,
        state: HandoffState::Pending,
    }
}

/// Build an observation ready to be inserted.
pub fn new_observation(
    session_id: SessionId,
    kind: EventKind,
    tool: Option<ToolRef>,
    body: BoundedBody,
    at: Timestamp,
) -> Observation {
    Observation {
        id: ObservationId::new(),
        session_id,
        kind,
        tool,
        at,
        body,
        sanitized: true,
    }
}

/// Build a session ready to be recorded.
pub fn new_session(
    id: SessionId,
    project_id: ProjectId,
    workspace_id: anamnesis_core::ids::WorkspaceId,
    agent: AgentKind,
    checkout_path: std::path::PathBuf,
    started_at: Timestamp,
    workstream_id: Option<WorkstreamId>,
) -> Session {
    Session {
        id,
        agent,
        workspace_id,
        project_id,
        workstream_id,
        checkout_path,
        started_at,
        ended_at: None,
        state: SessionState::Open,
        operator: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::fixture;
    use anamnesis_core::ids::WorkspaceId;

    fn now() -> Timestamp {
        "2026-08-19T09:00:00Z".parse().expect("timestamp")
    }

    fn session_for(project: ProjectId, workspace: WorkspaceId) -> Session {
        new_session(
            SessionId::derive(project, "agent-session-1"),
            project,
            workspace,
            AgentKind::ClaudeCode,
            "/repo".into(),
            now(),
            None,
        )
    }

    /// Write a page, optionally naming the page it replaces.
    fn write_page(
        store: &Store,
        project: ProjectId,
        path: &str,
        supersedes: Option<&str>,
    ) -> anamnesis_core::page::Page {
        use anamnesis_core::page::{Frontmatter, Page, PagePath};

        let mut frontmatter = Frontmatter::new("A page", Vec::new()).expect("frontmatter");
        frontmatter.supersedes = supersedes.map(|raw| PagePath::parse(raw).expect("path"));
        let page = Page::new(
            project,
            PagePath::parse(path).expect("path"),
            frontmatter,
            "Body about sqlite.",
        );
        store.upsert_page(&page, now()).expect("upsert");
        page
    }

    /// `(supersedes, is_latest)` as the index holds them for one page.
    fn chain(store: &Store, page: &anamnesis_core::page::Page) -> (Option<String>, bool) {
        store
            .connection()
            .query_row(
                "SELECT supersedes, is_latest FROM pages WHERE id = ?1",
                params![page.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row")
    }

    #[test]
    fn a_page_can_say_what_replaced_it() {
        let (_dir, store, project, _workspace) = fixture();
        let old = write_page(&store, project, "decisions/0001-storage.md", None);
        let new = write_page(
            &store,
            project,
            "decisions/0002-storage.md",
            Some("decisions/0001-storage.md"),
        );

        assert_eq!(
            store
                .superseded_by(project, &old.path)
                .expect("lookup")
                .map(|path| path.as_str().to_owned()),
            Some("decisions/0002-storage.md".to_owned())
        );
        assert_eq!(
            store.superseded_by(project, &new.path).expect("lookup"),
            None
        );
    }

    #[test]
    fn a_page_that_replaces_another_says_so_in_the_index() {
        let (_dir, store, project, _workspace) = fixture();
        let old = write_page(&store, project, "decisions/0001-storage.md", None);
        let new = write_page(
            &store,
            project,
            "decisions/0002-storage.md",
            Some("decisions/0001-storage.md"),
        );

        assert_eq!(chain(&store, &new), (Some(old.id.to_string()), true));
        assert_eq!(
            chain(&store, &old),
            (None, false),
            "the page it replaced is no longer the head of the chain"
        );
    }

    #[test]
    fn a_replaced_page_stops_being_retrievable() {
        // The whole point of the flag: an agent that says "this replaces that"
        // should stop being answered with what it replaced.
        let (_dir, store, project, _workspace) = fixture();
        write_page(&store, project, "decisions/0001-storage.md", None);
        assert_eq!(
            store
                .query_pages(project, "sqlite", 10, now(), None)
                .expect("query")
                .len(),
            1
        );

        write_page(
            &store,
            project,
            "decisions/0002-storage.md",
            Some("decisions/0001-storage.md"),
        );

        let hits = store
            .query_pages(project, "sqlite", 10, now(), None)
            .expect("query");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path.as_str(), "decisions/0002-storage.md");
    }

    #[test]
    fn naming_a_predecessor_that_arrives_later_still_resolves() {
        // The same shape as a wikilink written before its target: the authored
        // path is kept, so the pointer resolves when the page shows up. A
        // rebuild visits paths in an order nobody chose, which is exactly when
        // this happens.
        let (_dir, store, project, _workspace) = fixture();
        let new = write_page(
            &store,
            project,
            "decisions/0002-storage.md",
            Some("decisions/0001-storage.md"),
        );
        assert_eq!(chain(&store, &new), (None, true), "nothing to point at yet");

        let old = write_page(&store, project, "decisions/0001-storage.md", None);
        assert_eq!(chain(&store, &new), (Some(old.id.to_string()), true));
        assert_eq!(chain(&store, &old), (None, false));
    }

    #[test]
    fn taking_the_supersession_back_returns_the_page_to_the_head() {
        let (_dir, store, project, _workspace) = fixture();
        let old = write_page(&store, project, "decisions/0001-storage.md", None);
        write_page(
            &store,
            project,
            "decisions/0002-storage.md",
            Some("decisions/0001-storage.md"),
        );
        assert!(!chain(&store, &old).1);

        // Someone edits the frontmatter and removes the claim.
        let new = write_page(&store, project, "decisions/0002-storage.md", None);
        assert_eq!(chain(&store, &new), (None, true));
        assert_eq!(
            chain(&store, &old),
            (None, true),
            "nothing replaces it any more"
        );
    }

    #[test]
    fn moving_the_claim_to_another_page_frees_the_first() {
        let (_dir, store, project, _workspace) = fixture();
        let first = write_page(&store, project, "decisions/0001-storage.md", None);
        let second = write_page(&store, project, "decisions/0002-storage.md", None);
        write_page(
            &store,
            project,
            "decisions/0003-storage.md",
            Some("decisions/0001-storage.md"),
        );
        assert!(!chain(&store, &first).1);

        write_page(
            &store,
            project,
            "decisions/0003-storage.md",
            Some("decisions/0002-storage.md"),
        );
        assert!(chain(&store, &first).1, "no longer replaced");
        assert!(!chain(&store, &second).1, "replaced instead");
    }

    #[test]
    fn a_chain_leaves_exactly_one_head() {
        let (_dir, store, project, _workspace) = fixture();
        let first = write_page(&store, project, "decisions/0001-storage.md", None);
        let second = write_page(
            &store,
            project,
            "decisions/0002-storage.md",
            Some("decisions/0001-storage.md"),
        );
        let third = write_page(
            &store,
            project,
            "decisions/0003-storage.md",
            Some("decisions/0002-storage.md"),
        );

        assert!(!chain(&store, &first).1);
        assert!(!chain(&store, &second).1);
        assert!(chain(&store, &third).1);
        assert_eq!(chain(&store, &third).0, Some(second.id.to_string()));
    }

    #[test]
    fn rewriting_a_page_leaves_the_chain_as_it_was() {
        let (_dir, store, project, _workspace) = fixture();
        let old = write_page(&store, project, "decisions/0001-storage.md", None);
        let new = write_page(
            &store,
            project,
            "decisions/0002-storage.md",
            Some("decisions/0001-storage.md"),
        );

        for _ in 0..3 {
            write_page(
                &store,
                project,
                "decisions/0002-storage.md",
                Some("decisions/0001-storage.md"),
            );
        }
        assert_eq!(chain(&store, &new), (Some(old.id.to_string()), true));
        assert_eq!(chain(&store, &old), (None, false));
    }

    #[test]
    fn a_page_naming_itself_is_left_where_it_is() {
        // A typo should not be able to delete a page from retrieval.
        let (_dir, store, project, _workspace) = fixture();
        let page = write_page(
            &store,
            project,
            "decisions/0001-storage.md",
            Some("decisions/0001-storage.md"),
        );
        assert_eq!(chain(&store, &page), (None, true));
        assert_eq!(
            store
                .query_pages(project, "sqlite", 10, now(), None)
                .expect("query")
                .len(),
            1
        );
    }

    #[test]
    fn naming_a_page_in_another_project_resolves_to_nothing() {
        let (_dir, store, project, _workspace) = fixture();
        let page = write_page(
            &store,
            project,
            "decisions/0002-storage.md",
            Some("decisions/0001-storage.md"),
        );
        assert_eq!(chain(&store, &page), (None, true));
    }

    #[test]
    fn a_link_to_a_replaced_page_still_finds_it() {
        // It exists; being replaced changes how it ranks, not whether it is
        // there. Left unresolved, the wiki would look like it is asking for a
        // page it already holds.
        let (_dir, store, project, _workspace) = fixture();
        let old = write_page(&store, project, "decisions/0001-storage.md", None);
        write_page(
            &store,
            project,
            "decisions/0002-storage.md",
            Some("decisions/0001-storage.md"),
        );
        let source = write_page(&store, project, "sessions/a.md", None);
        store
            .set_page_links(
                project,
                source.id,
                &["decisions/0001-storage.md".to_owned()],
            )
            .expect("links");

        let resolved: Option<String> = store
            .connection()
            .query_row(
                "SELECT to_page_id FROM page_links WHERE from_page_id = ?1",
                params![source.id.to_string()],
                |row| row.get(0),
            )
            .expect("link");
        assert_eq!(resolved, Some(old.id.to_string()));
    }

    #[test]
    fn ensuring_a_session_twice_keeps_one_row() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);

        store.ensure_session(&session).expect("first");
        store.ensure_session(&session).expect("second");

        assert_eq!(store.session_count(project).expect("count"), 1);
    }

    #[test]
    fn a_session_round_trips_with_its_workspace() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("insert");

        let loaded = store
            .load_session(session.id)
            .expect("load")
            .expect("found");
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.agent, AgentKind::ClaudeCode);
        // The workspace was never stored on the session; it came back through
        // the project, which is why it cannot disagree.
        assert_eq!(loaded.workspace_id, workspace);
        assert!(loaded.is_open());
    }

    #[test]
    fn observations_come_back_in_the_order_they_happened() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");

        for (index, kind) in [
            EventKind::SessionStart,
            EventKind::UserPrompt,
            EventKind::ToolUse,
        ]
        .into_iter()
        .enumerate()
        {
            let at: Timestamp = format!("2026-08-19T09:0{index}:00Z").parse().expect("time");
            store
                .insert_observation(&new_observation(
                    session.id,
                    kind,
                    None,
                    BoundedBody::truncating(format!("event {index}"), 1024),
                    at,
                ))
                .expect("insert");
        }

        let loaded = store.observations(session.id).expect("load");
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].kind, EventKind::SessionStart);
        assert_eq!(loaded[2].kind, EventKind::ToolUse);
        assert_eq!(loaded[1].body.as_str(), "event 1");
    }

    #[test]
    fn tool_details_survive_a_round_trip() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");

        store
            .insert_observation(&new_observation(
                session.id,
                EventKind::ToolUse,
                Some(ToolRef {
                    name: "Bash".to_owned(),
                    ok: Some(false),
                }),
                BoundedBody::truncating("cargo test", 1024),
                now(),
            ))
            .expect("insert");

        let loaded = store.observations(session.id).expect("load");
        let tool = loaded[0].tool.as_ref().expect("tool recorded");
        assert_eq!(tool.name, "Bash");
        assert_eq!(tool.ok, Some(false));
    }

    #[test]
    fn truncation_is_remembered_across_storage() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");

        store
            .insert_observation(&new_observation(
                session.id,
                EventKind::UserPrompt,
                None,
                BoundedBody::truncating("x".repeat(100), 10),
                now(),
            ))
            .expect("insert");

        let loaded = store.observations(session.id).expect("load");
        assert!(loaded[0].body.is_truncated());
        assert_eq!(loaded[0].body.len(), 10);
    }

    /// Record a second session, as the SessionStart hook does before claiming.
    fn next_session(store: &Store, project: ProjectId, workspace: WorkspaceId) -> SessionId {
        let mut session = session_for(project, workspace);
        session.id = SessionId::derive(project, "agent-session-2");
        store.ensure_session(&session).expect("second session");
        session.id
    }

    #[test]
    fn a_handoff_can_only_be_claimed_once() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");
        store
            .record_handoff(&new_handoff(
                project,
                session.id,
                Slot::shared(),
                "carry on",
                now(),
            ))
            .expect("record");

        let claimant = next_session(&store, project, workspace);
        let first = store
            .claim_handoff(project, claimant, &Slot::shared(), now())
            .expect("claim");
        assert_eq!(first.as_deref(), Some("carry on"));

        let second = store
            .claim_handoff(project, claimant, &Slot::shared(), now())
            .expect("claim");
        assert_eq!(second, None, "a handoff is single use");
    }

    #[test]
    fn a_claimant_must_be_a_real_session() {
        // The foreign key is the reason a handoff can never be attributed to a
        // session that was never recorded.
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");
        store
            .record_handoff(&new_handoff(
                project,
                session.id,
                Slot::shared(),
                "carry on",
                now(),
            ))
            .expect("record");

        assert!(
            store
                .claim_handoff(project, SessionId::new(), &Slot::shared(), now())
                .is_err(),
            "an unknown claimant should be rejected"
        );
    }

    #[test]
    fn a_newer_handoff_replaces_one_nobody_read() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");

        store
            .record_handoff(&new_handoff(
                project,
                session.id,
                Slot::shared(),
                "older",
                now(),
            ))
            .expect("first");
        store
            .record_handoff(&new_handoff(
                project,
                session.id,
                Slot::shared(),
                "newer",
                now(),
            ))
            .expect("second");

        let claimant = next_session(&store, project, workspace);
        let claimed = store
            .claim_handoff(project, claimant, &Slot::shared(), now())
            .expect("claim");
        assert_eq!(claimed.as_deref(), Some("newer"));
    }

    #[test]
    fn claiming_with_nothing_pending_returns_nothing() {
        let (_dir, store, project, workspace) = fixture();
        let claimant = next_session(&store, project, workspace);
        assert_eq!(
            store
                .claim_handoff(project, claimant, &Slot::shared(), now())
                .expect("claim"),
            None
        );
    }

    /// The case this exists for: a note written from a bad reply, dropped
    /// before any session is handed it. Claiming it to be rid of it would put
    /// it in exactly the context it was being kept out of.
    struct FakeEmbedder;

    impl anamnesis_core::embedding::Embed for FakeEmbedder {
        fn model(&self) -> &str {
            "fake-embed-1"
        }
        fn embed(&self, _text: &str) -> std::result::Result<Vec<f32>, String> {
            Ok(vec![1.0, 0.0])
        }
    }

    struct BrokenEmbedder;

    impl anamnesis_core::embedding::Embed for BrokenEmbedder {
        fn model(&self) -> &str {
            "broken"
        }
        fn embed(&self, _text: &str) -> std::result::Result<Vec<f32>, String> {
            Err("no model loaded".to_owned())
        }
    }

    fn indexable_page(project: ProjectId) -> Page {
        let frontmatter = anamnesis_core::page::Frontmatter::new(
            "Why SQLite",
            vec![anamnesis_core::page::Entity::parse("SQLite").expect("entity")],
        )
        .expect("frontmatter");
        Page::new(
            project,
            anamnesis_core::page::PagePath::parse("decisions/0001-storage.md").expect("path"),
            frontmatter,
            "One file on disk. See [[notes/windows.md]].",
        )
    }

    /// The four writes that make a page findable, in one call. Any one of them
    /// missing is a page that is reachable through some streams and not
    /// others, which is what six of the seven old copies did with the vector.
    #[test]
    fn indexing_a_page_writes_the_row_the_names_the_links_and_the_vector() {
        let (_dir, store, project, _workspace) = fixture();
        let page = indexable_page(project);

        store
            .index_page(
                project,
                &page,
                &["notes/windows.md".to_owned()],
                Some(&FakeEmbedder),
                now(),
            )
            .expect("index");

        assert_eq!(store.page_count(project).expect("count"), 1);
        let hits = store
            .query_pages(project, "sqlite", 5, now(), None)
            .expect("query");
        assert_eq!(hits.len(), 1, "the entity stream should reach it");

        let by_vector = store
            .query_pages(
                project,
                "sqlite",
                5,
                now(),
                Some(("fake-embed-1", &[1.0, 0.0])),
            )
            .expect("query");
        assert_eq!(by_vector.len(), 1, "and so should the vector stream");
    }

    /// An embedding costs a page one retrieval stream when it fails. Refusing
    /// the write would cost the page.
    #[test]
    fn a_failing_embedder_does_not_cost_the_page() {
        let (_dir, store, project, _workspace) = fixture();
        let page = indexable_page(project);

        store
            .index_page(project, &page, &[], Some(&BrokenEmbedder), now())
            .expect("the page is still indexed");

        assert_eq!(store.page_count(project).expect("count"), 1);
        assert_eq!(
            store
                .query_pages(project, "sqlite", 5, now(), Some(("broken", &[1.0, 0.0])))
                .expect("query")
                .len(),
            1,
            "reachable by every stream but the one that failed"
        );
    }

    #[test]
    fn a_discarded_handoff_is_never_handed_to_anybody() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");
        store
            .record_handoff(&new_handoff(
                project,
                session.id,
                Slot::shared(),
                "nonsense the model wrote",
                now(),
            ))
            .expect("record");

        assert_eq!(
            store
                .discard_handoff(project, &Slot::shared())
                .expect("discard")
                .as_deref(),
            Some("nonsense the model wrote"),
            "the caller is told what it dropped, not just that it worked"
        );
        assert_eq!(
            store.peek_handoff(project, &Slot::shared()).expect("peek"),
            None
        );

        let claimant = SessionId::derive(project, "next");
        let next = Session {
            id: claimant,
            ..session_for(project, workspace)
        };
        store.ensure_session(&next).expect("session");
        assert_eq!(
            store
                .claim_handoff(project, claimant, &Slot::shared(), now())
                .expect("claim"),
            None
        );
    }

    #[test]
    fn discarding_nothing_says_nothing_rather_than_failing() {
        let (_dir, store, project, _workspace) = fixture();
        assert_eq!(
            store
                .discard_handoff(project, &Slot::shared())
                .expect("discard"),
            None
        );
    }

    /// Slots are separate for discarding as they are for everything else: one
    /// operator throwing away their own note must not throw away another's.
    #[test]
    fn discarding_one_slot_leaves_the_others_pending() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");

        let alice = Slot::shared().for_operator(Some(
            anamnesis_core::scope::OperatorName::parse("alice").expect("name"),
        ));
        for (slot, body) in [(Slot::shared(), "shared"), (alice.clone(), "alice's")] {
            store
                .record_handoff(&new_handoff(project, session.id, slot, body, now()))
                .expect("record");
        }

        store.discard_handoff(project, &alice).expect("discard");

        assert_eq!(
            store
                .peek_handoff(project, &Slot::shared())
                .expect("peek")
                .as_deref(),
            Some("shared")
        );
        assert_eq!(store.peek_handoff(project, &alice).expect("peek"), None);
    }

    #[test]
    fn peeking_at_a_handoff_leaves_it_claimable() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");
        store
            .record_handoff(&new_handoff(
                project,
                session.id,
                Slot::shared(),
                "carry on",
                now(),
            ))
            .expect("record");

        assert_eq!(
            store
                .peek_handoff(project, &Slot::shared())
                .expect("peek")
                .as_deref(),
            Some("carry on")
        );
        // Twice, to be sure looking is not a one-shot read either.
        assert_eq!(
            store
                .peek_handoff(project, &Slot::shared())
                .expect("peek")
                .as_deref(),
            Some("carry on")
        );

        let claimant = next_session(&store, project, workspace);
        assert_eq!(
            store
                .claim_handoff(project, claimant, &Slot::shared(), now())
                .expect("claim")
                .as_deref(),
            Some("carry on"),
            "peeking must not have consumed it"
        );
        assert_eq!(
            store.peek_handoff(project, &Slot::shared()).expect("peek"),
            None
        );
    }

    #[test]
    fn recent_sessions_come_back_newest_first_with_their_counts() {
        let (_dir, store, project, workspace) = fixture();

        for (index, agent) in ["claude-code", "codex"].into_iter().enumerate() {
            let mut session = session_for(project, workspace);
            session.id = SessionId::derive(project, &format!("agent-session-{index}"));
            session.agent = agent.parse().expect("agent");
            session.started_at = format!("2026-08-19T0{index}:00:00Z").parse().expect("time");
            store.ensure_session(&session).expect("session");

            for _ in 0..=index {
                store
                    .insert_observation(&new_observation(
                        session.id,
                        EventKind::UserPrompt,
                        None,
                        BoundedBody::truncating("hello", 1024),
                        now(),
                    ))
                    .expect("observation");
            }
        }

        let sessions = store.recent_sessions(project, 10).expect("list");
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].agent, "codex", "newest session should lead");
        assert_eq!(sessions[0].observation_count, 2);
        assert_eq!(sessions[1].observation_count, 1);
        assert!(sessions[0].workstream.is_none());
    }

    #[test]
    fn recent_sessions_honours_its_limit() {
        let (_dir, store, project, workspace) = fixture();
        for index in 0..3 {
            let mut session = session_for(project, workspace);
            session.id = SessionId::derive(project, &format!("agent-session-{index}"));
            store.ensure_session(&session).expect("session");
        }
        assert_eq!(store.recent_sessions(project, 2).expect("list").len(), 2);
    }

    #[test]
    fn an_open_session_reports_its_newest_observation() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");
        for minute in ["09:05", "09:20", "09:12"] {
            let at: Timestamp = format!("2026-08-19T{minute}:00Z").parse().expect("time");
            store
                .insert_observation(&new_observation(
                    session.id,
                    EventKind::ToolUse,
                    None,
                    BoundedBody::truncating("event", 1024),
                    at,
                ))
                .expect("insert");
        }

        let open = store.open_sessions().expect("list");

        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, session.id);
        assert_eq!(open[0].checkout_path, PathBuf::from("/repo"));
        assert_eq!(
            open[0].last_seen,
            "2026-08-19T09:20:00Z".parse::<Timestamp>().expect("time"),
            "the newest observation should win, not the last one inserted"
        );
    }

    /// A session that has only been started has no observation to date it by,
    /// and it is the one most likely to be abandoned.
    #[test]
    fn a_session_with_no_observations_is_dated_by_its_start() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");

        let open = store.open_sessions().expect("list");

        assert_eq!(open.len(), 1);
        assert_eq!(open[0].last_seen, session.started_at);
    }

    #[test]
    fn a_closed_session_is_not_open() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");
        store.close_session(session.id, now()).expect("close");

        assert!(store.open_sessions().expect("list").is_empty());
    }

    /// The point of the claim: two ticks overlapping must not both summarise
    /// the same session.
    #[test]
    fn only_the_first_claim_on_a_session_succeeds() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");

        assert!(store.begin_ending(session.id).expect("claim"));
        assert!(!store.begin_ending(session.id).expect("claim again"));
        assert!(
            store.open_sessions().expect("list").is_empty(),
            "a claimed session is no longer on offer"
        );
    }

    /// The claim is released only from `ending`. A session that was properly
    /// closed must not come back from a stray call.
    #[test]
    fn releasing_a_claim_does_not_resurrect_a_closed_session() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");
        store.close_session(session.id, now()).expect("close");

        store.reopen_session(session.id).expect("reopen");

        assert!(store.open_sessions().expect("list").is_empty());
    }

    /// And the converse: resuming is for a session that ended, not for one
    /// somebody is summarising right now.
    #[test]
    fn resuming_leaves_a_session_that_is_being_summarised_alone() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");
        store.begin_ending(session.id).expect("claim");

        store.resume_session(session.id).expect("resume");

        assert!(store.open_sessions().expect("list").is_empty());
    }

    #[test]
    fn reopening_a_claimed_session_puts_it_back_on_offer() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");
        store.begin_ending(session.id).expect("claim");

        store.reopen_session(session.id).expect("reopen");

        assert_eq!(store.open_sessions().expect("list").len(), 1);
        assert!(store.begin_ending(session.id).expect("claim"));
    }

    /// An agent that goes quiet for hours and then keeps working: the session
    /// was summarised in the meantime, and the rest of it must not be lost.
    #[test]
    fn resuming_a_closed_session_clears_the_end_it_was_given() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");
        store.close_session(session.id, now()).expect("close");

        store.resume_session(session.id).expect("resume");

        let loaded = store
            .load_session(session.id)
            .expect("load")
            .expect("found");
        assert!(loaded.is_open());
        assert_eq!(loaded.ended_at, None);
    }

    #[test]
    fn closing_a_session_records_when_it_ended() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");

        let ended: Timestamp = "2026-08-19T11:00:00Z".parse().expect("time");
        store.close_session(session.id, ended).expect("close");

        let loaded = store
            .load_session(session.id)
            .expect("load")
            .expect("found");
        assert!(!loaded.is_open());
        assert_eq!(loaded.ended_at, Some(ended));
    }

    #[test]
    fn a_page_the_index_has_never_seen_is_not_current() {
        let (_dir, store, project, _workspace) = fixture();
        let page = write_page(&store, project, "a.md", None);
        store.delete_page(page.id).expect("delete");
        assert!(!store.page_is_current(&page).expect("check"));
    }

    #[test]
    fn a_page_written_back_unchanged_is_current() {
        let (_dir, store, project, _workspace) = fixture();
        let page = write_page(&store, project, "a.md", None);
        assert!(store.page_is_current(&page).expect("check"));
    }

    /// One thing an author could change in a page's markdown, by name.
    type Edit = (&'static str, fn(&mut anamnesis_core::page::Page));

    /// The watcher leans on this to tell a real edit from its own write, so
    /// every field an author can change from the markdown has to count.
    #[test]
    fn any_field_an_author_can_edit_makes_a_page_stale() {
        let (_dir, store, project, _workspace) = fixture();
        let page = write_page(&store, project, "a.md", None);

        use anamnesis_core::page::{PageStatus, Tier};

        let edits: [Edit; 8] = [
            ("body", |p| p.body = "Something else.".to_owned()),
            ("title", |p| p.frontmatter.title = "Renamed".to_owned()),
            ("tier", |p| p.frontmatter.tier = Tier::Semantic),
            ("status", |p| {
                p.frontmatter.status = PageStatus::DoNotAnswerFrom
            }),
            ("pinned", |p| p.frontmatter.pinned = true),
            ("canonical", |p| p.frontmatter.canonical = true),
            ("salience", |p| p.frontmatter.salience = 0.25),
            ("expires_at", |p| p.frontmatter.expires_at = Some(now())),
        ];

        for (field, edit) in edits {
            let mut edited = page.clone();
            edit(&mut edited);
            assert!(
                !store.page_is_current(&edited).expect("check"),
                "editing {field} should make the page stale"
            );
        }
    }

    #[test]
    fn a_project_that_has_captured_nothing_reports_no_last_event() {
        let (_dir, store, project, _workspace) = fixture();
        assert_eq!(store.last_observation_at(project).expect("query"), None);
    }

    #[test]
    fn the_last_event_is_the_newest_one_the_project_captured() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");

        // Written out of order on purpose: the answer is the newest timestamp,
        // not the last row inserted.
        let newest: Timestamp = "2026-08-19T12:00:00Z".parse().expect("time");
        for at in [
            "2026-08-19T10:00:00Z",
            "2026-08-19T12:00:00Z",
            "2026-08-19T11:00:00Z",
        ] {
            store
                .insert_observation(&new_observation(
                    session.id,
                    EventKind::UserPrompt,
                    None,
                    BoundedBody::truncating("what happened", 1024),
                    at.parse().expect("time"),
                ))
                .expect("observation");
        }

        assert_eq!(
            store.last_observation_at(project).expect("query"),
            Some(newest)
        );
    }

    /// One server serves every project, so "when did capture last reach the
    /// index" has to mean *this* project, not whichever one is busiest.
    #[test]
    fn the_last_event_ignores_other_projects() {
        let (dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");
        store
            .insert_observation(&new_observation(
                session.id,
                EventKind::UserPrompt,
                None,
                BoundedBody::truncating("ours", 1024),
                now(),
            ))
            .expect("observation");

        std::fs::write(
            dir.path().join(".anamnesis.toml"),
            "[scope]
workspace = \"default\"
project = \"other\"
",
        )
        .expect("marker");
        let other = anamnesis_core::scope::resolve_scope(dir.path()).expect("scope");
        store.upsert_project(&other, now()).expect("project");

        assert_eq!(
            store.last_observation_at(other.project_id).expect("query"),
            None
        );
    }

    #[test]
    fn session_ids_derived_from_one_agent_session_agree() {
        let (_dir, _store, project, _workspace) = fixture();
        assert_eq!(
            SessionId::derive(project, "abc-123"),
            SessionId::derive(project, "abc-123")
        );
        assert_ne!(
            SessionId::derive(project, "abc-123"),
            SessionId::derive(project, "abc-124")
        );
    }

    /// Two operators on one server, one project. The whole point of the slot:
    /// each is handed what their own last session left, and neither can
    /// consume the other's by starting first.
    #[test]
    fn one_operators_handoff_is_not_anothers_to_claim() {
        let (_dir, store, project, workspace) = fixture();
        let alice = operator("alice");
        let bob = operator("bob");

        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");
        store
            .record_handoff(&new_handoff(
                project,
                session.id,
                Slot::shared().for_operator(Some(alice.clone())),
                "alice was here",
                now(),
            ))
            .expect("record");
        store
            .record_handoff(&new_handoff(
                project,
                session.id,
                Slot::shared().for_operator(Some(bob.clone())),
                "bob was here",
                now(),
            ))
            .expect("record");

        let claimant = next_session(&store, project, workspace);
        let bobs = store
            .claim_handoff(
                project,
                claimant,
                &Slot::shared().for_operator(Some(bob)),
                now(),
            )
            .expect("claim");
        assert_eq!(bobs.as_deref(), Some("bob was here"));

        // Bob starting first took nothing from alice.
        let alices = store
            .peek_handoff(project, &Slot::shared().for_operator(Some(alice)))
            .expect("peek");
        assert_eq!(alices.as_deref(), Some("alice was here"));
    }

    /// A newer handoff expires the unread one it replaces — but only in its own
    /// slot. Superseding across operators would be the same theft by another
    /// route: alice finishing a session would silently discard bob's note.
    #[test]
    fn a_new_handoff_supersedes_only_its_own_operators() {
        let (_dir, store, project, workspace) = fixture();
        let alice = operator("alice");
        let bob = operator("bob");
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");

        for (who, body) in [
            (Some(alice.clone()), "alice first"),
            (Some(bob.clone()), "bob only"),
            (Some(alice.clone()), "alice second"),
            (None, "the shared slot"),
        ] {
            store
                .record_handoff(&new_handoff(
                    project,
                    session.id,
                    Slot::shared().for_operator(who),
                    body,
                    now(),
                ))
                .expect("record");
        }

        assert_eq!(
            store
                .peek_handoff(project, &Slot::shared().for_operator(Some(alice)))
                .expect("peek")
                .as_deref(),
            Some("alice second"),
        );
        assert_eq!(
            store
                .peek_handoff(project, &Slot::shared().for_operator(Some(bob)))
                .expect("peek")
                .as_deref(),
            Some("bob only"),
        );
        assert_eq!(
            store
                .peek_handoff(project, &Slot::shared())
                .expect("peek")
                .as_deref(),
            Some("the shared slot"),
        );
    }

    /// Every caller the server cannot name shares one slot, which is what an
    /// unauthenticated install has always had.
    #[test]
    fn callers_without_a_name_share_the_slot_they_always_shared() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");
        store
            .record_handoff(&new_handoff(
                project,
                session.id,
                Slot::shared(),
                "carry on",
                now(),
            ))
            .expect("record");

        let claimant = next_session(&store, project, workspace);
        assert_eq!(
            store
                .claim_handoff(project, claimant, &Slot::shared(), now())
                .expect("claim")
                .as_deref(),
            Some("carry on"),
        );
    }

    /// Provenance, not slot: who ran a session survives the round trip whether
    /// or not the project separates slots by it.
    #[test]
    fn a_session_remembers_whose_it_was() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace).with_operator(Some(operator("alice")));
        store.ensure_session(&session).expect("session");

        let loaded = store
            .load_session(session.id)
            .expect("load")
            .expect("session exists");
        assert_eq!(loaded.operator, Some(operator("alice")));
    }

    fn operator(name: &str) -> anamnesis_core::scope::OperatorName {
        anamnesis_core::scope::OperatorName::parse(name).expect("valid operator")
    }
}
