import { useRef, useState, type DragEvent } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { errorMessage } from '../../lib/ipc';
import { pageTexts } from '../../lib/pdf';
import { review, type ReportView } from '../../lib/review';
import { readFile } from './common';
import styles from './review.module.css';

type FileState =
  | { kind: 'reading' }
  | { kind: 'matched'; reportId: string; title: string }
  | { kind: 'unmatched'; stagedId: string }
  | { kind: 'failed'; error: string };

interface Item {
  key: string;
  name: string;
  state: FileState;
}

/** Files read at once; each can be tens of megabytes. */
const PARALLEL = 3;

/**
 * Drop in any number of PDFs the user downloaded themselves. Each is matched
 * to its record by DOI (file name or metadata) or by its title on the first
 * pages, and attached there. Files that match nothing wait for the user to
 * pick the record.
 */
export function BatchUpload({ reviewId, waiting }: { reviewId: string; waiting: ReportView[] }) {
  const qc = useQueryClient();
  const [items, setItems] = useState<Item[]>([]);
  const [dragging, setDragging] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  const seq = useRef(0);

  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ['reports', reviewId] });
    void qc.invalidateQueries({ queryKey: ['progress', reviewId] });
  };
  const update = (key: string, state: FileState) =>
    setItems((all) => all.map((i) => (i.key === key ? { ...i, state } : i)));

  const matchOne = async (key: string, file: File) => {
    try {
      const first = await review.batchUploadMatch(reviewId, file.name, await readFile(file));
      if (first.matched) {
        update(key, { kind: 'matched', reportId: first.reportId ?? '', title: first.title ?? '' });
        refresh();
        return;
      }
      const stagedId = first.stagedId ?? '';
      try {
        // A fresh copy: pdf.js may detach the buffer it is given.
        const pages = await pageTexts(await file.arrayBuffer(), 2);
        const second = await review.batchMatchStaged(reviewId, stagedId, file.name, pages.join('\n'));
        if (second.matched) {
          update(key, { kind: 'matched', reportId: second.reportId ?? '', title: second.title ?? '' });
          refresh();
          return;
        }
      } catch {
        // Unreadable text (a scan, say): the user picks the record below.
      }
      update(key, { kind: 'unmatched', stagedId });
    } catch (e) {
      update(key, { kind: 'failed', error: errorMessage(e) });
    }
  };

  const add = (files: File[]) => {
    const pdfs = files.filter((f) => f.type === 'application/pdf' || f.name.toLowerCase().endsWith('.pdf'));
    const batch = pdfs.map((file) => {
      seq.current += 1;
      return { key: `${seq.current}`, file };
    });
    setItems((all) => [...all, ...batch.map(({ key, file }) => ({ key, name: file.name, state: { kind: 'reading' } as const }))]);
    let next = 0;
    const worker = async () => {
      for (let job = batch[next]; job; job = batch[next]) {
        next += 1;
        await matchOne(job.key, job.file);
      }
    };
    void Promise.all(Array.from({ length: Math.min(PARALLEL, batch.length) }, worker));
  };

  const assign = async (item: Item, stagedId: string, reportId: string) => {
    try {
      const out = await review.batchAssignStaged(reviewId, stagedId, item.name, reportId);
      update(item.key, { kind: 'matched', reportId, title: out.title ?? '' });
      refresh();
    } catch (e) {
      update(item.key, { kind: 'failed', error: errorMessage(e) });
    }
  };

  const onDrop = (e: DragEvent) => {
    e.preventDefault();
    setDragging(false);
    add(Array.from(e.dataTransfer.files));
  };

  const attachedIds = new Set(items.flatMap((i) => (i.state.kind === 'matched' ? [i.state.reportId] : [])));
  const stillWaiting = waiting.filter((w) => !attachedIds.has(w.reportId));
  const matched = attachedIds.size;
  const reading = items.filter((i) => i.state.kind === 'reading').length;

  return (
    <section className={styles.card} style={{ marginBottom: 16 }} aria-labelledby="batch-title">
      <h2 id="batch-title" className={styles.cardTitle}>
        Upload PDFs you already have
      </h2>
      <p className={`${styles.small} ${styles.muted}`} style={{ marginTop: 6 }}>
        Add several PDFs at once, in any order and with any file names. Sift reads each one, finds its record among the{' '}
        {waiting.length} papers without a full text, and attaches it there.
      </p>
      <div
        className={`${styles.dropZone} ${dragging ? styles.dropZoneActive : ''}`}
        onDragOver={(e) => {
          e.preventDefault();
          setDragging(true);
        }}
        onDragLeave={() => setDragging(false)}
        onDrop={onDrop}
      >
        <p className={styles.small} style={{ margin: '0 0 10px' }}>
          Drop PDFs here, or
        </p>
        <button type="button" className={`${styles.button} ${styles.primary}`} onClick={() => input.current?.click()}>
          Choose PDFs
        </button>
        <input
          ref={input}
          type="file"
          accept="application/pdf,.pdf"
          multiple
          className={styles.visuallyHidden}
          tabIndex={-1}
          aria-hidden="true"
          onChange={(e) => {
            add(Array.from(e.target.files ?? []));
            e.target.value = '';
          }}
        />
      </div>
      {items.length > 0 && (
        <>
          <p className={styles.small} role="status" style={{ marginTop: 12 }}>
            {matched} of {items.length} matched{reading > 0 ? `; reading ${reading}…` : '.'}
          </p>
          <ul className={`${styles.fileList} ${styles.small}`}>
            {items.map((item) => (
              <li key={item.key}>
                <span className={styles.mono}>{item.name}</span>
                {item.state.kind === 'reading' && (
                  <span className={`${styles.badge} ${styles.badgeMuted}`}>Reading</span>
                )}
                {item.state.kind === 'matched' && (
                  <>
                    <span className={`${styles.badge} ${styles.badgeOk}`}>Attached</span>
                    <span>{item.state.title}</span>
                  </>
                )}
                {item.state.kind === 'failed' && (
                  <>
                    <span className={`${styles.badge} ${styles.badgeExclude}`}>Not attached</span>
                    <span className={styles.error}>{item.state.error}</span>
                  </>
                )}
                {item.state.kind === 'unmatched' && (
                  <UnmatchedChoice
                    item={item}
                    stagedId={item.state.stagedId}
                    waiting={stillWaiting}
                    onPick={(stagedId, reportId) => void assign(item, stagedId, reportId)}
                  />
                )}
              </li>
            ))}
          </ul>
        </>
      )}
    </section>
  );
}

function UnmatchedChoice({
  item,
  stagedId,
  waiting,
  onPick,
}: {
  item: Item;
  stagedId: string;
  waiting: ReportView[];
  onPick: (stagedId: string, reportId: string) => void;
}) {
  return (
    <>
      <span className={`${styles.badge} ${styles.badgeWarn}`}>No match</span>
      <div style={{ flex: '1 1 280px', minWidth: 0 }}>
        <select
          aria-label={`Which paper is ${item.name}?`}
          className={styles.select}
          style={{ width: '100%' }}
          defaultValue=""
          onChange={(e) => e.target.value && onPick(stagedId, e.target.value)}
        >
          <option value="">This is…</option>
          {waiting.map((w) => (
            <option key={w.reportId} value={w.reportId}>
              {(w.record.title ?? 'Untitled').slice(0, 90)}
              {w.record.year ? ` (${w.record.year})` : ''}
            </option>
          ))}
        </select>
      </div>
    </>
  );
}
