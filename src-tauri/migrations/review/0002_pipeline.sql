-- Review file schema, version 2: what the pipeline needs beyond §5.
-- None of these tables stores a count; counts come from the event log.

-- Per-review options chosen on the Start screen (years, languages, sources, caps).
CREATE TABLE review_setting (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL              -- JSON
);

-- Run control. Operational state, not a review decision, so not an event.
CREATE TABLE run_state (
  review_id TEXT PRIMARY KEY REFERENCES review(id),
  paused INTEGER NOT NULL DEFAULT 0,
  pause_reason TEXT,
  rate_limited INTEGER NOT NULL DEFAULT 0,   -- 1: resumes by itself after a cooldown
  updated_at TEXT NOT NULL
);

-- Every model call, with the raw response (principle 6: reproducible runs).
CREATE TABLE llm_call (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  review_id TEXT NOT NULL REFERENCES review(id),
  task TEXT NOT NULL,
  provider TEXT NOT NULL,
  model_requested TEXT NOT NULL,
  model_reported TEXT,
  routed_via TEXT,
  prompt_version TEXT NOT NULL,
  temperature_applied REAL,
  structured_output INTEGER NOT NULL,
  tokens_in INTEGER,
  tokens_out INTEGER,
  cost_usd REAL,
  request TEXT NOT NULL,           -- JSON: system prompt, messages, schema
  raw_response TEXT NOT NULL,
  json_valid INTEGER,
  created_at TEXT NOT NULL
);

-- Synthesis outputs: codes, descriptive and analytical themes, SWiM groups,
-- certainty judgements. Each is created by an event at stage 'synth'.
CREATE TABLE synthesis_item (
  id TEXT PRIMARY KEY,
  review_id TEXT NOT NULL REFERENCES review(id),
  kind TEXT NOT NULL,              -- code | descriptive_theme | analytical_theme | swim_group | cerqual | grade
  parent_id TEXT REFERENCES synthesis_item(id),
  label TEXT NOT NULL,
  body TEXT NOT NULL,              -- JSON
  created_at TEXT NOT NULL
);
CREATE INDEX synthesis_item_kind ON synthesis_item (kind);

-- Which verified extraction values (and so which quotes) support a code or theme.
CREATE TABLE synthesis_link (
  item_id TEXT NOT NULL REFERENCES synthesis_item(id),
  extraction_value_id TEXT NOT NULL REFERENCES extraction_value(id),
  PRIMARY KEY (item_id, extraction_value_id)
);

-- Text the review author writes (PRISMA items 3, 25, 26) or edits in drafts.
CREATE TABLE author_text (
  key TEXT PRIMARY KEY,
  text TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE INDEX record_run ON record (search_run_id);
CREATE INDEX report_record ON report (record_id);
CREATE INDEX extraction_study ON extraction_value (study_id);
CREATE INDEX event_stage ON event (stage, actor_id);

-- Retrieval hints captured at search time (open-access PDF, landing page).
ALTER TABLE record ADD COLUMN pdf_url TEXT;
ALTER TABLE record ADD COLUMN landing_url TEXT;
