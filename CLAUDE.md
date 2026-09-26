# Sift: automated PRISMA systematic review desktop app

This file is the build brief for Claude Code. Read it fully before writing any code. Work through the phases in Section 12 in order, and stop for review at the end of each phase.

"Sift" is a working name. Keep it in one constant (`APP_NAME`) so it can change later.

---

## 1. What we are building

A local-first desktop app (macOS first, then Windows and Linux) that turns one research question into a complete, PRISMA 2020 compliant systematic review. It runs end to end on autopilot, pauses at three optional human gates, and discloses every automated decision in the report it produces.

Users bring their own API keys for AI providers and bibliographic databases. There is no backend server. All review data stays on the user's machine.

Target users: academic researchers in education and the social sciences. Version 1 covers quantitative, qualitative and mixed-methods reviews. Clinical (Cochrane-standard) reviews are out of scope.

---

## 2. Non-negotiable principles

These override convenience. If a design choice conflicts with one of them, stop and ask.

1. **The event log is the source of truth.** Every decision (screening, extraction, appraisal, protocol change) is an append-only row in `event`. Counts, the PRISMA flow diagram and the checklist are computed from the log. Never store a count that can be derived.
2. **Records, reports and studies are separate entities.** A record is one citation from one search. A report is one retrieved full text. A study is the unit of synthesis and may have several reports. PRISMA 2020 counts them separately.
3. **No claim without a source.** Every screening decision carries a rationale and a criterion code. Every extracted value carries a verbatim quote and a page number. A quote that cannot be found verbatim in the source text voids the value.
4. **Automation is disclosed.** Every event records whether an agent or a human made it, plus provider, model ID and prompt version.
5. **The LLM never does arithmetic.** Effect sizes, meta-analysis and agreement statistics are computed in code (R via WebR, or Rust).
6. **Reproducible runs.** Protocol, search strings, prompts, model IDs, temperature and raw API responses are stored per run.
7. **Keys never touch the frontend.** API keys live in the OS keychain and are read only by the Rust core. The webview never sees a key.
8. **Verification status is visible.** A review is "Unverified" until all three human gates pass. Every export carries its status.

---

## 3. Tech stack

| Layer | Choice | Notes |
| --- | --- | --- |
| Shell | Tauri 2 | Rust core, web frontend |
| Frontend | React 18 + TypeScript + Vite | Strict TypeScript |
| Styling | CSS modules or vanilla-extract with the tokens in Section 10 | No Tailwind defaults; match the design |
| State | TanStack Query for data from Rust commands; Zustand for UI state | |
| Database | SQLite via `rusqlite` (bundled feature) in the Rust core | One `.sift` file (SQLite) per review, plus an app-level `app.db` for settings |
| Migrations | `refinery` or hand-rolled versioned SQL files | |
| Secrets | `keyring` crate (macOS Keychain, Windows Credential Manager, Secret Service on Linux) | |
| HTTP | `reqwest` with rustls | All outbound calls from Rust |
| Jobs | Persistent job queue in SQLite, worker pool on `tokio` | Must resume after sleep, crash or restart |
| Fuzzy matching | `strsim` | Deduplication |
| PDF text | pdf.js in the frontend, text per page sent to Rust for storage | GROBID is a later optional add-on |
| Statistics | WebR (R in WebAssembly) running `metafor` | Runs in the webview; inputs come from Rust commands |
| Document export | `docx` npm package for DOCX; SVG for the flow diagram | |
| Tests | `cargo test`, Vitest, Playwright for key UI flows | |

WebR note: it runs best with cross-origin isolation (SharedArrayBuffer). Configure Tauri to send `Cross-Origin-Opener-Policy: same-origin` and `Cross-Origin-Embedder-Policy: require-corp`, or use WebR's PostMessage channel as a fallback. Verify with the current WebR docs.

---

## 4. Repository layout

