// Typed wrappers over the Rust commands in src-tauri/src/commands. The
// frontend never talks to the network or sees an API key; everything goes
// through these calls.
import { invoke } from '@tauri-apps/api/core';

export type Verification = 'unverified' | 'verified';

export interface ReviewSummary {
  id: string;
  question: string;
  verification: Verification;
  createdAt: string;
  path: string;
}

export type JobStatus = 'queued' | 'running' | 'done' | 'failed' | 'paused';

export interface Job {
  id: number;
  reviewId: string;
  kind: string;
  payload: unknown;
  status: JobStatus;
  attempts: number;
  lastError: string | null;
  updatedAt: string;
}

export interface CommandError {
  message: string;
}

export function errorMessage(e: unknown): string {
  if (typeof e === 'object' && e !== null && 'message' in e && typeof e.message === 'string') {
    return e.message;
  }
  return String(e);
}

// ---- Settings ------------------------------------------------------------

export type Protocol = 'openai_compat' | 'anthropic';

export interface Preset {
  id: string;
  displayName: string;
  protocol: Protocol;
  baseUrl: string;
  requiresKey: boolean;
  local: boolean;
  getKeyUrl: string | null;
  dataPolicyWarning: boolean;
  structuredOutput: boolean;
  maxTokensParam: string;
}

/** All the frontend ever learns about a stored key. */
export interface SecretStatus {
  present: boolean;
  hint: string | null;
}

export interface ModelInfo {
  id: string;
  displayName: string | null;
  structuredOutput: boolean | null;
}

export type ProviderStatus = 'connected' | 'failed' | 'not_running' | 'unreachable' | 'untested';

export interface ProviderSummary {
  id: string;
  preset: string;
  displayName: string;
  protocol: Protocol;
  baseUrl: string;
  hostLabel: string;
  key: SecretStatus;
  requiresKey: boolean;
  local: boolean;
  models: ModelInfo[];
  dataPolicyWarning: boolean;
  concurrency: number;
  status: ProviderStatus;
  statusMessage: string | null;
  urlWarning: string | null;
  getKeyUrl: string | null;
}

/** Sent from the add/edit form. `apiKey` travels to Rust only and is never read back. */
export interface ProviderDraft {
  id: string | null;
  preset: string;
  displayName: string;
  baseUrl: string;
  apiKey: string | null;
  removeKey: boolean;
  dataPolicyWarning: boolean;
  concurrency: number;
  testModel: string | null;
}

export type ModelMatch = 'exact' | 'snapshot' | 'different' | 'unknown';

export interface TestResult {
  ok: boolean;
  model: string | null;
  reportedModel: string | null;
  modelMatch: ModelMatch;
  latencyMs: number;
  message: string;
}

export interface TestOutcome {
  ok: boolean;
  status: ProviderStatus;
  message: string;
  result: TestResult | null;
}

export type TaskId =
  | 'screener_a'
  | 'screener_b'
  | 'arbiter'
  | 'extraction'
  | 'appraisal'
  | 'protocol_search'
  | 'synthesis';

export interface Assignment {
  task: TaskId;
  label: string;
  providerId: string | null;
  modelId: string | null;
}

export interface Price {
  inputPerMtok: number;
  outputPerMtok: number;
}

export interface PriceRow {
  providerId: string;
  modelId: string;
  price: Price;
}

export interface AppSettings {
  strictMode: boolean;
  warnDataPolicy: boolean;
  unpaywallEmail: string | null;
  libraryProxyPrefix: string | null;
  spendingCapUsd: number | null;
  /** Python with laya-mlx installed; detected when empty. */
  layaPython: string | null;
  /** On an institutional network or VPN: follow DOI links to publisher PDFs. */
  institutionalAccess: boolean;
  /** New reviews start with bypass mode on. */
  bypassByDefault: boolean;
  authorName: string | null;
  authorAffiliation: string | null;
  /** Your own funding statement (PRISMA item 25). */
  fundingStatement: string | null;
  /** Your own competing-interests statement (PRISMA item 26). */
  competingInterests: string | null;
  /** Serve the MCP server so Claude or ChatGPT can drive the app. */
  mcpEnabled: boolean;
  mcpPort: number;
}

