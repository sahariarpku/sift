-- app.db schema, version 2. AI providers, per-task model assignment, prices.
-- API keys are never stored here; `api_key_ref` names the OS keychain entry.

CREATE TABLE provider (
  id TEXT PRIMARY KEY,
  preset TEXT NOT NULL,                 -- id from llm/presets.json, or 'custom'
  display_name TEXT NOT NULL,
  protocol TEXT NOT NULL CHECK (protocol IN ('openai_compat', 'anthropic')),
  base_url TEXT NOT NULL,
  api_key_ref TEXT,                     -- keychain entry name; NULL for keyless local servers
  models TEXT NOT NULL DEFAULT '[]',    -- JSON: cached ModelInfo list from the last fetch
  data_policy_warning INTEGER NOT NULL DEFAULT 0,
  concurrency INTEGER NOT NULL DEFAULT 4 CHECK (concurrency BETWEEN 1 AND 64),
  options TEXT NOT NULL DEFAULT '{}',   -- JSON: ProviderOptions
  last_test_status TEXT,                -- ok | failed | unreachable
  last_test_message TEXT,
  last_test_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE task_assignment (
  task TEXT PRIMARY KEY,                -- screener_a | screener_b | arbiter | extraction | appraisal | protocol_search | synthesis
  provider_id TEXT NOT NULL REFERENCES provider(id) ON DELETE CASCADE,
  model_id TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE model_price (
  provider_id TEXT NOT NULL REFERENCES provider(id) ON DELETE CASCADE,
  model_id TEXT NOT NULL,
  input_per_mtok REAL NOT NULL CHECK (input_per_mtok >= 0),
  output_per_mtok REAL NOT NULL CHECK (output_per_mtok >= 0),
  PRIMARY KEY (provider_id, model_id)
);
