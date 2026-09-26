// Manuscript DOCX (§8.11) as an APA 7 professional paper, assembled in the
// window with the `docx` package from content Rust computed. Every number
// comes from the database.
//
// Layout follows the APA Publication Manual, 7th edition (paper format,
// sections 2.16 to 2.28; headings 2.27; tables and figures 7.8 to 7.24;
// reference list 9.43 to 9.52): Times New Roman 12 pt, double spacing,
// 1-inch margins, half-inch first-line indents, a running head with page
// numbers, title page, abstract, text, references, then appendices.
import {
  AlignmentType,
  BorderStyle,
  Document,
  Header,
  ImageRun,
  Packer,
  PageNumber,
  Paragraph,
  TabStopPosition,
  TabStopType,
  Table,
  TableCell,
  TableRow,
  TextRun,
  WidthType,
} from 'docx';
import { SOURCE_NAMES, type ApaReference, type Disclosure, type Manuscript, type Section } from './review';

const FONT = 'Times New Roman';
const SIZE = 24; // half-points: 12 pt
const TABLE_SIZE = 20; // 10 pt, allowed in tables (APA 7.13)
const INDENT = 720; // twips: 0.5 inch
const INCH = 1440;

type RunOpts = { bold?: boolean; italics?: boolean; size?: number; font?: string };

/**
 * Text runs for one paragraph. Bracketed placeholders and `[CHECK: ...]`
 * markers are highlighted so the author cannot miss them.
 */
function runs(text: string, o: RunOpts = {}): TextRun[] {
  return text
    .split(/(\[[^\]]+\])/)
    .filter((t) => t !== '')
    .map((t) =>
      /^\[[^\]]+\]$/.test(t)
        ? new TextRun({ text: t, ...o, highlight: 'yellow' })
        : new TextRun({ text: t, ...o }),
    );
}

/** Runs for text that marks italics with asterisks, e.g. "*8*(2), 45". */
function starRuns(text: string, o: RunOpts = {}): TextRun[] {
  return text
    .split(/(\*[^*]+\*)/)
    .filter((t) => t !== '')
    .map((t) => (t.startsWith('*') && t.endsWith('*') && t.length > 2 ? new TextRun({ text: t.slice(1, -1), ...o, italics: true }) : new TextRun({ text: t, ...o })));
}

const lines = (text: string) =>
  text
    .split(/\n+/)
    .map((t) => t.trim())
    .filter(Boolean);

/** Body paragraphs with a first-line indent. */
const body = (text: string): Paragraph[] => lines(text).map((t) => new Paragraph({ indent: { firstLine: INDENT }, children: runs(t) }));

const flush = (text: string, o: RunOpts = {}) => new Paragraph({ children: runs(text, o) });

const centred = (text: string, o: RunOpts = {}, pageBreakBefore = false) =>
  new Paragraph({ alignment: AlignmentType.CENTER, pageBreakBefore, children: runs(text, o) });

const MINOR = new Set(['a', 'an', 'the', 'and', 'but', 'or', 'nor', 'for', 'so', 'yet', 'as', 'at', 'by', 'in', 'of', 'off', 'on', 'per', 'to', 'up', 'via', 'vs']);

/** APA title case: major words and words of four letters or more capitalised. */
export function titleCase(s: string): string {
  let afterColon = true;
  return s
    .split(/(\s+)/)
    .map((w) => {
      if (/^\s+$/.test(w)) return w;
      const lower = w.toLowerCase();
      const first = afterColon;
      afterColon = /[:.?!]$/.test(w);
      if (/[A-Z]/.test(w.slice(1))) return w; // acronyms, iPad, PRISMA-S
      if (!first && MINOR.has(lower.replace(/[^a-z]/g, ''))) return lower;
      return lower.replace(/(^|-)([a-z])/g, (_, p: string, c: string) => p + c.toUpperCase());
    })
    .join('');
}

/** Level 1: centred, bold, title case. */
const h1 = (text: string, pageBreakBefore = false) => centred(titleCase(text), { bold: true }, pageBreakBefore);
/** Level 2: flush left, bold, title case. */
const h2 = (text: string) => new Paragraph({ keepNext: true, children: runs(titleCase(text), { bold: true }) });
/** Level 3: flush left, bold italic, title case. */
const h3 = (text: string) => new Paragraph({ keepNext: true, children: runs(titleCase(text), { bold: true, italics: true }) });

const RULE = { style: BorderStyle.SINGLE, size: 6, color: '000000' } as const;
const NONE = { style: BorderStyle.NONE, size: 0, color: 'FFFFFF' } as const;

