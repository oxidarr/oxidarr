//! Application repository, backed by the `applications` table.

use chrono::{DateTime, Utc};
use oxidarr_core::ids::AppId;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Db, DbError};

/// Which downstream application this row configures, matching the
/// `applications.kind` `CHECK` constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppKind {
    /// A Sonarr instance.
    Sonarr,
    /// A Radarr instance.
    Radarr,
}

impl AppKind {
    /// The lowercase string this kind is stored as, matching the values the
    /// `applications.kind` `CHECK` constraint allows.
    fn as_str(self) -> &'static str {
        match self {
            Self::Sonarr => "sonarr",
            Self::Radarr => "radarr",
        }
    }

    /// Parses a stored `applications.kind` value back into an `AppKind`.
    fn from_stored(value: &str) -> Result<Self, DbError> {
        match value {
            "sonarr" => Ok(Self::Sonarr),
            "radarr" => Ok(Self::Radarr),
            other => Err(DbError::Corrupt {
                field: "applications.kind",
                reason: format!("unknown application kind {other:?}"),
            }),
        }
    }
}

/// How aggressively indexers are synced to this application, matching the
/// `applications.sync_level` `CHECK` constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncLevel {
    /// No indexers are synced.
    Disabled,
    /// New indexers are added but existing ones are never removed.
    AddOnly,
    /// Indexers are fully kept in sync, including removals.
    FullSync,
}

impl SyncLevel {
    /// The camelCase string this level is stored as, matching the values the
    /// `applications.sync_level` `CHECK` constraint allows.
    fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::AddOnly => "addOnly",
            Self::FullSync => "fullSync",
        }
    }

    /// Parses a stored `applications.sync_level` value back into a
    /// `SyncLevel`.
    fn from_stored(value: &str) -> Result<Self, DbError> {
        match value {
            "disabled" => Ok(Self::Disabled),
            "addOnly" => Ok(Self::AddOnly),
            "fullSync" => Ok(Self::FullSync),
            other => Err(DbError::Corrupt {
                field: "applications.sync_level",
                reason: format!("unknown sync level {other:?}"),
            }),
        }
    }
}

/// Fields required to create a new application. `id` and `added` are
/// assigned by the repository on [`ApplicationRepo::insert`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewApplication {
    /// Display name; must be unique across all applications.
    pub name: String,
    /// Which downstream application this is.
    pub kind: AppKind,
    /// Base URL the application is reachable at.
    pub base_url: String,
    /// API key used to authenticate with the application.
    pub api_key: String,
    /// How aggressively indexers are synced to this application.
    pub sync_level: SyncLevel,
}

/// A persisted application row: every [`NewApplication`] field plus the
/// columns the database assigns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationRow {
    /// The row's primary key.
    pub id: AppId,
    /// When this application was created.
    pub added: DateTime<Utc>,
    /// Display name; unique across all applications.
    pub name: String,
    /// Which downstream application this is.
    pub kind: AppKind,
    /// Base URL the application is reachable at.
    pub base_url: String,
    /// API key used to authenticate with the application.
    pub api_key: String,
    /// How aggressively indexers are synced to this application.
    pub sync_level: SyncLevel,
}

/// Read/write access to the `applications` table.
#[derive(Debug)]
pub struct ApplicationRepo<'a>(&'a Db);

impl<'a> ApplicationRepo<'a> {
    /// Builds a repo borrowing `db` for the lifetime of the calls made
    /// through it.
    #[must_use]
    pub fn new(db: &'a Db) -> Self {
        Self(db)
    }

