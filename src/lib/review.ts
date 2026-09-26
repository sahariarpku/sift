// Typed wrappers for the review pipeline commands (src-tauri/src/commands/pipeline.rs).
import { invoke } from "@tauri-apps/api/core";
import type { ReviewSummary } from "./ipc";

export type QuestionType =
  "intervention" | "exposure" | "experience" | "policy" | "mixed";
export type Framework = "PICO" | "PEO" | "SPIDER" | "PICo" | "SPICE";
export type TestableAt = "title_abstract" | "full_text" | "both";

export interface Criterion {
  code: string;
  text: string;
  testable_at: TestableAt;
}

export interface Concept {
  name: string;
  synonyms: string[];
  controlled_vocab: string[];
}

export interface ProtocolBody {
  question_type: QuestionType;
  framework: Framework;
  search_framework: Framework;
  framework_elements: { element: string; description: string }[];
  concepts: Concept[];
  inclusion_criteria: Criterion[];
  exclusion_criteria: Criterion[];
  date_range: { from: number; to: number };
  languages: string[];
  study_designs: string[];
  planned_synthesis: string;
  open_questions: string[];
}

export interface ProtocolVersion {
  id: string;
  version: number;
  body: ProtocolBody;
  lockedAt: string | null;
}

export interface ReviewOptions {
  yearFrom: number;
  yearTo: number;
  languages: string[];
  reviewType: string;
  sources: string[];
  maxRecordsPerSource: number;
  citationChasing: boolean;
  seedCheck: boolean;
  prescreen: Prescreen;
  /** Records still uncertain after the title/abstract arbiter go to full text. */
  uncertainToFullText: boolean;
  /** Hours a report may wait for a PDF before it is marked not retrieved. */
  retrievalDeadlineHours: number;
}

export type PrescreenMode = "off" | "safety_net" | "speed";

export interface Prescreen {
  mode: PrescreenMode;
  speedFraction: number;
  netFraction: number;
  model: string;
}

export interface Rendered {
  query: string;
  params: [string, string][];
  simplifications: string[];
}

export interface ProtocolView {
  question: string;
  draft: ProtocolVersion | null;
  locked: ProtocolVersion | null;
  options: ReviewOptions;
  previews: [string, Rendered][];
}

export type Halt =
  | { kind: "working"; detail: string }
  | { kind: "waiting"; detail: string }
  | { kind: "paused"; detail: string }
  | { kind: "done" };

export interface Verification {
  verified: boolean;
  gatesPassed: number;
  skipped: number[];
  /** Stages the author stopped early, by name. */
  stopped: string[];
  label: string;
}

/** A stage the review author stopped early; the work already done is kept. */
export interface Skip {
  stage: string;
  name: string;
  eventId: number;
  at: string;
  /** What was kept, in words ready for the methods section. */
  rationale: string;
}

export type PipelineStatus = "done" | "running" | "waiting" | "pending" | "skipped";

export interface StageState {
  id: string;
  status: PipelineStatus;
  /** The author may skip it now. */
  skippable: boolean;
}

export interface Progress {
  halt: Halt;
  stages: StageState[];
  pause: { reason: string; rateLimited: boolean; since: string } | null;
  queued: number;
  running: number;
  failed: number;
  verification: Verification;
  chasing: ChasingState;
  skips: Skip[];
}

export interface ChasingState {
  status: "off" | "pending" | "running" | "screening" | "done" | "skipped";
  /** Unique records the citation search returned. */
  records: number;
  screened: number;
  canSkip: boolean;
}

export interface Prisma {
  identifiedDatabases: number;
  identifiedOther: number;
  bySource: [string, number][];
  duplicatesRemoved: number;
  recordsScreened: number;
  recordsExcluded: number;
  /** Records set aside unscreened because screening was stopped early. */
  recordsNotScreened: number;
  reportsSought: number;
  reportsNotRetrieved: number;
  reportsAssessed: number;
  reportsExcluded: number;
  /** Retrieved reports left unclassified because full-text screening was stopped early. */
  reportsAwaiting: number;
  exclusionReasons: [string, number][];
  studiesIncluded: number;
  reportsIncluded: number;
}

