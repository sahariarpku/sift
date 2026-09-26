import { useQuery } from '@tanstack/react-query';
import { useUi } from '../../app/store';
import { Sidebar } from '../../components/Sidebar';
import { errorMessage, ipc } from '../../lib/ipc';
import { review } from '../../lib/review';
import type { StageId, StageStatus } from '../../lib/stages';
import { Appraisal } from './Appraisal';
import { Extraction } from './Extraction';
import { FullText } from './FullText';
import { ActivityBar, ActivityPanel, useActivityWatcher } from './LiveActivity';
import { Overview } from './Overview';
import { Protocol } from './Protocol';
import { Report } from './Report';
import { Screening } from './Screening';
import { Search } from './Search';
import { ShellContext } from './StageControl';
import { Synthesis } from './Synthesis';
import styles from './review.module.css';

export function ReviewShell({ reviewId, stage }: { reviewId: string; stage: StageId }) {
  const go = useUi((s) => s.go);
  const reviews = useQuery({ queryKey: ['reviews'], queryFn: ipc.listReviews });
  const progress = useQuery({ queryKey: ['progress', reviewId], queryFn: () => review.progress(reviewId), refetchInterval: 2500 });
  const summary = reviews.data?.find((r) => r.id === reviewId);
  const { activity, changedAt } = useActivityWatcher(reviewId);
  const activityOpen = useUi((s) => s.activityOpen);

  if (reviews.isError) return <p className={styles.error}>{errorMessage(reviews.error)}</p>;
  if (!summary) return null;

  const statuses: Partial<Record<StageId, StageStatus>> = {};
  for (const s of progress.data?.stages ?? []) statuses[s.id as StageId] = s.status;
  if (progress.data) statuses.overview = progress.data.halt.kind === 'done' ? 'done' : progress.data.halt.kind === 'waiting' ? 'waiting' : 'running';
  const v = progress.data?.verification;

  const props = { reviewId, question: summary.question, progress: progress.data ?? null };
  const screen = (() => {
    switch (stage) {
      case 'overview':
        return <Overview {...props} />;
      case 'protocol':
        return <Protocol {...props} />;
      case 'search':
        return <Search {...props} />;
      case 'screening':
        return <Screening {...props} />;
      case 'fulltext':
        return <FullText {...props} />;
      case 'extraction':
        return <Extraction {...props} />;
      case 'appraisal':
        return <Appraisal {...props} />;
      case 'synthesis':
        return <Synthesis {...props} />;
      case 'report':
        return <Report {...props} />;
    }
  })();

  return (
    <div className={styles.shell}>
      <Sidebar
        question={summary.question}
        current={stage}
        statuses={statuses}
        verification={v?.verified ? 'verified' : 'unverified'}
        {...(v ? { verificationLabel: v.label } : {})}
        gatesPassed={v?.gatesPassed ?? 0}
        onSelect={(s) => go({ name: 'review', reviewId, stage: s })}
        onHome={() => go({ name: 'start' })}
        onSettings={() => go({ name: 'settings', from: reviewId })}
      />
      <main className={styles.main}>
        <ShellContext.Provider value={{ reviewId, stage, progress: progress.data ?? null }}>
          <ActivityBar reviewId={reviewId} activity={activity} changedAt={changedAt} />
          {screen}
        </ShellContext.Provider>
      </main>
      {activityOpen && <ActivityPanel reviewId={reviewId} stage={stage} activity={activity} changedAt={changedAt} />}
    </div>
  );
}
