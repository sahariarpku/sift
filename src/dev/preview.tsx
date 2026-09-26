// Dev-only design preview: renders screens in a plain browser with mocked
// Rust commands, so layout can be checked without the Tauri shell. Served by
// `npm run dev` at /preview.html; not part of the production build.
import { StrictMode } from 'react';
import flowSvg from './sample-flow.svg?raw';
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { mockIPC } from '@tauri-apps/api/mocks';
import '../theme/fonts';
import '../theme/global.css';
import { Settings } from '../screens/settings/Settings';
import { BatchUpload } from '../screens/review/BatchUpload';
import { ReviewShell } from '../screens/review/ReviewShell';
import { Start } from '../screens/Start';
import type { ReportView, Skip } from '../lib/review';
import type { StageId } from '../lib/stages';

// ?screen=review&stage=overview: a review part-way through extraction.
const REVIEW_ID = 'rev';
const QUESTION =
  'How do international students in UK higher education experience belonging, and what supports it?';
const NAMES: Record<string, string> = {
  search: 'searching', screening: 'title and abstract screening', fulltext: 'full-text retrieval and screening',
  extraction: 'data extraction', appraisal: 'risk of bias and quality appraisal', synthesis: 'synthesis', report: 'manuscript drafting',
};
let skips: Skip[] = [];
function progress() {
  const base: [string, string][] = [
    ['protocol', 'done'], ['search', 'done'], ['screening', 'done'], ['fulltext', 'done'],
    ['extraction', 'running'], ['appraisal', 'pending'], ['synthesis', 'pending'], ['report', 'pending'],
  ];
  const stages = base.map(([id, status]) => {
    const skipped = skips.some((k) => k.stage === id);
    const st = skipped && status !== 'done' ? 'skipped' : status;
    return { id, status: st, skippable: id !== 'protocol' && !skipped && status !== 'done' };
  });
  const stopped = skips.map((k) => k.name);
  return {
    halt: { kind: 'running', detail: 'Extracting data · 23 of 77 studies' },
    stages, pause: null, queued: 54, running: 4, failed: 0,
    verification: {
      verified: false, gatesPassed: 2, skipped: [], stopped,
      label: `Unverified · 2 of 3 gates passed${stopped.length ? ` · stopped early: ${stopped.join(', ')}` : ''}`,
    },
    chasing: { status: 'done', records: 12, screened: 12, canSkip: false },
    skips,
  };
}
const logEntries = [
  ['10:42', 'Extraction agent', 'extracted', 'Chinese students and their experiences of UK higher education', '14 fields · 13 verified, 1 quote not found'],
  ['10:41', 'Extraction agent', 'extracted', 'Belonging and loneliness among international postgraduates', '12 fields verified'],
  ['10:39', 'Linker', 'link', 'Smith 2019 ↔ Smith 2021', 'Same registration ID'],
  ['10:37', 'You', 'resolve', 'Academic adjustment of international doctoral students', 'Included at full text'],
  ['10:31', 'Screener A', 'exclude', 'Housing costs in London universities', 'EX-POP · Not international students'],
  ['10:30', 'Arbiter', 'include', 'Peer mentoring and sense of belonging', 'IN-OUT · Reports belonging outcome'],
].map(([t, actor, decision, target, detail], i) => ({
  id: 100 - i, time: `2026-09-26T${t}:00Z`, actor, human: actor === 'You', stage: 'extract', decision, target, detail,
}));