export interface Stratum {
  name: string;
  population: number;
  relevant: number;
  sampled: number;
  passedByAgents: boolean;
}

export interface Gate2Metrics {
  sampled: number;
  judged: number;
  recall: number | null;
  recallCi: [number, number] | null;
  kappa: number | null;
  strata: Stratum[];
  passed: boolean;
}

export interface Quality {
  gate2: Gate2Metrics | null;
  gate2Outcome: string | null;
  kappaAb: number | null;
  kappaPairs: number;
  quoteRate: number | null;
  quotesChecked: number;
  seedFound: [number, number] | null;
  drift: number | null;
  driftN: number;
}

export interface LogEntry {
  id: number;
  time: string;
  actor: string;
  human: boolean;
  stage: string;
  decision: string;
  target: string;
  detail: string;
}

export interface SearchRun {
  id: string;
  source: string;
  queryString: string;
  simplifications: string[];
  iteration: number;
  runAt: string;
  hits: number;
  records: number;
  current: boolean;
}

export interface RecordRow {
  id: string;
  searchRunId: string;
  source: string;
  sourceId: string | null;
  doi: string | null;
  title: string | null;
  abstract: string | null;
  authors: string[];
  year: number | null;
  venue: string | null;
  pdfUrl: string | null;
  landingUrl: string | null;
}

export interface EventRow {
  id: number;
  actor_id: string;
  decision: string;
  reason_code: string | null;
  rationale: string | null;
  evidence_quote: string | null;
  confidence: number | null;
  model_id: string | null;
}

export interface QueueItem {
  targetId: string;
  record: RecordRow;
  screenerA: EventRow | null;
  screenerB: EventRow | null;
  arbiter: EventRow | null;
}

export interface Calibration {
  sample: [string, string | null][];
  outcome: string | null;
  metrics: Gate2Metrics | null;
  manual: boolean;
}

export interface ReportView {
  reportId: string;
  record: RecordRow;
  retrieval: string | null;
  retrievalNote: string | null;
  pages: number;
  ftDecision: string | null;
  ftReason: string | null;
  /** The agents could not decide; the full-text decision is the user's. */
  needsYou: boolean;
  studyId: string | null;
}

export interface StudyRow {
  id: string;
  label: string;
  /** APA narrative citation, e.g. "Smith and Lee (2021)". */
  citation: string;
  title: string;
  values: [string, string][];
  tool: string | null;
  appraisal: [string, string][];
  reports: number;
}

export interface ValueRow {
  id: string;
  studyId: string;
  reportId: string;
  field: string;
  value: unknown;
  sourceQuote: string | null;
  page: number | null;
  validation: "verified" | "quote_not_found" | "not_reported";
}

export interface FieldSpec {
  key: string;
  label: string;
  instruction: string;
}

export interface StudyExtraction {
  values: ValueRow[];
  reportId: string | null;
  fields: FieldSpec[];
}

export interface AppraisalRow {
  studyId: string;
  tool: string;
  domain: string;
  judgement: string;
  rationale: string;
  sourceQuote: string | null;
  page: number | null;
}

export interface SynthesisItem {
  id: string;
  kind:
    | "code"
    | "descriptive_theme"
    | "analytical_theme"
    | "cerqual"
    | "swim_group"
    | "grade";
  parentId: string | null;
  label: string;
  body: Record<string, unknown>;
  valueIds: string[];
}

export type SectionKind = "auto" | "drafted" | "author";

export interface Section {
  item: string;
  heading: string;
  kind: SectionKind;
  text: string;
  key: string | null;
  done: boolean;
}

export interface ThemeRow {
  id: string;
  label: string;
  statement: string;
  certainty: string | null;
  certaintyConfirmed: boolean;
  descriptive: string[];
  quotes: [string, string, number | null][];
}

