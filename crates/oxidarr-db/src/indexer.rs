//! Indexer repository, backed by the `indexers` table.

use chrono::{DateTime, Utc};
use oxidarr_core::ids::IndexerId;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Db, DbError};

/// Which protocol/definition family an indexer speaks, matching the
/// `indexers.kind` `CHECK` constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexerKind {
    /// A Cardigann YAML definition, scraped via the templating engine.
    Cardigann,
    /// A Newznab-compatible Usenet indexer.
    Newznab,
    /// A Torznab-compatible torrent indexer.
    Torznab,
}

impl IndexerKind {
    /// The lowercase string this kind is stored as, matching the values the
    /// `indexers.kind` `CHECK` constraint allows.
    fn as_str(self) -> &'static str {
        match self {
            Self::Cardigann => "cardigann",
            Self::Newznab => "newznab",
            Self::Torznab => "torznab",
        }
    }

    /// Parses a stored `indexers.kind` value back into an `IndexerKind`.
    fn from_stored(value: &str) -> Result<Self, DbError> {
        match value {
            "cardigann" => Ok(Self::Cardigann),
            "newznab" => Ok(Self::Newznab),
            "torznab" => Ok(Self::Torznab),
            other => Err(DbError::Corrupt {
                field: "indexers.kind",
                reason: format!("unknown indexer kind {other:?}"),
            }),
        }
    }
}

/// Fields required to create a new indexer. `id` and `added` are assigned by
/// the repository on [`IndexerRepo::insert`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewIndexer {
    /// Display name; must be unique across all indexers.
    pub name: String,
    /// Which definition (Cardigann YAML id, or built-in Newznab/Torznab
    /// profile) this indexer is configured from.
    pub definition_id: String,
    /// Which protocol family this indexer speaks.
    pub kind: IndexerKind,
    /// Whether this indexer is queried during searches.
    pub enabled: bool,
    /// Definition-specific configuration (credentials, base URL, etc.).
    pub settings: serde_json::Map<String, serde_json::Value>,
    /// Search priority; lower values are preferred.
    pub priority: i32,
}

/// A persisted indexer row: every [`NewIndexer`] field plus the columns the
/// database assigns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexerRow {
    /// The row's primary key.
    pub id: IndexerId,
    /// When this indexer was created.
    pub added: DateTime<Utc>,
    /// Display name; unique across all indexers.
    pub name: String,
    /// Which definition this indexer is configured from.
    pub definition_id: String,
    /// Which protocol family this indexer speaks.
    pub kind: IndexerKind,
    /// Whether this indexer is queried during searches.
    pub enabled: bool,
    /// Definition-specific configuration.
    pub settings: serde_json::Map<String, serde_json::Value>,
    /// Search priority; lower values are preferred.
    pub priority: i32,
}

/// Read/write access to the `indexers` table.
#[derive(Debug)]
pub struct IndexerRepo<'a>(&'a Db);

impl<'a> IndexerRepo<'a> {
    /// Builds a repo borrowing `db` for the lifetime of the calls made
    /// through it.
    #[must_use]
    pub fn new(db: &'a Db) -> Self {
        Self(db)
    }

