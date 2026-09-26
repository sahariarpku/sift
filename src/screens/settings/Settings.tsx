import { useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { useUi } from '../../app/store';
import { APP_NAME } from '../../lib/app';
import { errorMessage, ipc } from '../../lib/ipc';
import { Icon } from '../../components/Icon';
import { ProvidersCard } from './ProvidersCard';
import { LocalModelCard } from './LocalModelCard';
import { DatabasesCard, FullTextCard, SpendingCard } from './SourcesCards';
import { TaskModelsCard } from './TaskModelsCard';
import { AuthorCard, AutomationCard, McpCard } from './AssistantCards';
import styles from './Settings.module.css';

const SECTIONS = [
  { id: 'ai-models', label: 'AI models' },
  { id: 'local-model', label: 'Local pre-screen' },
  { id: 'databases', label: 'Databases' },
  { id: 'full-text', label: 'Full-text access' },
  { id: 'spending', label: 'Spending' },
  { id: 'automation', label: 'Automation' },
  { id: 'author', label: 'Author details' },
  { id: 'assistants', label: 'Claude or ChatGPT' },
] as const;

export function Settings({ from }: { from: string | null }) {
  const go = useUi((s) => s.go);
  const settings = useQuery({ queryKey: ['settings'], queryFn: ipc.getSettings });
  const [active, setActive] = useState<string>('ai-models');

  const jump = (id: string) => {
    setActive(id);
    document.getElementById(id)?.scrollIntoView({ behavior: 'smooth', block: 'start' });
  };
  const back = () => go(from ? { name: 'review', reviewId: from, stage: 'overview' } : { name: 'start' });

  return (
    <div className={styles.shell}>
      <nav className={styles.nav} aria-label="Settings sections">
        <button type="button" className={styles.brand} onClick={back}>
          <span className={styles.mark} aria-hidden="true">
            S
          </span>
          {APP_NAME}
        </button>
        <p className={styles.navEyebrow}>Settings</p>
        <ul className={styles.navList}>
          {SECTIONS.map((s) => (
            <li key={s.id}>
              <button
                type="button"
                className={styles.navItem}
                aria-current={active === s.id ? 'true' : undefined}
                onClick={() => jump(s.id)}
              >
                {s.label}
              </button>
            </li>
          ))}
        </ul>
        <div className={styles.navSpacer} />
        <p className={styles.keyNote}>
          <Icon name="shield" size={17} className={styles.keyIcon} />
          <span>Keys are stored in the macOS Keychain. Each key is sent only to the service it belongs to.</span>
        </p>
      </nav>

      <main className={styles.main}>
        <header className={styles.header}>
          <div>
            <p className={styles.eyebrow}>Preferences</p>
            <h1 className={styles.title}>Settings</h1>
          </div>
          <button type="button" className={styles.small} onClick={back}>
            {from ? 'Back to review' : 'Back'}
          </button>
        </header>
        {settings.isError && <p className={styles.warnText}>{errorMessage(settings.error)}</p>}
        {settings.data && (
          <div className={styles.grid}>
            <div className={styles.column}>
              <ProvidersCard />
              <TaskModelsCard settings={settings.data} />
              <LocalModelCard settings={settings.data} />
              <McpCard settings={settings.data} />
            </div>
            <div className={styles.column}>
              <DatabasesCard />
              <FullTextCard settings={settings.data} />
              <SpendingCard settings={settings.data} reviewId={from} />
              <AutomationCard settings={settings.data} />
              <AuthorCard settings={settings.data} />
            </div>
          </div>
        )}
      </main>
    </div>
  );
}
