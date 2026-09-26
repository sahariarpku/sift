# Progress

## Phase 0: Scaffold

### Plan

Goal (from CLAUDE.md §12): the app launches on macOS, creates a review file, and a job survives an app restart.

1. **Toolchain.** Rust stable (installed via Homebrew `rustup`), Node 22, npm.
2. **Frontend scaffold** (`/src`, Vite + React 18 + strict TypeScript).
   - `src/theme/tokens.ts`: the §10 tokens verbatim, mirrored as CSS custom properties in `src/theme/global.css`.
   - Fonts bundled locally from `@fontsource` packages (Source Serif 4, IBM Plex Sans, IBM Plex Mono; all SIL OFL). No runtime Google Fonts.
   - CSS modules for component styles.
   - TanStack Query for Rust command data, Zustand for UI state (current screen, current review).
   - App shell: left sidebar (app name, current review, stage list with done/running/pending dots, verification badge) per the Overview design; a Start screen with a "Create review" action that calls Rust; placeholder screens for the other routes.
   - `APP_NAME` constant in `src/lib/app.ts` (and `APP_NAME` in Rust).
3. **Rust core** (`/src-tauri`, Tauri 2).
   - `db/`: hand-rolled versioned migrations (`migrations/review/0001_init.sql`, `migrations/app/0001_init.sql`) tracked with `PRAGMA user_version`. The review schema is §5 verbatim. WAL mode, foreign keys on.
   - Review file: one `.sift` SQLite file per review under `<app data>/reviews/`. `app.db` holds settings and the list of known review files.
   - `events/`: append-only event writer (insert only; no update/delete API). Triggers in SQL reject UPDATE and DELETE on `event`.
   - `jobs/`: persistent queue in the review file's `job` table. Enqueue, claim (atomic `queued -> running`), complete, fail with backoff counter, and `recover()` on startup that returns interrupted `running` jobs to `queued`. A tokio worker loop with a handler registry; Phase 0 ships a single `noop` kind for testing.
   - `secrets/`: `SecretStore` trait with a `keyring` implementation (macOS Keychain, Windows Credential Manager, Secret Service) and an in-memory implementation for tests. Only a masked hint ("key ending 4Kf2") can leave the module toward the frontend. No secret commands exposed yet (Phase 1).
   - `commands/`: `create_review`, `list_reviews`, `open_review`, `enqueue_noop_job`, `list_jobs`. Tauri capability file allows only these commands plus core window defaults; no shell, no fs, no http plugins.
   - COOP/COEP headers and a strict CSP set in `tauri.conf.json` now, so WebR (Phase 7) is not blocked later.
   - Empty module folders for later phases (`llm/`, `sources/`, `query/`, `agents/`, `dedup/`) are not created yet; they arrive with their phase.
4. **Tests.**
   - Rust: migrations apply and are idempotent; schema matches §5 tables; event rows cannot be updated or deleted; job enqueue/claim/complete; **job survives restart** (enqueue, claim, drop connection mid-run, reopen, recover, worker completes it); secret masking; in-memory secret store round trip.
   - Frontend (Vitest): tokens match §10; sidebar renders stage dots and verification badge.
   - `cargo clippy -- -D warnings`, `tsc --noEmit`.
5. **Manual check.** `npm run tauri dev` launches the window on macOS; "Create review" writes a `.sift` file.

### Dependencies added beyond §3

These are implied by the brief but not named in the §3 table. Flagging for review:

- `uuid` (v7 feature): §5 requires UUID v7 keys.
- `thiserror`: required by §13.
- `tempfile` (dev only): temp directories for DB tests.
- `@fontsource/*`: the packaged OFL font files, bundled at build time.
- `jsdom`, `@testing-library/react` (dev only): Vitest DOM tests.

Timestamps are produced in Rust with the standard library (no `chrono`), formatted as ISO 8601 UTC.

### Status: complete, awaiting review (2026-09-24)

**Exit criteria, checked in the real app** (debug `.app` bundle, macOS 27, Apple silicon):
- Launches, renders the Start screen with the bundled fonts.
- "Create review" wrote `~/Library/Application Support/org.sift.desktop/reviews/<uuid-v7>.sift` and opened the review shell with the sidebar.
- Queued a 20-second job, killed the process with SIGKILL while it was `running`, relaunched: the job was requeued at startup (attempt 2) and finished `done`.

**Tests:** 23 Rust tests pass (19 unit, 4 integration in `tests/job_restart.rs`), 1 ignored (real-keychain round trip, opt-in with `cargo test -- --ignored`). 6 Vitest tests pass. `cargo clippy --all-targets -D warnings` clean. `tsc --noEmit` clean.

**Built**
- `src-tauri/src/db`: migrations via `PRAGMA user_version`; refuses a file from a newer app. WAL, foreign keys on.
- `src-tauri/migrations/review/0001_init.sql`: §5 schema verbatim, plus triggers that make `event` append-only at the database level and an index on `job(status, id)`.
- `src-tauri/src/events`: `append` and `current` (latest non-superseded event per target and stage).
- `src-tauri/src/reviews.rs`: create (writes to a temp name, then renames, so a half-made file is never registered), list, path lookup.
- `src-tauri/src/jobs`: queue (enqueue, atomic claim, complete, fail with retry up to 3, pause, recover) and a tokio worker. Unknown job kinds are paused, not dropped. A panicking handler fails its job without killing the worker.
- `src-tauri/src/secrets`: `SecretStore` trait, keychain and in-memory implementations, masked hint ("key ending 4Kf2", nothing shown for keys under 12 characters).
- Capabilities: the window may call only the 4 app commands plus core app/event/window defaults. No shell, fs or http plugins.
- `tauri.conf.json`: strict CSP, COOP/COEP headers for WebR (also set on the Vite dev server).
- Frontend: tokens and CSS variables (a test keeps them in sync), fonts bundled from `@fontsource`, sidebar with status in text as well as colour, Start screen, review shell, Settings placeholder. Overview temporarily shows a job-queue panel for the restart check; Phase 4 replaces it.

