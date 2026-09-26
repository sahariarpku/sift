import { useCallback, useEffect, useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { errorMessage } from '../../lib/ipc';
import { review, type QueueItem, type RecordRow } from '../../lib/review';
import { Header, stageText, pct, type ScreenProps } from './common';
import styles from './review.module.css';

type Tab = 'calibration' | 'queue' | 'fulltext';

export function Screening({ reviewId, question, progress }: ScreenProps) {
  const cal = useQuery({ queryKey: ['calibration', reviewId], queryFn: () => review.calibration(reviewId) });
  const ta = useQuery({ queryKey: ['queue', reviewId, 'ta_screen'], queryFn: () => review.queue(reviewId, 'ta_screen') });
  const ft = useQuery({ queryKey: ['queue', reviewId, 'ft_screen'], queryFn: () => review.queue(reviewId, 'ft_screen') });
  const needsCalibration = !!cal.data && cal.data.sample.length > 0 && cal.data.outcome == null;
  const [tab, setTab] = useState<Tab>('calibration');
  useEffect(() => {
    if (!needsCalibration && (ta.data?.length ?? 0) > 0) setTab('queue');
  }, [needsCalibration, ta.data?.length]);

  return (
    <>
      <Header eyebrow={`Screening · ${stageText(progress, 'screening')}`} title={question} />
      <div className={styles.tabs} role="tablist">
        <button type="button" role="tab" className={styles.tab} aria-selected={tab === 'calibration'} onClick={() => setTab('calibration')}>
          Gate 2 calibration
        </button>
        <button type="button" role="tab" className={styles.tab} aria-selected={tab === 'queue'} onClick={() => setTab('queue')}>
          Your title and abstract queue ({ta.data?.length ?? 0})
        </button>
        <button type="button" role="tab" className={styles.tab} aria-selected={tab === 'fulltext'} onClick={() => setTab('fulltext')}>
          Your full-text queue ({ft.data?.length ?? 0})
        </button>
      </div>
      {tab === 'calibration' && <CalibrationPanel reviewId={reviewId} />}
      {tab === 'queue' && <QueuePanel reviewId={reviewId} stage="ta_screen" items={ta.data ?? []} />}
      {tab === 'fulltext' && <QueuePanel reviewId={reviewId} stage="ft_screen" items={ft.data ?? []} />}
    </>
  );
}

function Criteria({ reviewId }: { reviewId: string }) {
  const p = useQuery({ queryKey: ['protocol', reviewId], queryFn: () => review.protocol(reviewId) });
  const body = p.data?.locked?.body ?? p.data?.draft?.body;
  if (!body) return null;
  return (
    <section className={styles.card} aria-labelledby="criteria-title">
      <h2 id="criteria-title" className={styles.cardTitle}>
        Criteria
      </h2>
      <table className={styles.table}>
        <tbody>
          {[...body.inclusion_criteria, ...body.exclusion_criteria].map((c) => (
            <tr key={c.code}>
              <td className={styles.mono}>{c.code}</td>
              <td>{c.text}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}

function useExclusionCodes(reviewId: string): string[] {
  const p = useQuery({ queryKey: ['protocol', reviewId], queryFn: () => review.protocol(reviewId) });
  const body = p.data?.locked?.body ?? p.data?.draft?.body;
  return body ? [...body.exclusion_criteria, ...body.inclusion_criteria].map((c) => c.code) : [];
}

function RecordCard({ record }: { record: RecordRow }) {
  return (
    <>
      <p className={`${styles.small} ${styles.muted}`}>
        {[record.venue, record.year, record.source.toUpperCase()].filter(Boolean).join(' · ')}
      </p>
      <h2 style={{ fontFamily: 'var(--font-display)', fontSize: 26, margin: '8px 0' }}>{record.title ?? '(no title)'}</h2>
      <p className={`${styles.small} ${styles.muted}`}>{record.authors.join('; ')}</p>
      <p className={styles.reading}>{record.abstract ?? 'No abstract available.'}</p>
    </>
  );
}

function CalibrationPanel({ reviewId }: { reviewId: string }) {
  const qc = useQueryClient();
  const cal = useQuery({ queryKey: ['calibration', reviewId], queryFn: () => review.calibration(reviewId) });
  const codes = useExclusionCodes(reviewId);
  const [reason, setReason] = useState('');
  const sample = cal.data?.sample ?? [];
  const nextIndex = sample.findIndex(([, d]) => d == null);
  const [index, setIndex] = useState<number | null>(null);
  const current = index ?? (nextIndex === -1 ? null : nextIndex);
  const recordId = current != null ? sample[current]?.[0] : undefined;
  const record = useQuery({ queryKey: ['record', reviewId, recordId], queryFn: () => review.record(reviewId, recordId ?? ''), enabled: !!recordId });
  const judged = sample.filter(([, d]) => d != null).length;
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ['calibration', reviewId] });
    void qc.invalidateQueries({ queryKey: ['progress', reviewId] });
  };
  const judge = useMutation({
    mutationFn: (d: 'include' | 'exclude' | 'uncertain') => review.judge(reviewId, recordId ?? '', d, d === 'exclude' ? reason || null : null),
    onSuccess: () => {
      setReason('');
      setIndex(null);
      refresh();
    },
  });
  const finish = useMutation({ mutationFn: () => review.finishCalibration(reviewId), onSuccess: refresh });
  const skip = useMutation({ mutationFn: () => review.skipGate(reviewId, 2), onSuccess: refresh });
  const manual = useMutation({ mutationFn: () => review.handScreeningToMe(reviewId), onSuccess: refresh });

  const onKey = useCallback(
    (e: KeyboardEvent) => {
      if (!recordId || judge.isPending) return;
      const tag = (e.target as HTMLElement | null)?.tagName;
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
      const k = e.key.toLowerCase();
      if (k === 'i') judge.mutate('include');
      if (k === 'e') judge.mutate('exclude');
      if (k === 'u') judge.mutate('uncertain');
    },
    [recordId, judge],
  );
  useEffect(() => {
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onKey]);

  if (!cal.data) return null;
  if (sample.length === 0) {
    return <p className={styles.empty}>The calibration sample is drawn when title and abstract screening finishes.</p>;
  }
  const m = cal.data.metrics;
  if (cal.data.outcome || (current == null && m)) {
    return (
      <div className={styles.gridEven}>
        <section className={styles.card} aria-labelledby="result-title">
          <h2 id="result-title" className={styles.cardTitle}>
            {cal.data.outcome === 'pass' ? 'Gate 2 passed' : cal.data.outcome === 'fail' ? 'Gate 2 failed' : cal.data.outcome === 'skip' ? 'Gate 2 skipped' : 'Calibration complete'}
          </h2>
          {m && (
            <>
              <div className={styles.metric}>
                <span>Estimated recall (weighted by strata)</span>
                <span className={styles.metricValue}>{pct(m.recall)}</span>
              </div>
              <p className={`${styles.small} ${styles.muted}`}>
                95% CI {m.recallCi ? `${pct(m.recallCi[0])} to ${pct(m.recallCi[1])}` : 'n/a'} (bootstrap). Needs 95% or more.
              </p>
              <div className={styles.metric}>
                <span>Cohen&apos;s κ, you vs the agents</span>
                <span className={styles.metricValue}>{m.kappa?.toFixed(2) ?? 'n/a'}</span>
              </div>
              <p className={`${styles.small} ${styles.muted}`}>Needs 0.6 or more.</p>
              <table className={styles.table}>
                <thead>
                  <tr>
                    <th>Agent stratum</th>
                    <th>Records</th>
                    <th>You screened</th>
                    <th>You kept</th>
                  </tr>
                </thead>
                <tbody>
                  {m.strata.map((s) => (
                    <tr key={s.name}>
                      <td>{s.name}</td>
                      <td>{s.population}</td>
                      <td>{s.sampled}</td>
                      <td>{s.relevant}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </>
          )}
          {!cal.data.outcome && (
            <button type="button" className={styles.primary} style={{ marginTop: 16 }} onClick={() => finish.mutate()} disabled={finish.isPending}>
              Score calibration
            </button>
          )}
          {cal.data.outcome === 'fail' && !cal.data.manual && (
            <div style={{ marginTop: 16 }}>
              <p>The automated screening did not meet the threshold, so the pipeline has stopped. Choose how to continue:</p>
              <div className={styles.row}>
                <button type="button" className={styles.button} onClick={() => manual.mutate()}>
                  Screen the remaining records myself
                </button>
                <button type="button" className={styles.dangerLink} onClick={() => skip.mutate()}>
                  Skip Gate 2 (review stays unverified)
                </button>
              </div>
              <p className={`${styles.small} ${styles.muted}`}>To revise the screening prompts instead, a new prompt version is needed; see the methods guide.</p>
            </div>
          )}
          {(finish.isError || skip.isError) && <p className={styles.error}>{errorMessage(finish.error ?? skip.error)}</p>}
        </section>
      </div>
    );
  }

  return (
    <div className={styles.grid}>
      <section className={styles.card} aria-labelledby="record-title">
        <div className={styles.cardHead}>
          <h2 id="record-title" className={styles.cardTitle}>
            Record {(current ?? 0) + 1} of {sample.length}
          </h2>
          <span className={`${styles.small} ${styles.muted}`}>{judged} screened</span>
        </div>
        <div className={styles.progressBar} aria-hidden="true">
          <div className={styles.progressFill} style={{ width: `${(judged / sample.length) * 100}%` }} />
        </div>
        <div style={{ marginTop: 18 }}>{record.data ? <RecordCard record={record.data} /> : <p className={styles.muted}>Loading…</p>}</div>
        <p className={`${styles.small} ${styles.muted}`} style={{ marginTop: 24, padding: 12, background: 'var(--color-surface-muted)', borderRadius: 8 }}>
          The agents&apos; decisions on this record stay hidden until you finish all {sample.length}, so your judgement is independent.
        </p>
      </section>
      <div className={styles.column}>
        <section className={styles.card} aria-labelledby="decision-title">
          <h2 id="decision-title" className={styles.cardTitle}>
            Your decision
          </h2>
          <div style={{ display: 'grid', gap: 10, marginTop: 14 }}>
            <button type="button" className={styles.primary} onClick={() => judge.mutate('include')} disabled={!recordId || judge.isPending}>
              Include <span className={styles.kbd}>I</span>
            </button>
            <button
              type="button"
              className={styles.button}
              style={{ borderColor: 'var(--color-exclude)', color: 'var(--color-exclude)', fontWeight: 600 }}
              onClick={() => judge.mutate('exclude')}
              disabled={!recordId || judge.isPending}
            >
              Exclude <span className={styles.kbd}>E</span>
            </button>
            <button type="button" className={styles.button} onClick={() => judge.mutate('uncertain')} disabled={!recordId || judge.isPending}>
              Unsure <span className={styles.kbd}>U</span>
            </button>
          </div>
          <label className={styles.field} style={{ marginTop: 14 }}>
            <span className={styles.label}>Exclusion reason</span>
            <select className={styles.select} value={reason} onChange={(e) => setReason(e.target.value)}>
              <option value="">Choose a criterion</option>
              {codes.map((c) => (
                <option key={c}>{c}</option>
              ))}
            </select>
          </label>
          {judge.isError && <p className={styles.error}>{errorMessage(judge.error)}</p>}
          {current != null && current > 0 && (
            <button type="button" className={styles.link} onClick={() => setIndex(current - 1)}>
              Back to the previous record
            </button>
          )}
        </section>
        <Criteria reviewId={reviewId} />
        <p className={`${styles.small}`}>
          <button type="button" className={styles.dangerLink} onClick={() => skip.mutate()}>
            Skip Gate 2
          </button>{' '}
          <span className={styles.muted}>(the review stays unverified)</span>
        </p>
      </div>
    </div>
  );
}

function agentLine(label: string, e: QueueItem['screenerA']) {
  if (!e) return null;
  return (
    <p className={styles.small} style={{ margin: '6px 0' }}>
      <strong>{label}:</strong> {e.decision}
      {e.reason_code ? ` (${e.reason_code})` : ''}
      {e.confidence != null ? `, confidence ${e.confidence.toFixed(2)}` : ''}. {e.rationale}
      {e.evidence_quote && <span className={styles.quote} style={{ display: 'block' }}>“{e.evidence_quote}”</span>}
    </p>
  );
}

function QueuePanel({ reviewId, stage, items }: { reviewId: string; stage: 'ta_screen' | 'ft_screen'; items: QueueItem[] }) {
  const qc = useQueryClient();
  const codes = useExclusionCodes(reviewId);
  const [i, setI] = useState(0);
  const [reason, setReason] = useState('');
  const [note, setNote] = useState('');
  const item = useMemo(() => items[Math.min(i, Math.max(items.length - 1, 0))], [items, i]);
  const decide = useMutation({
    mutationFn: (d: 'include' | 'exclude') => review.decide(reviewId, stage, item?.targetId ?? '', d, d === 'exclude' ? reason || null : null, note || null),
    onSuccess: () => {
      setReason('');
      setNote('');
      void qc.invalidateQueries({ queryKey: ['queue', reviewId, stage] });
      void qc.invalidateQueries({ queryKey: ['progress', reviewId] });
    },
  });
  const retryable = useQuery({
    queryKey: ['retryable', reviewId, stage, items.length],
    queryFn: () => review.retryableCount(reviewId, stage),
  });
  const retry = useMutation({
    mutationFn: () => review.retryWithAgents(reviewId, stage),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ['queue', reviewId, stage] });
      void qc.invalidateQueries({ queryKey: ['retryable', reviewId, stage] });
      void qc.invalidateQueries({ queryKey: ['progress', reviewId] });
    },
  });
  if (!item) return <p className={styles.empty}>Nothing is waiting for you here.</p>;
  const canRetry = (retryable.data ?? 0) > 0;
  return (
    <div className={styles.grid}>
      <section className={styles.card}>
        <div className={styles.cardHead}>
          <h2 className={styles.cardTitle}>
            {Math.min(i, items.length - 1) + 1} of {items.length} waiting
          </h2>
          {canRetry && (
            <button type="button" className={styles.button} onClick={() => retry.mutate()} disabled={retry.isPending}>
              Retry {retryable.data} with AI
            </button>
          )}
        </div>
        {canRetry && (
          <p className={styles.small}>
            {retryable.data} of these are here because an agent gave no usable answer. Retrying asks the agents again; the
            new decisions replace the unusable ones in the log.
          </p>
        )}
        {retry.isSuccess && <p className={styles.small}>Sent {retry.data} records back to the AI screeners.</p>}
        {retry.isError && <p className={styles.error}>{errorMessage(retry.error)}</p>}
        <RecordCard record={item.record} />
        <hr style={{ border: 0, borderTop: '1px solid var(--color-line)', margin: '18px 0' }} />
        <h3 className={styles.label}>What the agents said</h3>
        {agentLine('Screener A', item.screenerA)}
        {agentLine('Screener B', item.screenerB)}
        {agentLine('Arbiter', item.arbiter)}
      </section>
      <div className={styles.column}>
        <section className={styles.card}>
          <h2 className={styles.cardTitle}>Your decision</h2>
          <label className={styles.field} style={{ marginTop: 12 }}>
            <span className={styles.label}>Exclusion reason</span>
            <select className={styles.select} value={reason} onChange={(e) => setReason(e.target.value)}>
              <option value="">Choose a criterion</option>
              {codes.map((c) => (
                <option key={c}>{c}</option>
              ))}
            </select>
          </label>
          <label className={styles.field}>
            <span className={styles.label}>Note (optional)</span>
            <input className={styles.input} value={note} onChange={(e) => setNote(e.target.value)} />
          </label>
          <div className={styles.row}>
            <button type="button" className={styles.primary} onClick={() => decide.mutate('include')} disabled={decide.isPending}>
              Include
            </button>
            <button type="button" className={styles.button} style={{ borderColor: 'var(--color-exclude)', color: 'var(--color-exclude)' }} onClick={() => decide.mutate('exclude')} disabled={decide.isPending}>
              Exclude
            </button>
          </div>
          {decide.isError && <p className={styles.error}>{errorMessage(decide.error)}</p>}
          <div className={styles.row} style={{ marginTop: 14 }}>
            <button type="button" className={styles.link} onClick={() => setI(Math.max(0, i - 1))} disabled={i === 0}>
              Previous
            </button>
            <button type="button" className={styles.link} onClick={() => setI(Math.min(items.length - 1, i + 1))} disabled={i >= items.length - 1}>
              Next
            </button>
          </div>
        </section>
        <Criteria reviewId={reviewId} />
      </div>
    </div>
  );
}