```
sift/
  CLAUDE.md
  design/                  screenshots of the 7 designed screens (provided by the user)
  prompts/                 versioned prompt files, e.g. screening_a.v1.md
  src-tauri/
    src/
      main.rs
      commands/            Tauri commands exposed to the frontend
      db/                  schema, migrations, queries
      events/              event log writer and derived views
      jobs/                queue, worker, scheduler
      llm/                 provider trait, openai_compat.rs, anthropic.rs
      sources/             database adapters: openalex, eric, wos, scopus, pubmed, core, semantic_scholar, unpaywall
      query/               search AST and per-database renderers
      agents/              protocol, search, screening, retrieval, extraction, appraisal, synthesis, reporting
      dedup/
      secrets/             keychain access
    tests/fixtures/        recorded API responses
  src/
    app/                   routes and layout
    screens/               Start, Protocol, Overview, Calibration, Extraction, Report, Settings
    components/
    theme/                 tokens from Section 10
    stats/                 WebR integration
    lib/
```

---

## 5. Data model (SQLite)

Use TEXT UUIDs (v7, time-ordered) as primary keys, ISO 8601 UTC timestamps, and JSON stored as TEXT validated with serde.

```sql
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
```

Rules:
- Nothing ever UPDATEs or DELETEs an `event` row. Corrections are new events with `supersedes` set.
- Current state of a target at a stage = its latest event not superseded by another.
- Write a SQL view per PRISMA box (`v_prisma_counts`) and unit-test it against fixture logs.

---

## 6. AI provider layer

### 6.1 Provider model

A provider is: `id`, `display_name`, `protocol` (`openai_compat` | `anthropic`), `base_url`, `api_key_ref` (keychain entry name, optional for local models), `models` (cached list), `data_policy_warning` (bool).

Implement a Rust trait:

```rust
#[async_trait]
pub trait LlmProvider {
    async fn list_models(&self) -> Result<Vec<ModelInfo>>;
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse>;
    async fn test(&self) -> Result<TestResult>;   // one-token request
}
```

`CompletionRequest` carries: system prompt, messages, optional JSON schema, temperature (default 0), max tokens.
`CompletionResponse` carries: text, parsed JSON if requested, the model ID actually reported by the response (or any gateway header that reports it), token usage, raw body.

### 6.2 Protocols

- **OpenAI-compatible:** `POST {base_url}/chat/completions`, `GET {base_url}/models`, header `Authorization: Bearer {key}` when a key is set. Use `response_format` with a JSON schema when the model supports it; otherwise instruct JSON in the prompt and validate.
- **Anthropic native:** `POST https://api.anthropic.com/v1/messages`, headers `x-api-key` and `anthropic-version`. Use tool use or structured output for JSON.

Verify every endpoint and header against current provider documentation before hardcoding.

### 6.3 Presets

Ship presets for: Anthropic, OpenAI, OpenRouter, Google Gemini (OpenAI-compatible endpoint), Groq, Mistral, Ollama (`http://localhost:11434/v1`, no key), LM Studio (`http://localhost:1234/v1`, no key), and Custom. Each preset has a "Get key" URL. Store presets in `src-tauri/src/llm/presets.json` so they can be updated without code changes. A self-hosted FreeLLM gateway (github.com/Devansh-365/freellm) works as a Custom provider.

### 6.4 Rules

- **Per-task assignment:** Screener A, Screener B, Arbiter, Extraction, Appraisal, Protocol/Search, Synthesis each map to one provider + model. Recommend different providers for Screener A and B.
- **Strict mode, on by default:** never substitute a model. If the configured model fails after retries, pause the job and surface the error. When a response reports a different model ID than requested, log it and warn.
- **Retries:** exponential backoff on 429 and 5xx, honour `Retry-After`. Two invalid JSON responses for one item send it to the human queue.
- **Concurrency:** per-provider limit, user-configurable, default 4.
- **Cost:** estimate cost from token usage and a user-editable price table; store per event; pause the review at the spending cap.