/** An APA table: horizontal rules above and below the header and below the body only. */
function apaTable(head: string[], rows: string[][]): Table {
  const cell = (t: string, header: boolean, last: boolean) =>
    new TableCell({
      borders: { top: header ? RULE : NONE, bottom: header || last ? RULE : NONE, left: NONE, right: NONE },
      children: [new Paragraph({ spacing: { line: 240, before: 40, after: 40 }, children: runs(t, { size: TABLE_SIZE }) })],
    });
  const body = rows.length ? rows : [head.map((_, i) => (i === 0 ? 'None' : ''))];
  return new Table({
    width: { size: 100, type: WidthType.PERCENTAGE },
    borders: { top: NONE, bottom: NONE, left: NONE, right: NONE, insideHorizontal: NONE, insideVertical: NONE },
    rows: [
      new TableRow({ tableHeader: true, children: head.map((h) => cell(h, true, false)) }),
      ...body.map((r, i) => new TableRow({ cantSplit: true, children: r.map((c) => cell(c, false, i === body.length - 1)) })),
    ],
  });
}

class Numbering {
  table = 0;
  figure = 0;
}

/** "Table N" in bold, the title in italic title case, the table, then an optional note. */
function tableBlock(n: Numbering, title: string, head: string[], rows: string[][], note?: string): (Paragraph | Table)[] {
  n.table += 1;
  return [
    new Paragraph({ keepNext: true, children: [new TextRun({ text: `Table ${n.table}`, bold: true })] }),
    new Paragraph({ keepNext: true, children: [new TextRun({ text: titleCase(title), italics: true })] }),
    apaTable(head, rows),
    ...(note ? [new Paragraph({ spacing: { line: 240, before: 80 }, children: [new TextRun({ text: 'Note. ', italics: true, size: TABLE_SIZE }), ...runs(note, { size: TABLE_SIZE })] })] : []),
    new Paragraph({ children: [] }),
  ];
}

/** Renders the flow-diagram SVG to PNG bytes for embedding. */
export async function svgToPng(svg: string, scale = 2): Promise<{ data: Uint8Array; width: number; height: number } | null> {
  const m = /width="(\d+)" height="(\d+)"/.exec(svg);
  if (!m) return null;
  const width = Number(m[1]);
  const height = Number(m[2]);
  const url = URL.createObjectURL(new Blob([svg], { type: 'image/svg+xml' }));
  try {
    const img = new Image();
    await new Promise<void>((resolve, reject) => {
      img.onload = () => resolve();
      img.onerror = () => reject(new Error('svg render failed'));
      img.src = url;
    });
    const canvas = document.createElement('canvas');
    canvas.width = width * scale;
    canvas.height = height * scale;
    const ctx = canvas.getContext('2d');
    if (!ctx) return null;
    ctx.scale(scale, scale);
    ctx.drawImage(img, 0, 0);
    const blob = await new Promise<Blob | null>((r) => canvas.toBlob(r, 'image/png'));
    if (!blob) return null;
    return { data: new Uint8Array(await blob.arrayBuffer()), width, height };
  } catch {
    return null;
  } finally {
    URL.revokeObjectURL(url);
  }
}

const FIELD_ORDER = ['country', 'design', 'sample_size', 'participants'];

/** Where each PRISMA 2020 item sits in the manuscript, for the completed checklist. */
export function location(item: string): string {
  const n = Number(item);
  if (n === 1) return 'Title page';
  if (n === 2) return 'Abstract';
  if (n <= 4) return 'Introduction';
  if (n <= 15) return 'Method';
  if (n <= 22) return 'Results';
  if (n === 23) return 'Discussion';
  return 'Author note';
}

const KIND_LABEL: Record<Section['kind'], string> = {
  auto: 'Generated from the decision log',
  drafted: 'Drafted by an LLM for the author to revise',
  author: 'Written by the author',
};

const mono = (text: string) =>
  text.split(/\n/).map((t) => new Paragraph({ indent: { left: INDENT }, spacing: { line: 240, after: 60 }, children: [new TextRun({ text: t, font: 'Courier New', size: 18 })] }));

/** The running head: a short title of at most 50 characters in capitals (APA 2.18). */
export function runningHead(title: string): string {
  const base = (title.split(':')[0] ?? title).trim().toUpperCase();
  if (base.length <= 50) return base;
  let out = '';
  for (const w of base.split(/\s+/)) {
    if ((out ? `${out} ${w}` : w).length > 50) break;
    out = out ? `${out} ${w}` : w;
  }
  return out;
}