    /// Inserts a new application, assigning its id and `added` timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlx`] if the query fails, including a unique
    /// constraint violation on `name`. Returns [`DbError::Corrupt`] if the
    /// assigned row id overflows `i32`.
    pub async fn insert(&self, new: &NewApplication) -> Result<ApplicationRow, DbError> {
        let added = Utc::now().to_rfc3339();

        let id: i64 = sqlx::query_scalar(
            "INSERT INTO applications (name, kind, base_url, api_key, sync_level, added)
             VALUES (?, ?, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(&new.name)
        .bind(new.kind.as_str())
        .bind(&new.base_url)
        .bind(&new.api_key)
        .bind(new.sync_level.as_str())
        .bind(&added)
        .fetch_one(self.0.pool())
        .await?;

        Ok(ApplicationRow {
            id: AppId(row_id_to_i32(id)?),
            added: parse_added(&added)?,
            name: new.name.clone(),
            kind: new.kind,
            base_url: new.base_url.clone(),
            api_key: new.api_key.clone(),
            sync_level: new.sync_level,
        })
    }

    /// Looks up an application by id.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::NotFound`] if no application has this id, or
    /// [`DbError::Corrupt`] if the stored row cannot be decoded. Returns
    /// [`DbError::Sqlx`] if the query fails.
    pub async fn get(&self, id: AppId) -> Result<ApplicationRow, DbError> {
        let row = sqlx::query(
            "SELECT id, name, kind, base_url, api_key, sync_level, added
             FROM applications WHERE id = ?",
        )
        .bind(id.0)
        .fetch_optional(self.0.pool())
        .await?
        .ok_or_else(|| DbError::NotFound {
            entity: "application",
            key: id.to_string(),
        })?;

        row_to_application(&row)
    }

    /// Lists every application, ordered by id.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Corrupt`] if a stored row cannot be decoded, or
    /// [`DbError::Sqlx`] if the query fails.
    pub async fn list(&self) -> Result<Vec<ApplicationRow>, DbError> {
        let rows = sqlx::query(
            "SELECT id, name, kind, base_url, api_key, sync_level, added
             FROM applications ORDER BY id",
        )
        .fetch_all(self.0.pool())
        .await?;

        rows.iter().map(row_to_application).collect()
    }

    /// Overwrites every mutable column of the application identified by
    /// `row.id` with the values in `row`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::NotFound`] if no application has `row.id`. Returns
    /// [`DbError::Sqlx`] if the query fails, including a unique constraint
    /// violation on `name`.
    pub async fn update(&self, row: &ApplicationRow) -> Result<(), DbError> {
        let result = sqlx::query(
            "UPDATE applications
             SET name = ?, kind = ?, base_url = ?, api_key = ?, sync_level = ?
             WHERE id = ?",
        )
        .bind(&row.name)
        .bind(row.kind.as_str())
        .bind(&row.base_url)
        .bind(&row.api_key)
        .bind(row.sync_level.as_str())
        .bind(row.id.0)
        .execute(self.0.pool())
        .await?;

        if result.rows_affected() == 0 {
            return Err(DbError::NotFound {
                entity: "application",
                key: row.id.to_string(),
            });
        }
        Ok(())
    }

    /// Deletes the application identified by `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::NotFound`] if no application has this id. Returns
    /// [`DbError::Sqlx`] if the query fails.
    pub async fn delete(&self, id: AppId) -> Result<(), DbError> {
        let result = sqlx::query("DELETE FROM applications WHERE id = ?")
            .bind(id.0)
            .execute(self.0.pool())
            .await?;

        if result.rows_affected() == 0 {
            return Err(DbError::NotFound {
                entity: "application",
                key: id.to_string(),
            });
        }
        Ok(())
    }
}

/// Parses the RFC 3339 timestamp stored in `applications.added`.
fn parse_added(value: &str) -> Result<DateTime<Utc>, DbError> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|err| DbError::Corrupt {
            field: "applications.added",
            reason: err.to_string(),
        })
}

/// Narrows a `SQLite` rowid to the `i32` `AppId` newtype's contract.
fn row_id_to_i32(id: i64) -> Result<i32, DbError> {
    i32::try_from(id).map_err(|err| DbError::Corrupt {
        field: "applications.id",
        reason: err.to_string(),
    })
}

