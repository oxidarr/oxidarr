//! Schema/repository agreement gate.
//!
//! For each of the crate's four tables, a row is inserted through its
//! repository and then read back with a raw SQL `SELECT` that names every
//! column of that table explicitly, decoding each value independently of
//! the repository's own mapping code (`kind`/`sync_level`/`settings`/`added`
//! are parsed right here, not by calling into `oxidarr_db`'s private
//! helpers). Field-for-field equality between that independently-decoded
//! row and what the repository itself returned proves the repository's
//! column list, its serialisation choices, and `migrations/0001_initial.sql`
//! all agree — none of them can drift out of sync with the others without
//! this failing.
//!
//! The negative-control tests at the bottom prove the gate is not vacuous
//! in the schema direction: a raw `INSERT` naming a `kind`/`sync_level`
//! value outside the column's `CHECK` constraint must be rejected by
//! `SQLite` itself, independent of any application-level validation.
#![allow(clippy::unwrap_used, clippy::panic)]

use chrono::{DateTime, Utc};
use oxidarr_core::ids::{AppId, IndexerId, RemoteIndexerId};
use oxidarr_db::{
    AppKind, ApplicationRepo, ApplicationRow, ConfigRepo, Db, IndexerKind, IndexerRepo, IndexerRow,
    MappingRepo, MappingRow, NewApplication, NewIndexer, SyncLevel,
};
use sqlx::Row;

/// Independently parses a stored `indexers.kind` value, mirroring the
/// repository's own decoding without calling into it.
fn parse_indexer_kind(value: &str) -> IndexerKind {
    match value {
        "cardigann" => IndexerKind::Cardigann,
        "newznab" => IndexerKind::Newznab,
        "torznab" => IndexerKind::Torznab,
        other => panic!("unexpected indexers.kind {other:?}"),
    }
}

/// Independently parses a stored `applications.kind` value.
fn parse_app_kind(value: &str) -> AppKind {
    match value {
        "sonarr" => AppKind::Sonarr,
        "radarr" => AppKind::Radarr,
        other => panic!("unexpected applications.kind {other:?}"),
    }
}

/// Independently parses a stored `applications.sync_level` value.
fn parse_sync_level(value: &str) -> SyncLevel {
    match value {
        "disabled" => SyncLevel::Disabled,
        "addOnly" => SyncLevel::AddOnly,
        "fullSync" => SyncLevel::FullSync,
        other => panic!("unexpected applications.sync_level {other:?}"),
    }
}