**Deviations and open questions**
1. **Toolchain versions.** Pinned to current stable majors: TypeScript 5, Vite 6, Vitest 3, React 18 (as specified). Newer majors exist (TS 7, Vite 8, Vitest 5); I avoided them to reduce risk. Rust 1.98 was installed through Homebrew `rustup` because no Rust was present.
2. **`keyring` 3.x, not 4.x.** 4.x restructured the crate into pluggable stores; 3.6 has the stable `Entry` API. Worth revisiting before release.
3. **No retry delay in the job queue.** §5's `job` table has no `run_after` column, so a retryable failure is requeued immediately. HTTP backoff (429, 5xx, `Retry-After`) belongs in the provider layer per §6.4, so this may be fine. If you want job-level backoff, I'd add `run_after TEXT` in migration 0002. Your call.
4. **Single instance.** Two copies of the app open on the same review would both run workers, and startup recovery in one could requeue a job the other is running. `tauri-plugin-single-instance` fixes this but is a dependency not in §3. Approve adding it?
5. **Start screen button** says "Create review" rather than "Draft protocol" until the protocol agent exists (Phase 2). The design's database chips, year/language/review-type fields and the top-bar links are also left for Phase 2.
6. **Pending sidebar labels** use `inkMuted` rather than the lighter grey in the design, to keep text contrast above 4.5:1.
7. **Not a git repository yet.** I didn't run `git init`. Say if you want it.

---

## Phase 1: Settings and providers

### Plan

Goal (§12): a user can add Anthropic, OpenRouter and a local Ollama provider and get a successful test response from each (mocked HTTP in tests).

1. **Storage** (`app.db` migration 0002): `provider` (preset, name, protocol, base URL, keychain ref, cached models, data-policy flag, concurrency, options, last test result), `task_assignment` (task -> provider + model), `model_price` (user-editable price per million tokens). App settings (strict mode, data-policy warnings, Unpaywall email, library proxy prefix, spending cap) in the existing `setting` table.
2. **LLM layer** (`src-tauri/src/llm/`):
   - `LlmProvider` trait as in §6.1: `list_models`, `complete`, `test`.
   - `openai_compat.rs` and `anthropic.rs` on `reqwest` (rustls). JSON output via `response_format: json_schema` (OpenAI-compatible) or `output_config.format` (Anthropic). If a server rejects structured output or `temperature` with a 400, retry once without it and record that it was not applied, so the run stays honestly documented.
   - Retries: exponential backoff on 408/429/5xx/529 and connection errors, honouring `Retry-After`.
   - Every response keeps the reported model ID, the gateway route header when present (`X-Routed-Via`), token usage, and the raw body.
   - Model check: exact / dated snapshot / different. A different model is logged and returned as a warning (§6.4).
   - `LlmService`: task -> provider + model, per-provider concurrency limit (default 4), JSON schema validation with one retry; two invalid JSON responses return an error that later phases route to the human queue. Cost from the price table.
   - `presets.json` for the 9 presets with Get key URLs.
3. **Sources** (`src-tauri/src/sources/`): catalogue of the 8 databases (auth kind, Get key URL, docs URL) and a minimal authenticated test request per source. The full adapters come in Phase 3.
4. **Secrets**: provider keys at `provider:<id>`, source keys at `source:<id>`. The frontend sends a key once when saving and only ever gets back "key ending xxxx".
5. **Commands** (each added to the capability file): providers (list, preview models, test, save, delete), tasks, prices, settings, sources (list, set key, delete key, test), review spend, open a Get key page (Rust opens only URLs in the presets or sources catalogue).
6. **Settings screen** matching `design/…page-0004.jpg`: settings nav; AI providers list with status and Edit; add/edit form (preset, name, base URL, key, Fetch models, Test connection, Save); model for each of the 7 tasks; strict mode and data-policy toggles; databases with Get key, Add, Test; Unpaywall email and library proxy; spending cap. Warn on non-HTTPS remote URLs.
7. **Tests**: a small in-process mock HTTP server (tokio, no new dependency) serves canned responses. Covers both adapters (success, JSON mode, fallbacks, retries with `Retry-After`, auth errors, model mismatch), the Anthropic + OpenRouter + Ollama add-and-test flow, source key tests, cost, settings, and the Settings UI (Vitest).

### Dependencies added (both named in CLAUDE.md)
- `reqwest` (§3), rustls TLS.
- `jsonschema` (§13) for validating agent JSON. `schemars` waits until Phase 2, when agent output types exist.

### Status: complete, awaiting review (2026-09-24)

**Exit criterion.** `providers::tests::anthropic_openrouter_and_ollama_can_be_added_and_tested` adds the three providers through the same code the Settings screen uses, fetches their models, runs Test connection and gets "Connected" for each, all against mocked HTTP. It also checks that each key reached only its own server and that no key appears in the summaries sent to the frontend.

**Tests.** 86 Rust tests pass (82 unit, 4 integration), plus 3 opt-in tests that stay ignored by default (real Keychain, live gateway, live databases). 14 Vitest tests pass. `clippy --all-targets -D warnings`, `rustfmt` and `tsc --noEmit` are clean.