/// Maps a raw `applications` row to an [`ApplicationRow`].
fn row_to_application(row: &SqliteRow) -> Result<ApplicationRow, DbError> {
    let id: i64 = row.try_get("id")?;
    let kind: String = row.try_get("kind")?;
    let sync_level: String = row.try_get("sync_level")?;
    let added: String = row.try_get("added")?;

    Ok(ApplicationRow {
        id: AppId(row_id_to_i32(id)?),
        added: parse_added(&added)?,
        name: row.try_get("name")?,
        kind: AppKind::from_stored(&kind)?,
        base_url: row.try_get("base_url")?,
        api_key: row.try_get("api_key")?,
        sync_level: SyncLevel::from_stored(&sync_level)?,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::Db;
    use oxidarr_core::ids::AppId;

    fn sample_new_application(name: &str) -> NewApplication {
        NewApplication {
            name: name.to_string(),
            kind: AppKind::Sonarr,
            base_url: "http://localhost:8989".to_string(),
            api_key: "abc123".to_string(),
            sync_level: SyncLevel::FullSync,
        }
    }

    #[tokio::test]
    async fn insert_returns_populated_row() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = ApplicationRepo::new(&db);
        let new = sample_new_application("app-one");

        let row = repo.insert(&new).await.unwrap();

        assert!(row.id.0 > 0);
        assert_eq!(row.name, new.name);
        assert_eq!(row.kind, new.kind);
        assert_eq!(row.base_url, new.base_url);
        assert_eq!(row.api_key, new.api_key);
        assert_eq!(row.sync_level, new.sync_level);

        let age = chrono::Utc::now().signed_duration_since(row.added);
        assert!(age.num_seconds() >= 0);
        assert!(age.num_seconds() < 5);
    }

    #[tokio::test]
    async fn get_round_trips_every_app_kind() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = ApplicationRepo::new(&db);

        for (name, kind) in [
            ("sonarr-app", AppKind::Sonarr),
            ("radarr-app", AppKind::Radarr),
        ] {
            let mut new = sample_new_application(name);
            new.kind = kind;
            let inserted = repo.insert(&new).await.unwrap();

            let fetched = repo.get(inserted.id).await.unwrap();

            assert_eq!(fetched, inserted);
            assert_eq!(fetched.kind, kind);
        }
    }

    #[tokio::test]
    async fn get_round_trips_every_sync_level() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = ApplicationRepo::new(&db);

        for (name, level) in [
            ("disabled-app", SyncLevel::Disabled),
            ("add-only-app", SyncLevel::AddOnly),
            ("full-sync-app", SyncLevel::FullSync),
        ] {
            let mut new = sample_new_application(name);
            new.sync_level = level;
            let inserted = repo.insert(&new).await.unwrap();

            let fetched = repo.get(inserted.id).await.unwrap();

            assert_eq!(fetched, inserted);
            assert_eq!(fetched.sync_level, level);
        }
    }

    #[tokio::test]
    async fn duplicate_name_returns_err() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = ApplicationRepo::new(&db);
        let first = sample_new_application("dup-name");
        let second = sample_new_application("dup-name");
        repo.insert(&first).await.unwrap();

        let result = repo.insert(&second).await;

        assert!(matches!(result, Err(crate::DbError::Sqlx(_))));
    }

    #[tokio::test]
    async fn get_on_missing_id_returns_not_found() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = ApplicationRepo::new(&db);

        let result = repo.get(AppId(999)).await;

        match result {
            Err(crate::DbError::NotFound { entity, .. }) => assert_eq!(entity, "application"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_on_missing_id_returns_not_found() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = ApplicationRepo::new(&db);
        let new = sample_new_application("ghost");
        let mut row = repo.insert(&new).await.unwrap();
        repo.delete(row.id).await.unwrap();
        row.base_url = "http://changed:1".to_string();

        let result = repo.update(&row).await;

        match result {
            Err(crate::DbError::NotFound { entity, .. }) => assert_eq!(entity, "application"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_on_missing_id_returns_not_found() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = ApplicationRepo::new(&db);

        let result = repo.delete(AppId(999)).await;

        match result {
            Err(crate::DbError::NotFound { entity, .. }) => assert_eq!(entity, "application"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_is_ordered_by_id() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = ApplicationRepo::new(&db);
        let a = repo.insert(&sample_new_application("alpha")).await.unwrap();
        let b = repo.insert(&sample_new_application("beta")).await.unwrap();
        let c = repo.insert(&sample_new_application("gamma")).await.unwrap();

        let listed = repo.list().await.unwrap();

        assert_eq!(
            listed.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![a.id, b.id, c.id]
        );
    }

    #[tokio::test]
    async fn update_changes_persist() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = ApplicationRepo::new(&db);
        let mut row = repo
            .insert(&sample_new_application("mutable"))
            .await
            .unwrap();

        row.name = "renamed".to_string();
        row.kind = AppKind::Radarr;
        row.base_url = "http://changed:7878".to_string();
        row.api_key = "new-key".to_string();
        row.sync_level = SyncLevel::Disabled;
        repo.update(&row).await.unwrap();

        let fetched = repo.get(row.id).await.unwrap();

        assert_eq!(fetched.name, "renamed");
        assert_eq!(fetched.kind, AppKind::Radarr);
        assert_eq!(fetched.base_url, "http://changed:7878");
        assert_eq!(fetched.api_key, "new-key");
        assert_eq!(fetched.sync_level, SyncLevel::Disabled);
    }

    #[tokio::test]
    async fn delete_removes_the_row() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = ApplicationRepo::new(&db);
        let row = repo
            .insert(&sample_new_application("doomed"))
            .await
            .unwrap();

        repo.delete(row.id).await.unwrap();

        let result = repo.get(row.id).await;
        assert!(matches!(result, Err(crate::DbError::NotFound { .. })));
    }
}
