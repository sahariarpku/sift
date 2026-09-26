import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { errorMessage, ipc, type AppSettings, type Assignment, type ProviderSummary } from '../../lib/ipc';
import styles from './Settings.module.css';

const SEP = '\u0000';

function optionValue(providerId: string, modelId: string) {
  return `${providerId}${SEP}${modelId}`;
}

export function TaskModelsCard({ settings }: { settings: AppSettings }) {
  const qc = useQueryClient();
  const providers = useQuery({ queryKey: ['providers'], queryFn: ipc.listProviders });
  const assignments = useQuery({ queryKey: ['assignments'], queryFn: ipc.listTaskAssignments });

  const assign = useMutation({
    mutationFn: ({ task, value }: { task: Assignment['task']; value: string }) => {
      const [providerId, modelId] = value ? value.split(SEP) : [];
      return ipc.setTaskAssignment(task, providerId ?? null, modelId ?? null);
    },
    onSuccess: (list) => qc.setQueryData(['assignments'], list),
  });

  const updateSettings = useMutation({
    mutationFn: (next: AppSettings) => ipc.updateSettings(next),
    onSuccess: (s) => qc.setQueryData(['settings'], s),
  });

  const list = providers.data ?? [];
  const byId = new Map(list.map((p) => [p.id, p]));
  const a = assignments.data ?? [];
  const screenerA = a.find((x) => x.task === 'screener_a');
  const screenerB = a.find((x) => x.task === 'screener_b');
  const sameScreenerProvider =
    screenerA?.providerId != null && screenerA.providerId === screenerB?.providerId;

  return (
    <section id="task-models" className={styles.card} aria-labelledby="tasks-title">
      <h2 id="tasks-title" className={styles.cardTitle}>
        Model for each task
      </h2>
      <p className={styles.cardIntro}>Use two different providers for the screeners so their mistakes are less alike.</p>

      {list.length === 0 && <p className={styles.hint}>Add a provider above, then fetch its models.</p>}

      {a.map((row) => {
        const provider = row.providerId ? byId.get(row.providerId) : undefined;
        const current = row.providerId && row.modelId ? optionValue(row.providerId, row.modelId) : '';
        const id = `task-${row.task}`;
        return (
          <div key={row.task}>
            <div className={styles.taskRow}>
              <label htmlFor={id} className={styles.taskLabel}>
                {row.label}
              </label>
              <select
                id={id}
                className={styles.select}
                value={current}
                disabled={assign.isPending}
                onChange={(e) => assign.mutate({ task: row.task, value: e.target.value })}
              >
                <option value="">Not set</option>
                {list.map((p) => (
                  <optgroup key={p.id} label={p.displayName}>
                    {modelIds(p, row).map((m) => (
                      <option key={m} value={optionValue(p.id, m)}>
                        {p.displayName} · {m}
                      </option>
                    ))}
                  </optgroup>
                ))}
              </select>
              <span className={styles.fallback}>Fallback: none</span>
            </div>
            {settings.warnDataPolicy && provider?.dataPolicyWarning && (
              <p className={styles.rowWarn}>
                {provider.displayName} may use prompts for training. Full texts will not be sent to it without your
                confirmation.
              </p>
            )}
          </div>
        );
      })}

      {sameScreenerProvider && (
        <p className={styles.warnText}>
          Screener A and Screener B use the same provider. Different providers make their mistakes less alike.
        </p>
      )}
      {list.some((p) => p.models.length === 0) && (
        <p className={styles.hint}>Providers without fetched models are listed empty. Use Fetch models when editing them.</p>
      )}
      {assign.isError && <p className={styles.warnText}>{errorMessage(assign.error)}</p>}

      <hr className={styles.divider} />
      <div className={styles.toggles}>
        <label className={styles.check}>
          <input
            type="checkbox"
            checked={settings.strictMode}
            onChange={(e) => updateSettings.mutate({ ...settings, strictMode: e.target.checked })}
          />
          <span>
            <strong>Strict mode.</strong> Never switch to another model mid-review. Keeps the run reproducible and the
            methods section accurate.
          </span>
        </label>
        <label className={styles.check}>
          <input
            type="checkbox"
            checked={settings.warnDataPolicy}
            onChange={(e) => updateSettings.mutate({ ...settings, warnDataPolicy: e.target.checked })}
          />
          <span>
            <strong>Warn about data policies.</strong> Flag providers whose free tiers may use your prompts for training.
          </span>
        </label>
        {updateSettings.isError && <p className={styles.warnText}>{errorMessage(updateSettings.error)}</p>}
      </div>
    </section>
  );
}

/** The provider's cached models, plus the assigned one if it is no longer listed. */
function modelIds(p: ProviderSummary, row: Assignment): string[] {
  const ids = p.models.map((m) => m.id);
  if (row.providerId === p.id && row.modelId && !ids.includes(row.modelId)) ids.unshift(row.modelId);
  return ids;
}