**Live checks (opt-in, run by hand on 2026-09-24).**
- Local FreeLLM gateway (`cargo test --test live_gateway -- --ignored`): model list returned 253 models. Test connection passed and flagged that `auto` was answered by `openrouter/dots-studio/dots-3-note-preview:free`. The schema-bound JSON request returned valid JSON, with the route recorded from `X-Routed-Via`.
- Databases (`cargo test --test live_sources -- --ignored`): ERIC and PubMed reachable. Semantic Scholar's keyless pool returned 429 (reported as rate limited). OpenAlex, Web of Science, Scopus, CORE and Unpaywall were skipped: no key or email was provided.
- Settings layout checked in a browser with mocked commands (`npm run dev`, then `/preview.html`) against `design/…page-0004.jpg`. Screen capture of the native window was unavailable during this session.

**Built**
- `llm/`: provider trait; OpenAI-compatible and Anthropic adapters; retries (408/429/5xx/529, `Retry-After`, no sleeping through waits over 60 s); structured-output and temperature fallbacks that record what was actually applied; model-match check (exact / snapshot / different); JSON parse and schema validation; presets file; cost estimates; `LlmService` (task routing, per-provider limit, one JSON retry then `needs_human`).
- `providers.rs`: create, edit, delete, fetch models, test, "Not running" for local servers, plain-http warning for remote hosts.
- `sources/`: catalogue of the 8 sources with Get key and docs URLs, and one authenticated test request each (auth scheme per the docs cited at the top of `keytest.rs`).
- `settings.rs`: strict mode, data-policy warnings, Unpaywall email, library proxy, spending cap (default $50), with validation.
- `app.db` migration 0002: `provider`, `task_assignment`, `model_price`.
- 18 new commands, each listed in `build.rs` and the capability file. `open_external` opens only URLs from the presets and sources catalogue.
- Settings screen: providers list and add/edit form, 7 task selectors, toggles, databases, full-text access, spending with a price table.
- Fixtures in `tests/fixtures/llm/`, provenance in its README. The gateway fixtures are real recorded responses; the Anthropic and Ollama fixtures follow the documented shapes until keys are available.

**Security notes**
- Keys: frontend to Rust once when saving; never returned. `ProviderConn` and `ProviderDraft` redact keys in `Debug`. Network errors drop the URL, because PubMed carries its key in the query string.
- The design's sidebar note said keys "never leave this computer". That is not accurate, since each key must be sent to its own service. The copy now reads "Each key is sent only to the service it belongs to."

**Deviations and open questions**
1. **`test()` sends up to 16 tokens, not 1.** Some reasoning models reject or return nothing with a 1-token cap.
2. **Strict mode has nothing to switch off yet.** v1 has no fallback models, so every task shows "Fallback: none" either way. The toggle is stored for when fallbacks exist. Should strict mode also *fail*, rather than warn, when a different model answers? Right now it warns (§6.4 says "log it and warn").
3. **Data-policy defaults.** The preset flag is on for OpenRouter, Google Gemini, Mistral and Custom, and off for the others. These reflect free-tier training policies as I understand them; please confirm.
4. **Unverified Get key URLs.** Some were not confirmed on an official page: OpenAI, OpenRouter, the PubMed account settings page, Semantic Scholar and Scopus. Anthropic, Gemini, Groq, Mistral, OpenAlex and CORE were confirmed.
5. **Unpaywall email is sent in the query string.** Unpaywall's API requires it there.
6. **For Phase 4.** In the live run, the gateway's model put `confidence: 95` where 0 to 1 was meant. Screening schemas must set `minimum`/`maximum`.
7. **Dev preview page.** `preview.html` + `src/dev/preview.tsx` are dev-only and not in the production build (Vite builds `index.html` only). Keep or remove?
8. **Earlier Phase 0 questions (single-instance plugin, git) are still open.**

## Phases 2–8: protocol through report (built together, 2026-09-24/25)

You asked for the app to be ready to run a whole review, so I built Phases 2–8 in one pass instead of stopping after each phase. Please review them as a block.

### Status: built, all tests pass; a live run is in progress

**Tests.** 138 Rust lib tests plus 4 integration tests pass, `clippy -D warnings` is clean, and so is `tsc`. `pipeline/e2e_test.rs` runs a full mocked review, from question through protocol, search, dedup, dual screening, Gate 2, retrieval, full text, extraction, appraisal, synthesis and Gate 3, to a verified sign-off with every export. The checklist comes out 18 auto / 6 drafted / 3 author.

**Built**
- Phase 2: Start screen, protocol agent (`prompts/protocol.v2.md`), editable Gate 1, locking with a `gate1` pass/skip event.
- Phase 3: query AST and renderers for ERIC, OpenAlex, CORE and Semantic Scholar, with fixtures recorded from real responses. Also the seed-set loop (up to 3 broadening iterations), RIS/BibTeX/NBIB import, dedup (exact DOI, then Jaro-Winkler with a 0.90–0.95 review list), and `v_prisma_counts`.
- Phase 4: Screener A and B plus the arbiter, the human queue, Gate 2 calibration (a blind 40/40/20 sample, stratified-weighted recall with a bootstrap CI, κ with a Wilson CI), the drift check, and an Overview with a live SVG flow diagram and decision log.
- Phase 5: a retrieval chain (record PDF, then Unpaywall, CORE, OpenAlex OA, then the library proxy), an upload queue, `not_retrieved`, and pdf.js page text in the webview. Also full-text screening, citation chasing via OpenAlex, and report-to-study linking.
- Phase 6: an extraction form generated from the protocol, and a quote validator that handles whitespace, hyphenation, typographic punctuation, adjacent pages and page breaks. Effect values must appear inside their quote. There is an Extraction screen, and appraisal uses CASP, RoB 2, ROBINS-I, JBI or MMAT with two passes plus reconciliation.
- Phase 7: thematic synthesis (codes, then descriptive themes, then analytical themes with quote links), GRADE-CERQual drafts, SWiM tables with vote counting done in code, and GRADE drafts.
- Phase 8: the checklist, automation disclosure, a DOCX manuscript (`docx` npm), an SVG flow diagram, RIS and CSV exports, a Report screen, and Gate 3 sign-off.