const providers = [
  {
    id: 'p1', preset: 'anthropic', displayName: 'Anthropic', protocol: 'anthropic', baseUrl: 'https://api.anthropic.com',
    hostLabel: 'api.anthropic.com', key: { present: true, hint: 'key ending 4Kf2' }, requiresKey: true, local: false,
    models: [{ id: 'claude-sonnet-5', displayName: null, structuredOutput: true }], dataPolicyWarning: false, concurrency: 4,
    status: 'connected', statusMessage: null, urlWarning: null, getKeyUrl: null,
  },
  {
    id: 'p2', preset: 'openrouter', displayName: 'OpenRouter', protocol: 'openai_compat', baseUrl: 'https://openrouter.ai/api/v1',
    hostLabel: 'openrouter.ai/api/v1', key: { present: true, hint: 'key ending 9aQ1' }, requiresKey: true, local: false,
    models: [{ id: 'openai/gpt-5.2', displayName: null, structuredOutput: null }], dataPolicyWarning: true, concurrency: 4,
    status: 'connected', statusMessage: null, urlWarning: null, getKeyUrl: null,
  },
  {
    id: 'p3', preset: 'ollama', displayName: 'Ollama (on this computer)', protocol: 'openai_compat', baseUrl: 'http://localhost:11434/v1',
    hostLabel: 'localhost:11434/v1', key: { present: false, hint: null }, requiresKey: false, local: true, models: [],
    dataPolicyWarning: false, concurrency: 4, status: 'not_running', statusMessage: null, urlWarning: null, getKeyUrl: null,
  },
];
const presets = ['anthropic', 'openai', 'openrouter', 'gemini', 'groq', 'mistral', 'ollama', 'lmstudio', 'custom'].map((id) => ({
  id, displayName: id === 'custom' ? 'Custom (OpenAI-compatible)' : id, protocol: 'openai_compat',
  baseUrl: 'https://', requiresKey: false, local: false, getKeyUrl: null, dataPolicyWarning: false,
  structuredOutput: true, maxTokensParam: 'max_tokens',
}));
const tasks = [
  ['screener_a', 'Screener A', 'p1', 'claude-sonnet-5'], ['screener_b', 'Screener B', 'p2', 'openai/gpt-5.2'],
  ['arbiter', 'Arbiter', 'p1', 'claude-sonnet-5'], ['extraction', 'Extraction', 'p1', 'claude-sonnet-5'],
  ['appraisal', 'Appraisal', null, null], ['protocol_search', 'Protocol and search', null, null], ['synthesis', 'Synthesis', null, null],
].map(([task, label, providerId, modelId]) => ({ task, label, providerId, modelId }));
const key = (present: boolean, hint: string | null = null) => ({ present, hint });
const sources = [
  ['eric', 'ERIC', 'none', 'No key needed', null, key(false), true],
  ['openalex', 'OpenAlex', 'key', 'Free key', 'https://openalex.org/settings/api', key(true, 'key ending 7Tx0'), true],
  ['wos', 'Web of Science', 'key', 'Free Starter plan or institutional key', 'x', key(false), false],
  ['scopus', 'Scopus', 'key_and_inst_token', 'API key + institution token', 'x', key(false), false],
  ['pubmed', 'PubMed', 'optional_key', 'Optional key raises rate limit', 'x', key(false), true],
  ['semantic_scholar', 'Semantic Scholar', 'optional_key', 'Optional', 'x', key(false), true],
  ['core', 'CORE', 'key', 'Free key · open-access full texts', 'x', key(false), false],
  ['unpaywall', 'Unpaywall', 'email', 'Email', null, key(false), false],
].map(([id, name, auth, note, getKeyUrl, k, ready]) => ({
  id, name, auth, note, getKeyUrl, docsUrl: 'x', key: k, instToken: auth === 'key_and_inst_token' ? key(false) : null, ready,
}));

// ?screen=batch: the batch upload card with three papers missing.
const waiting: ReportView[] = [
  ['pa', 'Chinese students and their experiences of UK higher education', 2011, '10.1080/03075079.2010.536528'],
  ['pb', 'Belonging and loneliness among international postgraduates in England', 2019, null],
  ['pc', 'Academic adjustment of international doctoral students in Australian universities', 2016, null],
].map(([reportId, title, year, doi]) => ({
  reportId: reportId as string, retrieval: 'not_retrieved', retrievalNote: null, pages: 0, ftDecision: null,
  ftReason: null, needsYou: false, studyId: null,
  record: { id: `r${reportId as string}`, title, year, doi, authors: ['Author'], abstract: null, venue: null },
} as unknown as ReportView));
const titleOf = (id: string) => waiting.find((w) => w.reportId === id)?.record.title ?? '';

