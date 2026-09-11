//! [`DefinitionStore`]: a cached, read-through store of Cardigann
//! definitions over a directory of `<id>.yml` files.
//!
//! This replaces `server`'s former uncached `load_definition` (see git
//! history): every `t=caps` and every Cardigann-kind search used to read
//! and parse `<definition_id>.yml` from scratch, on every request. Fetching
//! the definition corpus itself from upstream is the binary's job (a
//! startup-time shell-out to `scripts/fetch-definitions.sh`, wired up
//! separately) — this store only ever reads a local directory that is
//! assumed to already be populated.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use oxidarr_cardigann::CardigannError;
use oxidarr_cardigann::model::{Definition, parse_definition};

/// Failure loading and parsing `<id>.yml`, distinguishing "the file could
/// not be read" from "the file was read but is not a valid Cardigann
/// definition" purely for a clearer [`Display`](std::fmt::Display). Both
/// variants name the definition's path directly (thiserror special-cases
/// `PathBuf` fields to interpolate via [`Path::display`], so `{path}`
/// below is not a stray unformatted-`Debug` bug).
#[derive(Debug, thiserror::Error)]
pub enum DefinitionError {
    /// The file (or, from [`DefinitionStore::list_ids`], the directory
    /// itself) could not be read at all.
    #[error("reading definition {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The file was read but does not parse as a Cardigann v11 definition.
    #[error("parsing definition {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: CardigannError,
    },
    /// `id` is not a single plain file stem: it contains a path separator
    /// or `..`, or is empty/`.`. Checked before `id` is ever joined onto
    /// the store's directory, so a value like `../../etc/passwd` never
    /// reaches disk. See [`DefinitionStore::get`]'s doc comment for why
    /// this check lives in the store rather than only at a caller.
    #[error("definition id {id:?} is not a plain file stem")]
    InvalidId { id: String },
}

/// A per-id read-through cache over a directory of `<id>.yml` Cardigann
/// definitions.
///
/// # Cache semantics
///
/// [`get`](Self::get) parses and caches an id's [`Definition`] the first
/// time it is requested; every later call for that same id returns the
/// cached [`Arc`] without touching disk again, until
/// [`refresh`](Self::refresh) clears the whole cache.
///
/// [`list_ids`](Self::list_ids), by contrast, deliberately reads the
/// directory fresh on *every* call rather than caching anything. It backs
/// the `t=caps` schema-discovery path — "which definitions exist" rather
/// than "parse this one" — where a directory listing is cheap next to
/// parsing YAML, and where caching it would mean a definition dropped into
/// (or removed from) the directory after startup stays invisible until an
/// explicit [`refresh`](Self::refresh). This is a deliberate asymmetry:
/// `refresh` only clears the `get` id cache, since `list_ids` never caches
/// in the first place.
///
/// # Locking
///
/// The id cache is guarded by a [`std::sync::RwLock`] rather than
/// [`tokio::sync::RwLock`]: every critical section below does plain
/// in-memory `HashMap` work with no `.await` inside it (each guard is
/// dropped before any `.await`), so a blocking std lock never straddles a
/// yield point — and it is lighter than an async lock for exactly that
/// reason.
#[derive(Clone, Debug)]
pub struct DefinitionStore(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    dir: PathBuf,
    cache: RwLock<HashMap<String, Arc<Definition>>>,
}

