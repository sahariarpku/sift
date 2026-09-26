import { useMutation, useQueryClient } from '@tanstack/react-query';
import { createContext } from 'react';
import { Icon } from '../../components/Icon';
import { errorMessage } from '../../lib/ipc';
import { review, type Progress } from '../../lib/review';
import styles from './review.module.css';

/** What a skip keeps, said before the author confirms. The backend writes the exact counts once it is recorded. */
export const KEEPS: Record<string, { name: string; keeps: string }> = {
  search: {
    name: 'searching',
    keeps: 'Every record already found is kept and goes forward to screening.',
  },
  screening: {
    name: 'title and abstract screening',
    keeps: 'Decisions already made stand. Records not yet screened are reported as not screened and do not go forward.',
  },
  fulltext: {
    name: 'full-text retrieval and screening',
    keeps: 'Reports already classified stand. The rest are reported as awaiting classification or not retrieved.',
  },
  extraction: {
    name: 'data extraction',
    keeps: 'Studies already extracted keep their verified values and go forward to appraisal and synthesis.',
  },
  appraisal: {
    name: 'risk of bias and quality appraisal',
    keeps: 'Judgements already made are kept. Studies without one are reported as not appraised.',
  },
  synthesis: {
    name: 'synthesis',
    keeps: 'Whatever synthesis has finished is used in the manuscript.',
  },
  report: {
    name: 'manuscript drafting',
    keeps: 'You write the manuscript sections yourself. The checklist, flow diagram and exports still work.',
  },
};

function when(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso.slice(0, 10) : d.toLocaleDateString([], { day: 'numeric', month: 'short', year: 'numeric' });
}

export function useSkip(reviewId: string) {
  const qc = useQueryClient();
  const refresh = () =>
    Promise.all(
      [['progress', reviewId], ['prismaSvg', reviewId], ['prisma', reviewId], ['log', reviewId], ['disclosure', reviewId], ['manuscript', reviewId], ['gate3', reviewId]].map(
        (queryKey) => qc.invalidateQueries({ queryKey }),
      ),
    );
  const skip = useMutation({ mutationFn: (stage: string) => review.skipStage(reviewId, stage), onSuccess: refresh });
  const resume = useMutation({ mutationFn: (stage: string) => review.resumeStage(reviewId, stage), onSuccess: refresh });
  return { skip, resume };
}

/** Review context for the header, so every stage screen gets skip and resume without extra props. */
export const ShellContext = createContext<{ reviewId: string; stage: string; progress: Progress | null } | null>(null);

/**
 * Skip or resume one stage. Skipping asks first and says what is kept; the
 * decision is a human event disclosed in the methods section. `button`
 * renders the quiet trigger for the header; `panel` renders the confirmation
 * or the stopped notice beneath it.
 */
export function StageControl({
  reviewId,
  stage,
  progress,
  part,
  confirming,
  setConfirming,
}: {
  reviewId: string;
  stage: string;
  progress: Progress | null;
  part: 'button' | 'panel';
  confirming: boolean;
  setConfirming: (v: boolean) => void;
}) {
  const { skip, resume } = useSkip(reviewId);
  const info = KEEPS[stage];
  const state = progress?.stages.find((s) => s.id === stage);
  const record = progress?.skips.find((k) => k.stage === stage);
  if (!info || !state) return null;
  const error = skip.error ?? resume.error;
  const stopped = state.status === 'skipped' && record;

  if (part === 'button') {
    if (stopped || !state.skippable || confirming) return null;
    return (
      <button type="button" className={styles.quiet} onClick={() => setConfirming(true)}>
        <Icon name="skip" size={14} />
        Skip this stage
      </button>
    );
  }

  if (stopped) {
    return (
      <section className={`${styles.stageBar} ${styles.stageBarStopped}`} aria-label={`${info.name} stopped early`}>
        <span className={styles.stageBarIcon}>
          <Icon name="skip" size={16} />
        </span>
        <div className={styles.stageBarBody}>
          <p className={styles.stageBarTitle}>Stopped early on {when(record.at)}. The work already done is kept.</p>
          <p className={styles.stageBarText}>{record.rationale} This is disclosed in the methods section.</p>
          {error && <p className={styles.error}>{errorMessage(error)}</p>}
        </div>
        <button type="button" className={styles.button} disabled={resume.isPending} onClick={() => resume.mutate(stage)}>
          <Icon name="undo" size={15} />
          {resume.isPending ? 'Resuming' : 'Resume this stage'}
        </button>
      </section>
    );
  }

  if (!confirming || !state.skippable) return null;

  return (
    <section className={`${styles.stageBar} ${styles.stageBarConfirm}`} aria-label={`Skip ${info.name}`}>
      <span className={styles.stageBarIcon}>
        <Icon name="alert" size={16} />
      </span>
      <div className={styles.stageBarBody}>
        <p className={styles.stageBarTitle}>Skip {info.name}?</p>
        <p className={styles.stageBarText}>
          {info.keeps} Nothing already done is lost, and you can resume later. The skip is recorded in the decision log,
          disclosed in the methods section, and shown on the verification badge.
        </p>
        {error && <p className={styles.error}>{errorMessage(error)}</p>}
      </div>
      <div className={styles.bannerActions}>
        <button type="button" className={styles.button} onClick={() => setConfirming(false)}>
          Cancel
        </button>
        <button
          type="button"
          className={styles.primary}
          disabled={skip.isPending}
          onClick={() => skip.mutate(stage, { onSuccess: () => setConfirming(false) })}
        >
          {skip.isPending ? 'Skipping' : 'Skip and keep work'}
        </button>
      </div>
    </section>
  );
}
