import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useUi } from '../../app/store';
import { errorMessage, ipc } from '../../lib/ipc';
import { review, type ReportView } from '../../lib/review';
import { Header, stageText, readFile, type ScreenProps } from './common';
import { BatchUpload } from './BatchUpload';
import { DownloadHelper } from './DownloadHelper';
import { RetrievalLive } from './RetrievalLive';
import styles from './review.module.css';

function status(r: ReportView): { label: string; tone: string } {
  if (r.retrieval === 'not_retrieved') return { label: 'Not retrieved', tone: styles.badgeMuted ?? '' };
  if (r.retrieval === 'queued_for_upload') return { label: 'Waiting for upload', tone: styles.badgeWarn ?? '' };
  if (r.retrieval && r.pages === 0) return { label: 'Reading PDF', tone: styles.badgeMuted ?? '' };
  if (r.retrieval === 'uploaded') return { label: `Uploaded · ${r.pages} pages`, tone: styles.badgeOk ?? '' };
  if (r.retrieval === 'open_access') return { label: `Open access · ${r.pages} pages`, tone: styles.badgeOk ?? '' };
  if (r.retrieval === 'proxy') return { label: `Via institution · ${r.pages} pages`, tone: styles.badgeOk ?? '' };
  return { label: 'Looking for a copy', tone: styles.badgeMuted ?? '' };
}

const hasPdf = (r: ReportView) => ['open_access', 'proxy', 'uploaded'].includes(r.retrieval ?? '');

function PdfIcon() {
  return (
    <svg width="14" height="16" viewBox="0 0 14 16" aria-hidden="true" focusable="false">
      <path d="M2 0.75h6.5L12.25 4.5V15.25H2Z" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round" />
      <path d="M8.5 0.75V4.5h3.75" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round" />
      <text x="7" y="12.6" textAnchor="middle" fontSize="4.6" fontWeight="700" fill="currentColor" fontFamily="system-ui">
        PDF
      </text>
    </svg>
  );
}

