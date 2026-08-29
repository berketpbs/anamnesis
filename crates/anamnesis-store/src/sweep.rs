//! The index side of forgetting: the facts a sweep judges, and the row it
//! removes once a page is gone from the wiki.
//!
//! The rules themselves are not here. Which pages are exempt, and what a
//! retention score has to be before a page is forgotten, is decided by
//! [`anamnesis_core::sweep`] over plain values — this module's whole job is
//! to hand it those values and to delete what it condemned.
//!
//! **Deleting an index row does not forget anything on its own.** The wiki is
//! the source of truth; a page whose row is gone but whose markdown remains
//! comes straight back on the next `anamnesis reindex`. That is exactly why
//! a sweep drops the row *first* and removes the file second: interrupted
//! halfway, it leaves a page that is briefly unfindable and is restored by a
//! rebuild. The other order leaves the index pointing at markdown that no
//! longer exists, which no rebuild repairs.

use anamnesis_core::ids::{PageId, ProjectId};
use anamnesis_core::page::{PagePath, PageStatus, Tier};
use anamnesis_core::sweep::PageFacts;
use rusqlite::params;

use crate::convert::{parse_id, parse_page_path, parse_time};
use crate::{Result, Store};

/// One page, as a sweep sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct SweepRow {
    /// Identifies the page.
    pub page_id: PageId,
    /// Project-relative path, and the file the wiki must lose.
    pub path: PagePath,
    /// Title from frontmatter, for a report someone reads.
    pub title: String,
    /// What the decision is made from.
    pub facts: PageFacts,
}

impl Store {
    /// Every page in a project, with the facts a sweep judges it on.
    ///
    /// Reads all of them, pinned rows included, rather than letting SQL apply
    /// the exemptions the partial `idx_pages_sweep` index was built for. Two
    /// reasons, both about the report rather than the scan: a sweep that
    /// filters in SQL can only say how many pages it deleted, never how many
    /// it spared and why — and a page that is both pinned and past its own
    /// `expires_at`, the one contradiction worth surfacing, would never be
    /// seen at all. This is a maintenance command over one project's pages,
    /// not a query on the retrieval path.
    ///
    /// Ordered by path so two runs against an unchanged index produce the
    /// same report; the caller ranks by score for display.
    pub fn sweep_rows(&self, project_id: ProjectId) -> Result<Vec<SweepRow>> {
        let conn = self.connection();
        let mut statement = conn.prepare(
            "SELECT id, path, title, tier, status, pinned, canonical, salience,
                    access_count, last_accessed_at, expires_at, updated_at
             FROM pages WHERE project_id = ?1 ORDER BY path",
        )?;

        let rows = statement.query_map(params![project_id.to_string()], read_sweep_row)?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// The same facts, for one page.
    ///
    /// Shares the read above rather than scanning it: a caller asking about
    /// one page — the browser showing what retention has in store for it — is
    /// not a maintenance pass over a project, and the two must not be able to
    /// disagree about what a row says.
    pub fn sweep_row(&self, project_id: ProjectId, path: &PagePath) -> Result<Option<SweepRow>> {
        let conn = self.connection();
        let mut statement = conn.prepare(
            "SELECT id, path, title, tier, status, pinned, canonical, salience,
                    access_count, last_accessed_at, expires_at, updated_at
             FROM pages WHERE project_id = ?1 AND path = ?2",
        )?;

        let mut rows = statement.query_map(
            params![project_id.to_string(), path.as_str()],
            read_sweep_row,
        )?;
        match rows.next() {
            None => Ok(None),
            Some(row) => Ok(Some(row?)),
        }
    }

    /// Drop a page's index row, and with it everything derived from the page.
    ///
    /// Entities, links out, embeddings, and feedback go with it by foreign
    /// key; links *into* it become unresolved rather than disappearing, which
    /// is the truth — a page that pointed at this one still says so, and will
    /// resolve again if the page is ever written back.
    ///
    /// Returns whether a row was actually removed, so a caller sweeping a
    /// page the index no longer holds can tell the difference between doing
    /// work and doing nothing.
    pub fn delete_page(&self, page_id: PageId) -> Result<bool> {
        let conn = self.connection();
        let removed = conn.execute(
            "DELETE FROM pages WHERE id = ?1",
            params![page_id.to_string()],
        )?;
        Ok(removed > 0)
    }
}

/// One row of the sweep's projection, however it was selected.
fn read_sweep_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SweepRow> {
    Ok(SweepRow {
        page_id: parse_id(row.get::<_, String>(0)?),
        path: parse_page_path(&row.get::<_, String>(1)?),
        title: row.get(2)?,
        facts: PageFacts {
            tier: Tier::from_storage(&row.get::<_, String>(3)?),
            status: PageStatus::from_storage(&row.get::<_, String>(4)?),
            pinned: row.get(5)?,
            canonical: row.get(6)?,
            salience: row.get(7)?,
            // Stored as INTEGER and read as one: a negative count is
            // impossible from this crate, and saturating is closer to the
            // truth than wrapping to four billion reads.
            access_count: row.get::<_, i64>(8)?.clamp(0, i64::from(u32::MAX)) as u32,
            last_accessed_at: row.get::<_, Option<String>>(9)?.map(|raw| parse_time(&raw)),
            expires_at: row
                .get::<_, Option<String>>(10)?
                .map(|raw| parse_time(&raw)),
            written_at: parse_time(&row.get::<_, String>(11)?),
        },
    })
}

#[cfg(test)]
mod tests {
    use anamnesis_core::ids::ProjectId;
    use anamnesis_core::page::{Entity, Frontmatter, Page, PagePath, Tier};
    use anamnesis_core::sweep::{Exemption, SweepPolicy, Verdict, judge};
    use jiff::Timestamp;