**Resilience found in live testing**
- The model put `AND` inside search terms. Such terms are now split or dropped with a disclosed note, and `protocol.v2` forbids them.
- `*` inside phrases is stripped, with a note.
- A 429, 5xx or timeout from the router now pauses the run and auto-resumes the same model after 2 minutes. There is no model substitution (strict mode). Configuration errors still stop the run.
- Any failed job halts the orchestrator with a visible Retry/Dismiss.

**Deviations and open questions**
1. **Meta-analysis is not built.** WebR with `metafor` is not built, so meta-analysis is reported as "not run". Qualitative reviews like the test question do not need it. This remains the Phase 7 exit criterion.
2. **Some sources wait for Phase 9.** PubMed, Web of Science and Scopus adapters are not built yet.
3. **OpenAlex works without a key.** It uses the keyless daily budget, and a key is recommended in Settings. This deviates from the "required" note in §7, because live calls succeeded without one.
4. **Keychain prompts on rebuilds.** Every debug rebuild changes the code signature, so macOS asks again for Keychain access. Signed release builds will not do this.
5. **Router throughput.** Many free models on the local router return 429. Working assignments today are Screener A and extraction on `gpt-oss-120b`, Screener B on `gemini-3.7-flash`, and the arbiter on `gemma-4-31b-it`.
6. **No git repository yet.** Should I run `git init` and make the first commit?
7. **Dependencies added:** `schemars`, `strsim` and `docx` (all named in CLAUDE.md), plus `pdfjs-dist` (pdf.js, named in §3).

### Additions during the live run (2026-09-25)

**Router resilience**
- HTTP retries: 6 retries, backoff from 2 s capped at 120 s, and ±50% jitter so parallel jobs don't retry in lockstep. A `Retry-After` of up to 2 minutes is waited out instead of pausing the run. The local FreeLLM router sends `Retry-After: 28–89` s during a per-model cooldown.
- A 5xx, 429 or timeout that outlasts the retries pauses the run with `rate_limited`, which auto-resumes the same model after 2 minutes. Strict mode is unchanged: no substitution.
- Local provider concurrency raised to 6. All tasks now use the router's `auto` model at your request. Every event records the model that actually answered (`X-Routed-Via`), so the disclosure names the real models.

**Lenient parsing of schema-ignoring models**
Several `auto` routes ignore `response_format`, and 110 of 236 responses were rejected. Two changes:
- `llm::coerce_to_schema` fixes unambiguous shape slips before validation, guided by the schema:
  - A `{code: verdict}` map is converted to the expected list of `{code, judgement}` objects.
  - Enum synonyms ("meets" → "met", "fails" → "failed", "cannot be judged" → "cannot_judge", "Excluded" → "exclude") are mapped to the schema's values.
  - Numbers sent as strings become numbers.
  - Anything ambiguous is still rejected ("maybe" is not guessed).
- A property whose schema description contains `(may be omitted)` is optional in local validation only. Providers are still asked for it. It is used only for the screeners' per-criterion `criteria` list, a reasoning aid that is never recorded. Decision, criterion code, rationale and quote stay required.
- Raw responses are always stored in `llm_call`.

**Optional local pre-screen: Laya-MLX** (`src-tauri/src/local/laya.rs`, `agents/prescreen.rs`, `prompts/laya_prescreen.v1.md`)
- Laya-MLX (github.com/mizorewww/laya-mlx, Apache-2.0) is a 421M-parameter typed-decision encoder on Apple Silicon. It outputs probabilities only, no text, so it never decides a record (principle 3).
- It is chosen per review on the Start screen (`ReviewOptions.prescreen`: off / safety net / speed). Settings has a status/test card and an optional Python path.
- Before title/abstract screening, a `prescreen_ta` job scores every record: P(met) for each inclusion criterion, combined by geometric mean. Each score is a `ta_prescreen` event (actor `laya`, provider `local`, model `aac6fef/laya-mlx@laya-mlx 0.2.0`, per-criterion probabilities in the rationale).
- Screening then runs in descending score order.
- **Speed:** records ranked in the lowest 30% get Screener A only. If A excludes, a resolver event says so; otherwise Screener B and the arbiter run as usual.
- **Safety net** (both modes): records ranked in the top 10% that both screeners excluded go to the arbiter.
- Both path changes are logged as `ta_prescreen` events (`single_screen`, `escalate`), counted in the disclosure, and described in the manuscript's methods (item 8) and Appendix A.
- Sift does not bundle Python. It runs the user's interpreter as a JSON-lines helper process with `HF_HUB_OFFLINE=1`. No new Rust crates were added; tokio gained its `process` feature.
- If Laya is unavailable, the job fails with a clear message and can be dismissed, and screening falls back to two screeners.
- **Trial on this review's 284 records (8 GB M1):** about 2.2 s per record. Against the AI's first 86 decisions, the per-criterion geometric mean reached AUC 0.89. Laya's own include/exclude choice reached only 0.67, which is why it is not used to decide.
- **Tests:** scoring, rank routing (DB-backed), question building, the helper protocol with a stub, and an opt-in live test (`cargo test --test live_laya -- --ignored`).

