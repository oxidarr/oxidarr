//! Key/value config repository, backed by the `config` table.
//!
//! Also owns generation of the instance API key: a random value persisted
//! under a well-known config key on first use, so it survives restarts.

use std::fmt::Write as _;

use crate::{Db, DbError};

/// The config key the instance API key is stored under.
const API_KEY_CONFIG_KEY: &str = "instance_api_key";

/// Read/write access to the `config` key/value table.
#[derive(Debug)]
pub struct ConfigRepo<'a>(&'a Db);

impl<'a> ConfigRepo<'a> {
    /// Builds a repo borrowing `db` for the lifetime of the calls made
    /// through it.
    #[must_use]
    pub fn new(db: &'a Db) -> Self {
        Self(db)
    }

    /// Looks up `key`, returning `None` if no row is stored under it.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlx`] if the query fails.
    pub async fn get(&self, key: &str) -> Result<Option<String>, DbError> {
        let value = sqlx::query_scalar::<_, String>("SELECT value FROM config WHERE key = ?")
            .bind(key)
            .fetch_optional(self.0.pool())
            .await?;
        Ok(value)
    }

    /// Stores `value` under `key`, inserting a new row or overwriting an
    /// existing one.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlx`] if the query fails.
    pub async fn set(&self, key: &str, value: &str) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO config (key, value) VALUES (?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(self.0.pool())
        .await?;
        Ok(())
    }

    /// Returns the stored instance API key, generating and persisting a
    /// 32-char lowercase hex key on first call. Subsequent calls return the
    /// same value.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlx`] if the underlying queries fail, or
    /// [`DbError::NotFound`] if the generate path's own insert-or-ignore
    /// somehow leaves no row behind to read back (should not happen).
    pub async fn api_key(&self) -> Result<String, DbError> {
        if let Some(key) = self.get(API_KEY_CONFIG_KEY).await? {
            return Ok(key);
        }
        self.generate_and_persist_api_key().await
    }

    /// Generates a candidate key and persists it with insert-or-ignore, then
    /// reads back whichever value actually landed. `set()`'s blind-overwrite
    /// semantics would let two concurrent first callers each observe a miss
    /// on `get()` and then stomp each other's write over the multi-connection
    /// pool; `DO NOTHING` on conflict plus a re-read makes both callers
    /// converge on whichever row won, instead of the caller holding a key
    /// that no longer matches what is persisted.
    async fn generate_and_persist_api_key(&self) -> Result<String, DbError> {
        let candidate = generate_api_key();
        sqlx::query(
            "INSERT INTO config (key, value) VALUES (?, ?)
             ON CONFLICT(key) DO NOTHING",
        )
        .bind(API_KEY_CONFIG_KEY)
        .bind(&candidate)
        .execute(self.0.pool())
        .await?;
        self.get(API_KEY_CONFIG_KEY)
            .await?
            .ok_or_else(|| DbError::NotFound {
                entity: "config",
                key: API_KEY_CONFIG_KEY.to_string(),
            })
    }
}

/// Generates a 32-char lowercase hex key from 16 random bytes.
fn generate_api_key() -> String {
    let mut bytes = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
    bytes.iter().fold(String::with_capacity(32), |mut hex, b| {
        // `fmt::Write` for `String` never fails: writing to an in-memory
        // buffer cannot hit an I/O error, so the result is discarded.
        let _ = write!(hex, "{b:02x}");
        hex
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test]
    async fn get_on_missing_key_returns_none() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = ConfigRepo::new(&db);

        assert_eq!(repo.get("missing").await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_then_get_round_trips() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = ConfigRepo::new(&db);

        repo.set("greeting", "hello").await.unwrap();

        assert_eq!(
            repo.get("greeting").await.unwrap(),
            Some("hello".to_string())
        );
    }

    #[tokio::test]
    async fn set_twice_overwrites() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = ConfigRepo::new(&db);

        repo.set("greeting", "hello").await.unwrap();
        repo.set("greeting", "goodbye").await.unwrap();

        assert_eq!(
            repo.get("greeting").await.unwrap(),
            Some("goodbye".to_string())
        );
    }

    #[tokio::test]
    async fn api_key_is_32_lowercase_hex_chars() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = ConfigRepo::new(&db);

        let key = repo.api_key().await.unwrap();

        assert_eq!(key.len(), 32);
        assert!(
            key.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[tokio::test]
    async fn api_key_is_persisted_not_regenerated() {
        let db = Db::open_in_memory().await.unwrap();
        let repo = ConfigRepo::new(&db);

        let first = repo.api_key().await.unwrap();
        let second = repo.api_key().await.unwrap();

        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn concurrent_first_callers_converge_on_whichever_key_landed_first() {
        // Simulates a caller reaching the generate path (get() already
        // returned None for it) at the same moment another caller has just
        // won the race and persisted its own key — by inserting that
        // winning key directly, bypassing api_key()'s own get/set, and then
        // calling the generate path itself, which must not blindly clobber
        // the row that is already there.
        let db = Db::open_in_memory().await.unwrap();
        let repo = ConfigRepo::new(&db);
        let already_persisted = "0".repeat(32);
        sqlx::query("INSERT INTO config (key, value) VALUES (?, ?)")
            .bind(API_KEY_CONFIG_KEY)
            .bind(&already_persisted)
            .execute(db.pool())
            .await
            .unwrap();

        let result = repo.generate_and_persist_api_key().await.unwrap();

        assert_eq!(result, already_persisted);
    }
}
