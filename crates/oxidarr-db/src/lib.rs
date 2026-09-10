//! `SQLite` persistence layer for Oxidarr
//!
//! [`Db`] owns a `SQLx` connection pool opened against either a file
//! ([`Db::open`]) or an in-memory database ([`Db::open_in_memory`]), applying
//! the crate's embedded migrations on open so callers never see a
//! partially-migrated schema. Repository types built on top of [`Db`] land
//! in later work; this crate currently only owns the pool and the schema.

pub mod config;
pub mod error;
pub mod indexer;

pub use config::ConfigRepo;
pub use error::DbError;
pub use indexer::{IndexerKind, IndexerRepo, IndexerRow, NewIndexer};

use sqlx::SqlitePool;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::path::Path;

/// The crate's embedded migrations, shared with `oxidarr-migrate`.
static MIGRATOR: Migrator = sqlx::migrate!();

/// A `SQLite` connection pool with the crate's migrations applied.
#[derive(Debug)]
pub struct Db(SqlitePool);

impl Db {
    /// Opens the database file at `path` (creating it if absent), enables
    /// WAL journaling and foreign-key enforcement, and applies any pending
    /// embedded migrations.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlx`] if the connection cannot be established and
    /// [`DbError::Migrate`] if a migration fails to apply.
    pub async fn open(path: &Path) -> Result<Self, DbError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new().connect_with(options).await?;
        Self::migrator().run(&pool).await?;
        Ok(Self(pool))
    }

    /// Opens an in-memory database for tests and tooling, and applies the
    /// crate's embedded migrations.
    ///
    /// `SQLite`'s `:memory:` database is private to the connection that
    /// created it, so the pool is capped at a single connection — a second
    /// connection would see an empty, unmigrated database. That alone is not
    /// enough: `SQLx`'s default reaper can still close an idle or expired
    /// connection and open a fresh one in its place, which for `:memory:`
    /// means silently losing the schema. `idle_timeout(None)`,
    /// `max_lifetime(None)`, and `min_connections(1)` disable that reaping so
    /// the one connection — and the schema on it — lives for the pool's
    /// lifetime.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlx`] if the connection cannot be established and
    /// [`DbError::Migrate`] if a migration fails to apply.
    pub async fn open_in_memory() -> Result<Self, DbError> {
        let options = SqliteConnectOptions::new()
            .filename(":memory:")
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(options)
            .await?;
        Self::migrator().run(&pool).await?;
        Ok(Self(pool))
    }

    /// The underlying connection pool.
    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.0
    }

    /// The embedded migrator, shared with `oxidarr-migrate`.
    #[must_use]
    pub fn migrator() -> &'static Migrator {
        &MIGRATOR
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test]
    async fn open_in_memory_applies_migrations_and_enforces_foreign_keys() {
        let db = Db::open_in_memory().await.unwrap();
        // migrations applied: the tables exist
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN
             ('indexers','applications','app_indexer_map','config')",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(n, 4);
        // foreign keys enforced: inserting a mapping with no parent rows fails
        let err = sqlx::query(
            "INSERT INTO app_indexer_map (app_id, indexer_id, remote_indexer_id) VALUES (1,2,3)",
        )
        .execute(db.pool())
        .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn open_creates_the_file_and_reopen_is_a_migration_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oxidarr.db");
        {
            Db::open(&path).await.unwrap();
        }
        assert!(path.exists());
        // second open must not error (idempotent migrations)
        Db::open(&path).await.unwrap();
    }

    #[tokio::test]
    async fn open_sets_wal_journal_mode_and_enforces_foreign_keys_on_the_file_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oxidarr.db");
        let db = Db::open(&path).await.unwrap();

        // regression pin: WAL journaling is enabled on the file-backed pool,
        // not just asserted in the doc comment.
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(mode, "wal");

        // foreign keys are enforced on the file-backed pool too, not only
        // on the in-memory one.
        let err = sqlx::query(
            "INSERT INTO app_indexer_map (app_id, indexer_id, remote_indexer_id) VALUES (1,2,3)",
        )
        .execute(db.pool())
        .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn open_in_memory_disables_the_connection_reaper() {
        // regression pin: the sole `:memory:` connection must never be
        // reaped and replaced, or the schema silently vanishes. SQLx has no
        // hook to observe an actual reap, so this pins the pool options
        // that prevent one instead.
        let db = Db::open_in_memory().await.unwrap();
        let options = db.pool().options();
        assert_eq!(options.get_max_connections(), 1);
        assert_eq!(options.get_min_connections(), 1);
        assert_eq!(options.get_idle_timeout(), None);
        assert_eq!(options.get_max_lifetime(), None);
    }
}