/** One reference with a hanging indent and its italic parts. */
function reference(r: ApaReference): Paragraph {
  const children: TextRun[] = [new TextRun(`${r.included ? '*' : ''}${r.authors} (${r.year}). `)];
  const title = r.title.trim();
  children.push(new TextRun(/[.?!]$/.test(title) ? `${title} ` : `${title}. `));
  if (r.source) {
    children.push(new TextRun({ text: r.source, italics: true }));
    if (r.detail) children.push(new TextRun(', '), ...starRuns(r.detail));
    children.push(new TextRun('. '));
  }
  if (r.doi) children.push(new TextRun(`https://doi.org/${r.doi}`));
  return new Paragraph({ indent: { left: INDENT, hanging: INDENT }, children });
}

/**
 * A quotation (APA 8.26 to 8.27): 40 words or more as an indented block
 * without quotation marks, the citation after the final punctuation;
 * shorter quotations in quotation marks.
 */
function quotation(q: string, cite: string): Paragraph {
  const t = q.trim();
  if (t.split(/\s+/).length >= 40) {
    return new Paragraph({ indent: { left: INDENT }, children: runs(`${/[.?!]$/.test(t) ? t : `${t}.`} (${cite})`) });
  }
  return new Paragraph({ indent: { left: INDENT }, children: runs(`“${t.replace(/[.]$/, '')}” (${cite}).`) });
}

/** PRISMA-S: every database, the full string exactly as sent, when, and what came back. */
function searchAppendix(m: Manuscript, n: Numbering): (Paragraph | Table)[] {
  const db = m.searches.filter((r) => r.source !== 'citation_chasing' && r.source !== 'manual_import');
  const other = m.searches.filter((r) => r.source === 'citation_chasing' || r.source === 'manual_import');
  const name = (s: string) => SOURCE_NAMES[s] ?? s;
  return [
    ...body(
      'Each database was searched through its public interface by the review software. The strings below are exactly as sent; the date is when the search was run, and the counts are the hits reported by the database and the records downloaded. Anything the database could not express is listed under the string.',
    ),
    ...tableBlock(
      n,
      'Searches run for this review',
      ['Source', 'Date searched', 'Iteration', 'Hits reported', 'Records downloaded'],
      m.searches.map((r) => [name(r.source), r.runAt.slice(0, 10), String(r.iteration), r.hits.toLocaleString('en-US'), r.records.toLocaleString('en-US')]),
    ),
    ...db.flatMap((r) => [h2(`${name(r.source)}, ${r.runAt.slice(0, 10)}`), ...mono(r.queryString), ...r.simplifications.flatMap((x) => body(`Note: ${x}`))]),
    ...(other.length
      ? [
          h2('Other methods'),
          ...other.flatMap((r) =>
            body(
              r.source === 'citation_chasing'
                ? `Citation searching (${r.runAt.slice(0, 10)}): backward and forward citations of the included studies were retrieved from OpenAlex (${r.records.toLocaleString('en-US')} records).`
                : `Records imported by the review author (${r.runAt.slice(0, 10)}): ${r.records.toLocaleString('en-US')} records.`,
            ),
          ),
        ]
      : []),
    h2('Deduplication'),
    ...body(
      'Duplicates were identified automatically: first by exact DOI match, then by normalized title similarity (Jaro-Winkler 0.95 or higher) together with a publication year within 1 year and a matching first-author surname. Pairs with a similarity from 0.90 to 0.95 were listed for the review author to check.',
    ),
  ];
}

function automationAppendix(d: Disclosure, n: Numbering): (Paragraph | Table)[] {
  return [
    ...body(
      `Large language model (LLM) agents made ${d.agentDecisions.toLocaleString('en-US')} decisions and the review author made ${d.humanDecisions.toLocaleString('en-US')}. The author overrode ${d.humanOverrides} screening decisions, and ${d.needsHumanItems} items were sent to the author because the agents could not decide or produced invalid output. Verification status: ${d.verification.label}.`,
    ),
    ...tableBlock(
      n,
      'Language models used, by task',
      ['Task', 'Provider', 'Model reported', 'Prompt', 'Calls', 'Tokens in/out'],
      d.models.map((x) => [x.task, x.provider, x.modelReported ?? x.modelRequested, x.promptVersion, String(x.calls), `${x.tokensIn}/${x.tokensOut}`]),
    ),
    ...(d.bypass.length
      ? [
          h2('Decisions made in bypass mode'),
          ...body(
            'The review author ran this review in bypass mode: every point that would have waited for a person was settled by a fixed rule instead. Each rule and the number of decisions it made are listed below. Because of this the review is unverified.',
          ),
          ...tableBlock(
            n,
            'Decisions made by bypass-mode rules',
            ['Stage', 'Decision', 'Count', 'Rule applied'],
            d.bypass.map((b) => [b.stage, b.decision, String(b.count), b.rule]),
          ),
        ]
      : []),
    ...(d.searchSimplifications.length ? [h2('Search simplifications'), ...d.searchSimplifications.flatMap(([src, list]) => body(`${src}: ${list.join(' ')}`))] : []),
    ...(d.prescreen ? [h2('Local pre-screen'), ...body(d.prescreen.note)] : []),
    ...(d.metaAnalysisNote ? [h2('Meta-analysis'), ...body(d.metaAnalysisNote)] : []),
  ];
}

