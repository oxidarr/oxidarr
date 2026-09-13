//! Wire-shape parity: every fixture in
//! `crates/oxidarr-prowl/tests/fixtures/wire/` is pinned, byte-for-byte,
//! from the server's own DTOs by
//! `crates/oxidarr-prowl/tests/wire_fixtures.rs` (see that file's own module
//! docs for the canonical format and the update procedure). This file
//! proves the other half of the contract: this crate's own mirrored DTOs
//! (`oxidarr_ui::dto`) deserialize each fixture and re-serialize it
//! byte-for-byte identical, both directions. A field name, `camelCase`
//! rename, default, or `skip_serializing_if` mismatch between this crate's
//! DTOs and the server's own shows up here as a real assertion failure, not
//! a "looks right" judgment call.
//!
//! Six fixtures, chosen for variety rather than just one happy-path sample
//! per type: a release with `seeders: null`, a magnet-only release (no
//! `downloadUrl`), an indexer schema entry with a `select` field (carrying
//! `selectOptions`), an indexer resource and an application resource each
//! carrying a `syncError`, and the system status response.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

use oxidarr_ui::dto::{ApplicationResource, IndexerResource, ReleaseResource, SystemStatus};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Reads one committed wire fixture by file name.
fn fixture(name: &str) -> String {
    let path = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../oxidarr-prowl/tests/fixtures/wire"
    ))
    .join(name);
    fs::read_to_string(&path).unwrap()
}

/// Deserializes `name`'s fixture as `T`, re-serializes it (pretty-printed
/// plus a trailing newline — the same canonical format
/// `wire_fixtures.rs` writes fixtures in), and asserts the result is
/// byte-for-byte identical to the fixture file.
fn assert_round_trips<T>(name: &str)
where
    T: Serialize + DeserializeOwned,
{
    let raw = fixture(name);
    let value: T = serde_json::from_str(&raw).unwrap();
    let mut rendered = serde_json::to_string_pretty(&value).unwrap();
    rendered.push('\n');
    assert_eq!(
        rendered, raw,
        "fixture {name} did not round-trip byte-for-byte through oxidarr_ui::dto"
    );
}

#[test]
fn system_status_round_trips() {
    assert_round_trips::<SystemStatus>("system_status.json");
}

#[test]
fn an_indexer_schema_entry_with_a_select_field_round_trips() {
    assert_round_trips::<IndexerResource>("indexer_schema_select.json");
}

#[test]
fn an_indexer_resource_carrying_a_sync_error_round_trips() {
    assert_round_trips::<IndexerResource>("indexer_resource_sync_error.json");
}

#[test]
fn an_application_resource_carrying_a_sync_error_round_trips() {
    assert_round_trips::<ApplicationResource>("application_resource_sync_error.json");
}

#[test]
fn a_release_with_null_seeders_round_trips() {
    assert_round_trips::<ReleaseResource>("release_seeders_null.json");
}

#[test]
fn a_magnet_only_release_round_trips() {
    assert_round_trips::<ReleaseResource>("release_magnet_only.json");
}
