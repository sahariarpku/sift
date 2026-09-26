import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { errorMessage } from '../../lib/ipc';
import { review, SOURCE_NAMES } from '../../lib/review';
import { Header, stageText, type ScreenProps } from './common';
import styles from './review.module.css';

export function Search({ reviewId, question, progress }: ScreenProps) {
  const qc = useQueryClient();
  const runs = useQuery({ queryKey: ['runs', reviewId], queryFn: () => review.runs(reviewId) });
  const prisma = useQuery({ queryKey: ['prisma', reviewId], queryFn: () => review.prisma(reviewId) });
  const quality = useQuery({ queryKey: ['quality', reviewId], queryFn: () => review.quality(reviewId) });
  const [label, setLabel] = useState('');
  const importFile = useMutation({
    mutationFn: async (file: File) => review.importRecords(reviewId, label.trim() || file.name, await file.text()),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ['runs', reviewId] });
      void qc.invalidateQueries({ queryKey: ['prisma', reviewId] });
    },
  });

  const all = runs.data ?? [];
  const current = all.filter((r) => r.current);
  const earlier = all.filter((r) => !r.current && !r.source.startsWith('seed'));
  const seeds = all.filter((r) => r.source.startsWith('seed'));
  const p = prisma.data;

  return (
    <>
      <Header eyebrow={`Search · ${stageText(progress, 'search')}`} title={question} />
      <div className={styles.grid}>
        <div className={styles.column}>
          <section className={styles.card} aria-labelledby="runs-title">
            <h2 id="runs-title" className={styles.cardTitle}>
              Searches
            </h2>
            {current.length === 0 && <p className={styles.empty}>No searches yet. They run after the protocol is locked.</p>}
            {current.map((r) => (
              <div key={r.id} style={{ padding: '14px 0', borderTop: '1px solid var(--color-line)' }}>
                <div className={styles.row} style={{ justifyContent: 'space-between' }}>
                  <strong>{SOURCE_NAMES[r.source] ?? r.source}</strong>
                  <span className={`${styles.small} ${styles.muted}`}>
                    {r.hits.toLocaleString()} hits · {r.records.toLocaleString()} records stored · iteration {r.iteration} · {r.runAt.slice(0, 16).replace('T', ' ')}
                  </span>
                </div>
                <p className={styles.mono} style={{ overflowWrap: 'anywhere' }}>
                  {r.queryString}
                </p>
                {r.simplifications.map((s, i) => (
                  <p key={i} className={`${styles.small} ${styles.muted}`} style={{ margin: '2px 0' }}>
                    Simplification: {s}
                  </p>
                ))}
              </div>
            ))}
            {earlier.length > 0 && (
              <p className={`${styles.small} ${styles.muted}`}>{earlier.length} earlier searches were replaced by a broadened rerun (kept in the log).</p>
            )}
          </section>
        </div>
        <div className={styles.column}>
          <section className={styles.card} aria-labelledby="counts-title">
            <h2 id="counts-title" className={styles.cardTitle}>
              Records
            </h2>
            {p && (
              <>
                {p.bySource.map(([s, n]) => (
                  <div key={s} className={styles.metric}>
                    <span>{SOURCE_NAMES[s] ?? s}</span>
                    <span>{n.toLocaleString()}</span>
                  </div>
                ))}
                <div className={styles.metric}>
                  <span>Duplicates removed</span>
                  <span>{p.duplicatesRemoved.toLocaleString()}</span>
                </div>
                <div className={styles.metric}>
                  <span>To screen</span>
                  <strong>{p.recordsScreened.toLocaleString()}</strong>
                </div>
              </>
            )}
          </section>
          <section className={styles.card} aria-labelledby="seed-title">
            <h2 id="seed-title" className={styles.cardTitle}>
              Seed-set check
            </h2>
            {seeds.length === 0 ? (
              <p className={styles.muted}>Not run yet.</p>
            ) : (
              <p>
                {quality.data?.seedFound
                  ? `${quality.data.seedFound[0]} of ${quality.data.seedFound[1]} seed studies were found by the full search.`
                  : 'Seed candidates found; the screeners are confirming them.'}
              </p>
            )}
          </section>
          <section className={styles.card} aria-labelledby="import-title">
            <h2 id="import-title" className={styles.cardTitle}>
              Import records
            </h2>
            <p className={`${styles.small} ${styles.muted}`}>
              Add results exported from another database (RIS, BibTeX or PubMed NBIB). They are deduplicated and screened like the rest.
            </p>
            <label className={styles.field}>
              <span className={styles.label}>Source name</span>
              <input className={styles.input} value={label} placeholder="e.g. Scopus export, 12 May" onChange={(e) => setLabel(e.target.value)} />
            </label>
            <label className={styles.field}>
              <span className={styles.label}>File</span>
              <input
                type="file"
                accept=".ris,.bib,.nbib,.txt"
                onChange={(e) => {
                  const f = e.target.files?.[0];
                  if (f) importFile.mutate(f);
                  e.target.value = '';
                }}
              />
            </label>
            {importFile.data != null && <p className={styles.small}>{importFile.data} records imported.</p>}
            {importFile.isError && <p className={styles.error}>{errorMessage(importFile.error)}</p>}
          </section>
        </div>
      </div>
    </>
  );
}
