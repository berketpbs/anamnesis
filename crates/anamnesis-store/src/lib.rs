//! Storage layer for the anamnesis memory system using SQLite.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::Connection;
use std::path::Path;
use std::sync::Arc;

/// Database connection pool wrapper.
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    /// Open or create a database at the given path.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let conn = Connection::open(path.as_ref())?;
        conn.pragma_update(rusqlite::OptionalExtension::is_none, "journal_mode", "WAL")?;
        conn.pragma_update(rusqlite::OptionalExtension::is_none, "synchronous", "NORMAL")?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Run database migrations.
    pub fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock();
        refinery::migrations::runner()
            .run(&mut &*conn)
            .map_err(|e| anyhow::anyhow!("Migration failed: {}", e))?;
        Ok(())
    }

    /// Get a reference to the database connection.
    pub fn connection(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }
}
