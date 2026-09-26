import { useEffect, useRef, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useUi } from '../../app/store';
import { Icon } from '../../components/Icon';
import { errorMessage } from '../../lib/ipc';
import { review, type ActiveItem, type Activity, type LogEntry, type Progress } from '../../lib/review';
import { STAGES, type StageId } from '../../lib/stages';
import { time } from './common';
import styles from './review.module.css';

const LABEL: Record<string, string> = {
  protocol_draft: 'drafting the protocol',
  seed_search: 'building the seed set',
  screen_seed: 'checking seed studies',
  search_source: 'searching databases',
  seed_check: 'checking seed studies',
  search_broaden: 'broadening the search',
  dedup: 'removing duplicates',
  prescreen_ta: 'local pre-screen',
  screen_ta: 'title and abstract screening',
  drift_check: 'drift check',
  retrieve: 'finding full texts',
  screen_ft: 'full-text screening',
  citation_chasing: 'chasing citations',
  link_studies: 'linking reports to studies',
  extract: 'extracting data',
  appraise: 'appraising studies',
  synthesis: 'synthesising',
  draft_manuscript: 'drafting the manuscript',
};

/** Event-log stages whose decisions each screen lists under "Finished". */
const EVENT_STAGES: Partial<Record<StageId, string[]>> = {
  protocol: ['protocol', 'gate1'],
  search: ['search', 'dedup'],
  screening: ['ta_screen', 'ta_prescreen', 'gate2'],
  fulltext: ['retrieval', 'ft_screen'],
  extraction: ['extract', 'link'],
  appraisal: ['appraise'],
  synthesis: ['synth'],
  report: ['gate3'],
};