    /// Inserts a new indexer, assigning its id and `added` timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlx`] if the query fails, including a unique
    /// constraint violation on `name`. Returns [`DbError::Corrupt`] if the
    /// settings map cannot be encoded as JSON, or if the assigned row id
    /// overflows `i32`.
    pub async fn insert(&self, new: &NewIndexer) -> Result<IndexerRow, DbError> {
        let settings = encode_settings(&new.settings)?;
        let added = Utc::now().to_rfc3339();

        let id: i64 = sqlx::query_scalar(
            "INSERT INTO indexers (name, definition_id, kind, enabled, settings, priority, added)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(&new.name)
        .bind(&new.definition_id)
        .bind(new.kind.as_str())
        .bind(new.enabled)
        .bind(&settings)
        .bind(new.priority)
        .bind(&added)
        .fetch_one(self.0.pool())
        .await?;

        Ok(IndexerRow {
            id: IndexerId(row_id_to_i32(id)?),
            added: parse_added(&added)?,
            name: new.name.clone(),
            definition_id: new.definition_id.clone(),
            kind: new.kind,
            enabled: new.enabled,
            settings: new.settings.clone(),
            priority: new.priority,
        })
    }

    /// Looks up an indexer by id.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::NotFound`] if no indexer has this id, or
    /// [`DbError::Corrupt`] if the stored row cannot be decoded. Returns
    /// [`DbError::Sqlx`] if the query fails.
    pub async fn get(&self, id: IndexerId) -> Result<IndexerRow, DbError> {
        let row = sqlx::query(
            "SELECT id, name, definition_id, kind, enabled, settings, priority, added
             FROM indexers WHERE id = ?",
        )
        .bind(id.0)
        .fetch_optional(self.0.pool())
        .await?
        .ok_or_else(|| DbError::NotFound {
            entity: "indexer",
            key: id.to_string(),
        })?;

        row_to_indexer(&row)
    }

    /// Lists every indexer, ordered by id.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Corrupt`] if a stored row cannot be decoded, or
    /// [`DbError::Sqlx`] if the query fails.
    pub async fn list(&self) -> Result<Vec<IndexerRow>, DbError> {
        let rows = sqlx::query(
            "SELECT id, name, definition_id, kind, enabled, settings, priority, added
             FROM indexers ORDER BY id",
        )
        .fetch_all(self.0.pool())
        .await?;

        rows.iter().map(row_to_indexer).collect()
    }

    /// Overwrites every mutable column of the indexer identified by
    /// `row.id` with the values in `row`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::NotFound`] if no indexer has `row.id`. Returns
    /// [`DbError::Corrupt`] if the settings map cannot be encoded as JSON.
    /// Returns [`DbError::Sqlx`] if the query fails, including a unique
    /// constraint violation on `name`.
    pub async fn update(&self, row: &IndexerRow) -> Result<(), DbError> {
        let settings = encode_settings(&row.settings)?;

        let result = sqlx::query(
            "UPDATE indexers
             SET name = ?, definition_id = ?, kind = ?, enabled = ?, settings = ?, priority = ?
             WHERE id = ?",
        )
        .bind(&row.name)
        .bind(&row.definition_id)
        .bind(row.kind.as_str())
        .bind(row.enabled)
        .bind(&settings)
        .bind(row.priority)
        .bind(row.id.0)
        .execute(self.0.pool())
        .await?;

        if result.rows_affected() == 0 {
            return Err(DbError::NotFound {
                entity: "indexer",
                key: row.id.to_string(),
            });
        }
        Ok(())
    }

    /// Deletes the indexer identified by `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::NotFound`] if no indexer has this id. Returns
    /// [`DbError::Sqlx`] if the query fails.
    pub async fn delete(&self, id: IndexerId) -> Result<(), DbError> {
        let result = sqlx::query("DELETE FROM indexers WHERE id = ?")
            .bind(id.0)
            .execute(self.0.pool())
            .await?;

        if result.rows_affected() == 0 {
            return Err(DbError::NotFound {
                entity: "indexer",
                key: id.to_string(),
            });
        }
        Ok(())
    }
}

/// Encodes a settings map as the JSON `TEXT` the `indexers.settings` column
/// stores.
fn encode_settings(
    settings: &serde_json::Map<String, serde_json::Value>,
) -> Result<String, DbError> {
    serde_json::to_string(settings).map_err(|err| DbError::Corrupt {
        field: "indexers.settings",
        reason: err.to_string(),
    })
}

/// Decodes the JSON `TEXT` stored in `indexers.settings` back into a map.
fn decode_settings(value: &str) -> Result<serde_json::Map<String, serde_json::Value>, DbError> {
    serde_json::from_str(value).map_err(|err| DbError::Corrupt {
        field: "indexers.settings",
        reason: err.to_string(),
    })
}

/// Parses the RFC 3339 timestamp stored in `indexers.added`.
fn parse_added(value: &str) -> Result<DateTime<Utc>, DbError> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|err| DbError::Corrupt {
            field: "indexers.added",
            reason: err.to_string(),
        })
}

/// Narrows a `SQLite` rowid to the `i32` `IndexerId` newtype's contract.
fn row_id_to_i32(id: i64) -> Result<i32, DbError> {
    i32::try_from(id).map_err(|err| DbError::Corrupt {
        field: "indexers.id",
        reason: err.to_string(),
    })
}

