//! Entry point for the `oxidarr-prowl` binary.
//!
//! Loads configuration ([`parse_args`] + [`resolve_config_path`] +
//! `oxidarr_prowl::load_config`), opens the database (applying embedded
//! migrations — `oxidarr_db::Db::open`), builds the three HTTP clients this
//! binary needs — the two `oxidarr_prowl::AppState` expects (a proxied one
//! for tracker traffic, a plain one for application-sync traffic — see that
//! struct's own "Two client fields" docs for why there are two) plus a
//! third, timeout-bound client used only by the background definitions
//! updater (see `oxidarr_prowl::definitions_sync`) — then serves
//! `oxidarr_prowl::app` with graceful shutdown on Ctrl-C.
//!
//! # Definitions
//!
//! By default (`definitions_auto_update = true`) this binary fetches and
//! refreshes the Cardigann definition corpus itself, in the background —
//! see `oxidarr_prowl::definitions_sync::spawn_updater_if_enabled`, which
//! `run` calls unconditionally and which itself honours the setting.
//! Startup never blocks on that fetch: an empty or missing `{data_dir}/definitions`
//! directory prints an advisory to stderr and the process *keeps serving*
//! regardless — the control-plane API (config, applications, indexer CRUD)
//! is useful on its own, and every Torznab/Cardigann search still runs; it
//! simply can't find a definition until the first fetch lands, which
//! already renders as a normal `300` error through `oxidarr_prowl::torznab`.
//!
//! Setting `definitions_auto_update = false` disables the background
//! fetch entirely and hands the directory back to the operator, who is
//! then expected to populate it with `scripts/fetch-definitions.sh` (the
//! stderr advisory says so in that case).
//!
//! # The API key is printed at startup
//!
//! `oxidarr_db::ConfigRepo::api_key` generates and persists this instance's
//! API key on first use. Real Prowlarr surfaces it in a web UI; this crate
//! has none yet, so the only way an operator can learn the key to configure
//! Sonarr/Radarr (or `curl`) against this instance is to read it from the
//! startup log line. It is not regenerated on restart — the same key
//! prints every time once persisted.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use oxidarr_db::{ConfigRepo, Db};
use oxidarr_indexer::ReqwestClient;
use oxidarr_prowl::{AppState, Config, DefinitionStore, app, load_config};
use tokio::net::TcpListener;

const USAGE: &str = "usage: oxidarr-prowl [--config <path>]";

/// Parsed command-line arguments. The only flag this binary accepts is
/// `--config <path>`; everything else (the config file's own contents, env
/// overrides) is handled by `oxidarr_prowl::load_config` once the path is
/// resolved.
struct Args {
    config: Option<PathBuf>,
}

/// Hand-rolled parser for the binary's one flag, mirroring
/// `oxidarr-migrate`'s own `parse_args` — `clap` is not worth pulling in
/// for a single option.
fn parse_args(args: &[String]) -> Result<Args, ()> {
    let mut config = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--config" => {
                let value = iter.next().ok_or(())?;
                config = Some(PathBuf::from(value));
            }
            _ => return Err(()),
        }
    }
    Ok(Args { config })
}

/// Resolves where to look for the config file, per the documented
/// precedence: an explicit `--config` flag, then `OXIDARR_CONFIG`, then
/// `{data_dir}/config.toml` — `data_dir` itself resolved from
/// `OXIDARR_DATA_DIR` or its own default, since the config file's *own*
/// `data_dir` setting can't be known before the file has been found and
/// read.
///
/// This only decides *where* to look; a missing file at the resolved path
/// is not an error (`oxidarr_prowl::load_config`'s own contract) — it is
/// resolved unconditionally and handed to `load_config` either way.
fn resolve_config_path(
    cli_config: Option<PathBuf>,
    env: &dyn Fn(&str) -> Option<String>,
) -> PathBuf {
    cli_config
        .or_else(|| env("OXIDARR_CONFIG").map(PathBuf::from))
        .unwrap_or_else(|| {
            let data_dir = env("OXIDARR_DATA_DIR").map_or_else(
                || PathBuf::from(oxidarr_prowl::config::DEFAULT_DATA_DIR),
                PathBuf::from,
            );
            data_dir.join("config.toml")
        })
}