mockIPC((cmd, args) => {
  const a = (args ?? {}) as Record<string, string>;
  switch (cmd) {
    case 'batch_upload_match': {
      // The mock gets the body, not the headers: match the DOI in the PDF bytes.
      const bytes = new TextDecoder().decode(args as unknown as Uint8Array);
      return bytes.includes('10.1080/03075079.2010.536528')
        ? { matched: true, reportId: 'pa', title: titleOf('pa'), stagedId: null }
        : { matched: false, reportId: null, title: null, stagedId: `0190${bytes.length}` };
    }
    case 'batch_match_staged': {
      const norm = (t: string) => t.toLowerCase().replace(/\s+/g, ' ');
      const hit = waiting.find((w) => norm(a.text ?? '').includes(norm(w.record.title ?? '')));
      return hit
        ? { matched: true, reportId: hit.reportId, title: hit.record.title, stagedId: null }
        : { matched: false, reportId: null, title: null, stagedId: a.stagedId };
    }
    case 'batch_assign_staged':
      return { matched: true, reportId: a.reportId, title: titleOf(a.reportId ?? ''), stagedId: null };
    case 'list_reviews':
      return [
        { id: REVIEW_ID, question: QUESTION, verification: 'unverified', createdAt: '2026-09-20T09:00:00Z', path: '/x.sift' },
        { id: 'rev2', question: 'What is the effect of retrieval practice on undergraduate exam performance?', verification: 'verified', createdAt: '2026-08-02T09:00:00Z', path: '/y.sift' },
      ];
    case 'review_progress': return progress();
    case 'prisma_svg': return flowSvg;
    case 'quality_checks':
      return {
        gate2: { judged: 100, sampled: 100, recall: 0.97 }, gate2Outcome: 'pass', kappaAb: 0.78, kappaPairs: 1228,
        quoteRate: 0.991, quotesChecked: 312, seedFound: [8, 8], drift: 0.04, driftN: 61,
      };
    case 'decision_log': return logEntries;
    case 'review_activity':
      return { token: 't', jobs: [['extract', 4, 50]], now: [] };
    case 'skip_stage': {
      const stage = a.stage ?? '';
      const k = {
        stage, name: NAMES[stage] ?? stage, eventId: Date.now(), at: new Date().toISOString(),
        rationale: stage === 'extraction'
          ? 'Data extraction was stopped by the review author after 23 studies were extracted. Only those studies contribute extracted data.'
          : `The review author stopped ${NAMES[stage] ?? stage}.`,
      };
      skips = [...skips, k];
      return k;
    }
    case 'resume_stage':
      skips = skips.filter((k) => k.stage !== a.stage);
      return null;
    case 'list_providers': return providers;
    case 'list_presets': return presets;
    case 'list_task_assignments': return tasks;
    case 'list_sources': return sources;
    case 'list_prices': return [];
    case 'check_base_url': return null;
    case 'review_spend': return 12.4;
    case 'get_settings':
      return { strictMode: true, warnDataPolicy: true, unpaywallEmail: null, libraryProxyPrefix: null, spendingCapUsd: 50, layaPython: null, institutionalAccess: false, bypassByDefault: false, authorName: null, authorAffiliation: null, fundingStatement: null, competingInterests: null, mcpEnabled: false, mcpPort: 31480 };
    case 'laya_status':
      return { available: true, python: '/opt/homebrew/bin/python3', version: '0.2.0', model: 'aac6fef/laya-mlx', modelDownloaded: true, message: 'Ready. Runs offline on this Mac.' };
    default: return null;
  }
});

function Screen() {
  const params = new URLSearchParams(window.location.search);
  switch (params.get('screen')) {
    case 'batch':
      return (
        <div style={{ maxWidth: 900, margin: '24px auto' }}>
          <BatchUpload reviewId="rev" waiting={waiting} />
        </div>
      );
    case 'review':
      return <ReviewShell reviewId={REVIEW_ID} stage={(params.get('stage') ?? 'overview') as StageId} />;
    case 'start':
      return <Start />;
    default:
      return <Settings from="preview" />;
  }
}

const root = document.getElementById('root');
if (root) {
  createRoot(root).render(
    <StrictMode>
      <QueryClientProvider client={new QueryClient()}>
        <Screen />
      </QueryClientProvider>
    </StrictMode>,
  );
}