---

## 7. Bibliographic source adapters

Each adapter implements:

```rust
#[async_trait]
pub trait SourceAdapter {
    fn render(&self, ast: &QueryAst) -> Rendered;          // string + list of simplifications
    async fn count(&self, q: &Rendered) -> Result<u64>;
    async fn fetch_page(&self, q: &Rendered, cursor: Option<String>) -> Result<Page>;
    fn normalise(&self, raw: &serde_json::Value) -> Vec<NormalisedRecord>;
}
```

Priority order for implementation: OpenAlex, ERIC, Unpaywall, CORE, then Web of Science Starter, PubMed, Semantic Scholar, Scopus.

| Source | Auth | Notes |
| --- | --- | --- |
| OpenAlex | Free API key (required since Feb 2026) | Backbone; cursor paging; citation chasing via `referenced_works` and `cites:` filter |
| ERIC | None | api.ies.ed.gov/eric; supports field searches and ERIC Thesaurus descriptors |
| Web of Science Starter | `X-ApiKey` header | Free plan is heavily rate-limited; institutional keys unlock more |
| Scopus | API key + optional institution token | Access often depends on institutional network |
| PubMed | Optional key | E-utilities: esearch then efetch |
| Semantic Scholar | Optional key | Abstract gap-filling |
| CORE | Free key | Open-access full texts |
| Unpaywall | Email parameter | OA PDF lookup by DOI |

Verify each API's base URL, parameters, paging and rate limits from current documentation and record them in a comment at the top of each adapter. Record real responses as fixtures in `tests/fixtures/` and write adapter tests against them. Never hit live APIs in tests.

### Query AST

```
Query = And(Vec<Concept>)
Concept = Or(Vec<Term>)
Term = { text, kind: Phrase | Word | Truncated, fields: [TitleAbstract | Subject], controlled_vocab: Option<(Vocab, Descriptor)> }
Filters = { years, languages, doc_types }
```

Each adapter renders the AST into its syntax and reports anything it had to drop or approximate. Simplifications are logged in `search_run.simplifications` and disclosed in the report.

---

## 8. Agent pipeline

The orchestrator is a state machine over stages. Each stage enqueues jobs; workers write events; the orchestrator advances when a stage's completion condition holds, and pauses at gates.

```
protocol -> [Gate 1] -> search -> dedup -> ta_screen -> [Gate 2] -> retrieval
-> ft_screen -> citation_chasing (feeds back to ta_screen) -> link -> extract
-> appraise -> synth -> report -> [Gate 3]
```

Every agent has a fixed JSON output schema. Store prompts in `prompts/` as markdown with a version suffix; the prompt version is logged on every event.

### 8.1 Protocol agent
Classify question type and choose framework:

| Question type | Framing | Searching |
| --- | --- | --- |
| Intervention effect | PICO | PICO |
| Exposure association | PEO | PEO |
| Lived experience | SPIDER | PICo (SPIDER alone loses sensitivity) |
| Policy or service evaluation | SPICE | PICO |
| Mixed | Split into sub-questions | Per sub-question |

Output JSON: `question_type, framework, concepts[{name, synonyms[], controlled_vocab[]}], inclusion_criteria[{code, text}], exclusion_criteria[{code, text}], date_range, languages[], study_designs[], planned_synthesis, open_questions[]`. Criterion codes look like `IN-POP`, `EX-DESIGN`. Every criterion must be testable from title/abstract or full text.

### 8.2 Search agent
1. Build the query AST from protocol concepts.
2. Build a seed set of 5 to 10 clearly relevant papers via a narrow OpenAlex search, confirmed by both screeners.
3. Run all sources. If any seed paper is missing from the combined results, broaden the concepts and rerun, up to 3 iterations, logging each.

