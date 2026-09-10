//! Error types for the `SQLite` persistence layer.

/// Failures arising while opening a database, running migrations, or
/// mapping stored rows to and from `oxidarr-core` types.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// A query, connection, or pool operation failed.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// Applying embedded migrations failed.
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// A lookup by key found no matching row.
    #[error("{entity} not found: {key}")]
    NotFound {
        /// The kind of entity that was looked up (e.g. `"indexer"`).
        entity: &'static str,
        /// The key the lookup was performed with.
        key: String,
    },
    /// A stored value could not be decoded into its Rust representation.
    #[error("invalid stored value for {field}: {reason}")]
    Corrupt {
        /// The column or field whose stored value is invalid.
        field: &'static str,
        /// Why the value could not be decoded.
        reason: String,
    },
}