/// Maps a raw `indexers` row to an [`IndexerRow`].
fn row_to_indexer(row: &SqliteRow) -> Result<IndexerRow, DbError> {
    let id: i64 = row.try_get("id")?;
    let kind: String = row.try_get("kind")?;
    let settings: String = row.try_get("settings")?;
    let added: String = row.try_get("added")?;

    Ok(IndexerRow {
        id: IndexerId(row_id_to_i32(id)?),
        added: parse_added(&added)?,
        name: row.try_get("name")?,
        definition_id: row.try_get("definition_id")?,
        kind: IndexerKind::from_stored(&kind)?,
        enabled: row.try_get("enabled")?,
        settings: decode_settings(&settings)?,
        priority: row.try_get("priority")?,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::Db;
    use oxidarr_core::ids::IndexerId;

    fn sample_settings() -> serde_json::Map<String, serde_json::Value> {
        let mut settings = serde_json::Map::new();
        settings.insert(
            "apiKey".to_string(),
            serde_json::Value::String("abc123".to_string()),
        );
        settings.insert("verified".to_string(), serde_json::Value::Bool(true));
        settings.insert(
            "timeoutSeconds".to_string(),
            serde_json::Value::Number(30.into()),
        );
        let mut nested = serde_json::Map::new();
        nested.insert("retries".to_string(), serde_json::Value::Number(3.into()));
        nested.insert("strict".to_string(), serde_json::Value::Bool(false));
        settings.insert("advanced".to_string(), serde_json::Value::Object(nested));
        settings
    }

    fn sample_new_indexer(name: &str) -> NewIndexer {
        NewIndexer {
            name: name.to_string(),
            definition_id: "def-1".to_string(),
            kind: IndexerKind::Cardigann,
            enabled: true,
            settings: sample_settings(),
            priority: 30,
        }
    }

    #[tokio::test]
    async fn insert_returns_populated_row() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = IndexerRepo::new(&db);
        let new = sample_new_indexer("indexer-one");

        let row = repo.insert(&new).await.unwrap();

        assert!(row.id.0 > 0);
        assert_eq!(row.name, new.name);
        assert_eq!(row.definition_id, new.definition_id);
        assert_eq!(row.kind, new.kind);
        assert_eq!(row.enabled, new.enabled);
        assert_eq!(row.settings, new.settings);
        assert_eq!(row.priority, new.priority);

        let age = chrono::Utc::now().signed_duration_since(row.added);
        assert!(age.num_seconds() >= 0);
        assert!(age.num_seconds() < 5);
    }

    #[tokio::test]
    async fn get_round_trips_settings_map_exactly() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = IndexerRepo::new(&db);
        let new = sample_new_indexer("indexer-two");
        let inserted = repo.insert(&new).await.unwrap();

        let fetched = repo.get(inserted.id).await.unwrap();

        assert_eq!(fetched, inserted);
        assert_eq!(fetched.settings, new.settings);
    }

    #[tokio::test]
    async fn duplicate_name_returns_err() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = IndexerRepo::new(&db);
        let first = sample_new_indexer("dup-name");
        let second = sample_new_indexer("dup-name");
        repo.insert(&first).await.unwrap();

        let result = repo.insert(&second).await;

        assert!(matches!(result, Err(crate::DbError::Sqlx(_))));
    }

    #[tokio::test]
    async fn get_on_missing_id_returns_not_found() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = IndexerRepo::new(&db);

        let result = repo.get(IndexerId(999)).await;

        match result {
            Err(crate::DbError::NotFound { entity, .. }) => assert_eq!(entity, "indexer"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_on_missing_id_returns_not_found() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = IndexerRepo::new(&db);
        let new = sample_new_indexer("ghost");
        let mut row = repo.insert(&new).await.unwrap();
        repo.delete(row.id).await.unwrap();
        row.priority = 1;

        let result = repo.update(&row).await;

        match result {
            Err(crate::DbError::NotFound { entity, .. }) => assert_eq!(entity, "indexer"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_on_missing_id_returns_not_found() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = IndexerRepo::new(&db);

        let result = repo.delete(IndexerId(999)).await;

        match result {
            Err(crate::DbError::NotFound { entity, .. }) => assert_eq!(entity, "indexer"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_is_ordered_by_id() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = IndexerRepo::new(&db);
        let a = repo.insert(&sample_new_indexer("alpha")).await.unwrap();
        let b = repo.insert(&sample_new_indexer("beta")).await.unwrap();
        let c = repo.insert(&sample_new_indexer("gamma")).await.unwrap();

        let listed = repo.list().await.unwrap();

        assert_eq!(
            listed.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![a.id, b.id, c.id]
        );
    }

    #[tokio::test]
    async fn update_changes_persist() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = IndexerRepo::new(&db);
        let mut row = repo.insert(&sample_new_indexer("mutable")).await.unwrap();

        row.name = "renamed".to_string();
        row.priority = 5;
        row.enabled = false;
        row.kind = IndexerKind::Torznab;
        let mut new_settings = serde_json::Map::new();
        new_settings.insert("changed".to_string(), serde_json::Value::Bool(true));
        row.settings = new_settings.clone();
        repo.update(&row).await.unwrap();

        let fetched = repo.get(row.id).await.unwrap();

        assert_eq!(fetched.name, "renamed");
        assert_eq!(fetched.priority, 5);
        assert!(!fetched.enabled);
        assert_eq!(fetched.kind, IndexerKind::Torznab);
        assert_eq!(fetched.settings, new_settings);
    }

    #[tokio::test]
    async fn delete_removes_the_row() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = IndexerRepo::new(&db);
        let row = repo.insert(&sample_new_indexer("doomed")).await.unwrap();

        repo.delete(row.id).await.unwrap();

        let result = repo.get(row.id).await;
        assert!(matches!(result, Err(crate::DbError::NotFound { .. })));
    }
}