    use crate::Store;
    use crate::convert::{fixture, fixture_now};

    fn days_ago(days: i64) -> Timestamp {
        fixture_now() - jiff::Span::new().hours(days * 24)
    }

    /// Write a page as it stood `age_days` ago.
    fn write_page(store: &Store, project_id: ProjectId, path: &str, age_days: i64) -> Page {
        write_page_with(store, project_id, path, age_days, |_| {})
    }

    fn write_page_with(
        store: &Store,
        project_id: ProjectId,
        path: &str,
        age_days: i64,
        edit: impl FnOnce(&mut Frontmatter),
    ) -> Page {
        let path = PagePath::parse(path).expect("path");
        let mut frontmatter = Frontmatter::new("A page", Vec::new()).expect("frontmatter");
        frontmatter.tier = Tier::Episodic;
        edit(&mut frontmatter);
        let page = Page::new(project_id, path, frontmatter, "Body text about sqlite.");
        store
            .upsert_page(&page, days_ago(age_days))
            .expect("upsert");
        page
    }

    fn row_for<'a>(rows: &'a [super::SweepRow], path: &str) -> &'a super::SweepRow {
        rows.iter()
            .find(|row| row.path.as_str() == path)
            .unwrap_or_else(|| panic!("no row for {path}"))
    }

    #[test]
    fn a_fresh_page_and_an_ancient_one_are_told_apart() {
        let (_dir, store, project, _workspace) = fixture();
        write_page(&store, project, "sessions/fresh.md", 0);
        write_page(&store, project, "sessions/ancient.md", 400);

        let rows = store.sweep_rows(project).expect("rows");
        assert_eq!(rows.len(), 2);

        let policy = SweepPolicy::default();
        assert!(
            !judge(
                &row_for(&rows, "sessions/fresh.md").facts,
                policy,
                fixture_now()
            )
            .forgets()
        );
        assert!(
            judge(
                &row_for(&rows, "sessions/ancient.md").facts,
                policy,
                fixture_now()
            )
            .forgets()
        );
    }

    #[test]
    fn pinned_rows_are_read_rather_than_filtered_out_in_sql() {
        // The report has to be able to say what it spared; a sweep that
        // cannot see pinned pages cannot report them either.
        let (_dir, store, project, _workspace) = fixture();
        write_page_with(&store, project, "notes/pinned.md", 400, |fm| {
            fm.pinned = true
        });

        let rows = store.sweep_rows(project).expect("rows");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].facts.pinned);
        assert_eq!(
            judge(&rows[0].facts, SweepPolicy::default(), fixture_now()),
            Verdict::Exempt {
                exemption: Exemption::Pinned,
                expired: false
            }
        );
    }

    #[test]
    fn a_retrieved_page_carries_its_access_statistics() {
        let (_dir, store, project, _workspace) = fixture();
        let page = write_page(&store, project, "sessions/read.md", 300);
        store.record_access(page.id, days_ago(2)).expect("access");
        store.record_access(page.id, days_ago(1)).expect("access");

        let rows = store.sweep_rows(project).expect("rows");
        assert_eq!(rows[0].facts.access_count, 2);
        assert_eq!(rows[0].facts.last_accessed_at, Some(days_ago(1)));
        // And that is enough to save a page the age term alone would forget.
        assert!(!judge(&rows[0].facts, SweepPolicy::default(), fixture_now()).forgets());
    }

    #[test]
    fn frontmatter_facts_survive_the_round_trip() {
        let (_dir, store, project, _workspace) = fixture();
        let expires = days_ago(3);
        write_page_with(&store, project, "notes/typed.md", 10, |fm| {
            fm.tier = Tier::Procedural;
            fm.canonical = true;
            fm.salience = 2.5;
            fm.expires_at = Some(expires);
        });

        let rows = store.sweep_rows(project).expect("rows");
        let facts = &rows[0].facts;
        assert_eq!(facts.tier, Tier::Procedural);
        assert!(facts.canonical);
        assert_eq!(facts.salience, 2.5);
        assert_eq!(facts.expires_at, Some(expires));
        assert_eq!(facts.written_at, days_ago(10));
    }

    #[test]
    fn only_the_project_being_swept_is_scanned() {
        let (_dir, store, project, _workspace) = fixture();
        write_page(&store, project, "sessions/mine.md", 1);

        let other = ProjectId::derive(
            &anamnesis_core::scope::WorkspaceName::default(),
            &anamnesis_core::scope::ProjectKey::from_name(
                &anamnesis_core::scope::ProjectName::sanitized("other").expect("name"),
            ),
        );
        assert!(store.sweep_rows(other).expect("rows").is_empty());
    }

    #[test]
    fn deleting_a_page_takes_its_derived_rows_with_it() {
        let (_dir, store, project, _workspace) = fixture();
        let entity = Entity::parse("SQLite").expect("entity");
        let target = write_page(&store, project, "decisions/0001-storage.md", 400);
        store
            .set_page_entities(project, target.id, std::slice::from_ref(&entity))
            .expect("entities");

        let source = write_page(&store, project, "sessions/mentions.md", 1);
        store
            .set_page_links(
                project,
                source.id,
                &["decisions/0001-storage.md".to_owned()],
            )
            .expect("links");

        assert!(store.delete_page(target.id).expect("delete"));

        let conn = store.connection();
        let entities: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM page_entities WHERE page_id = ?1",
                params_of(target.id),
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(entities, 0);

        // The page that pointed at it still says so; the link is simply
        // unresolved again.
        let resolved: Option<String> = conn
            .query_row(
                "SELECT to_page_id FROM page_links WHERE from_page_id = ?1",
                params_of(source.id),
                |row| row.get(0),
            )
            .expect("link");
        assert!(resolved.is_none());
    }

    #[test]
    fn a_deleted_page_stops_being_retrievable() {
        let (_dir, store, project, _workspace) = fixture();
        let page = write_page(&store, project, "decisions/0001-storage.md", 400);
        assert_eq!(
            store
                .query_pages(project, "sqlite", 10, fixture_now(), None)
                .expect("query")
                .len(),
            1
        );

        store.delete_page(page.id).expect("delete");
        assert!(
            store
                .query_pages(project, "sqlite", 10, fixture_now(), None)
                .expect("query")
                .is_empty()
        );
    }

    #[test]
    fn deleting_a_page_the_index_does_not_hold_is_not_an_error() {
        let (_dir, store, project, _workspace) = fixture();
        let page = write_page(&store, project, "sessions/gone.md", 1);
        assert!(store.delete_page(page.id).expect("first"));
        assert!(!store.delete_page(page.id).expect("second"));
    }

    /// One-element parameter list for the raw-SQL assertions above.
    fn params_of(id: anamnesis_core::ids::PageId) -> [String; 1] {
        [id.to_string()]
    }
}