export interface BridgeStatus {
  /** Model calls waiting for an assistant. */
  waiting: number;
  /** Calls an assistant has taken but not answered yet. */
  claimed: number;
  /** Seconds since an assistant last asked for work; null if none has. */
  lastPollSecs: number | null;
}

export interface McpStatus {
  enabled: boolean;
  running: boolean;
  port: number;
  url: string;
  bridgeUrl: string;
  /** Local capability token the user pastes into the assistant's config. Not a provider key. */
  token: string;
  stdioCommand: string | null;
  error: string | null;
  bridge: BridgeStatus;
}

export interface LayaStatus {
  available: boolean;
  python: string | null;
  version: string | null;
  model: string;
  modelDownloaded: boolean;
  message: string;
}

export interface LayaTest {
  ok: boolean;
  message: string;
  loadMs: number;
  answerMs: number;
}

export type SourceAuth = 'none' | 'key' | 'optional_key' | 'key_and_inst_token' | 'email';

export interface SourceStatus {
  id: string;
  name: string;
  auth: SourceAuth;
  note: string;
  getKeyUrl: string | null;
  docsUrl: string;
  key: SecretStatus;
  instToken: SecretStatus | null;
  ready: boolean;
}

export interface SourceTestResult {
  ok: boolean;
  status: number | null;
  message: string;
}

export const ipc = {
  createReview: (question: string) => invoke<ReviewSummary>('create_review', { question }),
  listReviews: () => invoke<ReviewSummary[]>('list_reviews'),
  enqueueNoopJob: (reviewId: string, sleepMs: number) =>
    invoke<number>('enqueue_noop_job', { reviewId, sleepMs }),
  listJobs: (reviewId: string) => invoke<Job[]>('list_jobs', { reviewId }),
  reviewSpend: (reviewId: string) => invoke<number>('review_spend', { reviewId }),

  listPresets: () => invoke<Preset[]>('list_presets'),
  listProviders: () => invoke<ProviderSummary[]>('list_providers'),
  checkBaseUrl: (url: string) => invoke<string | null>('check_base_url', { url }),
  fetchModels: (draft: ProviderDraft) => invoke<ModelInfo[]>('fetch_models', { draft }),
  testProvider: (draft: ProviderDraft) => invoke<TestOutcome>('test_provider', { draft }),
  saveProvider: (draft: ProviderDraft) => invoke<ProviderSummary>('save_provider', { draft }),
  deleteProvider: (id: string) => invoke<void>('delete_provider', { id }),

  listTaskAssignments: () => invoke<Assignment[]>('list_task_assignments'),
  setTaskAssignment: (task: TaskId, providerId: string | null, modelId: string | null) =>
    invoke<Assignment[]>('set_task_assignment', { input: { task, providerId, modelId } }),
  listPrices: () => invoke<PriceRow[]>('list_prices'),
  setPrice: (providerId: string, modelId: string, price: Price | null) =>
    invoke<PriceRow[]>('set_price', { input: { providerId, modelId, price } }),

  getSettings: () => invoke<AppSettings>('get_settings'),
  layaStatus: () => invoke<LayaStatus>('laya_status'),
  layaTest: () => invoke<LayaTest>('laya_test'),
  updateSettings: (settings: AppSettings) => invoke<AppSettings>('update_settings', { settings }),
  mcpStatus: () => invoke<McpStatus>('mcp_status'),
  regenerateMcpToken: () => invoke<McpStatus>('regenerate_mcp_token'),

  listSources: () => invoke<SourceStatus[]>('list_sources'),
  setSourceKey: (id: string, key: string | null, instToken: string | null) =>
    invoke<SourceStatus[]>('set_source_key', { input: { id, key, instToken } }),
  deleteSourceKey: (id: string) => invoke<SourceStatus[]>('delete_source_key', { id }),
  testSource: (id: string) => invoke<SourceTestResult>('test_source', { id }),
  openExternal: (url: string) => invoke<void>('open_external', { url }),
};