export async function buildManuscript(m: Manuscript, d: Disclosure, flowSvg: string): Promise<Uint8Array> {
  const n = new Numbering();
  const byItem = new Map<string, Section>(m.sections.map((s) => [s.item, s]));
  const text = (item: string): string => {
    const s = byItem.get(item);
    if (!s) return '';
    return s.text.trim() || `[To be written by the author: PRISMA item ${item}, ${s.heading}]`;
  };
  /** An item under its own Level 2 heading. */
  const sec = (item: string): Paragraph[] => {
    const s = byItem.get(item);
    return s ? [h2(s.heading), ...body(text(item))] : [];
  };

  const png = await svgToPng(flowSvg);
  n.figure += 1;
  const figure: Paragraph[] = [
    new Paragraph({ keepNext: true, children: [new TextRun({ text: `Figure ${n.figure}`, bold: true })] }),
    new Paragraph({ keepNext: true, children: [new TextRun({ text: 'PRISMA 2020 Flow Diagram of Study Selection', italics: true })] }),
    png
      ? new Paragraph({
          alignment: AlignmentType.CENTER,
          spacing: { line: 240 },
          children: [new ImageRun({ type: 'png', data: png.data, transformation: { width: 600, height: Math.round((600 * png.height) / png.width) } })],
        })
      : flush('[Insert Figure 1 from prisma-flow-diagram.svg]'),
    new Paragraph({
      spacing: { line: 240, before: 80, after: 240 },
      children: [
        new TextRun({ text: 'Note. ', italics: true, size: TABLE_SIZE }),
        new TextRun({ text: 'Adapted from the PRISMA 2020 flow diagram template (Page et al., 2021). Counts are computed from the review decision log.', size: TABLE_SIZE }),
      ],
    }),
  ];

  const characteristics = tableBlock(
    n,
    'Characteristics of included studies',
    ['Study', 'Country', 'Design', 'Sample', 'Participants'],
    m.studies.map((s) => [s.citation || s.label, ...FIELD_ORDER.map((f) => s.values.find(([k]) => k === f)?.[1] ?? 'NR')]),
    'NR = not reported.',
  );
  const appraisal = tableBlock(
    n,
    'Methodological quality of included studies',
    ['Study', 'Tool', 'Judgements by domain'],
    m.studies.map((s) => [s.citation || s.label, s.tool ?? 'Not appraised', s.appraisal.map(([dom, j]) => `${dom}: ${j}`).join('; ')]),
  );
  const themes = m.themes.flatMap((t) => [
    h3(t.label),
    ...body(t.statement),
    ...t.quotes.slice(0, 3).map(([study, q, p]) => quotation(q, `${study}${p ? `, p. ${p}` : ''}`)),
  ]);
  const certainty = m.themes.length
    ? tableBlock(
        n,
        'Confidence in the review findings (GRADE-CERQual)',
        ['Review finding', 'Confidence', 'Confirmed by author'],
        m.themes.map((t) => [t.label, t.certainty ?? 'Not assessed', t.certaintyConfirmed ? 'Yes' : 'No']),
      )
    : [];

  const refs: Paragraph[] = m.apaReferences.length
    ? m.apaReferences.map(reference)
    : m.references.map((r) => new Paragraph({ indent: { left: INDENT, hanging: INDENT }, children: runs(r) }));

  const bypassNote =
    m.bypassDecisions > 0
      ? ` The review was run in bypass mode: ${m.bypassDecisions} decisions that would have waited for a person were made by fixed rules, listed in Appendix A.`
      : '';
  const authorNote: Paragraph[] = [
    centred('Author Note', { bold: true }),
    ...['24', '27', '25', '26'].flatMap((i) => (byItem.get(i) ? body(text(i)) : [])),
    ...body(
      `This review was conducted with the Sift review software, in which large language model agents screened records, extracted data and drafted text under the review author's supervision; every automated decision is disclosed in Appendix A. Verification status: ${m.verification}.${bypassNote}`,
    ),
    ...body(`Correspondence concerning this article should be addressed to ${m.authorName?.trim() || '[author name]'}, ${m.affiliation?.trim() || '[affiliation]'}. Email: [email address]`),
  ];

  const appendices: { title: string; content: (Paragraph | Table)[] }[] = [
    { title: 'Automation disclosure', content: automationAppendix(d, n) },
    { title: 'Search strategies (PRISMA-S)', content: searchAppendix(m, n) },
    ...(d.skips.length
      ? [
          {
            title: 'Stages stopped early',
            content: [
              ...body('These stages were stopped before they finished. Work completed before each decision was kept and is reported; nothing was discarded.'),
              ...tableBlock(
                n,
                'Stages stopped before completion',
                ['Stage', 'Date', 'What was kept'],
                d.skips.map((k) => [k.name, k.at.slice(0, 10), k.rationale]),
              ),
            ],
          },
        ]
      : []),
    {
      title: 'PRISMA 2020 checklist',
      content: [
        ...body('Location of each PRISMA 2020 item (Page et al., 2021) in this manuscript, and who wrote it.'),
        ...tableBlock(
          n,
          'PRISMA 2020 checklist',
          ['Item', 'Topic', 'Location', 'Source', 'Status'],
          [...m.sections].sort((a, b) => Number(a.item) - Number(b.item)).map((x) => [x.item, x.heading, location(x.item), KIND_LABEL[x.kind], x.done ? 'Reported' : 'Not yet written']),
        ),
      ],
    },
  ];

  const keywords = m.keywords.map((k) => k.toLowerCase()).join(', ');

  const doc = new Document({
    creator: m.authorName ?? 'Sift',
    title: m.title,
    description: `Verification status: ${m.verification}`,
    styles: {
      default: {
        document: { run: { font: FONT, size: SIZE }, paragraph: { spacing: { line: 480, before: 0, after: 0 } } },
      },
    },
    sections: [
      {
        properties: { page: { margin: { top: INCH, bottom: INCH, left: INCH, right: INCH } } },
        headers: {
          default: new Header({
            children: [
              new Paragraph({
                spacing: { line: 240 },
                tabStops: [{ type: TabStopType.RIGHT, position: TabStopPosition.MAX }],
                children: [new TextRun(runningHead(m.title)), new TextRun({ children: ['\t', PageNumber.CURRENT] })],
              }),
            ],
          }),
        },
        children: [
          // Title page (APA 2.3): title three to four lines down, then byline and affiliation.
          new Paragraph({ alignment: AlignmentType.CENTER, spacing: { before: 2 * INCH }, children: runs(m.title, { bold: true }) }),
          new Paragraph({ children: [] }),
          centred(m.authorName?.trim() || '[Author name]'),
          centred(m.affiliation?.trim() || '[Department, university]'),
          new Paragraph({ children: [] }),
          new Paragraph({ children: [] }),
          ...authorNote,

          // Abstract (APA 2.9): one paragraph, no indent, then keywords.
          centred('Abstract', { bold: true }, true),
          ...lines(text('2')).map((t) => new Paragraph({ children: runs(t) })),
          new Paragraph({ indent: { firstLine: INDENT }, children: [new TextRun({ text: 'Keywords: ', italics: true }), new TextRun(keywords)] }),

          // Text: the title repeated; the introduction takes no heading.
          centred(m.title, { bold: true }, true),
          ...body(text('3')),
          ...body(text('4')),

          h1('Method'),
          ...['5', '6', '7', '8', '9', '10', '11', '12', '13', '14', '15'].flatMap(sec),

          h1('Results'),
          ...sec('16'),
          ...figure,
          ...sec('17'),
          ...characteristics,
          ...sec('18'),
          ...appraisal,
          ...sec('19'),
          ...sec('20'),
          ...themes,
          ...sec('21'),
          ...sec('22'),
          ...certainty,

          h1('Discussion'),
          ...body(text('23')),

          centred('References', { bold: true }, true),
          new Paragraph({ children: runs('References marked with an asterisk indicate studies included in the systematic review.') }),
          ...refs,

          ...appendices.flatMap((a, i) => {
            const letter = String.fromCharCode(65 + i);
            return [centred(`Appendix ${letter}`, { bold: true }, true), centred(titleCase(a.title), { bold: true }), ...a.content];
          }),
        ],
      },
    ],
  });
  const blob = await Packer.toBlob(doc);
  return new Uint8Array(await blob.arrayBuffer());
}
