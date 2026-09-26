-- Review file schema, version 1. Tables are CLAUDE.md §5 verbatim.

CREATE TABLE review (
  id TEXT PRIMARY KEY,
  question TEXT NOT NULL,
  question_type TEXT,           -- intervention | exposure | experience | policy | mixed
  framework TEXT,               -- PICO | PEO | SPIDER | PICo | SPICE
  verification TEXT NOT NULL DEFAULT 'unverified',
  created_at TEXT NOT NULL
);

CREATE TABLE protocol_version (
  id TEXT PRIMARY KEY,
  review_id TEXT NOT NULL REFERENCES review(id),
  version INTEGER NOT NULL,
  body TEXT NOT NULL,           -- JSON: concepts, criteria, date range, languages, designs, planned synthesis
  locked_at TEXT,
  UNIQUE (review_id, version)
);

CREATE TABLE search_run (
  id TEXT PRIMARY KEY,
  review_id TEXT NOT NULL REFERENCES review(id),
  source TEXT NOT NULL,         -- eric | openalex | wos | scopus | pubmed | citation_chasing | manual_import
  query_string TEXT NOT NULL,
  query_ast TEXT NOT NULL,      -- JSON
  simplifications TEXT,         -- JSON list of anything the API could not express
  iteration INTEGER NOT NULL DEFAULT 1,
  run_at TEXT NOT NULL,
  hits INTEGER NOT NULL,
  raw_path TEXT                 -- path to stored raw responses
);

CREATE TABLE record (
  id TEXT PRIMARY KEY,
  search_run_id TEXT NOT NULL REFERENCES search_run(id),
  source_id TEXT,               -- the database's own ID
  doi TEXT, title TEXT, abstract TEXT, authors TEXT, year INTEGER, venue TEXT,
  duplicate_of TEXT REFERENCES record(id)
);

CREATE TABLE study (
  id TEXT PRIMARY KEY,
  review_id TEXT NOT NULL REFERENCES review(id),
  label TEXT NOT NULL           -- e.g. "Smith 2021"
);

CREATE TABLE report (
  id TEXT PRIMARY KEY,
  record_id TEXT NOT NULL REFERENCES record(id),
  study_id TEXT REFERENCES study(id),
  pdf_path TEXT,
  retrieval_status TEXT NOT NULL,  -- pending | open_access | proxy | uploaded | not_retrieved
  page_texts TEXT                  -- JSON array, one string per page
);

CREATE TABLE extraction_value (
  id TEXT PRIMARY KEY,
  study_id TEXT NOT NULL REFERENCES study(id),
  report_id TEXT NOT NULL REFERENCES report(id),
  field TEXT NOT NULL,
  value TEXT,                   -- JSON; NULL when not reported or when validation failed
  source_quote TEXT,
  page INTEGER,
  validation TEXT NOT NULL      -- verified | quote_not_found | not_reported
);

CREATE TABLE appraisal (
  id TEXT PRIMARY KEY,
  study_id TEXT NOT NULL REFERENCES study(id),
  tool TEXT NOT NULL,           -- RoB2 | ROBINS-I | JBI | CASP | MMAT
  domain TEXT NOT NULL,
  judgement TEXT NOT NULL,
  rationale TEXT NOT NULL,
  source_quote TEXT,
  page INTEGER
);

CREATE TABLE event (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  review_id TEXT NOT NULL REFERENCES review(id),
  target_type TEXT NOT NULL,    -- protocol | record | report | study | extraction_value | appraisal
  target_id TEXT NOT NULL,
  stage TEXT NOT NULL,          -- protocol | search | dedup | ta_screen | ft_screen | retrieval | link | extract | appraise | synth | gate1 | gate2 | gate3
  actor_type TEXT NOT NULL CHECK (actor_type IN ('agent','human')),
  actor_id TEXT NOT NULL,       -- 'human' or 'screener_a'
  provider TEXT, model_id TEXT, prompt_version TEXT,
  decision TEXT NOT NULL,       -- include | exclude | uncertain | resolve | duplicate | link | lock | pass | fail ...
  reason_code TEXT,             -- a criterion code from the locked protocol
  rationale TEXT,
  evidence_quote TEXT,
  confidence REAL,
  blinded INTEGER NOT NULL DEFAULT 0,
  supersedes INTEGER REFERENCES event(id),
  tokens_in INTEGER, tokens_out INTEGER, cost_usd REAL,
  created_at TEXT NOT NULL
);
CREATE INDEX event_target ON event (target_type, target_id, stage);

-- The event log is append-only (principle 1). Corrections are new rows with
-- `supersedes` set; the database itself refuses UPDATE and DELETE.
CREATE TRIGGER event_no_update BEFORE UPDATE ON event
BEGIN
  SELECT RAISE(ABORT, 'event rows are append-only');
END;
CREATE TRIGGER event_no_delete BEFORE DELETE ON event
BEGIN
  SELECT RAISE(ABORT, 'event rows are append-only');
END;

CREATE TABLE job (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  review_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  payload TEXT NOT NULL,
  status TEXT NOT NULL,         -- queued | running | done | failed | paused
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  updated_at TEXT NOT NULL
);
CREATE INDEX job_status ON job (status, id);