### 8.3 Deduplication
Exact DOI match first; then normalised title similarity (Jaro-Winkler ≥ 0.95) plus year within 1 plus first-author surname match. Borderline pairs (0.90 to 0.95) go to a review list. Each merge is a `dedup` event.

### 8.4 Screening (title/abstract and full text)
Two independent screeners per record.

Screener A prompt (v1):
```
You are screening records for a systematic review.
Apply ONLY the criteria below. For each criterion, state whether the
record meets it, fails it, or cannot be judged from this text.
Decision rules at title/abstract stage:
- exclude only if a criterion is clearly failed; cite the criterion code
- if any criterion cannot be judged, decide "uncertain"
- otherwise include
Return JSON: decision, reason_code, rationale (max 40 words),
evidence_quote (verbatim from the record), confidence (0 to 1).
```
Screener B uses the same criteria but must argue for inclusion before deciding.

Resolution: both include -> include; both exclude -> exclude; any disagreement or "uncertain" -> Arbiter sees both rationales; if still uncertain -> human queue.

Full-text stage: same pattern on page texts, exclusion reason code mandatory.

### 8.5 Retrieval
For each included record: Unpaywall, then CORE, then OpenAlex OA locations, then library proxy prefix + DOI URL (only if the user configured one). Otherwise add to the upload queue; after the user-set deadline mark `not_retrieved`. Respect robots and publisher terms: no scraping beyond following the DOI link through the user's own proxy session.

### 8.6 Citation chasing
Backward and forward citations of included studies via OpenAlex. New records enter title/abstract screening as a separate `search_run` with source `citation_chasing`.

### 8.7 Report-to-study linking
Propose links using trial or study registration IDs, sample descriptions, and author overlap. Proposed links are events and are shown for confirmation at Gate 3.

### 8.8 Extraction
Fields are generated from the protocol. Prompt (v1):
```
Extract the fields below from the full text provided.
For every field return: value, source_quote (verbatim), page.
If the text does not state the value, return null. Never infer,
calculate or estimate. Numeric effect data must be copied exactly as
reported, with its statistic type (mean, SD, n, t, F, OR, r).
```
Validator: normalise whitespace and hyphenation, then require `source_quote` to appear in `page_texts[page]` (allow the adjacent page for quotes crossing a page break). Failure sets `value = NULL`, `validation = 'quote_not_found'`, and flags the field.

### 8.9 Appraisal
Tool by design: RoB 2 (randomised trials), ROBINS-I (non-randomised interventions), JBI (cross-sectional), CASP (qualitative), MMAT (mixed methods). Each domain judgement needs a rationale and a quote. Two passes, reconciled like screening.

### 8.10 Synthesis
- 3 or more studies with the same outcome construct and usable effect data: random-effects meta-analysis in `metafor` (REML, Hartung-Knapp), I² and τ², leave-one-out; funnel plot and Egger's test only with 10 or more studies.
- Otherwise: narrative synthesis following SWiM (grouping; vote counting by direction of effect, never by significance).
- Qualitative: thematic synthesis (Thomas and Harden): line-by-line codes, descriptive themes, analytical themes, each linked to source quotes.
- Certainty: GRADE for quantitative outcomes, GRADE-CERQual for qualitative findings. The LLM drafts judgements; the user confirms at Gate 3.

### 8.11 Reporting
Generate: PRISMA 2020 flow diagram (SVG, from `v_prisma_counts`), the 27-item checklist with status per item, a draft manuscript (DOCX), an automation disclosure (all agents, providers, model IDs, prompt versions, Gate 2 metrics, search simplifications), RIS of included studies, CSV of all events.

---

## 9. Human gates and quality metrics

