//! Runtime acquisition of the Cardigann definitions.
//!
//! The definitions are third-party and unlicensed, which is why this
//! repository never commits them and why no published artefact — container
//! layer, `.crate`, release archive — may contain them. They are fetched on
//! the operator's own machine at runtime, exactly as the operator would have
//! fetched them by hand with `scripts/fetch-definitions.sh`.
//!
//! Every function here takes its inputs explicitly (bytes, paths, a URL) so
//! the module is testable without a network. [`sync_once`] (by way of the
//! private `download`) is the only part that actually talks to the outside
//! world; `spawn_updater` merely schedules calls to it on an interval.

use std::io::Read;
use std::path::{Path, PathBuf};

/// The archive path prefix definitions live under, regardless of the
/// top-level directory name the archive happens to carry.
const V11_MARKER: &str = "definitions/v11/";

/// Everything the updater can fail with.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// The archive could not be downloaded.
    #[error("downloading definitions from {url}: {source}")]
    Download {
        /// The URL that failed.
        url: String,
        #[source]
        source: reqwest::Error,
    },
    /// The archive could not be read as gzipped tar.
    #[error("reading the definitions archive: {0}")]
    Archive(#[source] std::io::Error),
    /// The archive parsed but carried no `definitions/v11` entries. Treated
    /// as a failure so a mirror serving the wrong repository cannot install
    /// an empty directory over a working one.
    #[error("the archive contained no definitions/v11 entries")]
    NoDefinitions,
    /// A filesystem operation failed.
    #[error("{action} {path}: {source}")]
    Io {
        /// What was being attempted.
        action: &'static str,
        /// The path involved.
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Extracts every `definitions/v11/*.yml` entry in `archive` into `dest`,
/// flattened — `Indexers-master/definitions/v11/alpha.yml` becomes
/// `dest/alpha.yml`. Returns how many were written.
///
/// Flattening is required, not cosmetic: `DefinitionStore` reads
/// `<id>.yml` directly out of its own directory, so a nested result would
/// make every lookup miss.
///
/// # Errors
///
/// [`SyncError::Archive`] if `archive` is not a readable gzipped tar,
/// [`SyncError::NoDefinitions`] if it carries no matching entries, and
/// [`SyncError::Io`] if `dest` cannot be written.
pub fn extract_definitions(archive: &[u8], dest: &Path) -> Result<usize, SyncError> {
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    let mut written = 0usize;

    let entries = tar.entries().map_err(SyncError::Archive)?;
    for entry in entries {
        let mut entry = entry.map_err(SyncError::Archive)?;
        let path = entry.path().map_err(SyncError::Archive)?.into_owned();
        let Some(name) = definition_file_name(&path) else {
            continue;
        };

        let mut body = Vec::new();
        entry.read_to_end(&mut body).map_err(SyncError::Archive)?;

        let target = dest.join(name);
        std::fs::write(&target, &body).map_err(|source| SyncError::Io {
            action: "writing definition",
            path: target,
            source,
        })?;
        written += 1;
    }

    if written == 0 {
        return Err(SyncError::NoDefinitions);
    }
    Ok(written)
}

/// The flat `<id>.yml` name for an archive entry under `definitions/v11`,
/// or `None` for anything else — another schema version, a README, a
/// directory entry, or a hostile entry trying to escape `dest` once this
/// name is joined onto it in [`extract_definitions`]. Matches on the marker
/// rather than a fixed top-level directory because the archive's root is
/// named after the branch.
///
/// `rest` (everything after the marker) must be *exactly one* normal path
/// component: not empty, not `.` or `..`, and not `/`-nested. `\` is
/// rejected outright rather than left to [`Component`](std::path::Component)
/// parsing, because on a Unix build — including the one running this
/// module's own tests — `Path` never treats `\` as a separator, so
/// `definitions/v11/..\..\evil.yml` would otherwise sail through this check
/// as one harmless-looking component while still escaping `dest` on a
/// Windows deployment, where `\` *is* a separator and `dest.join` would
/// honour it. Tar entries always use `/`, so this remainder came from
/// `definitions_url` pointing at a hostile mirror, never from the pinned
/// default.
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "case sensitivity is intended: DefinitionStore resolves each definition as \
              <id>.yml exactly, so accepting a .YML entry here would install a file it \
              could never load on a case-sensitive filesystem"
)]
fn definition_file_name(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let rest = text.split_once(V11_MARKER)?.1;
    if rest.is_empty() || !rest.ends_with(".yml") || rest.contains('\\') {
        return None;
    }

    let mut components = Path::new(rest).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(component)), None)
            if component.to_str() == Some(rest) =>
        {
            Some(rest.to_string())
        }
        _ => None,
    }
}

