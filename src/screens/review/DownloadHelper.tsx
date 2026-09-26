import { useEffect, useRef, useState } from 'react';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { errorMessage } from '../../lib/ipc';
import { pageTexts } from '../../lib/pdf';
import { review, type DownloadFile, type ReportView } from '../../lib/review';
import styles from './review.module.css';

/**
 * Gets the remaining full texts through the user's institution: publisher
 * APIs and institutional access first, then the user's own browser, with
 * new PDFs in Downloads attached automatically.
 */
export function DownloadHelper({ reviewId, waiting }: { reviewId: string; waiting: ReportView[] }) {
  const qc = useQueryClient();
  const [watching, setWatching] = useState(false);
  const [attached, setAttached] = useState<[string, string][]>([]);
  const [unmatched, setUnmatched] = useState<DownloadFile[]>([]);
  const [error, setError] = useState<string | null>(null);
  const tried = useRef(new Set<string>());
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ['reports', reviewId] });
    void qc.invalidateQueries({ queryKey: ['progress', reviewId] });
  };

  const retry = useMutation({ mutationFn: () => review.retryRetrieval(reviewId), onSuccess: refresh });
  const exportRis = useMutation({ mutationFn: () => review.exportMissingRis(reviewId) });
  const zotero = useMutation({ mutationFn: () => review.importFromZotero(reviewId), onSuccess: refresh });
  const open = useMutation({
    mutationFn: () => review.openUploadBatch(reviewId, 5),
    onSuccess: () => setWatching(true),
  });
  const assign = useMutation({
    mutationFn: ({ path, reportId }: { path: string; reportId: string }) => review.assignDownload(reviewId, path, reportId),
    onSuccess: (title, v) => {
      setAttached((a) => [...a, [v.path.split('/').pop() ?? v.path, title]]);
      setUnmatched((u) => u.filter((f) => f.path !== v.path));
      refresh();
    },
    onError: (e) => setError(errorMessage(e)),
  });

  useEffect(() => {
    if (!watching) return;
    let stop = false;
    const tick = async () => {
      try {
        const out = await review.scanDownloads(reviewId);
        if (out.attached.length) {
          setAttached((a) => [...a, ...out.attached]);
          refresh();
        }
        const left: DownloadFile[] = [];
        for (const f of out.unmatched) {
          // Files without a DOI in their name or metadata: read the first pages once.
          if (!tried.current.has(f.path)) {
            tried.current.add(f.path);
            try {
              const pages = await pageTexts(await review.readDownload(f.path));
              const title = await review.matchDownloadText(reviewId, f.path, pages.slice(0, 2).join('\n'));
              if (title) {
                setAttached((a) => [...a, [f.name, title]]);
                refresh();
                continue;
              }
            } catch {
              // Unreadable PDF: offer the manual choice below.
            }
          }
          left.push(f);
        }
        setUnmatched(left);
        setError(null);
      } catch (e) {
        setError(errorMessage(e));
      }
      if (!stop) window.setTimeout(() => void tick(), 4000);
    };
    void tick();
    return () => {
      stop = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [watching, reviewId]);

  return (
    <section className={styles.card} style={{ marginBottom: 16 }} aria-labelledby="dl-title">
      <h2 id="dl-title" className={styles.cardTitle}>
        Find the {waiting.length} missing papers
      </h2>
      <ol className={styles.small} style={{ margin: '10px 0 14px', paddingLeft: 18, lineHeight: 1.6 }}>
        <li>
          On your institution&apos;s network or VPN, try the publishers again (institutional access and the Elsevier and
          Wiley APIs, set in Settings).{' '}
          <button type="button" className={styles.button} onClick={() => retry.mutate()} disabled={retry.isPending}>
            {retry.isPending ? 'Queuing…' : 'Try again through my institution'}
          </button>
          {retry.isSuccess && <span className={styles.muted}> Trying {retry.data} papers.</span>}
        </li>
        <li style={{ marginTop: 8 }}>
          For the rest, Sift opens each paper in your browser; click the publisher&apos;s PDF or Download button. New PDFs
          in your Downloads folder are attached automatically.{' '}
          <button type="button" className={styles.button} onClick={() => open.mutate()} disabled={open.isPending}>
            Open next 5 in my browser
          </button>{' '}
          {!watching && (
            <button type="button" className={styles.link} onClick={() => setWatching(true)}>
              Only watch Downloads
            </button>
          )}
          {open.isSuccess && open.data === 0 && <span className={styles.muted}> All waiting papers have been opened.</span>}
        </li>
        <li style={{ marginTop: 8 }}>
          With Zotero: import the missing papers into Zotero, select them and choose &quot;Find Available PDF&quot;
          (it uses your library access), then bring the PDFs into Sift.{' '}
          <button type="button" className={styles.button} onClick={() => exportRis.mutate()} disabled={exportRis.isPending}>
            Export missing papers for Zotero
          </button>{' '}
          <button type="button" className={styles.button} onClick={() => zotero.mutate()} disabled={zotero.isPending}>
            {zotero.isPending ? 'Importing…' : 'Import PDFs from Zotero'}
          </button>
          {exportRis.isSuccess && (
            <span className={styles.muted}>
              {' '}
              Saved {exportRis.data.split('/').pop()}.{' '}
              <button type="button" className={styles.link} onClick={() => void review.revealExports(reviewId)}>
                Show file
              </button>
            </span>
          )}
          {zotero.isSuccess && (
            <span className={styles.muted}>
              {' '}
              Attached {zotero.data.attached}; {zotero.data.stillMissing} still missing.
            </span>
          )}
        </li>
      </ol>
      {watching && (
        <p className={`${styles.small} ${styles.muted}`} role="status">
          Watching your Downloads folder{attached.length ? `: ${attached.length} attached so far` : ''}.
        </p>
      )}
      {attached.length > 0 && (
        <ul className={styles.small} style={{ paddingLeft: 18 }}>
          {attached.slice(-5).map(([file, title]) => (
            <li key={file + title}>
              {file} → {title}
            </li>
          ))}
        </ul>
      )}
      {unmatched.length > 0 && (
        <div style={{ marginTop: 10 }}>
          <h3 className={styles.label}>New PDFs Sift could not match</h3>
          {unmatched.map((f) => (
            <label key={f.path} className={styles.field} style={{ marginTop: 6 }}>
              <span className={styles.small}>{f.name}</span>
              <select
                className={styles.select}
                defaultValue=""
                onChange={(e) => e.target.value && assign.mutate({ path: f.path, reportId: e.target.value })}
              >
                <option value="">This is…</option>
                {waiting.map((w) => (
                  <option key={w.reportId} value={w.reportId}>
                    {(w.record.title ?? '').slice(0, 90)}
                  </option>
                ))}
              </select>
            </label>
          ))}
        </div>
      )}
      {(error || retry.isError || open.isError || zotero.isError || exportRis.isError) && (
        <p className={styles.error}>{error ?? errorMessage(retry.error ?? open.error ?? zotero.error ?? exportRis.error)}</p>
      )}
    </section>
  );
}