/// Looks up `key` in the real process environment. The only non-test call
/// site for the `env: &dyn Fn(&str) -> Option<String>` closure both
/// `resolve_config_path` and `load_config` accept.
fn env_lookup(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// Counts `dir`'s `*.yml` definitions via a fresh `DefinitionStore`, or `0`
/// if the directory can't be listed at all (doesn't exist, isn't a
/// directory, etc) — either way this is advisory startup logging, never a
/// startup failure. See this module's own "Definitions" docs.
async fn count_definitions(dir: &Path) -> usize {
    let store = DefinitionStore::new(dir.to_path_buf());
    store.list_ids().await.map_or(0, |ids| ids.len())
}

/// Prints the missing-definitions notice to stderr. Called only when
/// `count_definitions` found none.
///
/// The instruction depends on `auto_update`: when it is `false`, nothing
/// will ever populate the directory on its own, so the operator is told to
/// run `scripts/fetch-definitions.sh` by hand. When it is `true`, the
/// background updater spawned in `run` is already fetching, so telling the
/// operator to also do it themselves would be a lie — instead this says
/// definitions are being fetched in the background and that search results
/// will be limited until that first fetch completes.
fn print_missing_definitions_notice(dir: &Path, auto_update: bool) {
    eprintln!(
        "warning: no Cardigann definitions found in {}",
        dir.display()
    );
    if auto_update {
        eprintln!(
            "oxidarr-prowl is fetching them in the background; search and \
             t=caps for Cardigann indexers will be limited until that \
             first fetch completes."
        );
    } else {
        eprintln!(
            "oxidarr-prowl is configured not to fetch definitions \
             automatically (OXIDARR_DEFINITIONS_AUTO_UPDATE=false); \
             scripts/fetch-definitions.sh writes into <dest>/v11/, one \
             directory level below {} — see the README's \"Quick start\" \
             step 2 for the exact fetch-and-symlink commands.",
            dir.display()
        );
    }
    eprintln!(
        "the control-plane API will continue to serve without them — this \
         only affects search and t=caps for indexers whose definitions are \
         missing."
    );
}

/// Builds the tracker-bound and application-bound HTTP clients `AppState`
/// needs. `config.proxy` applies to the tracker client only — see
/// `oxidarr_indexer::ReqwestClient::with_proxy`'s own docs and
/// `AppState`'s "Two client fields" section for why application-bound
/// traffic (Sonarr/Radarr sync) never goes through a configured proxy.
fn build_clients(config: &Config) -> Result<(ReqwestClient, ReqwestClient), String> {
    let tracker_client = ReqwestClient::with_proxy(config.proxy.as_deref())
        .map_err(|err| format!("building tracker HTTP client: {err}"))?;
    let app_client =
        ReqwestClient::new().map_err(|err| format!("building application HTTP client: {err}"))?;
    Ok((tracker_client, app_client))
}

/// Resolves once `ctrl_c` is delivered — `axum::serve`'s graceful-shutdown
/// future.
async fn shutdown_signal() {
    // Only failure mode is the signal handler itself failing to install,
    // which is not recoverable and not worth surfacing differently from
    // "never shuts down gracefully" — the process still exits on a second,
    // OS-level signal.
    let _ = tokio::signal::ctrl_c().await;
}

/// The async body of the binary: everything from "database exists" through
/// "serving until Ctrl-C". Returns `Err` with a human-readable message on
/// any startup failure; `main` is responsible for printing it and choosing
/// the process exit code.
async fn run(config: Config) -> Result<(), String> {
    tokio::fs::create_dir_all(&config.data_dir)
        .await
        .map_err(|err| {
            format!(
                "creating data directory {}: {err}",
                config.data_dir.display()
            )
        })?;

    let db_path = config.data_dir.join("oxidarr.db");
    let db = Db::open(&db_path)
        .await
        .map_err(|err| format!("opening database {}: {err}", db_path.display()))?;
    let db = Arc::new(db);

    let definitions_dir = oxidarr_prowl::definitions_sync::definitions_dir(&config.data_dir);
    let definitions_count = count_definitions(&definitions_dir).await;
    if definitions_count == 0 {
        print_missing_definitions_notice(&definitions_dir, config.definitions_auto_update);
    }

    let (tracker_client, app_client) = build_clients(&config)?;

    let state = AppState {
        db: Arc::clone(&db),
        tracker_client,
        app_client,
        defs: DefinitionStore::new(definitions_dir),
        external_url: config.external_url.clone(),
    };
    let updater_defs = state.defs.clone();

    let api_key = ConfigRepo::new(&db)
        .api_key()
        .await
        .map_err(|err| format!("reading instance API key: {err}"))?;

    let router = app(state)
        .await
        .map_err(|err| format!("building application router: {err}"))?;

    let listener = TcpListener::bind(config.bind)
        .await
        .map_err(|err| format!("binding {}: {err}", config.bind))?;

    print_startup_banner(&config, definitions_count, &api_key);

    let definitions_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|err| format!("building the definitions HTTP client: {err}"))?;

    // The return value only matters to tests exercising the enabled/disabled
    // gate directly (see `definitions_sync::spawn_updater_if_enabled`'s own
    // tests); `run` itself has nothing further to do with it.
    let _ = oxidarr_prowl::definitions_sync::spawn_updater_if_enabled(
        definitions_client,
        config.definitions_url.clone(),
        config.data_dir.clone(),
        std::time::Duration::from_secs(config.definitions_interval),
        updater_defs,
        oxidarr_prowl::definitions_sync::should_sync_now(definitions_count),
        config.definitions_auto_update,
    );

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|err| format!("serving: {err}"))
}

