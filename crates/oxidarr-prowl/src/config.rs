//! Instance configuration: a TOML file with `OXIDARR_*` environment
//! variable overrides.
//!
//! [`load_config`] is the whole story: `defaults < file < env`, in that
//! precedence order, folded into one [`Config`]. It takes its environment
//! lookup as a closure (`&dyn Fn(&str) -> Option<String>`) rather than
//! reading `std::env::var` itself, so the precedence matrix is unit
//! testable without mutating the real process environment — the binary's
//! `main` passes a closure over [`std::env::var`] at the one call site that
//! matters.
//!
//! # Fields
//!
//! | Field | Env var | Default | Notes |
//! |---|---|---|---|
//! | `bind` | `OXIDARR_BIND` | `"0.0.0.0:9696"` | Prowlarr's own default port |
//! | `data_dir` | `OXIDARR_DATA_DIR` | `"./data"` | holds `oxidarr.db` and the `definitions/` directory |
//! | `external_url` | `OXIDARR_EXTERNAL_URL` | `http://{bind}` | see below |
//! | `proxy` | `OXIDARR_PROXY` | none | tracker traffic only — see `oxidarr_indexer::ReqwestClient::with_proxy` |
//! | `log` | `OXIDARR_LOG` | `"info"` | accepted but unused until a tracing pass wires it up |
//!
//! `external_url`'s default is derived from `bind` *literally* —
//! `http://0.0.0.0:9696` if `bind` is left at its own default. That is
//! almost never the address a client can actually reach: behind a reverse
//! proxy, NAT, or any container port mapping, the operator must set
//! `external_url` explicitly (file or `OXIDARR_EXTERNAL_URL`). This module
//! does not attempt to guess a better default (e.g. resolving a local
//! interface address) — there is no single right guess, and a wrong one
//! silently baked into every synced Sonarr/Radarr indexer's `baseUrl`
//! (`AppState::external_url`) is worse than an obviously-unreachable one an
//! operator notices immediately.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use url::Url;

/// Default `data_dir` when neither the config file nor `OXIDARR_DATA_DIR`
/// set one. Exposed (rather than a private constant) so `main` can resolve
/// the config file's own location (`{data_dir}/config.toml`) before
/// [`load_config`] has run — see that binary's `resolve_config_path`.
pub const DEFAULT_DATA_DIR: &str = "./data";

const DEFAULT_BIND: &str = "0.0.0.0:9696";
const DEFAULT_LOG: &str = "info";

/// Fully resolved instance configuration — the result of merging defaults,
/// an optional TOML file, and environment overrides. See the module docs
/// for the full field table and precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The address `axum::serve` binds to.
    pub bind: SocketAddr,
    /// Directory holding `oxidarr.db` and the `definitions/` subdirectory.
    pub data_dir: PathBuf,
    /// This instance's own publicly reachable base URL, forwarded verbatim
    /// into [`crate::AppState::external_url`]. See the module docs for why
    /// its default is not necessarily reachable.
    pub external_url: Url,
    /// An optional proxy URL (`http://`/`https://`/`socks5://`/`socks5h://`)
    /// applied to tracker-bound traffic only — see
    /// `oxidarr_indexer::ReqwestClient::with_proxy`.
    pub proxy: Option<String>,
    /// A log-level string, accepted for forward compatibility but not yet
    /// consumed by anything — no tracing subscriber is wired up in this
    /// crate yet.
    pub log: String,
}

/// Failure loading or validating configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The config file exists but could not be read (permissions, not a
    /// regular file, etc). A *missing* file is not this variant — see
    /// [`load_config`]'s own docs.
    #[error("reading config file {path}: {source}")]
    Io {
        /// The config file path that could not be read.
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The config file was read but is not valid TOML, or has a field of
    /// the wrong shape.
    #[error("parsing config file {path}: {source}")]
    Toml {
        /// The config file path that failed to parse.
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    /// The resolved `bind` value (from file, env, or the default) is not a
    /// valid `host:port` socket address.
    #[error("invalid bind address {value:?}: {source}")]
    Bind {
        /// The value that failed to parse.
        value: String,
        #[source]
        source: std::net::AddrParseError,
    },
    /// The resolved `external_url` value is not a valid URL.
    #[error("invalid external_url {value:?}: {source}")]
    ExternalUrl {
        /// The value that failed to parse.
        value: String,
        #[source]
        source: url::ParseError,
    },
}

/// The TOML file's own shape: every field optional, since any of them may
/// instead come from an env override or fall back to a default. `bind` and
/// `external_url` are read as plain `String`s here (not `SocketAddr`/`Url`)
/// so a malformed value fails through [`load_config`]'s own
/// [`ConfigError::Bind`]/[`ConfigError::ExternalUrl`] with a message naming
/// the value, rather than through a `toml::de::Error` that can't
/// distinguish "wrong field" from "unparseable address."
#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    bind: Option<String>,
    data_dir: Option<PathBuf>,
    external_url: Option<String>,
    proxy: Option<String>,
    log: Option<String>,
}