**Open questions**
1. Speed and safety-net fractions are fixed at 30% and 10% (stored per review). Should they be adjustable in the UI?
2. Single screening of low-ranked records is a methodological trade-off (Cochrane allows single screening only with verification). Gate 2 calibration still samples agent-excluded records, so recall is still measured. Should speed mode require Gate 2 to pass?

### Later additions (2026-09-25, afternoon)

**Screening robustness**
- The last `ta_screen` pass left 16 records whose agent answers had been unusable. A "Retry with AI" button in the queue re-asks the agents about them. New decisions supersede the unusable ones, and the arbiter rules again whenever its inputs changed (`screening::retry_with_agents`, `retryable_items`).
- Per-review option `uncertainToFullText` (off by default, set on Start). Records still uncertain after the title/abstract arbiter go to full-text screening as a disclosed rule instead of to the human queue (`carry_uncertain_forward`; named in item 8 of the manuscript).
- Two more model slips are coerced:
  - `"[[page 6]]"` becomes 6 for integer fields, but only when the string holds exactly one number.
  - Placeholder criterion codes ("none", "n/a") become null (`clean_code`).
- A malformed optional reasoning aid is dropped instead of rejecting the whole answer.

**Full-text retrieval**
- Lookups run in parallel across Unpaywall, CORE, OpenAlex and Semantic Scholar's `openAccessPdf` (new). Each uses a fail-fast retry policy (`adapter::QUICK`). Candidate PDFs download in parallel (`first_pdf`) with a 45 s timeout.
- A record without a DOI gets one from OpenAlex by exact title and year before the DOI routes run. 13 papers were recovered this way.
- Subscription access (`pipeline/fulltext_access.rs`):
  - Elsevier Article Retrieval and Wiley TDM APIs, using keys stored in Settings → Databases.
  - Opt-in institutional access: follow the DOI to the publisher's `citation_pdf_url`, identified honestly, 2 s apart per host.
  - SAGE and Taylor & Francis answer 403 to automated clients. That is not worked around.
- Browser-assisted upload (`pipeline/downloads.rs`): "Open next 5 in my browser" plus a watcher on ~/Downloads. New PDFs are matched by DOI in the file name or metadata, then by exact title from the text pdf.js reads. Unmatched files get a manual "This is…" menu.
- Batch upload (`pipeline/batch_upload.rs`, `BatchUpload.tsx`): a Full text card where the user drops or chooses any number of PDFs. Each file is matched against every report without a PDF, including those marked not retrieved. It is matched by DOI in the file name or PDF bytes, and if that fails the file is staged under `staged/` and matched by DOI or exact title in the first 2 pages pdf.js reads. Anything still unmatched gets a "This is…" menu. Each attachment is a human `uploaded` retrieval event naming the file and how it matched. Native Tauri drag-drop is off (`dragDropEnabled: false`) so HTML5 drops reach the page. Preview: `/preview.html?screen=batch`.
- Fix: the first live batch upload failed with "batch_upload_match not allowed". Its three commands had been registered in `lib.rs` but were not declared in `build.rs` or allowed in `capabilities/default.json`. They are now in both. A new test, `tests/command_permissions.rs`, fails whenever a registered command is missing from either file.
- Zotero (`pipeline/zotero.rs`): export the missing papers as RIS, the user runs Zotero's "Find Available PDF", then "Import PDFs from Zotero" reads `zotero.sqlite` read-only and immutable, matched by DOI or title.
- Retrieval deadline (§8.5): `retrievalDeadlineHours`, default 168. Reports still waiting after the deadline are marked `not_retrieved` by an agent event (`expire_waiting`). "Not retrieved" reports keep an Upload PDF button.
- Retrieval jobs are claimed ahead of AI jobs (`jobs::PRIORITY`). Worker concurrency is 12. Full-text screening runs on the texts already read while retrieval continues.
- Not built: Sci-Hub or Anna's Archive downloaders (PDFerret), which are copyright infringement. Automated driving of the user's logged-in Chrome was also dropped: the environment's safety check blocked it on publisher-terms grounds.

**App behaviour**
- Every Tauri command is now `async`, so none runs on the main thread. The window no longer freezes while jobs write to the database.
- Live status line (`LiveActivity`): polls a cheap change token every 1.5 s, including in the background, and refreshes screens at most every 3 s. It shows what is running, or "Waiting for you: …".
- Full text screen: a PDF button opens each retrieved PDF in Preview, and a "Needs your decision" badge opens the queue.
- Overview no longer shows a recall figure from a partial or skipped Gate 2 sample.

**Signing and Keychain**
- Builds are signed with the self-signed "Sift Development" identity (`bundle.macOS.signingIdentity`). Because it has no Apple team ID, macOS pins Keychain approvals to each build's cdhash, so every rebuild still prompts once.
- Fix: a free Apple Development certificate (team ID), then switch `signingIdentity` to it.

**Test review status (01a0d4e7)**
- Search and screening: 360 records identified, 86 duplicates, 274 screened. Laya speed mode single-screened 26 records and escalated 3. Screener A vs B agreement κ 0.93.
- Retrieval: 116 reports sought, 69 retrieved, 47 not retrieved.
- Full-text screening: 49 included and 12 excluded so far. 8 await a third AI retry, and whatever the AI still can't decide goes to the user's queue.
- Gates 1 and 2 were skipped (test run, so it is permanently Unverified).

### Later additions (2026-09-25, evening)

