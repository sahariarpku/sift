// PDF text extraction with pdf.js (§3): one string per page, sent to Rust.
import * as pdfjs from 'pdfjs-dist/legacy/build/pdf.mjs';
import workerUrl from 'pdfjs-dist/legacy/build/pdf.worker.min.mjs?url';

pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;

interface TextItemLike {
  str?: string;
  hasEOL?: boolean;
}

/**
 * Text of each page, with line breaks where pdf.js reports ends of lines.
 * `maxPages` stops early (matching a file only needs its first pages).
 */
export async function pageTexts(data: ArrayBuffer, maxPages?: number): Promise<string[]> {
  const task = pdfjs.getDocument({ data: new Uint8Array(data) });
  const doc = await task.promise;
  const pages: string[] = [];
  const last = Math.min(doc.numPages, maxPages ?? doc.numPages);
  for (let i = 1; i <= last; i += 1) {
    const page = await doc.getPage(i);
    const content = await page.getTextContent();
    let text = '';
    for (const raw of content.items as TextItemLike[]) {
      if (typeof raw.str !== 'string') continue;
      text += raw.str;
      text += raw.hasEOL ? '\n' : ' ';
    }
    pages.push(text.replace(/[ \t]+\n/g, '\n').replace(/ {2,}/g, ' ').trim());
    page.cleanup();
  }
  await task.destroy();
  return pages;
}