| Gate | User task | Pass condition |
| --- | --- | --- |
| 1 Protocol | Review and edit framework, criteria, search strings | Protocol locked (creates version 1) |
| 2 Calibration | Screen 100 records blind: 40 agent-included, 40 agent-excluded, 20 uncertain (random within strata) | Estimated recall ≥ 95% and Cohen's κ ≥ 0.6 vs human |
| 3 Sign-off | Resolve flagged extractions, confirm links and certainty judgements, write items 3, 25, 26 | All flags resolved |

- Recall estimate must account for the stratified sample (weight strata by their population share). Report a 95% confidence interval (Wilson or bootstrap).
- If Gate 2 fails, the pipeline does not continue. Offer: revise prompts and rerun screening, or hand screening to the user.
- Skipping a gate is allowed but sets `verification = 'unverified'` permanently for that review.
- Continuous checks shown on the Overview screen: seed recall, screener A vs B κ, quote validation rate (flag below 98%), drift check (rescreen 5% with a fresh seed; alert above 10% divergence).

---

## 10. UI

The `design/` folder holds screenshots of seven screens. Match them closely.

1. **Start:** research question textarea, years, languages, review type, database chips, "Draft protocol".
2. **Gate 1, Protocol:** framework card, eligibility criteria with codes, search strings per database (monospace), seed-set check, "Lock protocol and run search".
3. **Overview:** live PRISMA flow diagram, quality metrics, decision log feed, pause/resume.
4. **Gate 2, Calibration:** one record at a time, Include (I) / Exclude (E) / Unsure (U) keyboard shortcuts, exclusion reason dropdown, criteria panel, progress bar. Agent decisions stay hidden until all 100 are done.
5. **Extraction:** PDF on the left with the source quote highlighted; fields on the right with value, quote, page and validation status; flagged fields highlighted.
6. **Gate 3, Report:** checklist summary (auto / drafted / needs you), automation disclosure preview, exports, sign-off (disabled until requirements met).
7. **Settings:** AI providers (list, add form with preset, name, base URL, key, Fetch models, Test connection), model per task, strict mode and data-policy toggles, database keys each with a "Get key" link and status, Unpaywall email, library proxy prefix, spending cap.

Left sidebar on review screens: app name, current review, stage list with status dots (done filled teal, running teal ring, pending grey), verification badge at the bottom.

### Design tokens

```ts
export const color = {
  ground: '#F6F4EE', sidebar: '#EFECE3', surface: '#FFFFFF', surfaceMuted: '#F4F2EC',
  line: '#DDD8CC', lineStrong: '#C8C2B4',
  ink: '#1C1B19', inkBody: '#2A2824', inkSecondary: '#3D3A34', inkMuted: '#5E5A52',
  accent: '#1F5F5B', accentDeep: '#14403D', accentSoft: '#E3EEEC', accentBorder: '#B9D3CF',
  exclude: '#8A4A12',
  warnText: '#6B4510', warnSoft: '#F7EBD5', warnBorder: '#C9A56A',
  highlight: '#FCE9C8', pending: '#BDB7A8',
};
export const font = {
  display: "'Source Serif 4', Georgia, serif",   // headings, abstracts, reading text
  ui: "'IBM Plex Sans', system-ui, sans-serif",  // interface
  mono: "'IBM Plex Mono', monospace",            // search strings, criterion codes, times
};
export const radius = { sm: 8, md: 10, lg: 12 };
```

Bundle the fonts locally (all three are under the SIL Open Font License); do not load from Google at runtime. Support dark mode later; build light first.

Accessibility: real buttons, links and labelled inputs; visible focus rings; text contrast at least 4.5:1; all keyboard shortcuts also available as buttons; include/exclude distinguished by label and lightness, not colour alone.

---

## 11. Security and privacy

- Keys only in the OS keychain, read only in Rust, never logged, never sent to the frontend (the frontend sees "key ending 4Kf2" at most).
- Tauri capabilities: allow only the commands the frontend needs; no shell access; restrict HTTP to Rust.
- Local provider URLs (`localhost`) allowed; warn if a non-HTTPS remote URL is entered.
- Before sending full texts to a provider flagged with a data-policy warning, show a one-time confirmation per review.
- No telemetry in version 1.

