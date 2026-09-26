import { useEffect, useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { errorMessage } from '../../lib/ipc';
import { review, type ValueRow } from '../../lib/review';
import { Header, Highlighted, type ScreenProps } from './common';
import styles from './review.module.css';

function valueText(v: ValueRow): string {
  if (v.value == null) return v.validation === 'not_reported' ? 'Not reported' : 'Cleared: quote not found in the PDF';
  if (typeof v.value === 'string') return v.value;
  if (typeof v.value === 'object' && v.value !== null && 'finding' in v.value) return String((v.value as { finding: unknown }).finding);
  return JSON.stringify(v.value);
}

export function Extraction({ reviewId, progress }: ScreenProps) {
  const qc = useQueryClient();
  const studies = useQuery({ queryKey: ['studies', reviewId], queryFn: () => review.studies(reviewId) });
  const [si, setSi] = useState(0);
  const study = studies.data?.[Math.min(si, (studies.data?.length ?? 1) - 1)];
  const data = useQuery({
    queryKey: ['extraction', reviewId, study?.id],
    queryFn: () => review.extraction(reviewId, study?.id ?? ''),
    enabled: !!study,
  });
  const pages = useQuery({
    queryKey: ['pages', reviewId, data.data?.reportId],
    queryFn: () => review.pages(reviewId, data.data?.reportId ?? ''),
    enabled: !!data.data?.reportId,
  });
  const values = data.data?.values ?? [];
  const labels = useMemo(() => new Map((data.data?.fields ?? []).map((f) => [f.key, f.label])), [data.data]);
  const [selected, setSelected] = useState<string | null>(null);
  const sel = values.find((v) => v.id === selected) ?? values.find((v) => v.validation === 'quote_not_found') ?? values.find((v) => v.page != null);
  const [page, setPage] = useState(1);
  useEffect(() => {
    if (sel?.page) setPage(sel.page);
  }, [sel?.id, sel?.page]);

  const [draft, setDraft] = useState({ value: '', quote: '', page: '' });
  const resolve = useMutation({
    mutationFn: (v: ValueRow) =>
      review.resolveValue(reviewId, {
        valueId: v.id,
        value: draft.value.trim() || null,
        quote: draft.quote.trim() || null,
        page: draft.page ? Number(draft.page) : null,
      }),
    onSuccess: () => {
      setDraft({ value: '', quote: '', page: '' });
      void qc.invalidateQueries({ queryKey: ['extraction', reviewId] });
      void qc.invalidateQueries({ queryKey: ['studies', reviewId] });
    },
  });

  const list = studies.data ?? [];
  if (list.length === 0) {
    return (
      <>
        <Header eyebrow="Extraction" title="Extracted data" />
        <p className={styles.empty}>
          {progress?.halt.kind === 'working' ? `${progress.halt.detail}.` : 'Data are extracted once full-text screening and linking finish.'}
        </p>
      </>
    );
  }
  const verified = values.filter((v) => v.validation === 'verified').length;
  const flagged = values.filter((v) => v.validation === 'quote_not_found').length;
  const text = pages.data?.[page - 1] ?? '';

  return (
    <>
      <Header
        eyebrow={`Extraction · study ${si + 1} of ${list.length}`}
        title={`${study?.label ?? ''} · ${study?.reports ?? 1} linked report${(study?.reports ?? 1) === 1 ? '' : 's'}`}
        actions={
          <div className={styles.row}>
            <button type="button" className={styles.button} onClick={() => setSi(Math.max(0, si - 1))} disabled={si === 0}>
              Previous study
            </button>
            <button type="button" className={styles.button} onClick={() => setSi(Math.min(list.length - 1, si + 1))} disabled={si >= list.length - 1}>
              Next study
            </button>
          </div>
        }
      />
      <div className={styles.gridEven}>
        <section className={styles.card} aria-labelledby="text-title" style={{ background: 'var(--color-surface-muted)' }}>
          <div className={styles.cardHead}>
            <h2 id="text-title" className={`${styles.small} ${styles.muted}`} style={{ margin: 0, fontWeight: 400 }}>
              Full text
            </h2>
            <span className={styles.row}>
              <button type="button" className={styles.link} onClick={() => setPage(Math.max(1, page - 1))} disabled={page <= 1}>
                Previous page
              </button>
              <span className={styles.small}>
                Page {page} of {pages.data?.length ?? 0}
              </span>
              <button type="button" className={styles.link} onClick={() => setPage(Math.min(pages.data?.length ?? 1, page + 1))} disabled={page >= (pages.data?.length ?? 1)}>
                Next page
              </button>
            </span>
          </div>
          <div className={styles.reading} style={{ background: 'var(--color-surface)', padding: 24, borderRadius: 8, whiteSpace: 'pre-wrap', maxHeight: 640, overflowY: 'auto' }}>
            <Highlighted text={text} quote={sel && sel.page === page ? sel.sourceQuote : null} />
          </div>
        </section>
        <section className={styles.card} aria-labelledby="fields-title">
          <div className={styles.cardHead}>
            <h2 id="fields-title" className={styles.cardTitle}>
              Extracted fields
            </h2>
            <span className={`${styles.small} ${styles.muted}`}>
              {verified} verified · {flagged} flagged
            </span>
          </div>
          {values.map((v) => {
            const isSel = sel?.id === v.id;
            const flaggedRow = v.validation === 'quote_not_found';
            return (
              <div
                key={v.id}
                style={{
                  padding: '12px 14px',
                  borderTop: '1px solid var(--color-line)',
                  background: flaggedRow ? 'var(--color-warn-soft)' : isSel ? 'var(--color-highlight)' : undefined,
                }}
              >
                <button
                  type="button"
                  onClick={() => setSelected(v.id)}
                  style={{ all: 'unset', cursor: 'pointer', display: 'block', width: '100%' }}
                  aria-label={`Show ${labels.get(v.field) ?? v.field} in the text`}
                >
                  <div className={styles.row} style={{ justifyContent: 'space-between' }}>
                    <strong>{labels.get(v.field) ?? v.field.replace(/_/g, ' ')}</strong>
                    <span className={`${styles.small} ${flaggedRow ? '' : styles.muted}`}>
                      {v.validation === 'verified' ? `Verified · p. ${v.page}` : flaggedRow ? 'Flagged' : 'Not reported'}
                    </span>
                  </div>
                  <div>{valueText(v)}</div>
                  {v.sourceQuote && <p className={styles.quote}>“{v.sourceQuote}”</p>}
                </button>
                {flaggedRow && isSel && (
                  <form
                    style={{ marginTop: 12 }}
                    onSubmit={(e) => {
                      e.preventDefault();
                      resolve.mutate(v);
                    }}
                  >
                    <p className={styles.small}>Enter the value with a quote copied exactly from the text, or leave the value empty to record it as not reported.</p>
                    <label className={styles.field}>
                      <span className={styles.label}>Value</span>
                      <input className={styles.input} value={draft.value} onChange={(e) => setDraft({ ...draft, value: e.target.value })} />
                    </label>
                    <label className={styles.field}>
                      <span className={styles.label}>Supporting quote</span>
                      <textarea className={styles.textarea} value={draft.quote} onChange={(e) => setDraft({ ...draft, quote: e.target.value })} />
                    </label>
                    <label className={styles.field}>
                      <span className={styles.label}>Page</span>
                      <input className={styles.input} inputMode="numeric" value={draft.page} onChange={(e) => setDraft({ ...draft, page: e.target.value })} />
                    </label>
                    <button type="submit" className={styles.primary} disabled={resolve.isPending}>
                      {draft.value.trim() ? 'Save verified value' : 'Record as not reported'}
                    </button>
                    {resolve.isError && <p className={styles.error}>{errorMessage(resolve.error)}</p>}
                  </form>
                )}
              </div>
            );
          })}
        </section>
      </div>
    </>
  );
}
