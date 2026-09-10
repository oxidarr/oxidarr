//! `oxidarr-migrate`: a thin CLI over `oxidarr-db`'s embedded schema
//! migrations. Applies pending migrations against a `SQLite` database file,
//! or reports which migrations are applied/pending without changing
//! anything.
//!
//! ```text
//! oxidarr-migrate --db <path>            apply pending migrations
//! oxidarr-migrate --db <path> --status   report applied/pending, read-only
//! ```

use oxidarr_db::Db;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const USAGE: &str = "usage: oxidarr-migrate --db <path> [--status]";

struct Args {
    db: PathBuf,
    status: bool,
}

/// Hand-rolled parser for the binary's two flags. `clap` is not worth
/// pulling in for a pair of options.
fn parse_args(args: &[String]) -> Result<Args, ()> {
    let mut db: Option<PathBuf> = None;
    let mut status = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--db" => {
                let value = iter.next().ok_or(())?;
                db = Some(PathBuf::from(value));
            }
            "--status" => status = true,
            _ => return Err(()),
        }
    }
    let db = db.ok_or(())?;
    Ok(Args { db, status })
}

/// The set of migration versions recorded in `_sqlx_migrations`, or an
/// empty set if that table does not exist yet.
async fn applied_versions(pool: &SqlitePool) -> Result<BTreeSet<i64>, sqlx::Error> {
    let table_exists: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_one(pool)
    .await?;
    if table_exists == 0 {
        return Ok(BTreeSet::new());
    }
    let versions: Vec<i64> = sqlx::query_scalar("SELECT version FROM _sqlx_migrations")
        .fetch_all(pool)
        .await?;
    Ok(versions.into_iter().collect())
}

/// Reads the applied-migration snapshot without running any migrations and
/// without creating the database file if it is absent. The connection is
/// opened read-only, so the main database file cannot be modified. `SQLite`
/// still creates/touches `-wal`/`-shm` sidecar files for any connection to
/// a WAL-mode database, read-only included; the database contents are
/// untouched either way.
async fn snapshot_applied(path: &Path) -> Result<BTreeSet<i64>, sqlx::Error> {
    if !path.exists() {
        return Ok(BTreeSet::new());
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .read_only(true);
    let pool = SqlitePoolOptions::new().connect_with(options).await?;
    let versions = applied_versions(&pool).await;
    pool.close().await;
    versions
}

/// Opens (creating if absent) and migrates the database at `path`, printing
/// one `applied {version}_{description}` line per newly-applied migration,
/// or `up to date` when there was nothing to do.
async fn migrate(path: &Path) -> i32 {
    let before = match snapshot_applied(path).await {
        Ok(versions) => versions,
        Err(err) => {
            eprintln!("error: {err}");
            return 1;
        }
    };

    let db = match Db::open(path).await {
        Ok(db) => db,
        Err(err) => {
            eprintln!("error: {err}");
            return 1;
        }
    };

    let after = match applied_versions(db.pool()).await {
        Ok(versions) => versions,
        Err(err) => {
            eprintln!("error: {err}");
            return 1;
        }
    };

    let mut applied_any = false;
    for migration in Db::migrator().iter() {
        if after.contains(&migration.version) && !before.contains(&migration.version) {
            println!("applied {:04}_{}", migration.version, migration.description);
            applied_any = true;
        }
    }
    if !applied_any {
        println!("up to date");
    }
    0
}

/// Reports applied/pending status for every embedded migration without
/// opening a migrating connection, so a missing database file is left
/// untouched and every migration reports as pending. The connection used to
/// inspect an existing database is read-only, so `--status` never applies
/// migrations and never modifies the database contents (WAL `-wal`/`-shm`
/// sidecar files may still be created by `SQLite` for any WAL-mode
/// connection, read-only included).
async fn report_status(path: &Path) -> i32 {
    let applied = match snapshot_applied(path).await {
        Ok(versions) => versions,
        Err(err) => {
            eprintln!("error: {err}");
            return 1;
        }
    };

    for migration in Db::migrator().iter() {
        if applied.contains(&migration.version) {
            println!("applied {:04}_{}", migration.version, migration.description);
        } else {
            println!("pending {:04}_{}", migration.version, migration.description);
        }
    }
    0
}

/// Parses `args` (excluding the program name) and runs the requested
/// command, returning the process exit code.
///
/// Argument parsing is fully synchronous, so the exit-2 usage-error paths
/// never construct a tokio runtime and are unit-testable with plain `#[test]`
/// functions.
fn run(args: &[String]) -> i32 {
    let Ok(parsed) = parse_args(args) else {
        eprintln!("{USAGE}");
        return 2;
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("error: {err}");
            return 1;
        }
    };

    runtime.block_on(async {
        if parsed.status {
            report_status(&parsed.db).await
        } else {
            migrate(&parsed.db).await
        }
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(run(&args));
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn parses_required_db_flag() {
        let args = vec!["--db".to_string(), "/tmp/x.db".to_string()];
        let parsed = parse_args(&args).unwrap();
        assert_eq!(parsed.db, PathBuf::from("/tmp/x.db"));
        assert!(!parsed.status);
    }

    #[test]
    fn parses_status_flag_regardless_of_order() {
        let args = vec![
            "--status".to_string(),
            "--db".to_string(),
            "/tmp/x.db".to_string(),
        ];
        let parsed = parse_args(&args).unwrap();
        assert_eq!(parsed.db, PathBuf::from("/tmp/x.db"));
        assert!(parsed.status);
    }

    #[test]
    fn missing_db_flag_is_an_error() {
        assert!(parse_args(&[]).is_err());
    }

    #[test]
    fn db_flag_without_a_value_is_an_error() {
        let args = vec!["--db".to_string()];
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn unknown_flag_is_an_error() {
        let args = vec!["--bogus".to_string()];
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn run_exits_2_on_missing_db_flag() {
        assert_eq!(run(&[]), 2);
    }

    #[test]
    fn run_exits_2_on_unknown_flag() {
        let args = vec!["--bogus".to_string()];
        assert_eq!(run(&args), 2);
    }

    #[test]
    fn run_exits_2_on_db_flag_without_value() {
        let args = vec!["--db".to_string()];
        assert_eq!(run(&args), 2);
    }
}
