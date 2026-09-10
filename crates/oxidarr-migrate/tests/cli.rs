//! Black-box tests: run the compiled `oxidarr-migrate` binary and assert on
//! its exit code and output.
#![allow(clippy::unwrap_used)]

#[test]
fn migrate_creates_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("x.db");
    let bin = env!("CARGO_BIN_EXE_oxidarr-migrate");
    let out1 = std::process::Command::new(bin)
        .args(["--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out1.status.success());
    assert!(String::from_utf8_lossy(&out1.stdout).contains("applied 0001"));
    let out2 = std::process::Command::new(bin)
        .args(["--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out2.status.success());
    assert!(String::from_utf8_lossy(&out2.stdout).contains("up to date"));
}

#[test]
fn missing_db_flag_is_usage_error() {
    let bin = env!("CARGO_BIN_EXE_oxidarr-migrate");
    let out = std::process::Command::new(bin).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn status_on_missing_db_reports_pending_and_touches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("x.db");
    let bin = env!("CARGO_BIN_EXE_oxidarr-migrate");
    let out = std::process::Command::new(bin)
        .args(["--db", db.to_str().unwrap(), "--status"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("pending 0001"));
    assert!(!db.exists());
}

#[test]
fn status_after_migrate_reports_applied_and_does_not_mutate_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("x.db");
    let bin = env!("CARGO_BIN_EXE_oxidarr-migrate");
    let migrate_out = std::process::Command::new(bin)
        .args(["--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(migrate_out.status.success());

    // `migrate` leaves the WAL file uncheckpointed (the process exits via
    // `std::process::exit`, so the connection pool never closes cleanly),
    // so the `-wal`/`-shm` sidecars already exist by the time `--status`
    // runs. That makes "no new sidecar files appear" true trivially and not
    // a useful signal here; SQLite also touches those sidecars for *any*
    // connection to a WAL-mode database, read-only or not, since a reader
    // still needs the shared-memory index to see a consistent snapshot. So
    // the meaningful, non-flaky assertion for "`--status` is strictly
    // read-only" is that the main database file's bytes are unchanged by
    // `--status` — the sidecars are allowed to exist, but no committed data
    // may move as a result of running status.
    let before = std::fs::read(&db).unwrap();

    let status_out = std::process::Command::new(bin)
        .args(["--db", db.to_str().unwrap(), "--status"])
        .output()
        .unwrap();
    assert!(status_out.status.success());
    assert!(String::from_utf8_lossy(&status_out.stdout).contains("applied 0001"));

    let after = std::fs::read(&db).unwrap();
    assert_eq!(before, after, "--status must not mutate the database file");
}

#[test]
fn unknown_flag_is_usage_error() {
    let bin = env!("CARGO_BIN_EXE_oxidarr-migrate");
    let out = std::process::Command::new(bin)
        .args(["--bogus"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn db_flag_without_value_is_usage_error() {
    let bin = env!("CARGO_BIN_EXE_oxidarr-migrate");
    let out = std::process::Command::new(bin)
        .args(["--db"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn migrate_open_failure_exits_1_with_stderr() {
    // A directory can't be opened as a SQLite database file, so this
    // deterministically exercises the `Db::open` failure branch in
    // `migrate` without needing to fabricate a corrupt file.
    let dir = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_oxidarr-migrate");
    let out = std::process::Command::new(bin)
        .args(["--db", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(!out.stderr.is_empty());
}

#[test]
fn status_open_failure_exits_1_with_stderr() {
    // The path exists (it's a directory) so `--status` must attempt to open
    // it and surface the failure, rather than silently treating it like a
    // missing file and reporting everything pending.
    let dir = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_oxidarr-migrate");
    let out = std::process::Command::new(bin)
        .args(["--db", dir.path().to_str().unwrap(), "--status"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(!out.stderr.is_empty());
}