---

## 12. Build phases

Work through these in order. At the end of each phase: all tests pass, update `PROGRESS.md` with what was built and any open questions, then stop and wait for review.

**Phase 0: Scaffold**
Tauri 2 + React + TypeScript app, theme tokens, fonts, app shell with sidebar, SQLite with migrations, job queue skeleton, keychain module.
Done when: the app launches on macOS, creates a review file, and a job survives an app restart.

**Phase 1: Settings and providers**
Settings screen; provider CRUD with presets; OpenAI-compatible and Anthropic adapters; Fetch models; Test connection; per-task model assignment; strict mode; source key entry with Get key links and Test buttons.
Done when: a user can add Anthropic, OpenRouter and a local Ollama provider and get a successful test response from each (use mocked HTTP in tests).

**Phase 2: Protocol and Gate 1**
Start screen, protocol agent, protocol editing, locking and versioning.
Done when: a question produces a valid protocol JSON, the user can edit and lock it, and a `lock` event exists.

**Phase 3: Search and dedup**
Query AST, OpenAlex and ERIC adapters, seed-set loop, RIS/BibTeX/NBIB import for manual sources, deduplication.
Done when: fixture-based tests pass for both adapters, seed-set loop works, and dedup counts appear in `v_prisma_counts`.

**Phase 4: Screening and Gate 2**
Dual screening, arbiter, human queue, Calibration screen with keyboard shortcuts, recall and κ with CIs, Overview screen with live flow diagram and decision log.
Done when: a run on a SYNERGY benchmark dataset (public, with human labels) reports recall, and the flow diagram matches the event log.

**Phase 5: Full text**
Retrieval chain, library proxy, upload queue, pdf.js page extraction, full-text screening, citation chasing, report-to-study linking.
Done when: included records resolve to reports or `not_retrieved`, and linked studies count correctly.

**Phase 6: Extraction and appraisal**
Extraction form generation, extraction agent, quote validator, Extraction screen, appraisal tools.
Done when: quote validation tests pass, including hyphenation and page-break cases, and flagged fields show in the UI.

**Phase 7: Synthesis**
WebR with `metafor`, effect-size computation from extracted statistics, forest and funnel plots, SWiM tables, thematic synthesis, GRADE and GRADE-CERQual drafts.
Done when: meta-analysis results match `metafor` run in desktop R on a fixture dataset to 4 decimal places.

**Phase 8: Reporting and Gate 3**
Checklist computation, automation disclosure, DOCX manuscript, SVG flow diagram, RIS and CSV exports, Report screen, sign-off.
Done when: a full fixture review exports all files and the checklist shows 18 auto, 6 drafted, 3 human items.

**Phase 9: More sources and release**
Web of Science Starter, PubMed, Semantic Scholar, CORE, Scopus adapters; code signing and notarisation; auto-update; Windows build.

---

## 13. Working conventions for Claude Code

- Before each phase, write a short plan in `PROGRESS.md` and follow it.
- Ask before adding a dependency not listed in Section 3.
- Never invent API behaviour. When unsure about an external API, check its documentation, record the source URL in a code comment, and build a fixture from a real response.
- Keep prompts in `prompts/`, never inline in code. Changing a prompt means a new version file.
- Every agent output is validated against its schema (use `schemars` + `jsonschema` in Rust).
- Tests for every derived count, validator and statistic. No test calls a live API or a live LLM.
- Plain, specific UI copy. No emoji in the interface.
- Rust: `clippy` clean, `thiserror` for errors, no `unwrap()` outside tests. TypeScript: strict mode, no `any`.

---

## 14. Out of scope for version 1

Cloud sync, multi-user collaboration, clinical trial registries, non-English full-text extraction, living-review scheduling, and a mobile app.
