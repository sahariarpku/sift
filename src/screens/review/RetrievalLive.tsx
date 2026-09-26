import type { RetrievalLive as Live } from '../../lib/review';
import styles from './review.module.css';

const ago = (s: number) => (s < 5 ? 'just now' : s < 90 ? `${s} s ago` : `${Math.round(s / 60)} min ago`);

/** What full-text retrieval is doing right now, paper by paper. */
export function RetrievalLive({ live }: { live: Live | undefined }) {
  if (!live || (live.active.length === 0 && live.recent.length === 0)) return null;
  const found = live.recent.filter((r) => r.found).length;
  return (
    <section className={styles.card} style={{ marginBottom: 16 }} aria-labelledby="live-title">
      <div className={styles.cardHead}>
        <h2 id="live-title" className={styles.cardTitle}>
          {live.active.length ? `Finding PDFs · ${live.active.length} in progress` : 'Finding PDFs · finished'}
        </h2>
        {live.recent.length > 0 && (
          <span className={`${styles.small} ${styles.muted}`}>
            Latest {live.recent.length}: {found} found, {live.recent.length - found} not found
          </span>
        )}
      </div>
      {live.active.length > 0 && (
        <ul style={{ listStyle: 'none', padding: 0, margin: '10px 0 0' }} aria-live="polite">
          {live.active.map((a) => (
            <li key={a.reportId} className={styles.row} style={{ gap: 10, alignItems: 'baseline', padding: '6px 0' }}>
              <span className={styles.spinner} aria-hidden="true" />
              <span style={{ flex: 1, minWidth: 0 }}>
                <strong className={styles.small}>{a.title}</strong>
                <span className={`${styles.small} ${styles.muted}`} style={{ display: 'block' }}>
                  {a.step} · {a.seconds} s
                </span>
              </span>
            </li>
          ))}
        </ul>
      )}
      {live.recent.length > 0 && (
        <details style={{ marginTop: 10 }} open={live.active.length === 0}>
          <summary className={styles.small}>Latest results</summary>
          <ul style={{ listStyle: 'none', padding: 0, margin: '8px 0 0' }}>
            {live.recent.slice(0, 12).map((r) => (
              <li key={`${r.reportId}-${r.secondsAgo}`} className={styles.small} style={{ padding: '4px 0' }}>
                <span
                  aria-label={r.found ? 'Found' : 'Not found'}
                  style={{ fontWeight: 700, color: r.found ? 'var(--color-accent)' : 'var(--color-exclude)', marginRight: 6 }}
                >
                  {r.found ? '✓' : '✗'}
                </span>
                {r.title}
                <span className={styles.muted}>
                  {' '}
                  · {r.detail} · {ago(r.secondsAgo)}
                </span>
              </li>
            ))}
          </ul>
        </details>
      )}
    </section>
  );
}