/// Loads and merges configuration: defaults, then `path` (if given and if
/// it exists), then `env` overrides — in that precedence order, field by
/// field.
///
/// `path` is `None` when the caller has no file to consult at all (used by
/// tests exercising pure env/default precedence); `Some(path)` triggers a
/// read attempt. Either way, a file that does not exist is *not* an
/// error — it is treated identically to `path: None`, so an operator who
/// never created a config file gets defaults (plus any env overrides)
/// rather than a startup failure. A file that exists but fails to read or
/// parse *is* an error.
///
/// # Errors
///
/// Returns [`ConfigError::Io`] if `path` names a file that exists but
/// cannot be read, [`ConfigError::Toml`] if it can be read but is not valid
/// TOML, or [`ConfigError::Bind`]/[`ConfigError::ExternalUrl`] if the
/// resolved `bind`/`external_url` value (from any source) does not parse.
pub fn load_config(
    path: Option<&Path>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Config, ConfigError> {
    let file = match path {
        Some(path) => read_file_config(path)?,
        None => FileConfig::default(),
    };

    let bind_value = env("OXIDARR_BIND")
        .or(file.bind)
        .unwrap_or_else(|| DEFAULT_BIND.to_string());
    let bind: SocketAddr = bind_value.parse().map_err(|source| ConfigError::Bind {
        value: bind_value.clone(),
        source,
    })?;

    let data_dir = env("OXIDARR_DATA_DIR")
        .map(PathBuf::from)
        .or(file.data_dir)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR));

    let external_url_value = env("OXIDARR_EXTERNAL_URL")
        .or(file.external_url)
        .unwrap_or_else(|| format!("http://{bind_value}"));
    let external_url: Url =
        external_url_value
            .parse()
            .map_err(|source| ConfigError::ExternalUrl {
                value: external_url_value.clone(),
                source,
            })?;

    let proxy = env("OXIDARR_PROXY").or(file.proxy);
    let log = env("OXIDARR_LOG")
        .or(file.log)
        .unwrap_or_else(|| DEFAULT_LOG.to_string());

    Ok(Config {
        bind,
        data_dir,
        external_url,
        proxy,
        log,
    })
}

