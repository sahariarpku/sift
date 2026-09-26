import { useEffect } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { pageTexts } from '../lib/pdf';
import { review } from '../lib/review';

/**
 * Reads retrieved PDFs in the background while the app is open: pdf.js runs
 * in the window (§3), so this is where page texts come from. Renders nothing.
 */
export function PdfWorker() {
  const qc = useQueryClient();
  useEffect(() => {
    let stopped = false;
    const failed = new Set<string>();
    const tick = async () => {
      try {
        const pending = await review.pendingPdfTexts();
        for (const p of pending) {
          if (stopped || failed.has(p.reportId)) continue;
          try {
            const bytes = await review.readPdf(p.reviewId, p.reportId);
            const pages = await pageTexts(bytes);
            await review.storePageTexts(p.reviewId, p.reportId, pages);
          } catch {
            // An unreadable PDF: store no text so it returns to the upload queue.
            failed.add(p.reportId);
            await review.storePageTexts(p.reviewId, p.reportId, []).catch(() => undefined);
          }
          void qc.invalidateQueries({ queryKey: ['reports', p.reviewId] });
        }
      } catch {
        // The app may be starting up; try again on the next tick.
      }
      if (!stopped) window.setTimeout(tick, 4000);
    };
    void tick();
    return () => {
      stopped = true;
    };
  }, [qc]);
  return null;
}
