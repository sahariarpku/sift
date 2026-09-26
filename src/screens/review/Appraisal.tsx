import { useQuery } from '@tanstack/react-query';
import { review } from '../../lib/review';
import { Header, stageText, type ScreenProps } from './common';
import styles from './review.module.css';

const good = ['Yes', 'Low risk', 'Low'];
const bad = ['No', 'High risk', 'Serious', 'Critical'];

export function Appraisal({ reviewId, question, progress }: ScreenProps) {
  const rows = useQuery({ queryKey: ['appraisals', reviewId], queryFn: () => review.appraisals(reviewId) });
  const studies = useQuery({ queryKey: ['studies', reviewId], queryFn: () => review.studies(reviewId) });
  const label = new Map((studies.data ?? []).map((s) => [s.id, s.label]));
  const byStudy = new Map<string, typeof rows.data>();
  for (const r of rows.data ?? []) byStudy.set(r.studyId, [...(byStudy.get(r.studyId) ?? []), r]);

  return (
    <>
      <Header eyebrow={`Appraisal · ${stageText(progress, 'appraisal')}`} title={question} />
      {byStudy.size === 0 && <p className={styles.empty}>Studies are appraised after extraction.</p>}
      <div className={styles.column}>
        {[...byStudy.entries()].map(([sid, list]) => (
          <section key={sid} className={styles.card}>
            <div className={styles.cardHead}>
              <h2 className={styles.cardTitle}>{label.get(sid) ?? sid}</h2>
              <span className={`${styles.small} ${styles.muted}`}>{list?.[0]?.tool}</span>
            </div>
            <table className={styles.table}>
              <thead>
                <tr>
                  <th>Domain</th>
                  <th>Judgement</th>
                  <th>Rationale and evidence</th>
                </tr>
              </thead>
              <tbody>
                {(list ?? []).map((r, i) => (
                  <tr key={i}>
                    <td style={{ width: '32%' }}>{r.domain}</td>
                    <td>
                      <span className={`${styles.badge} ${good.includes(r.judgement) ? styles.badgeOk : bad.includes(r.judgement) ? styles.badgeExclude : styles.badgeWarn}`}>
                        {r.judgement}
                      </span>
                    </td>
                    <td>
                      {r.rationale}
                      {r.sourceQuote && (
                        <p className={styles.quote}>
                          “{r.sourceQuote}”{r.page ? ` (p. ${r.page})` : ''}
                        </p>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </section>
        ))}
      </div>
    </>
  );
}
