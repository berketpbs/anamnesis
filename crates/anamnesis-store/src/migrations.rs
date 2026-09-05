//! The schema migrations, and the one thing that keeps a database readable by
//! a different build of the same source.
//!
//! `refinery` records a checksum of each migration's SQL and refuses to open a
//! database whose recorded checksum no longer matches the text the binary
//! carries. That check is worth having: it catches a migration edited after it
//! was applied, which is a schema two databases no longer share.
//!
//! The checksum is over the bytes, and the bytes used to come straight out of
//! a git checkout. With nothing declaring how the files are stored, a checkout
//! on a machine with `core.autocrlf = true` writes CRLF and one without it
//! writes LF, so one commit compiled to binaries that hashed the same
//! migration differently. Every 1.0.0 artifact was built from the same tag and
//! there were three spellings between them: the Windows release CRLF
//! throughout, the Linux and macOS releases LF throughout, and a local Windows
//! build with the first four LF and the rest CRLF. None of the three could
//! open a database created by either of the others. What it said was
//! `applied migration V1__projects_and_sessions is different than filesystem
//! one V1__projects_and_sessions` — a message that names a migration and no
//! cause, on a project whose 1.0 promises that what one version writes, the
//! next one reads.
//!
//! Two things follow, and both are here rather than in a build setting.
//!
//! The text is normalised to LF before it is hashed, so what a database
//! records is a property of the repository and not of the machine that
//! compiled it. `.gitattributes` pins the checkout as well — but a file in the
//! repository cannot reach a source archive, a vendored copy, or an editor
//! that rewrites on save, and this can.
//!
//! And because normalising changes the checksum of every database whose
//! migrations were hashed as CRLF, [`repair_line_endings`] rewrites exactly
//! those rows, once, before the runner is allowed to look at them. It rewrites
//! a row only when the recorded checksum is the one the CRLF spelling of that
//! very migration would produce. A migration whose SQL was genuinely edited
//! matches neither spelling, so it still fails — which is the entire reason
//! the check exists.

use refinery::{Migration, Runner};
use rusqlite::{Connection, OptionalExtension};

/// Every migration, in version order, by the stem of its file.
///
/// Written out rather than globbed. `refinery::embed_migrations!` reads the
/// directory at compile time, and reading the directory is what let the text
/// vary in the first place; naming each file also makes adding one a line
/// somebody writes rather than a side effect of saving a file. The one thing
/// the glob was buying is bought back by `sources_match_the_directory` below,
/// which fails if this list and `migrations/` ever disagree.
const SOURCES: &[(&str, &str)] = &[
    (
        "V01__projects_and_sessions",
        include_str!("../migrations/V01__projects_and_sessions.sql"),
    ),
    ("V02__pages", include_str!("../migrations/V02__pages.sql")),
    (
        "V03__entities_and_links",
        include_str!("../migrations/V03__entities_and_links.sql"),
    ),
    (
        "V04__handoffs_and_feedback",
        include_str!("../migrations/V04__handoffs_and_feedback.sql"),
    ),
    (
        "V05__page_embeddings",
        include_str!("../migrations/V05__page_embeddings.sql"),
    ),
    (
        "V06__workstreams",
        include_str!("../migrations/V06__workstreams.sql"),
    ),
    (
        "V07__proposals",
        include_str!("../migrations/V07__proposals.sql"),
    ),
    (
        "V08__supersedes_target",
        include_str!("../migrations/V08__supersedes_target.sql"),
    ),
    (
        "V09__entity_tokens",
        include_str!("../migrations/V09__entity_tokens.sql"),
    ),
    (
        "V10__operator_slots",
        include_str!("../migrations/V10__operator_slots.sql"),
    ),
    (
        "V11__audit_log",
        include_str!("../migrations/V11__audit_log.sql"),
    ),
];

/// The name refinery gives its own bookkeeping table.
const HISTORY_TABLE: &str = "refinery_schema_history";

/// The text as this project hashes it, whatever the working tree holds.
fn canonical(sql: &str) -> String {
    sql.replace("\r\n", "\n")
}

/// The same migration as a CRLF checkout would have held it.
///
/// Derived from the canonical text rather than read from disk, so the repair
/// below behaves the same whichever way this copy happens to be checked out —
/// including on the machine whose checkout caused the problem.
fn as_crlf(sql: &str) -> String {
    canonical(sql).replace('\n', "\r\n")
}