/// Moves `staging` into place as `live`, replacing whatever was there.
///
/// `staging` must be a sibling of `live` — the same filesystem — because
/// the swap relies on `rename`, which is atomic only within a filesystem.
/// A crash at any point never leaves a *partially extracted* directory at
/// `live`: every file `stage` wrote is either fully installed or `live`
/// still holds exactly the set that predated this call. There is one
/// window this does not cover — if the process dies after the first
/// rename (moving `live` aside to `previous`) but before the second (moving
/// `staging` into `live`), `live` is briefly absent altogether, neither the
/// old set nor the new one. That window self-heals on the next successful
/// run: `install_definitions` only moves `live` aside `if live.exists()`,
/// so a missing `live` simply skips straight to installing `staging`.
///
/// The old directory is moved aside first and deleted after the swap,
/// rather than deleted before it, so the window in which `live` does not
/// exist is a single `rename` rather than the length of a recursive delete.
///
/// # Errors
///
/// [`SyncError::Io`] if any of the renames or the cleanup delete fails.
pub fn install_definitions(staging: &Path, live: &Path) -> Result<(), SyncError> {
    let previous = with_suffix(live, ".previous");

    // A previous run may have died after the swap but before its cleanup,
    // stranding this directory. `rename` fails with ENOTEMPTY against a
    // non-empty destination, so without this every later run would fail at
    // the move-aside below and definitions could never update again.
    if previous.exists() {
        std::fs::remove_dir_all(&previous).map_err(|source| SyncError::Io {
            action: "clearing a stranded previous definitions directory",
            path: previous.clone(),
            source,
        })?;
    }

    if live.exists() {
        std::fs::rename(live, &previous).map_err(|source| SyncError::Io {
            action: "moving the previous definitions aside",
            path: previous.clone(),
            source,
        })?;
    }

    std::fs::rename(staging, live).map_err(|source| SyncError::Io {
        action: "installing the new definitions",
        path: live.to_path_buf(),
        source,
    })?;

    if previous.exists() {
        std::fs::remove_dir_all(&previous).map_err(|source| SyncError::Io {
            action: "removing the previous definitions",
            path: previous,
            source,
        })?;
    }
    Ok(())
}

/// `path` with `suffix` appended to its final component — used to name the
/// staging and previous directories as siblings of the live one, keeping
/// every rename within a single filesystem.
///
/// `path` must not have a trailing separator, or the suffix would be appended
/// to the path string as a child rather than a sibling, silently breaking the
/// same-filesystem atomicity that `install_definitions` depends on.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// The `{data_dir}/definitions` path — the one directory both `main`
/// (counting and loading already-installed definitions) and [`sync_once`]
/// (the swap target it installs into) must agree on. Extracted into one
/// function so there is exactly one place that spells this join, rather
/// than two independently-computed paths that happen to match today but
/// could silently drift apart tomorrow.
#[must_use]
pub fn definitions_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("definitions")
}