/// Prints the fixed set of startup lines an operator needs: version, bind
/// address, data directory, external URL, how many definitions loaded, and
/// the instance API key (see this module's own "The API key is printed at
/// startup" docs for why the last one is printed at all).
fn print_startup_banner(config: &Config, definitions_count: usize, api_key: &str) {
    println!("oxidarr-prowl {}", env!("CARGO_PKG_VERSION"));
    println!("listening on {}", config.bind);
    println!("data dir: {}", config.data_dir.display());
    println!("external url: {}", config.external_url);
    println!("definitions loaded: {definitions_count}");
    println!("api key: {api_key}");
}

/// Loads configuration from `--config`/`OXIDARR_CONFIG`/`{data_dir}/config.toml`
/// (in that order) plus `OXIDARR_*` env overrides, printing a usage message
/// and returning `None` for `main` to treat as a hard exit if the arguments
/// themselves are malformed.
fn load_configured(args: &[String]) -> Option<Config> {
    let Ok(parsed) = parse_args(args) else {
        eprintln!("{USAGE}");
        return None;
    };
    let config_path = resolve_config_path(parsed.config, &env_lookup);
    match load_config(Some(&config_path), &env_lookup) {
        Ok(config) => Some(config),
        Err(err) => {
            eprintln!("error: {err}");
            None
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(config) = load_configured(&args) else {
        return ExitCode::FAILURE;
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run(config)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn parses_no_flags_as_no_config_path() {
        let parsed = parse_args(&[]).unwrap();
        assert_eq!(parsed.config, None);
    }

    #[test]
    fn parses_the_config_flag() {
        let args = vec!["--config".to_string(), "/etc/oxidarr.toml".to_string()];
        let parsed = parse_args(&args).unwrap();
        assert_eq!(parsed.config, Some(PathBuf::from("/etc/oxidarr.toml")));
    }

    #[test]
    fn config_flag_without_a_value_is_an_error() {
        let args = vec!["--config".to_string()];
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn an_unknown_flag_is_an_error() {
        let args = vec!["--bogus".to_string()];
        assert!(parse_args(&args).is_err());
    }

    fn no_env(_key: &str) -> Option<String> {
        None
    }

    #[test]
    fn resolve_config_path_prefers_the_cli_flag() {
        let env = |key: &str| match key {
            "OXIDARR_CONFIG" => Some("/from/env.toml".to_string()),
            _ => None,
        };
        let path = resolve_config_path(Some(PathBuf::from("/from/cli.toml")), &env);
        assert_eq!(path, PathBuf::from("/from/cli.toml"));
    }

    #[test]
    fn resolve_config_path_falls_back_to_the_env_var() {
        let env = |key: &str| match key {
            "OXIDARR_CONFIG" => Some("/from/env.toml".to_string()),
            _ => None,
        };
        let path = resolve_config_path(None, &env);
        assert_eq!(path, PathBuf::from("/from/env.toml"));
    }

    #[test]
    fn resolve_config_path_defaults_to_data_dir_config_toml() {
        let path = resolve_config_path(None, &no_env);
        assert_eq!(path, PathBuf::from("./data").join("config.toml"));
    }

    #[test]
    fn resolve_config_path_uses_an_overridden_data_dir_for_the_default() {
        let env = |key: &str| match key {
            "OXIDARR_DATA_DIR" => Some("/srv/oxidarr".to_string()),
            _ => None,
        };
        let path = resolve_config_path(None, &env);
        assert_eq!(path, PathBuf::from("/srv/oxidarr/config.toml"));
    }

    #[tokio::test]
    async fn count_definitions_of_a_missing_directory_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("definitions");
        assert_eq!(count_definitions(&missing).await, 0);
    }

    #[tokio::test]
    async fn count_definitions_counts_yml_files_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.yml"), "id: a\n").unwrap();
        std::fs::write(dir.path().join("b.yml"), "id: b\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a definition").unwrap();
        assert_eq!(count_definitions(dir.path()).await, 2);
    }
}