**Skipping citation searching (§8.6)**
- Citation chasing returned 1,228 new records, and screening them blocked extraction. PRISMA 2020 does not require citation searching, only that what was done is reported, so the author can now skip it.
- The Overview shows a banner while citation searching is pending, running or still screening. "Skip citation searching" opens an inline confirmation. The skip is refused once any citation record has reached full text.
- The skip is one human `skipped` event (stage `search`, reason code `citation_chasing`). Migration `0004_skip_citation_chasing.sql` redefines `v_current_run` to leave out citation runs once that event exists, so their records drop out of `v_screenable` and `v_prisma_counts`. Queued citation and TA jobs for those records are set aside (`jobs::set_aside`). `screen_record` and Laya's `unscored_one` ignore records that are not screenable.
- The manuscript discloses the skip under information sources (item 6).
- Code: `pipeline/chasing.rs` (`skip`, `state`, 3 tests), `skip_citation_chasing` command, `Progress.chasing`, `ChasingBanner` in `Overview.tsx`.
- Checked on a copy of the test review: after the skip, 360 identified, 86 duplicates, 274 screened, 116 sought, 77 assessed, 60 included.

**Laya runs side by side with the AI screeners**
- Before, TA screening waited until Laya had scored every record. Now the orchestrator enqueues a record for dual screening as soon as it has a score, while Laya keeps scoring the rest. The stage stays Working until both are done.
- Speed mode ranks records by percentile, so an early ranking is unreliable. `laya::MIN_RANKED` (50): while scoring continues and fewer than 50 records are scored, no record is single-screened. The safety net (escalating top-ranked records) still applies.
- Benchmark on the 8 GB Mac with 12–24 real records and 6 criteria:

  | Setting | Time per record | Scores |
  | --- | --- | --- |
  | Default (batch_size 16) | about 3.0 s | reference |
  | 2 records per call | 17.2 s | identical |
  | batch_size 1 | 6.7 s | |
  | batch_size 2 | 26.7 s | |

  The Mac is memory-bound, so batching across records or changing batch_size makes Laya slower. The default is kept, and the speed-up comes from overlap with LLM screening.
- The parallel LLM design is unchanged: Screeners A and B run together per record (`tokio::join!`), and many records run at once across the worker pool.

**Getting from full text to appraisal (2026-09-26)**
- A record that the arbiter had left uncertain in the skipped citation run stayed in the TA human queue and blocked the pipeline. Now `screening::human_queue` drops records whose search run is no longer current (`v_current_run`). A test covers this in `pipeline/chasing.rs`.
- Every extraction came back `needs_human`. The router's `auto` route sent these 13–22k-token requests to a local model, which replied `[]` or an object keyed by field name. The v1 prompt never described the JSON shape.
  - `prompts/extraction.v2.md` shows the exact shape, lists the field keys, and repeats the instruction after the full text.
  - `coerce_to_schema` now turns an object keyed by field name into the expected list.
  - `prompts/appraisal.v2.md` makes the same change for appraisal, before appraisal has run.
- A study that failed under an older extraction prompt is retried once with the current one (`extraction::settled`). The job payload now includes the prompt version, because `enqueue_once` never re-adds a payload it has already run. The retry supersedes the old failure event, so the Gate 3 count of "studies needing you" stays correct (it now reads `v_current_event`).
- Tests: 175 lib tests pass and clippy is clean.
- Appraisal passes returned valid JSON, but a single non-verbatim quote rejected the whole pass, so about half the studies went to "needs your review". Appraisal now works like extraction:
  - `appraisal::check` checks structure only.
  - `void_unfound_quotes` removes a domain's unverifiable quote and notes it in the rationale.
  - When both passes agree, the one with verified support is kept. A domain where neither pass has verified support is stored with actor `needs_human` and counted in Gate 3's "studies needing you".
  - `prompts/appraisal.v3.md` adds stricter rules for copying quotes.
  - Failures under v2 are retried once (`appraisal::settled`, prompt version in the job payload), and the retry supersedes the old failure event.
- Tests: 177 lib tests pass and clippy is clean.
- Live result: 56 of 56 studies appraised, and every v3 call returned valid JSON. 33 of 512 domain judgements are flagged for Gate 3.

**Synthesis prompts (2026-09-26)**
- Synthesis paused at the qualitative coding step. The local route returned a bare list with the key `ids` instead of `{"codes": [{"code", "finding_ids"}]}`.
- New prompt versions spell out the exact JSON shape and repeat it after the input: `synthesis_codes.v2`, `synthesis_themes.v2`, `synthesis_analytical.v2`, `cerqual.v2`, `swim.v2`, `grade.v2` and `manuscript.v2`. The v1 files stay registered because past calls refer to them.
- `coerce_to_schema` now makes two more repairs, both only when the match is unambiguous:
  - It wraps a non-empty bare list when the object's only property is a list.
  - It renames a single unknown key to the one missing required key that ends with `_` plus that name (`ids` becomes `finding_ids`).
  - An empty list is still rejected.
- The synthesis job clears `synthesis_item` and `synthesis_link` when it starts, so resuming it rebuilds everything cleanly.
- Tests: 178 lib tests pass and clippy is clean.
- Stage screen headers (Search, Screening, Full text, Appraisal, Synthesis) now show their own stage's status: "Complete", "Not started", or the run's halt message only while that stage is active. Before this, a synthesis pause showed up under "Appraisal". Overview still shows the run-wide status. The change is in the frontend only and has not been rebuilt yet.

**Live activity panel (2026-09-26)**
- The status line at the top of every review screen is now a panel with two lists:
  - **Now:** each item a worker is on, with its current step and elapsed time. The Overview lists every stage; the other screens list only their own stage.
  - **Finished recently:** the latest 12 decisions on this screen's event stages, read from the event log.
