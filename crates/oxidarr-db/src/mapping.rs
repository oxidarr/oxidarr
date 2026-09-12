//! App-indexer mapping repository, backed by the `app_indexer_map` table.

use oxidarr_core::ids::{AppId, IndexerId, RemoteIndexerId};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Db, DbError};

/// A single (application, indexer) pairing: which id the indexer is known by
/// inside that application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappingRow {
    /// The application side of the pairing.
    pub app_id: AppId,
    /// The indexer side of the pairing.
    pub indexer_id: IndexerId,
    /// The id this indexer is registered under inside the application.
    pub remote_indexer_id: RemoteIndexerId,
}

/// Read/write access to the `app_indexer_map` table.
#[derive(Debug)]
pub struct MappingRepo<'a>(&'a Db);

impl<'a> MappingRepo<'a> {
    /// Builds a repo borrowing `db` for the lifetime of the calls made
    /// through it.
    #[must_use]
    pub fn new(db: &'a Db) -> Self {
        Self(db)
    }

    /// Upserts `row`: a re-sync overwrites the remote id for the same
    /// (app, indexer) pair.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlx`] if the query fails, including a foreign-key
    /// violation when `row.app_id` or `row.indexer_id` names a row that does
    /// not exist.
    pub async fn set(&self, row: MappingRow) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO app_indexer_map (app_id, indexer_id, remote_indexer_id)
             VALUES (?, ?, ?)
             ON CONFLICT(app_id, indexer_id) DO UPDATE SET
                 remote_indexer_id = excluded.remote_indexer_id",
        )
        .bind(row.app_id.0)
        .bind(row.indexer_id.0)
        .bind(row.remote_indexer_id.0)
        .execute(self.0.pool())
        .await?;

        Ok(())
    }

    /// Looks up the mapping for `(app, indexer)`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Corrupt`] if the stored row cannot be decoded, or
    /// [`DbError::Sqlx`] if the query fails.
    pub async fn get(&self, app: AppId, indexer: IndexerId) -> Result<Option<MappingRow>, DbError> {
        let row = sqlx::query(
            "SELECT app_id, indexer_id, remote_indexer_id
             FROM app_indexer_map WHERE app_id = ? AND indexer_id = ?",
        )
        .bind(app.0)
        .bind(indexer.0)
        .fetch_optional(self.0.pool())
        .await?;

        row.as_ref().map(row_to_mapping).transpose()
    }

    /// Lists every mapping for `app`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Corrupt`] if a stored row cannot be decoded, or
    /// [`DbError::Sqlx`] if the query fails.
    pub async fn for_app(&self, app: AppId) -> Result<Vec<MappingRow>, DbError> {
        let rows = sqlx::query(
            "SELECT app_id, indexer_id, remote_indexer_id
             FROM app_indexer_map WHERE app_id = ? ORDER BY indexer_id",
        )
        .bind(app.0)
        .fetch_all(self.0.pool())
        .await?;

        rows.iter().map(row_to_mapping).collect()
    }

    /// Lists every mapping for `indexer`, across every application.
    ///
    /// Used by `oxidarr-prowl`'s indexer-delete handler to snapshot which
    /// applications currently map this indexer remotely *before* deleting
    /// the indexer row: `app_indexer_map.indexer_id` is `ON DELETE CASCADE`
    /// (see this table's migration), so every one of these rows would
    /// otherwise vanish the instant the indexer itself is deleted, taking
    /// the only record of the now-orphaned remote entry's id with it.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Corrupt`] if a stored row cannot be decoded, or
    /// [`DbError::Sqlx`] if the query fails.
    pub async fn for_indexer(&self, indexer: IndexerId) -> Result<Vec<MappingRow>, DbError> {
        let rows = sqlx::query(
            "SELECT app_id, indexer_id, remote_indexer_id
             FROM app_indexer_map WHERE indexer_id = ? ORDER BY app_id",
        )
        .bind(indexer.0)
        .fetch_all(self.0.pool())
        .await?;

        rows.iter().map(row_to_mapping).collect()
    }

    /// Deletes the mapping for `(app, indexer)`, if any.
    ///
    /// Unlike [`crate::ApplicationRepo::delete`] and
    /// [`crate::IndexerRepo::delete`], this is idempotent: deleting an
    /// absent mapping is `Ok`, not [`DbError::NotFound`]. Sync reconciliation
    /// calls this blindly to drop stale mappings without first checking
    /// whether they exist, so treating "already gone" as an error would
    /// force every caller to swallow it anyway.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlx`] if the query fails.
    pub async fn delete(&self, app: AppId, indexer: IndexerId) -> Result<(), DbError> {
        sqlx::query("DELETE FROM app_indexer_map WHERE app_id = ? AND indexer_id = ?")
            .bind(app.0)
            .bind(indexer.0)
            .execute(self.0.pool())
            .await?;

        Ok(())
    }
}