impl DefinitionStore {
    /// Builds a store reading `<id>.yml` files out of `dir`. Performs no
    /// I/O itself — the directory is only ever touched by
    /// [`get`](Self::get) and [`list_ids`](Self::list_ids).
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self(Arc::new(Inner {
            dir,
            cache: RwLock::new(HashMap::new()),
        }))
    }

    /// Returns `id`'s parsed definition, reading and parsing `<id>.yml`
    /// from disk only the first time `id` is requested (or the first time
    /// after [`refresh`](Self::refresh)); every other call returns the
    /// already-cached [`Arc`] without touching disk.
    ///
    /// # Errors
    ///
    /// Returns [`DefinitionError::InvalidId`] if `id` is not a single plain
    /// file stem (contains `/`, `\`, or `..`, is empty, or is `.`) —
    /// checked before `id` is ever joined onto this store's directory, so
    /// a value like `../../etc/passwd` never reaches disk. This is the
    /// store's own defense-in-depth gate: a caller that builds `id` from
    /// client input (e.g. a `POST /api/v1/indexer` request's
    /// `definitionName`) is expected to validate too, but this store must
    /// not be the only thing standing between a hostile `id` and a
    /// `dir.join(...)` file read.
    ///
    /// Returns [`DefinitionError::Io`] if `<id>.yml` cannot be read, or
    /// [`DefinitionError::Parse`] if it can be read but does not parse as a
    /// Cardigann v11 definition. No outcome is cached: a failed `get`
    /// leaves the cache untouched, so a later call (e.g. once the file
    /// shows up) tries again from disk.
    pub async fn get(&self, id: &str) -> Result<Arc<Definition>, DefinitionError> {
        validate_id(id)?;
        if let Some(def) = self.cached(id) {
            return Ok(def);
        }
        let def = Arc::new(load_definition(&self.0.dir, id).await?);
        Ok(self.cache_insert(id, def))
    }

    /// Returns this store's directory's `*.yml` file stems, sorted.
    ///
    /// Reads the directory fresh on every call — see the struct docs'
    /// "Cache semantics" section for why this, unlike
    /// [`get`](Self::get), never caches.
    ///
    /// # Errors
    ///
    /// Returns [`DefinitionError::Io`] if the directory cannot be listed.
    pub async fn list_ids(&self) -> Result<Vec<String>, DefinitionError> {
        let dir = &self.0.dir;
        let mut read_dir =
            tokio::fs::read_dir(dir)
                .await
                .map_err(|source| DefinitionError::Io {
                    path: dir.clone(),
                    source,
                })?;
        let mut ids = Vec::new();
        while let Some(entry) =
            read_dir
                .next_entry()
                .await
                .map_err(|source| DefinitionError::Io {
                    path: dir.clone(),
                    source,
                })?
        {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("yml") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                ids.push(stem.to_string());
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Clears the [`get`](Self::get) id cache, so the next `get` for any id
    /// re-reads and re-parses its file. Does not touch anything
    /// [`list_ids`](Self::list_ids) reads — `list_ids` never caches in the
    /// first place.
    ///
    /// # Errors
    ///
    /// Infallible in practice (clearing an in-memory map cannot fail); the
    /// `Result` return type mirrors [`get`](Self::get)/
    /// [`list_ids`](Self::list_ids) and leaves room for a future
    /// implementation that re-validates the directory on refresh.
    // `async` with no `.await` inside is deliberate: it keeps this method's
    // signature symmetric with `get`/`list_ids` (both genuinely async) and
    // free to grow one later (e.g. a refresh that re-stats the directory)
    // without becoming a breaking API change for callers.
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    pub async fn refresh(&self) -> Result<(), DefinitionError> {
        self.lock_write().clear();
        Ok(())
    }

    /// Returns `id`'s cached definition, if any, without touching disk.
    fn cached(&self, id: &str) -> Option<Arc<Definition>> {
        self.lock_read().get(id).cloned()
    }

    /// Inserts `def` under `id` unless another `get("id")` raced this one
    /// and already won; either way returns the `Arc` now in the cache.
    fn cache_insert(&self, id: &str, def: Arc<Definition>) -> Arc<Definition> {
        Arc::clone(self.lock_write().entry(id.to_string()).or_insert(def))
    }

    /// Read-locks the id cache, recovering from poisoning rather than
    /// panicking: a panic while holding the lock elsewhere would not leave
    /// this cache's `HashMap` in a logically inconsistent state (every
    /// mutation here is a single non-panicking `HashMap` operation), so
    /// there is nothing to protect against by propagating the poison.
    fn lock_read(&self) -> RwLockReadGuard<'_, HashMap<String, Arc<Definition>>> {
        self.0.cache.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Write-locks the id cache. See [`lock_read`](Self::lock_read) for why
    /// poisoning is recovered from rather than propagated.
    fn lock_write(&self) -> RwLockWriteGuard<'_, HashMap<String, Arc<Definition>>> {
        self.0.cache.write().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Rejects any `id` that is not safe to use as a single plain path
/// component: empty, exactly `.`, containing a path separator (`/` or
/// `\`), or containing `..` anywhere (blocking both a bare `..` component
/// and a traversal shaped like `../../etc/passwd`). This is the only gate
/// between an `id` and [`load_definition`]'s `dir.join(format!("{id}.yml"))`
/// — see [`DefinitionStore::get`]'s doc comment for why it must not be the
/// *only* gate overall, just this store's own.
fn validate_id(id: &str) -> Result<(), DefinitionError> {
    let is_plain_stem =
        !id.is_empty() && id != "." && !id.contains(['/', '\\']) && !id.contains("..");
    if is_plain_stem {
        Ok(())
    } else {
        Err(DefinitionError::InvalidId { id: id.to_string() })
    }
}

/// Reads and parses `<id>.yml` from `dir`.
///
/// # Errors
///
/// Returns [`DefinitionError::Io`] if the file cannot be read, or
/// [`DefinitionError::Parse`] if it can be read but does not parse as a
/// Cardigann v11 definition.
async fn load_definition(dir: &Path, id: &str) -> Result<Definition, DefinitionError> {
    let path = dir.join(format!("{id}.yml"));
    let text = tokio::fs::read_to_string(&path)
        .await
        .map_err(|source| DefinitionError::Io {
            path: path.clone(),
            source,
        })?;
    parse_definition(&text).map_err(|source| DefinitionError::Parse { path, source })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::fs;

    /// Mirrors `tests/fixtures/defs/example.yml`'s shape (see
    /// `oxidarr-prowl`'s router integration tests), trimmed to the fields
    /// `Definition`/`Search` actually require, so it is guaranteed to
    /// parse.
    const DEFINITION_V1: &str = r"
id: example
name: Example Tracker V1
links:
  - https://example.org/
search:
  paths:
    - path: browse
  rows:
    selector: tr.result
  fields:
    title:
      selector: td.name a
    details:
      selector: td.name a
      attribute: href
    download:
      selector: td.dl a
      attribute: href
";

    /// A second, equally valid definition, distinguished from
    /// [`DEFINITION_V1`] only by `name` — enough to prove which version a
    /// cached [`get`](DefinitionStore::get) actually returned.
    const DEFINITION_V2: &str = r"
id: example
name: Example Tracker V2
links:
  - https://example.org/
search:
  paths:
    - path: browse
  rows:
    selector: tr.result
  fields:
    title:
      selector: td.name a
    details:
      selector: td.name a
      attribute: href
    download:
      selector: td.dl a
      attribute: href
";

    const MALFORMED: &str = "id: [this is not\n  valid: yaml: at all";

    fn write_def(dir: &Path, id: &str, yaml: &str) {
        fs::write(dir.join(format!("{id}.yml")), yaml).unwrap();
    }

    #[tokio::test]
    async fn get_parses_a_fixture_definition() {
        let dir = tempfile::tempdir().unwrap();
        write_def(dir.path(), "example", DEFINITION_V1);
        let store = DefinitionStore::new(dir.path().to_path_buf());

        let def = store.get("example").await.unwrap();

        assert_eq!(def.name, "Example Tracker V1");
    }

    #[tokio::test]
    async fn get_caches_the_first_parse_until_refresh() {
        let dir = tempfile::tempdir().unwrap();
        write_def(dir.path(), "example", DEFINITION_V1);
        let store = DefinitionStore::new(dir.path().to_path_buf());

        let first = store.get("example").await.unwrap();
        assert_eq!(first.name, "Example Tracker V1");

        // Overwrite with a different, still-valid definition: `get` must
        // keep returning the originally cached value.
        write_def(dir.path(), "example", DEFINITION_V2);
        let still_cached = store.get("example").await.unwrap();
        assert_eq!(
            still_cached.name, "Example Tracker V1",
            "get() re-read the file instead of returning the cached value"
        );

        store.refresh().await.unwrap();
        let refreshed = store.get("example").await.unwrap();
        assert_eq!(
            refreshed.name, "Example Tracker V2",
            "get() after refresh() must re-read the (now different) file"
        );
    }

    #[tokio::test]
    async fn get_of_an_unknown_id_is_an_io_error_naming_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let store = DefinitionStore::new(dir.path().to_path_buf());

        let err = store.get("missing").await.unwrap_err();

        assert!(matches!(err, DefinitionError::Io { .. }), "err was: {err}");
        let expected_path = dir.path().join("missing.yml");
        assert!(
            err.to_string()
                .contains(&expected_path.display().to_string()),
            "error {err} did not name {}",
            expected_path.display()
        );
    }

    #[tokio::test]
    async fn get_of_malformed_yaml_is_a_parse_error_naming_the_file() {
        let dir = tempfile::tempdir().unwrap();
        write_def(dir.path(), "broken", MALFORMED);
        let store = DefinitionStore::new(dir.path().to_path_buf());

        let err = store.get("broken").await.unwrap_err();

        assert!(
            matches!(err, DefinitionError::Parse { .. }),
            "err was: {err}"
        );
        let expected_path = dir.path().join("broken.yml");
        assert!(
            err.to_string()
                .contains(&expected_path.display().to_string()),
            "error {err} did not name {}",
            expected_path.display()
        );
    }

    #[tokio::test]
    async fn list_ids_returns_sorted_yml_stems() {
        let dir = tempfile::tempdir().unwrap();
        write_def(dir.path(), "zebra", DEFINITION_V1);
        write_def(dir.path(), "alpha", DEFINITION_V1);
        fs::write(dir.path().join("notes.txt"), "not a definition").unwrap();
        let store = DefinitionStore::new(dir.path().to_path_buf());

        let ids = store.list_ids().await.unwrap();

        assert_eq!(ids, vec!["alpha".to_string(), "zebra".to_string()]);
    }

    /// The discriminating case: unlike `sub/dir`/`..`/empty (which are
    /// merely wrong, resolving to a nonexistent file), this id is one that
    /// unfixed code actually resolves *outside* the store directory and
    /// successfully reads — `escape.yml` lives next to `store_dir`, not in
    /// it. A validation-free `get` would happily return it; this proves the
    /// store refuses instead of reading it.
    #[tokio::test]
    async fn get_rejects_a_traversal_id_that_would_escape_the_store_directory() {
        let base = tempfile::tempdir().unwrap();
        let store_dir = base.path().join("defs");
        fs::create_dir(&store_dir).unwrap();
        fs::write(base.path().join("escape.yml"), DEFINITION_V1).unwrap();
        let store = DefinitionStore::new(store_dir);

        let err = store.get("../escape").await.unwrap_err();

        assert!(
            matches!(err, DefinitionError::InvalidId { .. }),
            "expected a path-traversal id to be rejected before ever touching disk, got: {err}"
        );
    }

    #[tokio::test]
    async fn get_rejects_ids_containing_a_path_separator() {
        let dir = tempfile::tempdir().unwrap();
        let store = DefinitionStore::new(dir.path().to_path_buf());

        let err = store.get("sub/dir").await.unwrap_err();

        assert!(
            matches!(err, DefinitionError::InvalidId { .. }),
            "err was: {err}"
        );
    }

    #[tokio::test]
    async fn get_rejects_the_literal_dotdot_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = DefinitionStore::new(dir.path().to_path_buf());

        let err = store.get("..").await.unwrap_err();

        assert!(
            matches!(err, DefinitionError::InvalidId { .. }),
            "err was: {err}"
        );
    }

    #[tokio::test]
    async fn get_rejects_the_empty_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = DefinitionStore::new(dir.path().to_path_buf());

        let err = store.get("").await.unwrap_err();

        assert!(
            matches!(err, DefinitionError::InvalidId { .. }),
            "err was: {err}"
        );
    }

    #[tokio::test]
    async fn list_ids_reads_the_directory_fresh_every_call() {
        let dir = tempfile::tempdir().unwrap();
        write_def(dir.path(), "alpha", DEFINITION_V1);
        let store = DefinitionStore::new(dir.path().to_path_buf());

        assert_eq!(store.list_ids().await.unwrap(), vec!["alpha".to_string()]);

        // No refresh() call in between: list_ids must still see `beta`
        // immediately, unlike get()'s cache.
        write_def(dir.path(), "beta", DEFINITION_V1);
        assert_eq!(
            store.list_ids().await.unwrap(),
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }
}
