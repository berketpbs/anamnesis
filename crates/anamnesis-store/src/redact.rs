//! Applying today's redaction to what was stored before it existed.
//!
//! Redaction runs once, when an event is captured. That is the right moment and
//! it is not enough: a rule added later protects every event after it and none
//! before. On the machine this project is developed on, the rule for Google's
//! `AQ.` keys landed on 2026-09-03, a day after a session in which such a key
//! was typed into a prompt — and that key sat in the raw spool and the index,
//! and in every backup taken since, while every check that looked at new
//! captures said redaction was working.
//!
//! So stored observations can be run through the current rules again: the
//! index rows here, the spool files in [`crate::RawSpool::redact_file`]. Both
//! count what they would change by rule name and never by value, and change
//! nothing unless asked to.

use std::collections::BTreeMap;

use anamnesis_core::sanitize::Redactor;
use rusqlite::params;

use crate::Store;

/// What running today's rules over stored text found, or changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Redaction {
    /// Records looked at: index rows, or spool lines.
    pub examined: usize,
    /// Records that held something a rule masks.
    pub changed: usize,
    /// How many records each rule fired on, by rule name.
    pub rules: BTreeMap<&'static str, usize>,
}

impl Redaction {
    /// Fold another result into this one.
    pub fn absorb(&mut self, other: &Redaction) {
        self.examined += other.examined;
        self.changed += other.changed;
        for (rule, count) in &other.rules {
            *self.rules.entry(rule).or_default() += count;
        }
    }

    /// Record one record's hits: the rule names a redaction reported for it.
    pub fn count(&mut self, hits: &[&'static str]) {
        if hits.is_empty() {
            return;
        }
        self.changed += 1;
        // A rule that fired twice on one record counts once: the number is
        // "how many records carried this", which is what somebody deciding
        // whether to run `--apply` wants.
        let mut seen: Vec<&'static str> = hits.to_vec();
        seen.sort_unstable();
        seen.dedup();
        for rule in seen {
            *self.rules.entry(rule).or_default() += 1;
        }
    }
}

impl Store {
    /// Run `redactor` over every observation body in the index.
    ///
    /// With `apply`, bodies that change are rewritten in one transaction and
    /// marked sanitized; without it, nothing is written. A body that is
    /// already clean is never touched, so running this twice changes nothing
    /// the second time.
    pub fn redact_observations(
        &self,
        redactor: &Redactor,
        apply: bool,
    ) -> crate::Result<Redaction> {
        let mut conn = self.connection();
        let mut found = Redaction::default();
        let mut rewrites: Vec<(String, String)> = Vec::new();
        {
            let mut statement = conn.prepare("SELECT id, body FROM observations")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (id, body) = row?;
                found.examined += 1;
                let redacted = redactor.redact(&body);
                // Compared, not asked: a rule reports a hit whenever its
                // pattern matches, and `api_key=[redacted]` matches the
                // assignment rule again and comes out the same. Counting those
                // made a first run report 130 records where far fewer held
                // anything still unmasked.
                if redacted.is_clean() || redacted.text() == body {
                    continue;
                }
                found.count(redacted.hits());
                if apply {
                    rewrites.push((id, redacted.into_text()));
                }
            }
        }
        if apply && !rewrites.is_empty() {
            // An UPDATE frees the old cell and, for a long body, its overflow
            // pages, and SQLite leaves freed bytes where they were. A rewrite
            // that masks a key in the row and keeps it in the free space of the
            // same file has masked nothing, so freed content is zeroed for the
            // length of this transaction.
            conn.pragma_update(None, "secure_delete", true)?;
            let written = (|| {
                let transaction = conn.transaction()?;
                for (id, body) in &rewrites {
                    transaction.execute(
                        "UPDATE observations SET body = ?2, sanitized = 1 WHERE id = ?1",
                        params![id, body],
                    )?;
                }
                transaction.commit()
            })();
            conn.pragma_update(None, "secure_delete", false)?;
            written?;
        }
        Ok(found)
    }