/// Independently parses a stored RFC 3339 `added` timestamp.
fn parse_added(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

fn sample_new_application(name: &str) -> NewApplication {
    NewApplication {
        name: name.to_string(),
        kind: AppKind::Radarr,
        base_url: "http://localhost:7878".to_string(),
        api_key: "app-key".to_string(),
        sync_level: SyncLevel::AddOnly,
    }
}

fn sample_new_indexer(name: &str) -> NewIndexer {
    let mut settings = serde_json::Map::new();
    settings.insert(
        "apiKey".to_string(),
        serde_json::Value::String("secret".to_string()),
    );
    settings.insert("verified".to_string(), serde_json::Value::Bool(true));
    NewIndexer {
        name: name.to_string(),
        definition_id: "def-1".to_string(),
        kind: IndexerKind::Torznab,
        enabled: true,
        settings,
        priority: 17,
    }
}

#[tokio::test]
async fn indexers_table_agrees_with_the_repo() {
    let db = Db::open_in_memory().await.unwrap();
    let repo = IndexerRepo::new(&db);
    let new = sample_new_indexer("gate-indexer");

    let inserted = repo.insert(&new).await.unwrap();

    let raw = sqlx::query(
        "SELECT id, name, definition_id, kind, enabled, settings, priority, added
         FROM indexers WHERE id = ?",
    )
    .bind(inserted.id.0)
    .fetch_one(db.pool())
    .await
    .unwrap();

    let id: i64 = raw.try_get("id").unwrap();
    let name: String = raw.try_get("name").unwrap();
    let definition_id: String = raw.try_get("definition_id").unwrap();
    let kind: String = raw.try_get("kind").unwrap();
    let enabled: bool = raw.try_get("enabled").unwrap();
    let settings: String = raw.try_get("settings").unwrap();
    let priority: i32 = raw.try_get("priority").unwrap();
    let added: String = raw.try_get("added").unwrap();

    let decoded = IndexerRow {
        id: IndexerId(i32::try_from(id).unwrap()),
        added: parse_added(&added),
        name,
        definition_id,
        kind: parse_indexer_kind(&kind),
        enabled,
        settings: serde_json::from_str(&settings).unwrap(),
        priority,
    };

    assert_eq!(decoded, inserted);
    assert_eq!(repo.get(inserted.id).await.unwrap(), inserted);
}

#[tokio::test]
async fn applications_table_agrees_with_the_repo() {
    let db = Db::open_in_memory().await.unwrap();
    let repo = ApplicationRepo::new(&db);
    let new = sample_new_application("gate-app");

    let inserted = repo.insert(&new).await.unwrap();

    let raw = sqlx::query(
        "SELECT id, name, kind, base_url, api_key, sync_level, added
         FROM applications WHERE id = ?",
    )
    .bind(inserted.id.0)
    .fetch_one(db.pool())
    .await
    .unwrap();

    let id: i64 = raw.try_get("id").unwrap();
    let name: String = raw.try_get("name").unwrap();
    let kind: String = raw.try_get("kind").unwrap();
    let base_url: String = raw.try_get("base_url").unwrap();
    let api_key: String = raw.try_get("api_key").unwrap();
    let sync_level: String = raw.try_get("sync_level").unwrap();
    let added: String = raw.try_get("added").unwrap();

    let decoded = ApplicationRow {
        id: AppId(i32::try_from(id).unwrap()),
        added: parse_added(&added),
        name,
        kind: parse_app_kind(&kind),
        base_url,
        api_key,
        sync_level: parse_sync_level(&sync_level),
    };

    assert_eq!(decoded, inserted);
    assert_eq!(repo.get(inserted.id).await.unwrap(), inserted);
}

#[tokio::test]
async fn app_indexer_map_table_agrees_with_the_repo() {
    let db = Db::open_in_memory().await.unwrap();
    let app = ApplicationRepo::new(&db)
        .insert(&sample_new_application("gate-app"))
        .await
        .unwrap();
    let indexer = IndexerRepo::new(&db)
        .insert(&sample_new_indexer("gate-indexer"))
        .await
        .unwrap();
    let mapping_repo = MappingRepo::new(&db);
    let row = MappingRow {
        app_id: app.id,
        indexer_id: indexer.id,
        remote_indexer_id: RemoteIndexerId(99),
    };

    mapping_repo.set(row).await.unwrap();

    let raw = sqlx::query(
        "SELECT app_id, indexer_id, remote_indexer_id FROM app_indexer_map
         WHERE app_id = ? AND indexer_id = ?",
    )
    .bind(app.id.0)
    .bind(indexer.id.0)
    .fetch_one(db.pool())
    .await
    .unwrap();

    let app_id: i64 = raw.try_get("app_id").unwrap();
    let indexer_id: i64 = raw.try_get("indexer_id").unwrap();
    let remote_indexer_id: i64 = raw.try_get("remote_indexer_id").unwrap();

    let decoded = MappingRow {
        app_id: AppId(i32::try_from(app_id).unwrap()),
        indexer_id: IndexerId(i32::try_from(indexer_id).unwrap()),
        remote_indexer_id: RemoteIndexerId(i32::try_from(remote_indexer_id).unwrap()),
    };

    assert_eq!(decoded, row);
    assert_eq!(
        mapping_repo.get(app.id, indexer.id).await.unwrap(),
        Some(row)
    );
}

#[tokio::test]
async fn config_table_agrees_with_the_repo() {
    let db = Db::open_in_memory().await.unwrap();
    let repo = ConfigRepo::new(&db);

    repo.set("gate-key", "gate-value").await.unwrap();

    let raw = sqlx::query("SELECT key, value FROM config WHERE key = ?")
        .bind("gate-key")
        .fetch_one(db.pool())
        .await
        .unwrap();

    let key: String = raw.try_get("key").unwrap();
    let value: String = raw.try_get("value").unwrap();

    assert_eq!(key, "gate-key");
    assert_eq!(repo.get("gate-key").await.unwrap(), Some(value));
}

// Negative controls: prove the gate above is not vacuous by showing SQLite
// itself, not just application code, rejects out-of-list `kind` and
// `sync_level` values. If these ever started passing, it would mean a
// migration had silently dropped or loosened the `CHECK` constraint the
// repositories rely on.

#[tokio::test]
async fn indexers_kind_check_rejects_values_outside_the_allowed_list() {
    let db = Db::open_in_memory().await.unwrap();

    let result =
        sqlx::query("INSERT INTO indexers (name, definition_id, kind, added) VALUES (?, ?, ?, ?)")
            .bind("bad-kind-indexer")
            .bind("def-1")
            .bind("bogus")
            .bind(Utc::now().to_rfc3339())
            .execute(db.pool())
            .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn applications_kind_check_rejects_values_outside_the_allowed_list() {
    let db = Db::open_in_memory().await.unwrap();

    let result = sqlx::query(
        "INSERT INTO applications (name, kind, base_url, api_key, added) VALUES (?, ?, ?, ?, ?)",
    )
    .bind("bad-kind-app")
    .bind("bogus")
    .bind("http://localhost:1")
    .bind("key")
    .bind(Utc::now().to_rfc3339())
    .execute(db.pool())
    .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn applications_sync_level_check_rejects_values_outside_the_allowed_list() {
    let db = Db::open_in_memory().await.unwrap();

    let result = sqlx::query(
        "INSERT INTO applications (name, kind, base_url, api_key, sync_level, added)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind("bad-sync-level-app")
    .bind("radarr")
    .bind("http://localhost:1")
    .bind("key")
    .bind("bogus")
    .bind(Utc::now().to_rfc3339())
    .execute(db.pool())
    .await;

    assert!(result.is_err());
}