/// Reads and parses `path` as a [`FileConfig`]. A file that does not exist
/// is not an error: it returns [`FileConfig::default`] (all fields absent),
/// per [`load_config`]'s own documented "missing file is not an error"
/// contract.
fn read_file_config(path: &Path) -> Result<FileConfig, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|source| ConfigError::Toml {
            path: path.to_path_buf(),
            source,
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(FileConfig::default()),
        Err(source) => Err(ConfigError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// An `env` closure that always answers "unset" — the common case for
    /// tests that only care about file/default precedence.
    fn no_env(_key: &str) -> Option<String> {
        None
    }

    #[test]
    fn defaults_are_used_when_no_file_and_no_env() {
        let config = load_config(None, &no_env).unwrap();

        assert_eq!(config.bind, "0.0.0.0:9696".parse::<SocketAddr>().unwrap());
        assert_eq!(config.data_dir, PathBuf::from("./data"));
        assert_eq!(config.external_url.as_str(), "http://0.0.0.0:9696/");
        assert_eq!(config.proxy, None);
        assert_eq!(config.log, "info");
    }

    #[test]
    fn a_missing_file_at_an_explicit_path_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.toml");

        let config = load_config(Some(&missing), &no_env).unwrap();

        assert_eq!(config.bind, "0.0.0.0:9696".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn file_values_override_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
            bind = "127.0.0.1:9697"
            data_dir = "/var/lib/oxidarr"
            external_url = "https://oxidarr.example.com"
            proxy = "http://proxy.example.com:8080"
            log = "debug"
            "#,
        )
        .unwrap();

        let config = load_config(Some(&path), &no_env).unwrap();

        assert_eq!(config.bind, "127.0.0.1:9697".parse::<SocketAddr>().unwrap());
        assert_eq!(config.data_dir, PathBuf::from("/var/lib/oxidarr"));
        assert_eq!(config.external_url.as_str(), "https://oxidarr.example.com/");
        assert_eq!(
            config.proxy.as_deref(),
            Some("http://proxy.example.com:8080")
        );
        assert_eq!(config.log, "debug");
    }

    #[test]
    fn env_values_override_file_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
            bind = "127.0.0.1:9697"
            data_dir = "/from/file"
            external_url = "https://from-file.example.com"
            proxy = "http://from-file-proxy:8080"
            log = "debug"
            "#,
        )
        .unwrap();
        let env = |key: &str| match key {
            "OXIDARR_BIND" => Some("127.0.0.1:9999".to_string()),
            "OXIDARR_DATA_DIR" => Some("/from/env".to_string()),
            "OXIDARR_EXTERNAL_URL" => Some("https://from-env.example.com".to_string()),
            "OXIDARR_PROXY" => Some("socks5://from-env-proxy:1080".to_string()),
            "OXIDARR_LOG" => Some("trace".to_string()),
            _ => None,
        };

        let config = load_config(Some(&path), &env).unwrap();

        assert_eq!(config.bind, "127.0.0.1:9999".parse::<SocketAddr>().unwrap());
        assert_eq!(config.data_dir, PathBuf::from("/from/env"));
        assert_eq!(
            config.external_url.as_str(),
            "https://from-env.example.com/"
        );
        assert_eq!(
            config.proxy.as_deref(),
            Some("socks5://from-env-proxy:1080")
        );
        assert_eq!(config.log, "trace");
    }

    #[test]
    fn env_values_override_defaults_with_no_file_at_all() {
        let env = |key: &str| match key {
            "OXIDARR_BIND" => Some("127.0.0.1:1234".to_string()),
            _ => None,
        };

        let config = load_config(None, &env).unwrap();

        assert_eq!(config.bind, "127.0.0.1:1234".parse::<SocketAddr>().unwrap());
        // external_url still derives from the (env-overridden) bind, since
        // neither file nor env set external_url explicitly here.
        assert_eq!(config.external_url.as_str(), "http://127.0.0.1:1234/");
    }

    #[test]
    fn an_unset_proxy_stays_none_even_with_a_file_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "bind = \"127.0.0.1:9697\"\n").unwrap();

        let config = load_config(Some(&path), &no_env).unwrap();

        assert_eq!(config.proxy, None);
    }

    #[test]
    fn an_invalid_bind_value_is_a_bind_error() {
        let env = |key: &str| match key {
            "OXIDARR_BIND" => Some("not-a-socket-addr".to_string()),
            _ => None,
        };

        let err = load_config(None, &env).unwrap_err();

        assert!(matches!(err, ConfigError::Bind { .. }), "err was: {err}");
    }

    #[test]
    fn an_invalid_external_url_value_is_an_external_url_error() {
        let env = |key: &str| match key {
            "OXIDARR_EXTERNAL_URL" => Some("::not a url::".to_string()),
            _ => None,
        };

        let err = load_config(None, &env).unwrap_err();

        assert!(
            matches!(err, ConfigError::ExternalUrl { .. }),
            "err was: {err}"
        );
    }

    #[test]
    fn a_file_that_exists_but_is_not_valid_toml_is_a_toml_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is not [valid toml").unwrap();

        let err = load_config(Some(&path), &no_env).unwrap_err();

        assert!(matches!(err, ConfigError::Toml { .. }), "err was: {err}");
    }

    #[test]
    fn a_file_that_cannot_be_read_at_all_is_an_io_error() {
        // A directory can't be `read_to_string`'d, so this exercises the
        // "exists but is not readable as a file" path distinctly from a
        // simply-missing path.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::create_dir(&path).unwrap();

        let err = load_config(Some(&path), &no_env).unwrap_err();

        assert!(matches!(err, ConfigError::Io { .. }), "err was: {err}");
    }
}