    /// Move everything in the write-ahead log into the database file, and
    /// empty the log.
    ///
    /// The index runs in WAL mode, so a rewritten row lives in `-wal` until
    /// a checkpoint, while the database file keeps the page as it was, old
    /// value and all. On the machine this was written for, `redact --apply` left
    /// the index rows clean and one copy of the key in `anamnesis.db`
    /// until this ran. Returns `false` when a reader holding an older snapshot
    /// kept it from finishing. The old page is then still in the file, and
    /// running it again once that reader is gone finishes the job.
    pub fn checkpoint(&self) -> crate::Result<bool> {
        let busy: i64 =
            self.connection()
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        Ok(busy == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key the rules mask today, in the shape a person pastes. Not a key.
    const KEY: &str = "AQ.testonlynotarealkey0123456789abcdefghijklmn";

    /// Rows as they were stored before the rule existed: marked sanitized,
    /// because they had been through the rules there were.
    fn store_with(bodies: &[&str]) -> Store {
        seeded(Store::open_in_memory().expect("open"), bodies)
    }

    fn seeded(store: Store, bodies: &[&str]) -> Store {
        store.migrate().expect("migrate");
        {
            let conn = store.connection();
            conn.execute(
                "INSERT INTO projects (id, workspace_id, workspace, name, project_key, created_at, updated_at)
                 VALUES ('p', 'w', 'default', 'demo', 'path:demo', '2026-09-02T00:00:00Z', '2026-09-02T00:00:00Z')",
                [],
            )
            .expect("project");
            conn.execute(
                "INSERT INTO sessions (id, project_id, agent, checkout_path, state, started_at)
                 VALUES ('s', 'p', 'claude-code', '/repo', 'closed', '2026-09-02T00:00:00Z')",
                [],
            )
            .expect("session");
            for (index, body) in bodies.iter().enumerate() {
                conn.execute(
                    "INSERT INTO observations (id, session_id, kind, at, body, sanitized)
                     VALUES (?1, 's', 'user-prompt', '2026-09-02T00:57:12Z', ?2, 1)",
                    params![format!("o{index}"), body],
                )
                .expect("observation");
            }
        }
        store
    }

    fn bodies(store: &Store) -> Vec<String> {
        let conn = store.connection();
        let mut statement = conn
            .prepare("SELECT body FROM observations ORDER BY rowid")
            .expect("prepare");
        statement
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows")
    }

    #[test]
    fn a_dry_run_counts_by_rule_and_changes_nothing() {
        let store = store_with(&[&format!("use {KEY} for gemini"), "nothing secret here"]);

        let found = store
            .redact_observations(&Redactor::new(), false)
            .expect("redact");

        assert_eq!(found.examined, 2);
        assert_eq!(found.changed, 1);
        assert_eq!(found.rules.get("google-auth-key"), Some(&1));
        assert!(bodies(&store)[0].contains(KEY), "a dry run wrote");
    }

    #[test]
    fn applying_masks_the_value_and_a_second_run_finds_nothing() {
        let store = store_with(&[&format!("use {KEY} for gemini")]);

        store
            .redact_observations(&Redactor::new(), true)
            .expect("apply");
        let after = bodies(&store);
        assert_eq!(after[0], "use [redacted:google-auth-key] for gemini");

        let again = store
            .redact_observations(&Redactor::new(), true)
            .expect("again");
        assert_eq!(again.changed, 0, "redaction is idempotent");
    }

    /// The row is not the only copy. The database file keeps the page the
    /// row was on until a checkpoint, and the free space an UPDATE leaves
    /// keeps the bytes that were there. Both ways kept the value in
    /// `anamnesis.db` after every row said it was masked.
    #[test]
    fn applying_leaves_no_copy_of_the_value_in_the_database_files() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("index.db");
        // Long enough to spill onto overflow pages, which an UPDATE frees
        // rather than rewrites.
        let body = format!(
            "{} use {KEY} for gemini {}",
            "a".repeat(6000),
            "b".repeat(6000)
        );
        let store = seeded(Store::open(&path).expect("open"), &[&body]);
        assert!(store.checkpoint().expect("checkpoint"), "seed checkpoint");
        let copies = |file: &std::path::Path| -> usize {
            let bytes = std::fs::read(file).unwrap_or_default();
            bytes
                .windows(KEY.len())
                .filter(|window| *window == KEY.as_bytes())
                .count()
        };
        assert_eq!(copies(&path), 1, "the value is in the file to begin with");

        store
            .redact_observations(&Redactor::new(), true)
            .expect("apply");
        assert!(
            store.checkpoint().expect("checkpoint"),
            "nothing else reads"
        );

        let wal = dir.path().join("index.db-wal");
        assert_eq!(copies(&path) + copies(&wal), 0, "a copy survived");
    }

    /// Text already masked matches the assignment rule again and comes out
    /// the same; that is not something left to redact. Counting it made the
    /// first run over real memory report 130 records where 11 held anything.
    #[test]
    fn a_value_masked_at_capture_is_not_counted_again() {
        let store = store_with(&[
            "export API_KEY=[redacted]",
            "Authorization: Bearer [redacted]",
        ]);

        let found = store
            .redact_observations(&Redactor::new(), false)
            .expect("redact");

        assert_eq!(found.changed, 0, "{:?}", found.rules);
    }

    #[test]
    fn results_add_up_across_sources() {
        let mut total = Redaction::default();
        let mut one = Redaction {
            examined: 3,
            ..Redaction::default()
        };
        one.count(&["google-auth-key", "google-auth-key", "jwt"]);
        total.absorb(&one);
        total.absorb(&one);

        assert_eq!(total.examined, 6);
        assert_eq!(total.changed, 2);
        assert_eq!(
            total.rules["google-auth-key"], 2,
            "once per record, not per match"
        );
        assert_eq!(total.rules["jwt"], 2);
    }
}