export function FullText({ reviewId, question, progress }: ScreenProps) {
  const qc = useQueryClient();
  const go = useUi((s) => s.go);
  const live = useQuery({ queryKey: ['retrievalLive', reviewId], queryFn: () => review.retrievalLive(reviewId), refetchInterval: 1000 });
  const activeStep = new Map((live.data?.active ?? []).map((a) => [a.reportId, a.step]));
  const reports = useQuery({ queryKey: ['reports', reviewId], queryFn: () => review.reports(reviewId) });
  const settings = useQuery({ queryKey: ['settings'], queryFn: ipc.getSettings });
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ['reports', reviewId] });
    void qc.invalidateQueries({ queryKey: ['progress', reviewId] });
  };
  const upload = useMutation({
    mutationFn: async ({ reportId, file }: { reportId: string; file: File }) => review.uploadPdf(reviewId, reportId, await readFile(file)),
    onSuccess: refresh,
  });
  const notRetrieved = useMutation({ mutationFn: (ids: string[]) => review.markNotRetrieved(reviewId, ids), onSuccess: refresh });
  const proxy = useMutation({ mutationFn: (reportId: string) => review.openViaProxy(reviewId, reportId) });
  const openPdf = useMutation({ mutationFn: (reportId: string) => review.openPdf(reviewId, reportId) });

  const list = reports.data ?? [];
  const waiting = list.filter((r) => r.retrieval === 'queued_for_upload');
  // Papers a PDF can still be found for: waiting ones and those the deadline closed.
  const missing = list.filter((r) => r.retrieval === 'queued_for_upload' || r.retrieval === 'not_retrieved');
  const hasProxy = !!settings.data?.libraryProxyPrefix;
  const count = (f: (r: ReportView) => boolean) => list.filter(f).length;

  return (
    <>
      <Header eyebrow={`Full text · ${stageText(progress, 'fulltext')}`} title={question} />
      {waiting.length > 0 && (
        <div className={`${styles.banner} ${styles.bannerWarn}`}>
          <span>
            {waiting.length} full texts have not been found yet. Use the options below, upload a PDF{hasProxy ? ', open it through your library,' : ''} or mark it not retrieved. Papers still missing after the retrieval deadline are marked not retrieved automatically.
          </span>
          <button type="button" className={styles.button} onClick={() => notRetrieved.mutate(waiting.map((w) => w.reportId))}>
            Mark all {waiting.length} not retrieved
          </button>
        </div>
      )}
      <RetrievalLive live={live.data} />
      {missing.length > 0 && <BatchUpload reviewId={reviewId} waiting={missing} />}
      {missing.length > 0 && <DownloadHelper reviewId={reviewId} waiting={missing} />}
      <div className={styles.row} style={{ marginBottom: 16, gap: 24 }}>
        <span>{list.length} sought</span>
        <span>{count((r) => r.pages > 0)} with text</span>
        <span>{count((r) => r.ftDecision === 'include')} included</span>
        <span>{count((r) => r.ftDecision === 'exclude')} excluded</span>
        <span>{count((r) => r.retrieval === 'not_retrieved')} not retrieved</span>
      </div>
      {(upload.isError || notRetrieved.isError || proxy.isError || openPdf.isError) && (
        <p className={styles.error}>{errorMessage(upload.error ?? notRetrieved.error ?? proxy.error ?? openPdf.error)}</p>
      )}
      <section className={styles.card}>
        {list.length === 0 ? (
          <p className={styles.empty}>Full texts are sought after title and abstract screening and Gate 2.</p>
        ) : (
          <table className={styles.table}>
            <thead>
              <tr>
                <th>Report</th>
                <th>Copy</th>
                <th>Full-text decision</th>
                <th>
                  <span className="visually-hidden">Actions</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {list.map((r) => {
                const s = status(r);
                return (
                  <tr key={r.reportId}>
                    <td style={{ maxWidth: 460 }}>
                      <strong>{r.record.title}</strong>
                      <div className={`${styles.small} ${styles.muted}`}>
                        {r.record.authors[0] ?? 'Unknown author'}
                        {r.record.authors.length > 1 ? ' et al.' : ''} · {r.record.year ?? 'n.d.'}
                        {r.record.doi ? ` · doi:${r.record.doi}` : ''}
                      </div>
                    </td>
                    <td>
                      <div className={styles.row} style={{ gap: 8, alignItems: 'center' }}>
                        {activeStep.has(r.reportId) ? (
                          <span className={`${styles.badge} ${styles.badgeMuted}`} title={activeStep.get(r.reportId)}>
                            <span className={styles.spinner} aria-hidden="true" /> Searching…
                          </span>
                        ) : (
                          <span className={`${styles.badge} ${s.tone}`}>{s.label}</span>
                        )}
                        {hasPdf(r) && (
                          <button
                            type="button"
                            className={styles.button}
                            onClick={() => openPdf.mutate(r.reportId)}
                            aria-label={`Open the PDF of ${r.record.title ?? 'this report'}`}
                            title="Open the PDF"
                            style={{ display: 'inline-flex', alignItems: 'center', gap: 6, padding: '4px 10px' }}
                          >
                            <PdfIcon />
                            PDF
                          </button>
                        )}
                      </div>
                    </td>
                    <td>
                      {r.ftDecision === 'include' && <span className={`${styles.badge} ${styles.badgeOk}`}>Included</span>}
                      {r.ftDecision === 'exclude' && <span className={`${styles.badge} ${styles.badgeExclude}`}>Excluded · {r.ftReason}</span>}
                      {!r.ftDecision && r.needsYou && (
                        <button
                          type="button"
                          className={`${styles.badge} ${styles.badgeWarn}`}
                          onClick={() => go({ name: 'review', reviewId, stage: 'screening' })}
                          title="Open your full-text queue"
                        >
                          Needs your decision
                        </button>
                      )}
                      {!r.ftDecision && !r.needsYou && r.pages > 0 && <span className={styles.muted}>Screening</span>}
                    </td>
                    <td>
                      {r.retrieval === 'not_retrieved' && (
                        <label className={styles.button} title="You found the PDF yourself">
                          Upload PDF
                          <input
                            type="file"
                            accept="application/pdf"
                            className="visually-hidden"
                            onChange={(e) => {
                              const f = e.target.files?.[0];
                              if (f) upload.mutate({ reportId: r.reportId, file: f });
                              e.target.value = '';
                            }}
                          />
                        </label>
                      )}
                      {r.retrieval === 'queued_for_upload' && (
                        <div className={styles.row}>
                          <label className={styles.button}>
                            Upload PDF
                            <input
                              type="file"
                              accept="application/pdf"
                              className="visually-hidden"
                              onChange={(e) => {
                                const f = e.target.files?.[0];
                                if (f) upload.mutate({ reportId: r.reportId, file: f });
                                e.target.value = '';
                              }}
                            />
                          </label>
                          {hasProxy && (
                            <button type="button" className={styles.button} onClick={() => proxy.mutate(r.reportId)}>
                              Open via library
                            </button>
                          )}
                          <button type="button" className={styles.dangerLink} onClick={() => notRetrieved.mutate([r.reportId])}>
                            Not retrieved
                          </button>
                        </div>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </section>
    </>
  );
}
