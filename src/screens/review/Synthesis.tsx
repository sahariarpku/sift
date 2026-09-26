import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { errorMessage } from '../../lib/ipc';
import { review, type SynthesisItem } from '../../lib/review';
import { Header, stageText, type ScreenProps } from './common';
import styles from './review.module.css';

const LEVELS = ['high', 'moderate', 'low', 'very low'];

/** A drafted judgement field as text; the draft may leave any of them out. */
function text(v: unknown): string {
  return typeof v === 'string' && v.trim() !== '' ? v : 'not rated';
}

/** A component with its note, e.g. "minor concerns: two studies lacked reflexivity". */
function withNote(v: unknown, note: unknown): string {
  return typeof note === 'string' && note.trim() !== '' ? `${text(v)}: ${note}` : text(v);
}

function Certainty({ reviewId, item }: { reviewId: string; item: SynthesisItem }) {
  const qc = useQueryClient();
  const confirm = useMutation({
    mutationFn: (overall: string) => review.confirmCertainty(reviewId, item.id, overall, null),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ['synthesis', reviewId] });
      void qc.invalidateQueries({ queryKey: ['gate3', reviewId] });
    },
  });
  const b = item.body;
  const confirmed = b.confirmed === true;
  const components: [string, string][] =
    item.kind === 'cerqual'
      ? [
          ['Methodological limitations', withNote(b.methodological_limitations, b.methodological_limitations_note)],
          ['Coherence', withNote(b.coherence, b.coherence_note)],
          ['Adequacy', withNote(b.adequacy, b.adequacy_note)],
          ['Relevance', withNote(b.relevance, b.relevance_note)],
        ]
      : [
          ['Risk of bias', text(b.risk_of_bias)],
          ['Inconsistency', text(b.inconsistency)],
          ['Indirectness', text(b.indirectness)],
          ['Imprecision', text(b.imprecision)],
          ['Publication bias', text(b.publication_bias)],
        ];
  // The draft may not carry an overall level; the author then chooses one before confirming.
  const overall = typeof b.overall === 'string' && LEVELS.includes(b.overall) ? b.overall : '';
  return (
    <div style={{ marginTop: 14, padding: 14, borderRadius: 8, background: 'var(--color-surface-muted)' }}>
      <div className={styles.row} style={{ justifyContent: 'space-between' }}>
        <strong>{item.kind === 'cerqual' ? 'GRADE-CERQual' : 'GRADE'} confidence</strong>
        <span className={`${styles.badge} ${confirmed ? styles.badgeOk : styles.badgeWarn}`}>{confirmed ? 'Confirmed by you' : 'Draft: needs you'}</span>
      </div>
      {components.map(([k, v]) => (
        <p key={k} className={styles.small} style={{ margin: '4px 0' }}>
          <strong>{k}:</strong> {v}
        </p>
      ))}
      {typeof b.explanation === 'string' && <p className={styles.small}>{b.explanation}</p>}
      <div className={styles.row}>
        <label className={styles.row}>
          <span className={styles.label}>Overall</span>
          <select
            className={styles.select}
            style={{ width: 180 }}
            value={overall}
            disabled={confirm.isPending}
            onChange={(e) => e.target.value && confirm.mutate(e.target.value)}
          >
            {overall === '' && (
              <option value="" disabled>
                Choose a level
              </option>
            )}
            {LEVELS.map((l) => (
              <option key={l} value={l}>
                {l}
              </option>
            ))}
          </select>
        </label>
        {!confirmed && (
          <button type="button" className={styles.button} disabled={overall === '' || confirm.isPending} onClick={() => confirm.mutate(overall)}>
            Confirm
          </button>
        )}
      </div>
      {confirm.isError && <p className={styles.error}>{errorMessage(confirm.error)}</p>}
    </div>
  );
}

export function Synthesis({ reviewId, question, progress }: ScreenProps) {
  const items = useQuery({ queryKey: ['synthesis', reviewId], queryFn: () => review.synthesis(reviewId) });
  const manuscript = useQuery({ queryKey: ['manuscript', reviewId], queryFn: () => review.manuscript(reviewId) });
  const all = items.data ?? [];
  const analytical = all.filter((i) => i.kind === 'analytical_theme');
  const swim = all.filter((i) => i.kind === 'swim_group');
  const descriptive = all.filter((i) => i.kind === 'descriptive_theme');
  const themeRows = new Map((manuscript.data?.themes ?? []).map((t) => [t.id, t]));

  return (
    <>
      <Header eyebrow={`Synthesis · ${stageText(progress, 'synthesis')}`} title={question} />
      {all.length === 0 && <p className={styles.empty}>The synthesis runs after extraction and appraisal.</p>}
      <div className={styles.column}>
        {analytical.map((t) => {
          const row = themeRows.get(t.id);
          const cert = all.find((c) => c.kind === 'cerqual' && c.parentId === t.id);
          return (
            <section key={t.id} className={styles.card}>
              <p className={styles.eyebrow}>Analytical theme</p>
              <h2 className={styles.cardTitle} style={{ fontFamily: 'var(--font-display)', fontSize: 24 }}>
                {t.label}
              </h2>
              <p className={styles.reading}>{t.body.statement as string}</p>
              {row && row.descriptive.length > 0 && (
                <p className={`${styles.small} ${styles.muted}`}>Draws on: {row.descriptive.join('; ')}</p>
              )}
              {row && row.quotes.length > 0 && (
                <details>
                  <summary className={styles.small}>{row.quotes.length} supporting quotes</summary>
                  {row.quotes.map(([study, q, p], i) => (
                    <p key={i} className={styles.quote}>
                      “{q}” ({study}
                      {p ? `, p. ${p}` : ''})
                    </p>
                  ))}
                </details>
              )}
              {cert && <Certainty reviewId={reviewId} item={cert} />}
            </section>
          );
        })}
        {descriptive.length > 0 && (
          <section className={styles.card}>
            <h2 className={styles.cardTitle}>Descriptive themes</h2>
            {descriptive.map((d) => (
              <div key={d.id} className={styles.metric}>
                <span>
                  <strong>{d.label}</strong> <span className={styles.muted}>· {d.body.description as string}</span>
                </span>
                <span className={styles.small}>{(d.body.code_ids as string[] | undefined)?.length ?? 0} codes</span>
              </div>
            ))}
          </section>
        )}
        {swim.map((g) => {
          const votes = g.body.votes as Record<string, number>;
          const grade = all.find((c) => c.kind === 'grade' && c.parentId === g.id);
          return (
            <section key={g.id} className={styles.card}>
              <p className={styles.eyebrow}>Outcome group (SWiM)</p>
              <h2 className={styles.cardTitle}>{g.label}</h2>
              <p>
                {votes.studies} studies: {votes.favoursIntervention} favour the intervention,{' '}
                {votes.favoursComparator} favour the comparator, {votes.noClearDifference} show no clear difference, {votes.unclear} unclear.
              </p>
              <p className={`${styles.small} ${styles.muted}`}>Meta-analysis: {g.body.meta_analysis as string}.</p>
              {grade && <Certainty reviewId={reviewId} item={grade} />}
            </section>
          );
        })}
      </div>
    </>
  );
}