function clip(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n)}…` : s;
}

function elapsed(s: number): string {
  return s < 60 ? `${s} s` : `${Math.floor(s / 60)} min ${s % 60} s`;
}

function Now({ items, all }: { items: ActiveItem[]; all: boolean }) {
  if (items.length === 0) return <p className={`${styles.small} ${styles.muted}`}>Nothing in progress on this stage.</p>;
  return (
    <ul className={styles.log}>
      {items.map((i) => (
        <li key={`${i.stage}:${i.reportId}`} className={styles.liveItem}>
          <span className={styles.logTime}>{elapsed(i.seconds)}</span>
          <span className={styles.logText}>
            <strong>{clip(i.title, 90)}</strong>
            {all && <span className={styles.muted}> · {STAGES.find((x) => x.id === i.stage)?.label ?? i.stage}</span>}
            <br />
            {i.step || 'Starting'}
          </span>
        </li>
      ))}
    </ul>
  );
}

function Finished({ entries }: { entries: LogEntry[] }) {
  if (entries.length === 0) return <p className={`${styles.small} ${styles.muted}`}>No decisions recorded on this stage yet.</p>;
  return (
    <ul className={styles.log}>
      {entries.map((e) => (
        <li key={e.id} className={styles.liveItem}>
          <span className={styles.logTime}>{time(e.time)}</span>
          <span className={styles.logText}>
            {e.actor} · {e.decision.replace(/_/g, ' ')}
            {e.target && (
              <>
                {' · '}
                <strong>{clip(e.target, 80)}</strong>
              </>
            )}
            {e.detail && !e.target.endsWith(e.detail) && <span className={styles.muted}> · {clip(e.detail, 120)}</span>}
          </span>
        </li>
      ))}
    </ul>
  );
}

/**
 * Watches one cheap change signal for the review and refreshes every screen
 * the moment anything changes. Mounted once per review, whether or not the
 * activity panel is open.
 */
export function useActivityWatcher(reviewId: string): { activity: Activity | null; changedAt: number } {
  const qc = useQueryClient();
  const activity = useQuery({
    queryKey: ['activity', reviewId],
    queryFn: () => review.activity(reviewId),
    refetchInterval: 1500,
  });
  const last = useRef<string | null>(null);
  const [changedAt, setChangedAt] = useState(() => Date.now());

  const token = activity.data?.token ?? null;
  const lastRefresh = useRef(0);
  const pending = useRef<number | null>(null);
  useEffect(() => {
    if (token === null || token === last.current) return;
    const first = last.current === null;
    last.current = token;
    if (first) return;
    setChangedAt(Date.now());
    // Refresh the screens at most every 3 s while jobs stream changes in.
    const refresh = () => {
      pending.current = null;
      lastRefresh.current = Date.now();
      void qc.invalidateQueries({
        predicate: (q) => q.queryKey[0] !== 'activity' && q.queryKey.includes(reviewId),
      });
    };
    const wait = 3000 - (Date.now() - lastRefresh.current);
    if (wait <= 0) refresh();
    else if (pending.current === null) pending.current = window.setTimeout(refresh, wait);
  }, [token, qc, reviewId]);
  useEffect(
    () => () => {
      if (pending.current !== null) window.clearTimeout(pending.current);
    },
    [],
  );
  return { activity: activity.data ?? null, changedAt };
}

/** Re-renders every second so elapsed times stay current. */
function useTick() {
  const [, tick] = useState(0);
  useEffect(() => {
    const t = window.setInterval(() => tick((n) => n + 1), 1000);
    return () => window.clearInterval(t);
  }, []);
}

type Tone = 'working' | 'waiting' | 'idle';

function summary(activity: Activity | null, halt: Progress['halt'] | undefined, changedAt: number) {
  const jobs = activity?.jobs ?? [];
  const secs = Math.max(0, Math.round((Date.now() - changedAt) / 1000));
  if (jobs.length) {
    const parts = jobs
      .slice(0, 3)
      .map(([kind, running, queued]) => `${LABEL[kind] ?? kind}: ${running} running${queued ? `, ${queued} queued` : ''}`);
    return { tone: 'working' as Tone, text: `Working · ${parts.join(' · ')} · updated ${secs < 2 ? 'just now' : `${secs} s ago`}` };
  }
  switch (halt?.kind) {
    case 'waiting':
      return { tone: 'waiting' as Tone, text: `Waiting for you · ${halt.detail}` };
    case 'paused':
      return { tone: 'waiting' as Tone, text: `Paused · ${halt.detail}` };
    case 'done':
      return { tone: 'idle' as Tone, text: 'Finished' };
    default:
      return { tone: 'idle' as Tone, text: 'Idle' };
  }
}

const DOT: Record<Tone, string> = {
  working: 'var(--color-accent)',
  waiting: 'var(--color-warn-border)',
  idle: 'var(--color-pending)',
};

function StatusLine({ tone, text, wrap }: { tone: Tone; text: string; wrap?: boolean }) {
  return (
    <p className={`${styles.statusLine} ${wrap ? '' : styles.statusClip}`} role="status" aria-live="polite" title={text}>
      <span aria-hidden="true" className={styles.statusDot} style={{ background: DOT[tone] }} />
      {text}
    </p>
  );
}

/** Turns bypass mode on (after confirming) or off for one review. */
function BypassChip({ reviewId }: { reviewId: string }) {
  const qc = useQueryClient();
  const bypass = useQuery({ queryKey: ['bypass', reviewId], queryFn: () => review.getBypass(reviewId) });
  const [confirming, setConfirming] = useState(false);
  const confirmRef = useRef<HTMLButtonElement>(null);
  const set = useMutation({
    mutationFn: (on: boolean) => review.setBypass(reviewId, on),
    onSuccess: () => {
      setConfirming(false);
      void qc.invalidateQueries({ predicate: (q) => q.queryKey.includes(reviewId) });
    },
  });
  useEffect(() => {
    if (confirming) confirmRef.current?.focus();
  }, [confirming]);

  const on = bypass.data === true;
  return (
    <div className={styles.bypassWrap}>
      <button
        type="button"
        className={`${styles.chip} ${on ? styles.chipOn : ''}`}
        aria-pressed={on}
        disabled={bypass.isPending || set.isPending}
        onClick={() => (on ? set.mutate(false) : setConfirming(true))}
      >
        <Icon name="bolt" size={14} />
        Bypass {on ? 'on' : 'off'}
      </button>
      {confirming && (
        <div
          className={styles.popover}
          role="alertdialog"
          aria-labelledby="bypass-title"
          aria-describedby="bypass-body"
          onKeyDown={(e) => {
            if (e.key === 'Escape') setConfirming(false);
          }}
        >
          <p id="bypass-title" className={styles.popoverTitle}>
            Run without stopping for you?
          </p>
          <div id="bypass-body" className={styles.popoverBody}>
            <p>
              Sift will settle every point that is waiting for you with a fixed rule: gates are skipped, records the
              agents cannot settle are carried forward, missing full texts are marked not retrieved, and flagged
              extractions are left out of synthesis.
            </p>
            <p>
              Each of these is logged and listed in the report. This review will stay Unverified permanently, even if
              you turn bypass off later.
            </p>
          </div>
          {set.isError && <p className={styles.error}>{errorMessage(set.error)}</p>}
          <div className={styles.popoverActions}>
            <button ref={confirmRef} type="button" className={styles.primary} onClick={() => set.mutate(true)}>
              Turn bypass on
            </button>
            <button type="button" className={styles.button} onClick={() => setConfirming(false)}>
              Cancel
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

/** One line at the top of every review screen: status, bypass, and the panel toggle. */
export function ActivityBar({
  reviewId,
  activity,
  changedAt,
}: {
  reviewId: string;
  activity: Activity | null;
  changedAt: number;
}) {
  useTick();
  const qc = useQueryClient();
  const open = useUi((s) => s.activityOpen);
  const toggle = useUi((s) => s.toggleActivity);
  const halt = qc.getQueryData<Progress>(['progress', reviewId])?.halt;
  const s = summary(activity, halt, changedAt);
  return (
    <div className={styles.activityBar}>
      <StatusLine tone={s.tone} text={s.text} />
      <BypassChip reviewId={reviewId} />
      <button
        type="button"
        className={`${styles.chip} ${open ? styles.chipOn : ''}`}
        aria-pressed={open}
        aria-controls="activity-panel"
        onClick={toggle}
      >
        <Icon name="panel" size={14} />
        Activity
      </button>
    </div>
  );
}

/** The collapsible right-hand panel: what is running now and what just finished. */
export function ActivityPanel({
  reviewId,
  stage,
  activity,
  changedAt,
}: {
  reviewId: string;
  stage: StageId;
  activity: Activity | null;
  changedAt: number;
}) {
  useTick();
  const qc = useQueryClient();
  const toggle = useUi((s) => s.toggleActivity);
  const eventStages = EVENT_STAGES[stage] ?? null;
  const all = stage === 'overview';
  const finished = useQuery({
    queryKey: ['activityLog', reviewId, all ? 'all' : stage],
    queryFn: () => review.log(reviewId, 20, eventStages),
  });
  const halt = qc.getQueryData<Progress>(['progress', reviewId])?.halt;
  const s = summary(activity, halt, changedAt);
  const now = (activity?.now ?? []).filter((i) => all || i.stage === stage);
  const stageLabel = STAGES.find((x) => x.id === stage)?.label ?? stage;

  return (
    <aside id="activity-panel" className={styles.activityPanel} aria-label="Activity">
      <header className={styles.panelHead}>
        <h2 className={styles.panelTitle}>Activity</h2>
        <button type="button" className={styles.iconButton} onClick={toggle}>
          <Icon name="close" size={16} />
          <span className={styles.visuallyHidden}>Close activity panel</span>
        </button>
      </header>
      <div className={styles.panelBody}>
        <StatusLine tone={s.tone} text={s.text} wrap />
        <section aria-label="Working on now">
          <p className={styles.eyebrow}>Now{now.length ? ` · ${now.length}` : ''}</p>
          <Now items={now} all={all} />
        </section>
        <section aria-label="Finished recently">
          <p className={styles.eyebrow}>Finished recently{all ? '' : ` · ${stageLabel}`}</p>
          <Finished entries={finished.data ?? []} />
        </section>
      </div>
    </aside>
  );
}