/// Build one migration from a source entry.
///
/// The names are literals in `SOURCES` and every one of them parses; the test
/// below runs this over all of them, so a name that stops parsing fails at
/// `cargo test` rather than at somebody's first startup.
fn migration(stem: &str, sql: &str) -> Migration {
    Migration::unapplied(stem, sql).expect("migration file names parse")
}

/// The migrations this build applies, in the spelling it hashes them by.
pub(crate) fn runner() -> Runner {
    let migrations: Vec<Migration> = SOURCES
        .iter()
        .map(|(stem, sql)| migration(stem, &canonical(sql)))
        .collect();
    Runner::new(&migrations)
}

/// Rewrite recorded checksums that differ from this build only by line endings.
///
/// Returns the versions it repaired, newest last — empty in the ordinary case,
/// which costs one query against `sqlite_master` when the table is not there
/// and one per migration when it is.
pub(crate) fn repair_line_endings(conn: &Connection) -> rusqlite::Result<Vec<u32>> {
    let history_exists: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [HISTORY_TABLE],
        |row| row.get(0),
    )?;
    if !history_exists {
        return Ok(Vec::new());
    }

    let mut repaired = Vec::new();
    for (stem, sql) in SOURCES {
        let wanted = migration(stem, &canonical(sql));
        let recorded: Option<String> = conn
            .query_row(
                "SELECT checksum FROM refinery_schema_history WHERE version = ?1",
                [wanted.version()],
                |row| row.get(0),
            )
            .optional()?;

        // Not applied here, or already recorded the way this build hashes it.
        let Some(recorded) = recorded else { continue };
        if recorded == wanted.checksum().to_string() {
            continue;
        }

        // Anything that is not the CRLF spelling of this same migration is a
        // real divergence, and belongs to the runner to refuse.
        if recorded != migration(stem, &as_crlf(sql)).checksum().to_string() {
            continue;
        }

        conn.execute(
            "UPDATE refinery_schema_history SET checksum = ?1 WHERE version = ?2",
            rusqlite::params![wanted.checksum().to_string(), wanted.version()],
        )?;
        repaired.push(wanted.version());
    }
    Ok(repaired)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Record `version` in the history table with a checksum somebody else
    /// computed, which is the only interesting starting state here.
    fn record(conn: &Connection, version: u32, name: &str, checksum: &str) {
        conn.execute(
            "INSERT INTO refinery_schema_history (version, name, applied_on, checksum)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![version, name, "2026-09-06T00:00:00Z", checksum],
        )
        .expect("record a migration");
    }

    fn history(conn: &Connection) {
        conn.execute(
            "CREATE TABLE refinery_schema_history(
                 version INT4 PRIMARY KEY,
                 name VARCHAR(255),
                 applied_on VARCHAR(255),
                 checksum VARCHAR(255))",
            [],
        )
        .expect("create the history table");
    }

    fn checksum_of(conn: &Connection, version: u32) -> String {
        conn.query_row(
            "SELECT checksum FROM refinery_schema_history WHERE version = ?1",
            [version],
            |row| row.get(0),
        )
        .expect("read the checksum back")
    }

    #[test]
    fn sources_match_the_directory() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .expect("read migrations/")
            .map(|entry| entry.expect("read an entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
            .map(|path| {
                path.file_stem()
                    .expect("a name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        on_disk.sort();

        let mut listed: Vec<String> = SOURCES.iter().map(|(stem, _)| (*stem).to_owned()).collect();
        listed.sort();

        assert_eq!(
            listed, on_disk,
            "SOURCES and migrations/ disagree — a migration was added to one and not the other"
        );
    }

    #[test]
    fn every_name_parses_and_the_versions_are_a_run() {
        let versions: Vec<u32> = SOURCES
            .iter()
            .map(|(stem, sql)| migration(stem, sql).version())
            .collect();
        let expected: Vec<u32> = (1..=SOURCES.len() as u32).collect();
        assert_eq!(versions, expected, "versions should be 1..=n with no gaps");
    }

    #[test]
    fn line_endings_do_not_change_what_is_hashed() {
        for (stem, sql) in SOURCES {
            let from_lf = migration(stem, &canonical(sql)).checksum();
            let from_crlf = migration(stem, &canonical(&as_crlf(sql))).checksum();
            assert_eq!(
                from_lf, from_crlf,
                "{stem} hashes differently depending on how it was checked out"
            );
        }
    }

    #[test]
    fn the_two_spellings_really_are_different_checksums() {
        // Without this the repair below could pass while doing nothing, and
        // the bug it exists for would look fixed.
        let (stem, sql) = SOURCES[0];
        assert_ne!(
            migration(stem, &canonical(sql)).checksum(),
            migration(stem, &as_crlf(sql)).checksum()
        );
    }

    #[test]
    fn a_crlf_checksum_is_repaired() {
        let conn = Connection::open_in_memory().expect("open");
        history(&conn);
        let (stem, sql) = SOURCES[0];
        let wanted = migration(stem, &canonical(sql));
        record(
            &conn,
            wanted.version(),
            wanted.name(),
            &migration(stem, &as_crlf(sql)).checksum().to_string(),
        );

        let repaired = repair_line_endings(&conn).expect("repair");

        assert_eq!(repaired, vec![wanted.version()]);
        assert_eq!(
            checksum_of(&conn, wanted.version()),
            wanted.checksum().to_string()
        );
    }

    #[test]
    fn an_edited_migration_is_left_for_the_runner_to_refuse() {
        let conn = Connection::open_in_memory().expect("open");
        history(&conn);
        let (stem, sql) = SOURCES[0];
        let wanted = migration(stem, &canonical(sql));
        let someone_elses = migration(stem, "SELECT 'not this migration at all';")
            .checksum()
            .to_string();
        record(&conn, wanted.version(), wanted.name(), &someone_elses);

        let repaired = repair_line_endings(&conn).expect("repair");

        assert!(repaired.is_empty(), "an edit is not a line ending");
        assert_eq!(checksum_of(&conn, wanted.version()), someone_elses);
    }

    #[test]
    fn a_matching_checksum_is_left_alone() {
        let conn = Connection::open_in_memory().expect("open");
        history(&conn);
        let (stem, sql) = SOURCES[0];
        let wanted = migration(stem, &canonical(sql));
        record(
            &conn,
            wanted.version(),
            wanted.name(),
            &wanted.checksum().to_string(),
        );

        assert!(repair_line_endings(&conn).expect("repair").is_empty());
    }

    #[test]
    fn nothing_to_repair_before_anything_is_applied() {
        let conn = Connection::open_in_memory().expect("open");
        assert!(repair_line_endings(&conn).expect("repair").is_empty());
    }

    /// The whole path, not just the repair: a database recorded the way the
    /// 1.0.0 Windows release recorded one, opened by a build that hashes LF.
    /// This is the failure itself, and it is the reason the rest of this file
    /// exists.
    #[test]
    fn a_database_recorded_by_a_crlf_build_opens() {
        let store = crate::Store::open_in_memory().expect("open");
        store.migrate().expect("first migrate");

        {
            let conn = store.connection();
            for (stem, sql) in SOURCES {
                let wanted = migration(stem, &canonical(sql));
                conn.execute(
                    "UPDATE refinery_schema_history SET checksum = ?1 WHERE version = ?2",
                    rusqlite::params![
                        migration(stem, &as_crlf(sql)).checksum().to_string(),
                        wanted.version()
                    ],
                )
                .expect("record it the way a CRLF build would have");
            }
        }

        store
            .migrate()
            .expect("a database from a CRLF build must still open");

        // And the repair is idempotent: the second open has nothing left to do.
        let conn = store.connection();
        assert!(repair_line_endings(&conn).expect("repair").is_empty());
    }

    /// The check that was worth keeping is still there. Without this the fix
    /// above would be indistinguishable from having turned the check off.
    #[test]
    fn an_edited_migration_still_stops_the_runner() {
        let store = crate::Store::open_in_memory().expect("open");
        store.migrate().expect("first migrate");

        {
            let conn = store.connection();
            conn.execute(
                "UPDATE refinery_schema_history SET checksum = ?1 WHERE version = 1",
                [
                    migration("V01__projects_and_sessions", "DROP TABLE projects;")
                        .checksum()
                        .to_string(),
                ],
            )
            .expect("record a migration nobody wrote");
        }

        assert!(
            store.migrate().is_err(),
            "a divergence that is not a line ending must still be refused"
        );
    }
}
