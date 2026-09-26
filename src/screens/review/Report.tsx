import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { buildManuscript } from '../../lib/docx';
import { errorMessage } from '../../lib/ipc';
import { review, type Section } from '../../lib/review';
import { Header, pct, type ScreenProps } from './common';
import styles from './review.module.css';

/** Standard wording journals accept, offered as a starting point; the author still saves it. */
const TEMPLATES: Record<string, { label: string; text: string }[]> = {
  item3_rationale: [
    {
      label: 'Outline',
      text: '[What is already known about the topic, with key references.] [What existing reviews have not covered, or why their conclusions are out of date.] [Why answering this question matters for practice, policy or research.] This review addresses that gap.',
    },
  ],
  item25_support: [
    { label: 'No funding', text: 'This review received no specific grant from any funding agency in the public, commercial or not-for-profit sectors.' },
    {
      label: 'Funded',
      text: 'This review was funded by [funder] (grant [number]). The funder had no role in the design of the review, the searches, study selection, data extraction, synthesis, interpretation or the decision to publish.',
    },
  ],
  item26_competing: [
    { label: 'None', text: 'The authors declare no competing interests.' },
    {
      label: 'Declare one',
      text: '[Author initials] [describe the interest, for example authorship of an included study]. [He/She/They] did not take part in decisions about that study. The other authors declare no competing interests.',
    },
  ],
};

function Editor({ reviewId, section, onClose }: { reviewId: string; section: Section; onClose: () => void }) {
  const qc = useQueryClient();
  const [text, setText] = useState(section.text);
  const save = useMutation({
    mutationFn: () => review.setAuthorText(reviewId, section.key ?? '', text),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ['manuscript', reviewId] });
      void qc.invalidateQueries({ queryKey: ['gate3', reviewId] });
      onClose();
    },
  });
  return (
    <form
      style={{ padding: '10px 0 16px' }}
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <label className="visually-hidden" htmlFor={`edit-${section.item}`}>
        {section.heading}
      </label>
      {section.key && TEMPLATES[section.key] && (
        <div className={styles.row} style={{ gap: 8, marginBottom: 8 }}>
          <span className={`${styles.small} ${styles.muted}`}>Start from:</span>
          {TEMPLATES[section.key]?.map((t) => (
            <button key={t.label} type="button" className={styles.button} onClick={() => setText(t.text)}>
              {t.label}
            </button>
          ))}
        </div>
      )}
      <textarea id={`edit-${section.item}`} className={styles.textarea} style={{ minHeight: 180 }} value={text} onChange={(e) => setText(e.target.value)} />
      {/\[[^\]]+\]/.test(text) && <p className={`${styles.small} ${styles.muted}`}>Replace the text in square brackets before signing off.</p>}
      <div className={styles.row} style={{ marginTop: 8 }}>
        <button type="submit" className={styles.primary} disabled={save.isPending}>
          Save
        </button>
        <button type="button" className={styles.button} onClick={onClose}>
          Cancel
        </button>
      </div>
      {save.isError && <p className={styles.error}>{errorMessage(save.error)}</p>}
    </form>
  );
}

