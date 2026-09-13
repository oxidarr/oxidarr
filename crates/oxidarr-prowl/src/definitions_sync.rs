//! Runtime acquisition of the Cardigann definitions.
//!
//! The definitions are third-party and unlicensed, which is why this
//! repository never commits them and why no published artefact — container
//! layer, `.crate`, release archive — may contain them. They are fetched on
//! the operator's own machine at runtime, exactly as the operator would have
//! fetched them by hand with `scripts/fetch-definitions.sh`.
//!
//! Every function here takes its inputs explicitly (bytes, paths, a URL) so
//! the module is testable without a network. The background updater added
//! later in this module is the only part that talks to the outside world.

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
/// directory entry. Matches on the marker rather than a fixed top-level
/// directory because the archive's root is named after the branch.
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "case sensitivity is intended: DefinitionStore resolves each definition as \
              <id>.yml exactly, so accepting a .YML entry here would install a file it \
              could never load on a case-sensitive filesystem"
)]
fn definition_file_name(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let rest = text.split_once(V11_MARKER)?.1;
    if rest.is_empty() || rest.contains('/') || !rest.ends_with(".yml") {
        return None;
    }
    Some(rest.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = include_bytes!("../tests/fixtures/definitions-sample.tar.gz");

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
}
