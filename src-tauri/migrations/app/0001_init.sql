-- app.db schema, version 1. App-level settings and the list of known review files.

CREATE TABLE setting (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL            -- JSON
);

CREATE TABLE review_file (
  review_id TEXT PRIMARY KEY,
  path TEXT NOT NULL UNIQUE,
  added_at TEXT NOT NULL
);