export interface Manuscript {
  title: string;
  question: string;
  verification: string;
  sections: Section[];
  studies: StudyRow[];
  themes: ThemeRow[];
  swim: [string, Record<string, unknown>][];
  prisma: Prisma;
  references: string[];
  apaReferences: ApaReference[];
  keywords: string[];
  authorName: string | null;
  affiliation: string | null;
  /** Decisions bypass mode made in place of a person. */
  bypassDecisions: number;
  /** Every current search run, for the PRISMA-S appendix. */
  searches: SearchRun[];
}

/** One APA 7 reference. `detail` wraps its italic parts in asterisks. */
export interface ApaReference {
  cite: string;
  narrative: string;
  authors: string;
  year: string;
  title: string;
  source: string | null;
  detail: string | null;
  doi: string | null;
  included: boolean;
  recordId: string | null;
}

export interface BypassRow {
  stage: string;
  decision: string;
  count: number;
  rule: string;
}

export interface ModelUse {
  task: string;
  provider: string;
  modelRequested: string;
  modelReported: string | null;
  promptVersion: string;
  calls: number;
  tokensIn: number;
  tokensOut: number;
  costUsd: number | null;
  promptJsonFallback: number;
  temperatureNotApplied: number;
}

export interface RetrievalLive {
  active: { reportId: string; title: string; step: string; seconds: number }[];
  recent: {
    reportId: string;
    title: string;
    found: boolean;
    detail: string;
    secondsAgo: number;
  }[];
}

/** One item a worker is on right now. `stage` is the screen it belongs to. */
export interface ActiveItem {
  stage: string;
  reportId: string;
  title: string;
  step: string;
  seconds: number;
}

export interface Activity {
  token: string;
  /** [job kind, running, queued] */
  jobs: [string, number, number][];
  now: ActiveItem[];
}

export interface ZoteroImport {
  attached: number;
  stillMissing: number;
  library: string;
}

export interface DownloadFile {
  path: string;
  name: string;
  size: number;
}

export interface ScanOutcome {
  /** [file name, report title] attached in this scan. */
  attached: [string, string][];
  unmatched: DownloadFile[];
  waiting: number;
}

export interface BatchUploadResult {
  matched: boolean;
  reportId: string | null;
  title: string | null;
  /** Set while an unmatched file waits for a text match or the user's choice. */
  stagedId: string | null;
}

export interface PrescreenUse {
  modelId: string;
  promptVersion: string;
  mode: string;
  scored: number;
  singleScreened: number;
  escalated: number;
  note: string;
}

export interface Disclosure {
  models: ModelUse[];
  searchSimplifications: [string, string[]][];
  verification: Verification;
  quality: Quality;
  humanDecisions: number;
  agentDecisions: number;
  humanOverrides: number;
  needsHumanItems: number;
  metaAnalysisNote: string | null;
  prescreen: PrescreenUse | null;
  carriedForward: number;
  skips: Skip[];
  bypass: BypassRow[];
}

export interface Gate3Requirements {
  flaggedValues: number;
  linksToConfirm: number;
  linksConfirmed: boolean;
  certaintyToConfirm: number;
  missingAuthorItems: string[];
  studiesNeedingYou: number;
  ready: boolean;
}

const r = (reviewId: string) => ({ reviewId });

