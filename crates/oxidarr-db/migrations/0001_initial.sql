CREATE TABLE indexers (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT NOT NULL UNIQUE,
    definition_id TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('cardigann','newznab','torznab')),
    enabled       INTEGER NOT NULL DEFAULT 1,
    settings      TEXT NOT NULL DEFAULT '{}',
    priority      INTEGER NOT NULL DEFAULT 25,
    added         TEXT NOT NULL
);
CREATE TABLE applications (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT NOT NULL UNIQUE,
    kind       TEXT NOT NULL CHECK (kind IN ('sonarr','radarr')),
    base_url   TEXT NOT NULL,
    api_key    TEXT NOT NULL,
    sync_level TEXT NOT NULL DEFAULT 'fullSync'
               CHECK (sync_level IN ('disabled','addOnly','fullSync')),
    added      TEXT NOT NULL
);
CREATE TABLE app_indexer_map (
    app_id            INTEGER NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
    indexer_id        INTEGER NOT NULL REFERENCES indexers(id) ON DELETE CASCADE,
    remote_indexer_id INTEGER NOT NULL,
    UNIQUE (app_id, indexer_id)
);
CREATE TABLE config (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
