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

        // Enable WAL mode and optimized pragmas
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "cache_size", "-64000")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Run database migrations.
    pub fn migrate(&self) -> Result<()> {
        let mut conn = self.conn.lock();
        // Migration runner will be implemented
        // For now, just ensure tables exist
        let _conn = &mut *conn;
        // TODO: Implement refinery migration runner
        Ok(())
    }

    /// Get a reference to the database connection.
    pub fn connection(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }
}