- `pipeline::live` is generalised from retrieval to any stage with `track_in(review, stage, item, title)` and `active(review)`. `review_activity` returns the result as `now`.
- Instrumented steps:
  - Screening (title/abstract, full text, seed): "Screener A and B reading …", then "Arbiter weighing both rationales".
  - Extraction: fields and pages, then the quote check.
  - Appraisal: two independent passes with the tool name and domain count.
  - Synthesis: coding study n of N, descriptive themes, analytical themes, CERQual or GRADE per theme or outcome.
  - Manuscript: section n of 3.
- Retrieval keeps its own tracking.
- `decision_log` takes an optional `stages` filter.
- The "Now" list lives in memory, so it is empty right after a restart until workers pick up items. "Finished" is durable.
- Tests: 178 lib tests, clippy, tsc and vitest (15 tests) all pass.

## 2026-09-26: redesign, skip, speed and academic improvements

The user asked for a premium redesign, much faster runs, a way to skip any stage without losing the work already done, and a methods review aimed at a publishable PRISMA paper with as little human input as possible.

**A. Redesign**
- New visual layer on top of the fixed §10 tokens (the tokens and `tokens.test.ts` are unchanged):
  - soft shadows, larger radii and a restrained gradient on the primary button;
  - Source Serif headings throughout.
- **Start:** hero, composer card, grouped options and a "where you come in" strip for the three gates.
- **Sidebar:** timeline with done, running and pending dots, and a verification badge.
- **Overview and Settings:** reworked to match.
- Line icons are drawn inline (`Icon.tsx`). They are decorative only; every control keeps a text label.
- Focus rings, labelled controls and contrast are unchanged.

**C. Skip and resume any stage** (`pipeline/skip.rs`, `StageControl.tsx`)
- Skippable stages: search, screening, full text, extraction, appraisal and synthesis.
- Skip and resume are human events with reason code `stage_skip`. Resuming supersedes the skip.
- Work already done is kept and flows on:
  - Decided records and reports stay counted.
  - Undecided records appear in the flow diagram as "not screened" or "awaiting classification".
- Every skip is disclosed in the methods sections, and in a new DOCX appendix (see D).

**B. Speed**
- Migration `0006_speed_indexes.sql` adds indexes on `event(supersedes)`, `event(stage, target_id, id)` and `job(kind, status)`.
  - `v_current_event` was checking for superseding events with a full scan per event.
  - Timings on the live 7,000-event review:

    | Query | Before | After |
    | --- | --- | --- |
    | `v_current_event` | 3.31 s | under 0.01 s |
    | PRISMA counts | 1.35 s | 0.05 s |

  - Every screen query and the orchestrator read these views, so this was the main source of lag.
- Per-screen `refetchInterval` polling is removed. `LiveActivity` already invalidates the review's queries when the event log or job table changes, so screens no longer run the heavy views every 1 to 3 seconds each.
- The backend was already parallel: 12 job workers, `par::bounded` inside jobs, and extraction and appraisal overlapping per study.

**D. Academic improvements**
- **Second request for unverified quotes** (`extraction_requote.v1`):
  - When the validator cannot find an extracted quote, the model is asked once for the exact passage. It sees only the claimed page and its neighbours, or the full text when no page was given.
  - The value cannot change, and the new quote goes through the same validator. Effect data must still contain the reported number.
  - Values verified this way carry the second call's provenance and a rationale saying so. Everything else stays flagged for Gate 3.
  - If the second call fails, the first pass is kept unchanged.
  - Item 9 of the manuscript describes the step.
- **Manuscript DOCX, new appendices:**
  - **B. Search strategies (PRISMA-S):** every search run with date, iteration, hits reported and records downloaded; the full string for each database exactly as sent; the simplifications; citation searching and imports; and the de-duplication method.
  - **C. Stages stopped early:** stage, date and what was kept. Present only when a stage was skipped.
  - **D. Completed PRISMA 2020 checklist:** each item's location in the manuscript, who wrote it (generated, drafted or author) and whether it is written.
- `Manuscript.searches` exposes the current search runs to the frontend.
- **Author items 3, 25 and 26:**
  - The editor offers standard starting text: an outline for the rationale, "no funding" or "funded" wording, and "no competing interests" or declaring one.
  - The author still saves the text, so the 18 / 6 / 3 split holds.
  - A hint appears while bracketed placeholders remain.
- The Synthesis certainty select now keeps its value.

**Checks**
- 188 lib tests pass, including two new ones: a second request verifies a flagged value but cannot invent one, and a second quote for effect data must contain the number.
- clippy clean, tsc clean, 15 vitest tests pass.