export const review = {
  start: (question: string, options: ReviewOptions) =>
    invoke<ReviewSummary>("start_review", { question, options }),
  protocol: (id: string) => invoke<ProtocolView>("get_protocol", r(id)),
  saveProtocol: (id: string, body: ProtocolBody) =>
    invoke<ProtocolVersion>("save_protocol", { ...r(id), body }),
  setOptions: (id: string, options: ReviewOptions) =>
    invoke<void>("set_review_options", { ...r(id), options }),
  lock: (id: string, reviewed: boolean) =>
    invoke<ProtocolVersion>("lock_protocol", { ...r(id), reviewed }),
  progress: (id: string) => invoke<Progress>("review_progress", r(id)),
  pause: (id: string) => invoke<void>("pause_review", r(id)),
  resume: (id: string) => invoke<void>("resume_review", r(id)),
  retryFailed: (id: string) => invoke<number>("retry_failed_jobs", r(id)),
  dismissFailed: (id: string) => invoke<number>("dismiss_failed_jobs", r(id)),
  skipStage: (id: string, stage: string) =>
    invoke<Skip>("skip_stage", { ...r(id), stage }),
  resumeStage: (id: string, stage: string) =>
    invoke<void>("resume_stage", { ...r(id), stage }),
  skipCitationChasing: (id: string) =>
    invoke<{ at: string; records: number }>("skip_citation_chasing", r(id)),
  prisma: (id: string) => invoke<Prisma>("get_prisma", r(id)),
  prismaSvg: (id: string) => invoke<string>("prisma_svg", r(id)),
  quality: (id: string) => invoke<Quality>("quality_checks", r(id)),
  log: (id: string, limit: number, stages: string[] | null = null) =>
    invoke<LogEntry[]>("decision_log", { ...r(id), limit, stages }),
  runs: (id: string) => invoke<SearchRun[]>("list_search_runs", r(id)),
  importRecords: (id: string, label: string, text: string) =>
    invoke<number>("import_records", { ...r(id), label, text }),
  calibration: (id: string) => invoke<Calibration>("calibration", r(id)),
  record: (id: string, recordId: string) =>
    invoke<RecordRow>("get_record", { ...r(id), recordId }),
  judge: (
    id: string,
    recordId: string,
    decision: string,
    reasonCode: string | null,
  ) =>
    invoke<void>("calibration_judge", {
      ...r(id),
      recordId,
      decision,
      reasonCode,
    }),
  finishCalibration: (id: string) =>
    invoke<Gate2Metrics>("calibration_finish", r(id)),
  skipGate: (id: string, gate: 1 | 2 | 3) =>
    invoke<void>("skip_gate", { ...r(id), gate }),
  setBypass: (id: string, on: boolean) =>
    invoke<void>("set_bypass", { ...r(id), on }),
  getBypass: (id: string) => invoke<boolean>("get_bypass", r(id)),
  handScreeningToMe: (id: string) =>
    invoke<void>("hand_screening_to_me", r(id)),
  queue: (id: string, stage: "ta_screen" | "ft_screen") =>
    invoke<QueueItem[]>("human_queue", { ...r(id), stage }),
  retryableCount: (id: string, stage: "ta_screen" | "ft_screen") =>
    invoke<number>("retryable_count", { ...r(id), stage }),
  retryWithAgents: (id: string, stage: "ta_screen" | "ft_screen") =>
    invoke<number>("retry_queue_with_agents", { ...r(id), stage }),
  decide: (
    id: string,
    stage: string,
    targetId: string,
    decision: string,
    reasonCode: string | null,
    note: string | null,
  ) =>
    invoke<void>("human_decide", {
      ...r(id),
      stage,
      targetId,
      decision,
      reasonCode,
      note,
    }),
  reports: (id: string) => invoke<ReportView[]>("list_reports", r(id)),
  uploadPdf: (id: string, reportId: string, bytes: Uint8Array) =>
    invoke<void>("upload_report_pdf", bytes, {
      headers: { "x-review-id": id, "x-report-id": reportId },
    }),
  batchUploadMatch: (id: string, filename: string, bytes: Uint8Array) =>
    invoke<BatchUploadResult>("batch_upload_match", bytes, {
      headers: {
        "x-review-id": id,
        "x-filename": encodeURIComponent(filename),
      },
    }),
  batchMatchStaged: (
    id: string,
    stagedId: string,
    filename: string,
    text: string,
  ) =>
    invoke<BatchUploadResult>("batch_match_staged", {
      ...r(id),
      stagedId,
      filename,
      text,
    }),
  batchAssignStaged: (
    id: string,
    stagedId: string,
    filename: string,
    reportId: string,
  ) =>
    invoke<BatchUploadResult>("batch_assign_staged", {
      ...r(id),
      stagedId,
      filename,
      reportId,
    }),
  markNotRetrieved: (id: string, reportIds: string[]) =>
    invoke<number>("mark_not_retrieved", { ...r(id), reportIds }),
  openViaProxy: (id: string, reportId: string) =>
    invoke<void>("open_via_proxy", { ...r(id), reportId }),
  retryRetrieval: (id: string) => invoke<number>("retry_retrieval", r(id)),
  activity: (id: string) => invoke<Activity>("review_activity", r(id)),
  retrievalLive: (id: string) => invoke<RetrievalLive>("retrieval_live", r(id)),
  openPdf: (id: string, reportId: string) =>
    invoke<void>("open_report_pdf", { ...r(id), reportId }),
  importFromZotero: (id: string) =>
    invoke<ZoteroImport>("import_from_zotero", r(id)),
  exportMissingRis: (id: string) =>
    invoke<string>("export_file", { ...r(id), kind: "missing_ris" }),
  openUploadBatch: (id: string, count: number) =>
    invoke<number>("open_upload_batch", { ...r(id), count }),
  scanDownloads: (id: string) => invoke<ScanOutcome>("scan_downloads", r(id)),
  readDownload: (path: string) =>
    invoke<ArrayBuffer>("read_download", { path }),
  matchDownloadText: (id: string, path: string, text: string) =>
    invoke<string | null>("match_download_text", { ...r(id), path, text }),
  assignDownload: (id: string, path: string, reportId: string) =>
    invoke<string>("assign_download", { ...r(id), path, reportId }),
  pendingPdfTexts: () =>
    invoke<{ reviewId: string; reportId: string }[]>("pending_pdf_texts"),
  readPdf: (id: string, reportId: string) =>
    invoke<ArrayBuffer>("read_report_pdf", { ...r(id), reportId }),
  storePageTexts: (id: string, reportId: string, pages: string[]) =>
    invoke<boolean>("store_page_texts", { ...r(id), reportId, pages }),
  pages: (id: string, reportId: string) =>
    invoke<string[]>("report_pages", { ...r(id), reportId }),
  studies: (id: string) => invoke<StudyRow[]>("list_studies", r(id)),
  extraction: (id: string, studyId: string) =>
    invoke<StudyExtraction>("study_extraction", { ...r(id), studyId }),
  resolveValue: (
    id: string,
    input: {
      valueId: string;
      value: string | null;
      quote: string | null;
      page: number | null;
    },
  ) => invoke<ValueRow>("resolve_value", { ...r(id), input }),
  appraisals: (id: string) => invoke<AppraisalRow[]>("list_appraisals", r(id)),
  synthesis: (id: string) => invoke<SynthesisItem[]>("synthesis_items", r(id)),
  confirmCertainty: (
    id: string,
    itemId: string,
    overall: string,
    note: string | null,
  ) => invoke<void>("confirm_certainty", { ...r(id), itemId, overall, note }),
  manuscript: (id: string) => invoke<Manuscript>("get_manuscript", r(id)),
  disclosure: (id: string) => invoke<Disclosure>("get_disclosure", r(id)),
  gate3: (id: string) => invoke<Gate3Requirements>("gate3_requirements", r(id)),
  setAuthorText: (id: string, key: string, text: string) =>
    invoke<void>("set_author_text", { ...r(id), key, text }),
  confirmLinks: (id: string) => invoke<void>("confirm_links", r(id)),
  signOff: (id: string) => invoke<void>("sign_off", r(id)),
  exportFile: (id: string, kind: "ris" | "csv" | "svg") =>
    invoke<string>("export_file", { ...r(id), kind }),
  saveExport: (id: string, fileName: string, bytes: Uint8Array) =>
    invoke<string>("save_export", bytes, {
      headers: { "x-review-id": id, "x-file-name": fileName },
    }),
  revealExports: (id: string) => invoke<void>("reveal_exports", r(id)),
};

export const SOURCE_NAMES: Record<string, string> = {
  eric: "ERIC",
  openalex: "OpenAlex",
  core: "CORE",
  semantic_scholar: "Semantic Scholar",
  citation_chasing: "Citation searching",
  manual_import: "Imported records",
};