export function Report({ reviewId, progress }: ScreenProps) {
  const qc = useQueryClient();
  const m = useQuery({ queryKey: ['manuscript', reviewId], queryFn: () => review.manuscript(reviewId) });
  const d = useQuery({ queryKey: ['disclosure', reviewId], queryFn: () => review.disclosure(reviewId) });
  const g = useQuery({ queryKey: ['gate3', reviewId], queryFn: () => review.gate3(reviewId) });
  const [editing, setEditing] = useState<string | null>(null);
  const [showAll, setShowAll] = useState(false);
  const [saved, setSaved] = useState<string | null>(null);
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ['gate3', reviewId] });
    void qc.invalidateQueries({ queryKey: ['progress', reviewId] });
    void qc.invalidateQueries({ queryKey: ['manuscript', reviewId] });
  };
  const confirmLinks = useMutation({ mutationFn: () => review.confirmLinks(reviewId), onSuccess: refresh });
  const signOff = useMutation({ mutationFn: () => review.signOff(reviewId), onSuccess: refresh });
  const skip = useMutation({ mutationFn: () => review.skipGate(reviewId, 3), onSuccess: refresh });
  const exportKind = useMutation({ mutationFn: (k: 'ris' | 'csv' | 'svg') => review.exportFile(reviewId, k), onSuccess: setSaved });
  const docx = useMutation({
    mutationFn: async () => {
      const [man, disc, svg] = await Promise.all([review.manuscript(reviewId), review.disclosure(reviewId), review.prismaSvg(reviewId)]);
      return review.saveExport(reviewId, 'manuscript.docx', await buildManuscript(man, disc, svg));
    },
    onSuccess: setSaved,
  });
  const reveal = useMutation({ mutationFn: () => review.revealExports(reviewId) });

  const sections = m.data?.sections ?? [];
  const author = sections.filter((s) => s.kind === 'author');
  const drafted = sections.filter((s) => s.kind === 'drafted');
  const auto = sections.filter((s) => s.kind === 'auto');
  const req = g.data;
  const disc = d.data;
  const signed = progress?.halt.kind === 'done';

  const row = (s: Section, action: 'write' | 'draft' | 'view') => (
    <div key={s.item} style={{ borderTop: '1px solid var(--color-line)' }}>
      <div className={styles.row} style={{ justifyContent: 'space-between', padding: '12px 0' }}>
        <span>
          <span className={styles.mono} style={{ display: 'inline-block', width: 36 }}>
            {s.item}
          </span>
          {s.heading}
        </span>
        {action === 'write' && (
          <button type="button" className={styles.button} style={{ borderColor: 'var(--color-exclude)', color: 'var(--color-exclude)' }} onClick={() => setEditing(s.item)}>
            {s.done ? 'Edit' : 'Write'}
          </button>
        )}
        {action === 'draft' && (
          <button type="button" className={styles.link} onClick={() => setEditing(s.item)}>
            Review draft
          </button>
        )}
        {action === 'view' && (
          <button type="button" className={styles.link} onClick={() => setEditing(editing === s.item ? null : s.item)}>
            {editing === s.item ? 'Hide' : 'View'}
          </button>
        )}
      </div>
      {editing === s.item && action !== 'view' && <Editor reviewId={reviewId} section={s} onClose={() => setEditing(null)} />}
      {editing === s.item && action === 'view' && <p className={styles.small} style={{ whiteSpace: 'pre-wrap' }}>{s.text}</p>}
    </div>
  );

  const screeners = disc?.models.filter((x) => x.task === 'screener_a' || x.task === 'screener_b') ?? [];
  const g2 = disc?.quality.gate2;

  return (
    <>
      <Header eyebrow="Gate 3 of 3 · Report and sign-off" title="PRISMA 2020 checklist" />
      <div className={styles.grid}>
        <section className={styles.card} aria-labelledby="checklist-title">
          <h2 id="checklist-title" className="visually-hidden">
            Checklist
          </h2>
          <div className={styles.row} style={{ gap: 60, paddingBottom: 18, borderBottom: '1px solid var(--color-line)' }}>
            <div>
              <div className={styles.metricValue}>{auto.length}</div>
              <div className={styles.small}>Generated from the log</div>
            </div>
            <div>
              <div className={styles.metricValue} style={{ color: 'var(--color-ink)' }}>
                {drafted.length}
              </div>
              <div className={styles.small}>Drafted for you to edit</div>
            </div>
            <div>
              <div className={styles.metricValue} style={{ color: 'var(--color-exclude)' }}>
                {author.length}
              </div>
              <div className={styles.small}>Need your input</div>
            </div>
          </div>
          <p className={styles.eyebrow} style={{ color: 'var(--color-ink-muted)', marginTop: 18 }}>
            Needs you
          </p>
          {author.map((s) => row(s, 'write'))}
          <p className={styles.eyebrow} style={{ color: 'var(--color-ink-muted)', marginTop: 18 }}>
            Drafted
          </p>
          {drafted.map((s) => row(s, 'draft'))}
          {showAll && (
            <>
              <p className={styles.eyebrow} style={{ color: 'var(--color-ink-muted)', marginTop: 18 }}>
                Generated from the log
              </p>
              {auto.map((s) => row(s, 'view'))}
            </>
          )}
          <button type="button" className={styles.link} style={{ marginTop: 14 }} onClick={() => setShowAll(!showAll)}>
            {showAll ? 'Show fewer items' : `Show all ${sections.length} items`}
          </button>
        </section>

        <div className={styles.column}>
          <section className={styles.card} aria-labelledby="disclosure-title">
            <h2 id="disclosure-title" className={styles.cardTitle}>
              Automation disclosure
            </h2>
            {disc && (
              <p className={styles.reading}>
                {screeners.length > 0
                  ? disc.prescreen && disc.prescreen.singleScreened > 0
                    ? `Two LLM agents (${screeners.map((x) => x.modelReported ?? x.modelRequested).join(' and ')}) screened the records independently, except ${disc.prescreen.singleScreened} low-ranked records that one agent excluded (speed mode). `
                    : `Two LLM agents (${screeners.map((x) => x.modelReported ?? x.modelRequested).join(' and ')}) screened all ${m.data?.prisma.recordsScreened.toLocaleString() ?? ''} records independently. `
                  : 'LLM agents have not screened any records yet. '}
                {g2 && g2.recall != null
                  ? `The review author screened a stratified sample of ${g2.sampled}; estimated recall was ${pct(g2.recall)}. `
                  : disc.quality.gate2Outcome === 'skip'
                    ? 'Calibration against the author was skipped. '
                    : ''}
                {disc.quality.quotesChecked > 0 ? 'Every extracted value was checked against a verbatim quote in the source PDF.' : ''}
              </p>
            )}
            {disc?.prescreen && <p className={styles.reading}>{disc.prescreen.note}</p>}
            <p className={`${styles.small} ${styles.muted}`}>Model and prompt versions are listed in Appendix A of the manuscript.</p>
          </section>

          <section className={styles.card} aria-labelledby="export-title">
            <h2 id="export-title" className={styles.cardTitle}>
              Export
            </h2>
            <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 10, marginTop: 14 }}>
              <button type="button" className={styles.button} onClick={() => docx.mutate()} disabled={docx.isPending}>
                {docx.isPending ? 'Building…' : 'Manuscript .docx'}
              </button>
              <button type="button" className={styles.button} onClick={() => exportKind.mutate('svg')}>
                Flow diagram
              </button>
              <button type="button" className={styles.button} onClick={() => exportKind.mutate('ris')}>
                Included .ris
              </button>
              <button type="button" className={styles.button} onClick={() => exportKind.mutate('csv')}>
                Decision log .csv
              </button>
            </div>
            {saved && (
              <p className={styles.small}>
                Saved to {saved}.{' '}
                <button type="button" className={styles.link} onClick={() => reveal.mutate()}>
                  Open folder
                </button>
              </p>
            )}
            {(docx.isError || exportKind.isError) && <p className={styles.error}>{errorMessage(docx.error ?? exportKind.error)}</p>}
            <p className={`${styles.small} ${styles.muted}`}>Every export states the review&apos;s verification status.</p>
          </section>

          <section className={styles.card} style={{ background: 'var(--color-accent-soft)', borderColor: 'var(--color-accent-border)' }} aria-labelledby="signoff-title">
            <h2 id="signoff-title" className="visually-hidden">
              Sign off
            </h2>
            {signed ? (
              <p>Signed off. {progress?.verification.label}.</p>
            ) : (
              <>
                <p>Signing off marks every export as human-verified.</p>
                {req && (
                  <ul className={styles.small}>
                    <li>{req.flaggedValues === 0 ? 'No flagged extractions' : `${req.flaggedValues} flagged extractions to resolve (Extraction screen)`}</li>
                    <li>
                      {req.linksConfirmed ? (
                        'Report-to-study links confirmed'
                      ) : (
                        <>
                          {req.linksToConfirm} report-to-study links to confirm{' '}
                          <button type="button" className={styles.link} onClick={() => confirmLinks.mutate()}>
                            Confirm links
                          </button>
                        </>
                      )}
                    </li>
                    <li>{req.certaintyToConfirm === 0 ? 'Certainty judgements confirmed' : `${req.certaintyToConfirm} certainty judgements to confirm (Synthesis screen)`}</li>
                    <li>{req.missingAuthorItems.length === 0 ? 'Author sections written' : `Write: ${req.missingAuthorItems.join(', ')}`}</li>
                    {req.studiesNeedingYou > 0 && <li>{req.studiesNeedingYou} studies where an agent could not finish; check them</li>}
                  </ul>
                )}
                <button type="button" className={styles.primary} style={{ width: '100%' }} disabled={!req?.ready || signOff.isPending} onClick={() => signOff.mutate()}>
                  Sign off review
                </button>
                {signOff.isError && <p className={styles.error}>{errorMessage(signOff.error)}</p>}
                <p className={styles.small} style={{ marginTop: 12 }}>
                  <button type="button" className={styles.dangerLink} onClick={() => skip.mutate()}>
                    Skip Gate 3
                  </button>{' '}
                  <span className={styles.muted}>(exports stay marked unverified)</span>
                </p>
              </>
            )}
          </section>
        </div>
      </div>
    </>
  );
}