**Open questions**
- Arbiter context could be richer (full criteria text plus both screeners' evidence quotes) to shrink the human queue further. This needs a new prompt version.
- A GRADE summary-of-findings table for quantitative outcomes is not in the DOCX yet; CERQual has one.

## 2026-09-26 (later): activity panel, bypass chip, APA manuscript, MCP server

The user asked for four things:
- move "what is happening" out of the top of every screen into a side panel that opens on demand;
- a bypass mode like Claude's "bypass permissions";
- a submittable APA-style literature review;
- an MCP server, so Claude or ChatGPT can operate the app and act as its AI instead of an API key.

**Activity side panel** (`LiveActivity.tsx`, `ReviewShell.tsx`)
- Each review screen now has a one-line bar at the top. It shows the current status, the bypass chip and an "Activity" toggle.
- The toggle opens a 340 px right-hand panel with two lists, "Now" and "Finished recently". The panel's open state is saved in `localStorage` (`sift.activityOpen`).
- Query invalidation lives in `useActivityWatcher`, throttled to 3 s, so it still works when the panel is closed.
- The banners that remain on Overview and Protocol ask the user to act (paused, failed jobs, your turn, Gate 1 hint), so they stayed.

**Bypass**
- The Rust side (`pipeline/bypass.rs`) was already in place.
- New in the UI:
  - A per-review chip in the top bar. Turning it on opens an inline confirmation that explains the rules and the permanent Unverified status. Turning it off takes effect immediately.
  - Settings → Automation, "Start new reviews in bypass mode".
- The bypass rules:
  - gates are skipped;
  - title/abstract uncertain records get one retry, then are carried forward to full text;
  - full texts are settled by the agents' votes;
  - papers no automatic route finds are marked not retrieved;
  - flagged extractions are left out of synthesis.
- Every bypassed step is an event and is disclosed.

**APA manuscript**
- The APA 7 manuscript export was already built. Settings → Author details now supplies the title page (name, affiliation) and the default text for items 25 and 26.
- Item 3 (rationale) is still written by the author.

**MCP server** (`src-tauri/src/mcp/`)
- **Transport:** MCP Streamable HTTP (spec 2025-11-25) at `http://127.0.0.1:{port}/mcp`.
  - The default port is 31480. The server binds loopback only.
  - Every route needs `Authorization: Bearer <token>`.
  - The server starts and stops with Settings (`mcp::sync` on launch and on every settings save).
- **Tools:** `sift_list_reviews`, `sift_start_review`, `sift_status`, `sift_pause`, `sift_resume`, `sift_set_bypass`, `sift_skip_gate`, `sift_skip_stage`, `sift_retry_failed`, `sift_human_queue`, `sift_decision_log`, `sift_get_protocol`, `sift_get_manuscript`, plus the three bridge tools below.
  - No tool records a human screening, extraction or sign-off decision. An assistant can skip a gate (which leaves the review Unverified), but it cannot pass one as the human.
- **Bridge (assistant as the AI):** `http://localhost:{port}/bridge/v1` is an OpenAI-compatible endpoint. Its new preset is `mcp_bridge`, "Claude or ChatGPT (via MCP)".
  - Each model call the app makes waits in a queue. The assistant takes it with `sift_next_task` and answers with `sift_submit_task`.
  - `sift_bridge_status` shows the queue.
  - The answering model is reported in `X-Routed-Via` and logged as the event's model ID.
  - The Test connection prompt waits only 60 s, and on timeout says to call `sift_next_task`.
  - The preset has `data_policy_warning` on, because full texts go to whatever assistant is connected.
- **stdio relay:** `sift --mcp-stdio` relays newline-delimited JSON-RPC to the running app, for Claude Desktop. It reads the port and token from `app.db`.
- **The token is shown in the frontend on purpose.**
  - It is a local capability token the user pastes into their assistant's config. It is not a provider key, so principle 7 still holds for provider and database keys.
  - It lives in `app.db` rather than the keychain so the stdio relay can read it without a Keychain prompt.
  - Settings masks it by default, and "New token" asks for confirmation inline.
- **Settings → Claude or ChatGPT (MCP):** enable toggle, port, live status, token, a copyable `claude mcp add` command for Claude Code, a copyable `claude_desktop_config.json` block, and ChatGPT notes.
- **ChatGPT limitation:** ChatGPT connectors only reach public HTTPS URLs. The user must put the server behind an HTTPS tunnel they control. The Settings copy warns that anyone with the tunnel URL and token can operate Sift.
- **Dependencies:** none new. Only tokio's `io-std` feature was added.

**Checks**
- cargo test passes and clippy is clean. Rust tests cover the MCP tools, the bridge, the token, the bridge URL and presets.
- tsc is clean and 23 vitest tests pass. Two are new: the MCP status with a masked token and the inline new-token confirmation, and port validation.

**Open questions**
- ChatGPT needs a tunnel. A built-in tunnel is out of scope, since that would be a hosted service.
- A long bridge wait holds a worker slot. With many tasks assigned to the bridge and no assistant polling, the review stalls. The status line shows how many calls are waiting.

## 2026-09-26 (later): live MCP test and a bridge fix

**Live test against the running app (curl as the MCP client):** 401 without or with a wrong token; `initialize` (2025-11-25); `tools/list` returns all 16 tools; `sift_list_reviews`, `sift_status`, `sift_get_protocol`, `sift_decision_log`, `sift_get_manuscript`, `sift_human_queue`, `sift_bridge_status` all return data. Bridge: `/bridge/v1/models` lists `mcp-assistant`. A chat call waits, `sift_next_task` hands it out, and `sift_submit_task` delivers it with `X-Routed-Via`. A schema-breaking answer is refused with a readable error and the corrected answer goes through. The stdio relay (`sift --mcp-stdio`) returns initialize, 16 tools, and a parse error for bad input.

**Bug found and fixed:** the HTTP server did not notice when a caller hung up. An abandoned model call stayed queued for up to 15 minutes, so the next assistant got dead work first. An abandoned `sift_next_task` long poll could also claim a task nobody would receive (it stayed stranded until the 10-minute claim TTL). `mcp::http::connection` now races the route against a read on the socket and drops the route when the client closes. That drop withdraws the bridge ticket. New test: `a_model_call_whose_caller_hangs_up_leaves_the_bridge`. Verified live.

**Not tested live:** a headless Claude Code run. The CLI bundled with Claude Desktop is not signed in on its own, and signing it in is the user's step. The mutating tools (`sift_pause`, `sift_set_bypass`, `sift_skip_*`, `sift_start_review`) are covered by unit tests and were not run against the user's real reviews.