/// Narrows a `SQLite` rowid to the `i32` id newtypes' contract.
fn row_id_to_i32(id: i64, field: &'static str) -> Result<i32, DbError> {
    i32::try_from(id).map_err(|err| DbError::Corrupt {
        field,
        reason: err.to_string(),
    })
}

/// Maps a raw `app_indexer_map` row to a [`MappingRow`].
fn row_to_mapping(row: &SqliteRow) -> Result<MappingRow, DbError> {
    let app_id: i64 = row.try_get("app_id")?;
    let indexer_id: i64 = row.try_get("indexer_id")?;
    let remote_indexer_id: i64 = row.try_get("remote_indexer_id")?;

    Ok(MappingRow {
        app_id: AppId(row_id_to_i32(app_id, "app_indexer_map.app_id")?),
        indexer_id: IndexerId(row_id_to_i32(indexer_id, "app_indexer_map.indexer_id")?),
        remote_indexer_id: RemoteIndexerId(row_id_to_i32(
            remote_indexer_id,
            "app_indexer_map.remote_indexer_id",
        )?),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::Db;
    use crate::application::{AppKind, ApplicationRepo, NewApplication, SyncLevel};
    use crate::indexer::{IndexerKind, IndexerRepo, NewIndexer};

    fn sample_new_application(name: &str) -> NewApplication {
        NewApplication {
            name: name.to_string(),
            kind: AppKind::Sonarr,
            base_url: "http://localhost:8989".to_string(),
            api_key: "abc123".to_string(),
            sync_level: SyncLevel::FullSync,
        }
    }

    fn sample_new_indexer(name: &str) -> NewIndexer {
        NewIndexer {
            name: name.to_string(),
            definition_id: "def-1".to_string(),
            kind: IndexerKind::Cardigann,
            enabled: true,
            settings: serde_json::Map::new(),
            priority: 25,
        }
    }

    /// Inserts one application and one indexer to satisfy the mapping
    /// table's foreign keys, returning their assigned ids.
    async fn seed_parents(db: &Db, app_name: &str, indexer_name: &str) -> (AppId, IndexerId) {
        let app = ApplicationRepo::new(db)
            .insert(&sample_new_application(app_name))
            .await
            .unwrap();
        let indexer = IndexerRepo::new(db)
            .insert(&sample_new_indexer(indexer_name))
            .await
            .unwrap();
        (app.id, indexer.id)
    }

    #[tokio::test]
    async fn set_without_existing_parents_returns_err() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = MappingRepo::new(&db);

        let result = repo
            .set(MappingRow {
                app_id: AppId(1),
                indexer_id: IndexerId(1),
                remote_indexer_id: RemoteIndexerId(1),
            })
            .await;

        assert!(matches!(result, Err(DbError::Sqlx(_))));
    }

    #[tokio::test]
    async fn set_then_get_round_trips() {
        let db = Db::open_in_memory().await.unwrap();
        let (app_id, indexer_id) = seed_parents(&db, "app-one", "indexer-one").await;
        let repo = MappingRepo::new(&db);
        let row = MappingRow {
            app_id,
            indexer_id,
            remote_indexer_id: RemoteIndexerId(42),
        };

        repo.set(row).await.unwrap();
        let fetched = repo.get(app_id, indexer_id).await.unwrap();

        assert_eq!(fetched, Some(row));
    }

    #[tokio::test]
    async fn get_on_missing_pair_returns_none() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = MappingRepo::new(&db);

        let result = repo.get(AppId(1), IndexerId(1)).await.unwrap();

        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn set_twice_overwrites_the_remote_id() {
        let db = Db::open_in_memory().await.unwrap();
        let (app_id, indexer_id) = seed_parents(&db, "app-one", "indexer-one").await;
        let repo = MappingRepo::new(&db);
        repo.set(MappingRow {
            app_id,
            indexer_id,
            remote_indexer_id: RemoteIndexerId(1),
        })
        .await
        .unwrap();

        repo.set(MappingRow {
            app_id,
            indexer_id,
            remote_indexer_id: RemoteIndexerId(2),
        })
        .await
        .unwrap();

        let fetched = repo.get(app_id, indexer_id).await.unwrap().unwrap();
        assert_eq!(fetched.remote_indexer_id, RemoteIndexerId(2));
    }

    #[tokio::test]
    async fn for_app_lists_only_that_apps_rows() {
        let db = Db::open_in_memory().await.unwrap();
        let (app_a, indexer_one) = seed_parents(&db, "app-a", "indexer-one").await;
        let indexer_two = IndexerRepo::new(&db)
            .insert(&sample_new_indexer("indexer-two"))
            .await
            .unwrap()
            .id;
        let app_b = ApplicationRepo::new(&db)
            .insert(&sample_new_application("app-b"))
            .await
            .unwrap()
            .id;
        let repo = MappingRepo::new(&db);
        repo.set(MappingRow {
            app_id: app_a,
            indexer_id: indexer_one,
            remote_indexer_id: RemoteIndexerId(1),
        })
        .await
        .unwrap();
        repo.set(MappingRow {
            app_id: app_a,
            indexer_id: indexer_two,
            remote_indexer_id: RemoteIndexerId(2),
        })
        .await
        .unwrap();
        repo.set(MappingRow {
            app_id: app_b,
            indexer_id: indexer_one,
            remote_indexer_id: RemoteIndexerId(3),
        })
        .await
        .unwrap();

        let listed = repo.for_app(app_a).await.unwrap();

        assert_eq!(
            listed.iter().map(|row| row.indexer_id).collect::<Vec<_>>(),
            vec![indexer_one, indexer_two]
        );
        assert!(listed.iter().all(|row| row.app_id == app_a));
    }

    #[tokio::test]
    async fn for_indexer_lists_only_that_indexers_rows_across_every_app() {
        let db = Db::open_in_memory().await.unwrap();
        let (app_a, indexer_one) = seed_parents(&db, "app-a", "indexer-one").await;
        let indexer_two = IndexerRepo::new(&db)
            .insert(&sample_new_indexer("indexer-two"))
            .await
            .unwrap()
            .id;
        let app_b = ApplicationRepo::new(&db)
            .insert(&sample_new_application("app-b"))
            .await
            .unwrap()
            .id;
        let repo = MappingRepo::new(&db);
        repo.set(MappingRow {
            app_id: app_a,
            indexer_id: indexer_one,
            remote_indexer_id: RemoteIndexerId(1),
        })
        .await
        .unwrap();
        repo.set(MappingRow {
            app_id: app_b,
            indexer_id: indexer_one,
            remote_indexer_id: RemoteIndexerId(2),
        })
        .await
        .unwrap();
        repo.set(MappingRow {
            app_id: app_a,
            indexer_id: indexer_two,
            remote_indexer_id: RemoteIndexerId(3),
        })
        .await
        .unwrap();

        let listed = repo.for_indexer(indexer_one).await.unwrap();

        assert_eq!(
            listed.iter().map(|row| row.app_id).collect::<Vec<_>>(),
            vec![app_a, app_b]
        );
        assert!(listed.iter().all(|row| row.indexer_id == indexer_one));
    }

    #[tokio::test]
    async fn for_indexer_on_an_unmapped_indexer_returns_empty() {
        let db = Db::open_in_memory().await.unwrap();
        let (_, indexer_id) = seed_parents(&db, "app-one", "indexer-one").await;
        let repo = MappingRepo::new(&db);

        let listed = repo.for_indexer(indexer_id).await.unwrap();

        assert_eq!(listed, Vec::new());
    }

    #[tokio::test]
    async fn deleting_the_application_cascades_the_mapping() {
        let db = Db::open_in_memory().await.unwrap();
        let (app_id, indexer_id) = seed_parents(&db, "app-one", "indexer-one").await;
        let repo = MappingRepo::new(&db);
        repo.set(MappingRow {
            app_id,
            indexer_id,
            remote_indexer_id: RemoteIndexerId(1),
        })
        .await
        .unwrap();

        ApplicationRepo::new(&db).delete(app_id).await.unwrap();

        let result = repo.get(app_id, indexer_id).await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn deleting_the_indexer_cascades_the_mapping() {
        let db = Db::open_in_memory().await.unwrap();
        let (app_id, indexer_id) = seed_parents(&db, "app-one", "indexer-one").await;
        let repo = MappingRepo::new(&db);
        repo.set(MappingRow {
            app_id,
            indexer_id,
            remote_indexer_id: RemoteIndexerId(1),
        })
        .await
        .unwrap();

        IndexerRepo::new(&db).delete(indexer_id).await.unwrap();

        let result = repo.get(app_id, indexer_id).await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn delete_is_ok_even_when_the_mapping_is_absent() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = MappingRepo::new(&db);

        let result = repo.delete(AppId(1), IndexerId(1)).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn delete_removes_an_existing_mapping() {
        let db = Db::open_in_memory().await.unwrap();
        let (app_id, indexer_id) = seed_parents(&db, "app-one", "indexer-one").await;
        let repo = MappingRepo::new(&db);
        repo.set(MappingRow {
            app_id,
            indexer_id,
            remote_indexer_id: RemoteIndexerId(1),
        })
        .await
        .unwrap();

        repo.delete(app_id, indexer_id).await.unwrap();

        let result = repo.get(app_id, indexer_id).await.unwrap();
        assert_eq!(result, None);
    }
}
