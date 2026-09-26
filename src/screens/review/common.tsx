import { useContext, useState, type ReactNode } from 'react';
import type { Halt, Progress } from '../../lib/review';
import type { StageId } from '../../lib/stages';
import styles from './review.module.css';
import { ShellContext, StageControl } from './StageControl';

export interface ScreenProps {
  reviewId: string;
  question: string;
  progress: Progress | null;
}

/** Screen header. Inside a review it also carries the stage's skip and resume controls. */
export function Header({ eyebrow, title, actions }: { eyebrow: string; title: string; actions?: ReactNode }) {
  const shell = useContext(ShellContext);
  const [confirming, setConfirming] = useState(false);
  const control = shell ? { ...shell, confirming, setConfirming } : null;
  return (
    <>
      <header className={styles.header}>
        <div>
          <p className={styles.eyebrow}>{eyebrow}</p>
          <h1 className={styles.title}>{title}</h1>
        </div>
        <div className={styles.headerActions}>
          {control && <StageControl {...control} part="button" />}
          {actions}
        </div>
      </header>
      {control && <StageControl {...control} part="panel" />}
    </>
  );
}

export function haltText(h: Halt | undefined): string {
  if (!h) return 'Loading';
  if (h.kind === 'done') return 'Signed off';
  return h.detail;
}

/** Status line for one stage's screen: the run's halt only while this stage is the active one. */
export function stageText(progress: Progress | null, stage: StageId): string {
  if (!progress) return 'Loading';
  const status = progress.stages.find((s) => s.id === stage)?.status;
  if (status === 'done') return 'Complete';
  if (status === 'pending') return 'Not started';
  if (status === 'skipped') return 'Stopped early; the work already done is kept';
  return haltText(progress.halt);
}

export function pct(x: number | null | undefined, digits = 0): string {
  return x == null ? 'n/a' : `${(x * 100).toFixed(digits)}%`;
}

export function time(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? '' : d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', hour12: false });
}

export async function readFile(file: File): Promise<Uint8Array> {
  return new Uint8Array(await file.arrayBuffer());
}

/** Highlights the first occurrence of `quote` in `text` (whitespace-insensitive). */
export function Highlighted({ text, quote }: { text: string; quote: string | null }) {
  if (!quote) return <>{text}</>;
  const words = quote.trim().split(/\s+/).map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'));
  if (words.length === 0) return <>{text}</>;
  const re = new RegExp(words.join('[\\s\\u00AD-]+'));
  const m = re.exec(text);
  if (!m) return <>{text}</>;
  return (
    <>
      {text.slice(0, m.index)}
      <mark>{m[0]}</mark>
      {text.slice(m.index + m[0].length)}
    </>
  );
}
