import { APP_NAME } from '../lib/app';
import type { Verification } from '../lib/ipc';
import { STAGES, type StageId, type StageStatus } from '../lib/stages';
import { Icon } from './Icon';
import styles from './Sidebar.module.css';

const STATUS_TEXT: Record<StageStatus, string> = {
  done: 'done',
  running: 'in progress',
  waiting: 'waiting for you',
  pending: 'not started',
  skipped: 'stopped early',
};

/** Short visible tag for states that need attention; done and pending speak through the dot. */
const STATUS_TAG: Partial<Record<StageStatus, string>> = {
  running: 'Running',
  waiting: 'Needs you',
  skipped: 'Stopped',
};

/** The three human gates sit on these stages (CLAUDE.md §9). */
const GATE: Partial<Record<StageId, number>> = { protocol: 1, screening: 2, report: 3 };

export interface SidebarProps {
  question: string;
  current: StageId;
  statuses: Partial<Record<StageId, StageStatus>>;
  verification: Verification;
  /** Badge text, e.g. "Unverified · 2 of 3 gates passed". */
  verificationLabel?: string;
  gatesPassed: number;
  onSelect: (stage: StageId) => void;
  onHome: () => void;
  onSettings: () => void;
}

export function Sidebar(props: SidebarProps) {
  const { question, current, statuses, verification, verificationLabel, gatesPassed, onSelect, onHome, onSettings } = props;
  const verified = verification === 'verified';
  return (
    <nav className={styles.sidebar} aria-label="Review">
      <button type="button" className={styles.brand} onClick={onHome}>
        <span className={styles.mark} aria-hidden="true">
          S
        </span>
        <span className={styles.appName}>{APP_NAME}</span>
        <span className="visually-hidden">, all reviews</span>
      </button>

      <div className={styles.current}>
        <p className={styles.eyebrow}>Current review</p>
        <p className={styles.question} title={question}>
          {question}
        </p>
      </div>

      <p className={styles.eyebrow + ' ' + styles.sectionLabel} aria-hidden="true">
        Pipeline
      </p>
      <ul className={styles.stages}>
        {STAGES.map(({ id, label }) => {
          const status = statuses[id] ?? 'pending';
          const tag = STATUS_TAG[status];
          const gate = GATE[id];
          return (
            <li key={id} className={`${styles.item} ${styles['line_' + status] ?? ''}`}>
              <button
                type="button"
                className={styles.stage}
                aria-current={id === current ? 'page' : undefined}
                onClick={() => onSelect(id)}
              >
                <span className={`${styles.dot} ${styles[status]}`} aria-hidden="true" />
                <span className={status === 'pending' && id !== current ? styles.pendingLabel : styles.label}>
                  {label}
                </span>
                <span className="visually-hidden">, {STATUS_TEXT[status]}</span>
                {gate !== undefined && !tag && (
                  <span className={styles.gate} aria-hidden="true">
                    Gate {gate}
                  </span>
                )}
                {tag && (
                  <span className={`${styles.tag} ${styles['tag_' + status]}`} aria-hidden="true">
                    {tag}
                  </span>
                )}
              </button>
            </li>
          );
        })}
      </ul>

      <div className={styles.spacer} />

      <button type="button" className={styles.settings} onClick={onSettings}>
        <Icon name="settings" size={17} />
        Settings
      </button>

      <div className={`${styles.badge} ${verified ? styles.verified : styles.unverified}`}>
        <Icon name="shield" size={18} className={styles.badgeIcon} />
        <div>
          <p className={styles.badgeText}>
            {verificationLabel ?? `${verified ? 'Verified' : 'Unverified'} · ${gatesPassed} of 3 gates passed`}
          </p>
          <span className={styles.pips} aria-hidden="true">
            {[0, 1, 2].map((i) => (
              <span key={i} className={i < gatesPassed ? styles.pipOn : styles.pip} />
            ))}
          </span>
        </div>
      </div>
    </nav>
  );
}