/// Downloads `url`, extracts it into a staging directory beside
/// `{data_dir}/definitions`, and installs it atomically. Returns how many
/// definitions were installed.
///
/// Staging is cleared before use and on every failure path, so a previous
/// interrupted run can never contaminate this one. `{data_dir}/definitions`
/// is only ever touched by the final [`install_definitions`] call — every
/// error before that point leaves the existing set exactly as it was.
///
/// # Errors
///
/// [`SyncError::Download`] for a transport failure or a non-success status,
/// and whatever [`extract_definitions`] or [`install_definitions`] return.
pub async fn sync_once(
    client: &reqwest::Client,
    url: &str,
    data_dir: &Path,
) -> Result<usize, SyncError> {
    let body = download(client, url).await?;

    let live = definitions_dir(data_dir);
    let staging = with_suffix(&live, ".staging");

    let result = stage(&body, &staging).and_then(|count| {
        install_definitions(&staging, &live)?;
        Ok(count)
    });

    if result.is_err() && staging.exists() {
        // Best effort: the next run clears staging before using it anyway,
        // so a failure to clean up here must not mask the real error.
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

/// Fetches `url`'s body, turning a non-success status into an error rather
/// than extracting an error page as though it were an archive.
async fn download(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, SyncError> {
    let response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|source| SyncError::Download {
            url: url.to_string(),
            source,
        })?;
    let bytes = response
        .bytes()
        .await
        .map_err(|source| SyncError::Download {
            url: url.to_string(),
            source,
        })?;
    Ok(bytes.to_vec())
}

/// Creates a clean `staging` directory and extracts `body` into it.
fn stage(body: &[u8], staging: &Path) -> Result<usize, SyncError> {
    if staging.exists() {
        std::fs::remove_dir_all(staging).map_err(|source| SyncError::Io {
            action: "clearing the staging directory",
            path: staging.to_path_buf(),
            source,
        })?;
    }
    std::fs::create_dir_all(staging).map_err(|source| SyncError::Io {
        action: "creating the staging directory",
        path: staging.to_path_buf(),
        source,
    })?;
    extract_definitions(body, staging)
}

/// Whether the updater should fetch immediately on startup rather than
/// waiting out its first interval. A fresh install has nothing to serve, so
/// it fetches at once; an install that already has definitions serves them
/// straight away and refreshes on schedule.
#[must_use]
pub const fn should_sync_now(definitions_count: usize) -> bool {
    definitions_count == 0
}

/// Spawns the background updater. Returns immediately — the caller goes on
/// to serve requests while this runs.
///
/// Callers that need to honour `definitions_auto_update` — i.e. every real
/// caller — should use [`spawn_updater_if_enabled`] instead; it wraps this
/// function with exactly that gate.
///
/// The task never fails the process: every error is logged and the previous
/// definitions stay in place. An instance whose first fetch has not finished
/// (or has failed) serves normally, with search and `t=caps` degraded for
/// Cardigann indexers only — the behaviour `main` already documents for a
/// missing definitions directory.
///
/// # Concurrency
///
/// This must remain the *only* caller of [`sync_once`] for a given
/// `data_dir`. Nothing in `sync_once` — nor [`install_definitions`] or
/// [`stage`] beneath it — serialises concurrent callers against the same
/// staging directory; two overlapping runs would race on
/// `{data_dir}/definitions.staging`, and one run's cleanup or swap could
/// land in the middle of the other's. Exactly one updater task exists
/// today, so no such race exists yet. But a future manual "refresh now"
/// trigger (deliberately deferred out of this milestone) would introduce a
/// second caller, and whoever adds it must serialise the two against each
/// other first — a mutex, a channel into this task, or similar — rather
/// than call `sync_once` directly from a second place.
pub fn spawn_updater(
    client: reqwest::Client,
    url: String,
    data_dir: PathBuf,
    interval: std::time::Duration,
    store: crate::definitions::DefinitionStore,
    sync_immediately: bool,
) {
    tokio::spawn(async move {
        if !sync_immediately {
            tokio::time::sleep(interval).await;
        }
        loop {
            match sync_once(&client, &url, &data_dir).await {
                Ok(count) => {
                    eprintln!("definitions updated: {count} loaded");
                    if let Err(err) = store.refresh().await {
                        eprintln!("warning: refreshing the definition cache failed: {err}");
                    }
                }
                Err(err) => {
                    eprintln!("warning: updating definitions failed: {err}");
                    eprintln!("         the previously installed definitions are still in use");
                }
            }
            tokio::time::sleep(interval).await;
        }
    });
}

/// Calls [`spawn_updater`] with the same arguments only when `auto_update`
/// is `true`; otherwise spawns nothing at all. Returns whether it spawned.
///
/// This is the one function that decides whether the process ever makes a
/// network request for definitions, so the gate lives here rather than as
/// an `if` at the call site in `main` — an accidentally-deleted `if` in
/// `main` would otherwise leave the whole test suite green while the binary
/// silently always fetched, regardless of `definitions_auto_update`.
#[must_use]
pub fn spawn_updater_if_enabled(
    client: reqwest::Client,
    url: String,
    data_dir: PathBuf,
    interval: std::time::Duration,
    store: crate::definitions::DefinitionStore,
    sync_immediately: bool,
    auto_update: bool,
) -> bool {
    if !auto_update {
        return false;
    }
    spawn_updater(client, url, data_dir, interval, store, sync_immediately);
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = include_bytes!("../tests/fixtures/definitions-sample.tar.gz");

    /// Serves `body` once on loopback and returns its URL. The listener is
    /// bound to port 0, so the OS picks a free port and tests never collide.
    async fn serve_once(body: &'static [u8], status: u16) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::AsyncWriteExt;
                let head = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(body).await;
                let _ = stream.shutdown().await;
            }
        });
        format!("http://{addr}/definitions.tar.gz")
    }

    #[test]
    fn extraction_writes_only_the_v11_yml_files() {
        let dir = tempfile::tempdir().unwrap();
        let count = extract_definitions(SAMPLE, dir.path()).unwrap();
        assert_eq!(count, 2);
        assert!(dir.path().join("alpha.yml").is_file());
        assert!(dir.path().join("beta.yml").is_file());
    }

    #[test]
    fn extraction_flattens_v11_rather_than_keeping_the_archive_prefix() {
        // The upstream archive nests under Indexers-master/definitions/v11.
        // DefinitionStore reads <id>.yml directly out of its own directory,
        // so anything but a flat result would make every lookup miss.
        let dir = tempfile::tempdir().unwrap();
        extract_definitions(SAMPLE, dir.path()).unwrap();
        assert!(!dir.path().join("Indexers-master").exists());
    }

    #[test]
    fn extraction_ignores_other_schema_versions_and_stray_files() {
        let dir = tempfile::tempdir().unwrap();
        extract_definitions(SAMPLE, dir.path()).unwrap();
        assert!(!dir.path().join("old.yml").exists());
        assert!(!dir.path().join("README.md").exists());
    }

    #[test]
    fn extraction_preserves_file_contents_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        extract_definitions(SAMPLE, dir.path()).unwrap();
        let body = std::fs::read_to_string(dir.path().join("alpha.yml")).unwrap();
        assert_eq!(body, "id: alpha\nname: Alpha\n");
    }

    #[test]
    fn a_corrupt_archive_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let result = extract_definitions(b"this is not a gzip stream", dir.path());
        assert!(matches!(result, Err(SyncError::Archive(_))));
    }

    #[test]
    fn definition_file_name_rejects_backslash_traversal() {
        // Tar entries always use `/`, so this remainder is unreachable from
        // the pinned default URL — but reachable through `definitions_url`
        // pointing at a hostile mirror. On a Windows deployment `\` is a
        // path separator, so accepting this would let `dest.join(name)`
        // escape the destination directory.
        let path = Path::new("definitions/v11/..\\..\\evil.yml");
        assert_eq!(definition_file_name(path), None);
    }

    #[test]
    fn definition_file_name_rejects_a_nested_subdirectory() {
        let path = Path::new("definitions/v11/sub/evil.yml");
        assert_eq!(definition_file_name(path), None);
    }

    #[test]
    fn definition_file_name_rejects_a_second_embedded_v11_marker() {
        let path = Path::new("definitions/v11/definitions/v11/evil.yml");
        assert_eq!(definition_file_name(path), None);
    }

    #[test]
    fn definition_file_name_accepts_a_plain_file() {
        let path = Path::new("definitions/v11/alpha.yml");
        assert_eq!(definition_file_name(path), Some("alpha.yml".to_string()));
    }

    #[test]
    fn an_archive_with_no_v11_directory_is_an_error() {
        // A mirror serving the wrong repository would otherwise install an
        // empty definitions directory over a working one.
        let mut empty = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut empty, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            builder.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            extract_definitions(&empty, dir.path()),
            Err(SyncError::NoDefinitions)
        ));
    }

    #[test]
    fn installing_replaces_an_existing_directory() {
        let root = tempfile::tempdir().unwrap();
        let live = root.path().join("definitions");
        std::fs::create_dir(&live).unwrap();
        std::fs::write(live.join("stale.yml"), "id: stale\n").unwrap();

        let staging = root.path().join("definitions.staging");
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("alpha.yml"), "id: alpha\n").unwrap();

        install_definitions(&staging, &live).unwrap();

        assert!(live.join("alpha.yml").is_file());
        assert!(!live.join("stale.yml").exists(), "the old set survived");
        assert!(!staging.exists(), "staging was left behind");
    }

    #[test]
    fn installing_works_when_nothing_is_there_yet() {
        let root = tempfile::tempdir().unwrap();
        let live = root.path().join("definitions");
        let staging = root.path().join("definitions.staging");
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("alpha.yml"), "id: alpha\n").unwrap();

        install_definitions(&staging, &live).unwrap();

        assert!(live.join("alpha.yml").is_file());
    }

    #[test]
    fn installing_leaves_no_backup_directory_behind() {
        // A leftover definitions.previous would be read by nothing and would
        // double the disk cost of every update.
        let root = tempfile::tempdir().unwrap();
        let live = root.path().join("definitions");
        std::fs::create_dir(&live).unwrap();
        std::fs::write(live.join("stale.yml"), "id: stale\n").unwrap();
        let staging = root.path().join("definitions.staging");
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("alpha.yml"), "id: alpha\n").unwrap();

        install_definitions(&staging, &live).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("definitions")]);
    }

    #[test]
    fn installing_recovers_from_a_stranded_previous_directory() {
        // A prior run died after its swap but before its cleanup. Without
        // recovery, rename(live -> previous) fails with ENOTEMPTY and the
        // definitions can never be updated again.
        let root = tempfile::tempdir().unwrap();
        let live = root.path().join("definitions");
        std::fs::create_dir(&live).unwrap();
        std::fs::write(live.join("current.yml"), "id: current\n").unwrap();

        let stranded = root.path().join("definitions.previous");
        std::fs::create_dir(&stranded).unwrap();
        std::fs::write(stranded.join("ancient.yml"), "id: ancient\n").unwrap();

        let staging = root.path().join("definitions.staging");
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("fresh.yml"), "id: fresh\n").unwrap();

        install_definitions(&staging, &live).unwrap();

        assert!(live.join("fresh.yml").is_file());
        assert!(!live.join("current.yml").exists());
        assert!(!stranded.exists(), "the stranded directory survived");
    }

    #[test]
    fn stage_clears_a_leftover_staging_directory_before_extracting() {
        // A previous crashed run could leave files in staging that don't
        // exist in the new archive at all. `stage`'s doc claims a clean
        // directory every time; if it merely extracted into whatever was
        // already there, `sync_once`'s "staging can never be contaminated"
        // guarantee would be hollow. Proven directly against `stage` rather
        // than through `sync_once`, since a clean tempdir alone can never
        // exercise this path.
        let root = tempfile::tempdir().unwrap();
        let staging = root.path().join("definitions.staging");
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("stale-from-a-crashed-run.yml"), "id: stale\n").unwrap();

        let count = stage(SAMPLE, &staging).unwrap();

        assert_eq!(count, 2);
        assert!(
            !staging.join("stale-from-a-crashed-run.yml").exists(),
            "stage left a file from a previous run in place"
        );
    }

    /// How long a test will wait for `sync_once` before treating it as
    /// hung rather than slow. A loopback transfer of a fixture this small
    /// finishes in milliseconds, so this never fires spuriously — it only
    /// turns a future regression that makes `download` await forever into
    /// a fast, legible test failure instead of a CI job that burns its
    /// time limit and reports nothing useful.
    const SYNC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    #[tokio::test]
    async fn a_successful_sync_installs_the_definitions() {
        let root = tempfile::tempdir().unwrap();
        let url = serve_once(SAMPLE, 200).await;

        let count = tokio::time::timeout(
            SYNC_TIMEOUT,
            sync_once(&reqwest::Client::new(), &url, root.path()),
        )
        .await
        .expect("sync_once hung — a timeout here means a regression, not a slow machine")
        .unwrap();

        assert_eq!(count, 2);
        assert!(root.path().join("definitions/alpha.yml").is_file());
    }

    #[tokio::test]
    async fn a_failed_download_leaves_existing_definitions_untouched() {
        let root = tempfile::tempdir().unwrap();
        let live = root.path().join("definitions");
        std::fs::create_dir(&live).unwrap();
        std::fs::write(live.join("keep.yml"), "id: keep\n").unwrap();

        let url = serve_once(b"", 500).await;
        let result = tokio::time::timeout(
            SYNC_TIMEOUT,
            sync_once(&reqwest::Client::new(), &url, root.path()),
        )
        .await
        .expect("sync_once hung — a timeout here means a regression, not a slow machine");

        assert!(result.is_err());
        assert!(
            live.join("keep.yml").is_file(),
            "a failed refresh destroyed the working set"
        );
    }

    #[tokio::test]
    async fn a_failed_sync_leaves_no_staging_directory_behind() {
        // Also plants a leftover staging directory from a hypothetical
        // earlier crashed run, so this exercises stage's clear-an-existing-
        // leftover branch at the sync_once level too.
        let root = tempfile::tempdir().unwrap();
        let staging = root.path().join("definitions.staging");
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("stale-from-a-crashed-run.yml"), "id: stale\n").unwrap();

        let url = serve_once(b"not a gzip stream", 200).await;

        let _ = tokio::time::timeout(
            SYNC_TIMEOUT,
            sync_once(&reqwest::Client::new(), &url, root.path()),
        )
        .await
        .expect("sync_once hung — a timeout here means a regression, not a slow machine");

        assert!(!staging.exists(), "staging leaked after a failure");
    }

    #[test]
    fn a_populated_directory_does_not_force_an_immediate_sync() {
        assert!(!should_sync_now(548));
    }

    #[test]
    fn an_empty_directory_forces_an_immediate_sync() {
        // A fresh install must not wait a whole interval before it can search.
        assert!(should_sync_now(0));
    }

    #[tokio::test]
    async fn spawn_updater_if_enabled_spawns_and_returns_true_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::definitions::DefinitionStore::new(dir.path().to_path_buf());
        let spawned = spawn_updater_if_enabled(
            reqwest::Client::new(),
            "http://127.0.0.1:0/definitions.tar.gz".to_string(),
            dir.path().to_path_buf(),
            std::time::Duration::from_secs(3600),
            store,
            false,
            true,
        );
        assert!(spawned, "auto_update: true must spawn the updater");
    }

    #[tokio::test]
    async fn spawn_updater_if_enabled_spawns_nothing_and_returns_false_when_disabled() {
        // This is the branch deciding whether the binary ever touches the
        // network at all: a deleted `if` guard here must fail this test,
        // not merely leave the rest of the suite green.
        let dir = tempfile::tempdir().unwrap();
        let store = crate::definitions::DefinitionStore::new(dir.path().to_path_buf());
        let spawned = spawn_updater_if_enabled(
            reqwest::Client::new(),
            "http://127.0.0.1:0/definitions.tar.gz".to_string(),
            dir.path().to_path_buf(),
            std::time::Duration::from_secs(3600),
            store,
            false,
            false,
        );
        assert!(!spawned, "auto_update: false must not spawn the updater");
    }
}
