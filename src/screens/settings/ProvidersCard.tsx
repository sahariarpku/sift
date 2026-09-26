import { useEffect, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  errorMessage,
  ipc,
  type ModelInfo,
  type Preset,
  type ProviderDraft,
  type ProviderStatus,
  type ProviderSummary,
} from '../../lib/ipc';
import styles from './Settings.module.css';

const STATUS: Record<ProviderStatus, { label: string; tone: 'ok' | 'warn' | 'muted' }> = {
  connected: { label: 'Connected', tone: 'ok' },
  not_running: { label: 'Not running', tone: 'warn' },
  unreachable: { label: 'Unreachable', tone: 'warn' },
  failed: { label: 'Test failed', tone: 'warn' },
  untested: { label: 'Not tested', tone: 'muted' },
};

export function StatusBadge({ status }: { status: ProviderStatus }) {
  const { label, tone } = STATUS[status];
  const cls = tone === 'ok' ? styles.badgeOk : tone === 'warn' ? styles.badgeWarn : styles.badgeMuted;
  return <span className={`${styles.badge} ${cls}`}>{label}</span>;
}

function keyLabel(p: ProviderSummary): string {
  if (p.key.present) return p.key.hint ?? 'key saved';
  return p.requiresKey ? 'no key saved' : 'no key needed';
}

export function ProvidersCard() {
  const providers = useQuery({ queryKey: ['providers'], queryFn: ipc.listProviders });
  const presets = useQuery({ queryKey: ['presets'], queryFn: ipc.listPresets });
  const [editing, setEditing] = useState<ProviderSummary | null>(null);

  return (
    <section id="ai-models" className={styles.card} aria-labelledby="providers-title">
      <h2 id="providers-title" className={styles.cardTitle}>
        AI providers
      </h2>
      <p className={styles.cardIntro}>
        Any OpenAI-compatible endpoint works: a paid API, a free gateway, or a model running on this computer.
      </p>

      {providers.isError && <p className={styles.warnText}>{errorMessage(providers.error)}</p>}
      {providers.data && providers.data.length > 0 && (
        <ul className={styles.list} aria-label="Saved providers">
          {providers.data.map((p) => (
            <li key={p.id} className={styles.row}>
              <div className={styles.rowMain}>
                <p className={styles.rowName}>{p.displayName}</p>
                <p className={styles.rowMeta}>
                  {p.hostLabel} · {keyLabel(p)}
                </p>
                {p.urlWarning && <p className={styles.rowWarn}>{p.urlWarning}</p>}
                {p.status !== 'connected' && p.status !== 'untested' && p.statusMessage && (
                  <p className={styles.rowWarn}>{p.statusMessage}</p>
                )}
              </div>
              <StatusBadge status={p.status} />
              <button type="button" className={styles.small} onClick={() => setEditing(p)}>
                Edit
              </button>
            </li>
          ))}
        </ul>
      )}

      {presets.data && (
        <ProviderForm
          key={editing?.id ?? 'new'}
          presets={presets.data}
          editing={editing}
          onDone={() => setEditing(null)}
        />
      )}
    </section>
  );
}

function draftFrom(p: ProviderSummary): ProviderDraft {
  return {
    id: p.id,
    preset: p.preset,
    displayName: p.displayName,
    baseUrl: p.baseUrl,
    apiKey: null,
    removeKey: false,
    dataPolicyWarning: p.dataPolicyWarning,
    concurrency: p.concurrency,
    testModel: p.models[0]?.id ?? null,
  };
}

function blankDraft(preset: Preset): ProviderDraft {
  return {
    id: null,
    preset: preset.id,
    displayName: preset.id === 'custom' ? '' : preset.displayName,
    baseUrl: preset.baseUrl,
    apiKey: null,
    removeKey: false,
    dataPolicyWarning: preset.dataPolicyWarning,
    concurrency: 4,
    testModel: null,
  };
}

interface FormProps {
  presets: Preset[];
  editing: ProviderSummary | null;
  onDone: () => void;
}

export function ProviderForm({ presets, editing, onDone }: FormProps) {
  const qc = useQueryClient();
  const firstPreset = presets.find((p) => p.id === 'custom') ?? presets[0];
  const [draft, setDraft] = useState<ProviderDraft>(() =>
    editing ? draftFrom(editing) : firstPreset ? blankDraft(firstPreset) : blankDraft(presets[0] as Preset),
  );
  const [models, setModels] = useState<ModelInfo[]>(editing?.models ?? []);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  const [urlWarning, setUrlWarning] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const preset = presets.find((p) => p.id === draft.preset);

  useEffect(() => {
    let live = true;
    ipc
      .checkBaseUrl(draft.baseUrl)
      .then((w) => live && setUrlWarning(w))
      .catch(() => live && setUrlWarning(null));
    return () => {
      live = false;
    };
  }, [draft.baseUrl]);

  const set = <K extends keyof ProviderDraft>(k: K, v: ProviderDraft[K]) => setDraft((d) => ({ ...d, [k]: v }));

  const choosePreset = (id: string) => {
    const next = presets.find((p) => p.id === id);
    if (!next) return;
    setDraft((d) => ({ ...blankDraft(next), apiKey: d.apiKey }));
    setModels([]);
    setResult(null);
  };

  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ['providers'] });
    void qc.invalidateQueries({ queryKey: ['assignments'] });
  };

  const fetchModels = useMutation({
    mutationFn: () => ipc.fetchModels(draft),
    onSuccess: (list) => {
      setModels(list);
      if (!draft.testModel && list[0]) set('testModel', list[0].id);
      setResult({ ok: true, text: `${list.length} model${list.length === 1 ? '' : 's'} found.` });
      if (draft.id) refresh();
    },
    onError: (e) => setResult({ ok: false, text: errorMessage(e) }),
  });

  const test = useMutation({
    mutationFn: () => ipc.testProvider(draft),
    onSuccess: (o) => {
      setResult({ ok: o.ok && o.result?.modelMatch !== 'different', text: o.message });
      if (draft.id) refresh();
    },
    onError: (e) => setResult({ ok: false, text: errorMessage(e) }),
  });

  const save = useMutation({
    mutationFn: () => ipc.saveProvider(draft),
    onSuccess: () => {
      refresh();
      onDone();
      if (!editing && firstPreset) {
        setDraft(blankDraft(firstPreset));
        setModels([]);
        setResult(null);
      }
    },
    onError: (e) => setResult({ ok: false, text: errorMessage(e) }),
  });

  const remove = useMutation({
    mutationFn: () => ipc.deleteProvider(draft.id ?? ''),
    onSuccess: () => {
      refresh();
      onDone();
    },
    onError: (e) => setResult({ ok: false, text: errorMessage(e) }),
  });

  const busy = fetchModels.isPending || test.isPending || save.isPending || remove.isPending;
  const keyPlaceholder = editing?.key.present
    ? `${editing.key.hint ?? 'Key saved'}. Leave blank to keep it.`
    : preset?.requiresKey
      ? 'Paste your API key'
      : 'Optional';

  return (
    <form
      className={styles.form}
      aria-labelledby="provider-form-title"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <h3 id="provider-form-title" className={styles.formTitle}>
        {editing ? `Edit ${editing.displayName}` : 'Add a provider'}
      </h3>
      <div className={styles.fields}>
        <label className={styles.field}>
          <span className={styles.label}>Preset</span>
          <select
            className={styles.select}
            value={draft.preset}
            disabled={editing !== null}
            onChange={(e) => choosePreset(e.target.value)}
          >
            {presets.map((p) => (
              <option key={p.id} value={p.id}>
                {p.displayName}
              </option>
            ))}
          </select>
        </label>
        <label className={styles.field}>
          <span className={styles.label}>Display name</span>
          <input
            className={styles.input}
            value={draft.displayName}
            onChange={(e) => set('displayName', e.target.value)}
            placeholder="My university gateway"
          />
        </label>
        <label className={styles.field}>
          <span className={styles.label}>Base URL</span>
          <input
            className={`${styles.input} ${styles.mono}`}
            value={draft.baseUrl}
            onChange={(e) => set('baseUrl', e.target.value)}
            spellCheck={false}
            autoCapitalize="off"
          />
          {urlWarning && <span className={styles.warnText}>{urlWarning}</span>}
        </label>
        <label className={styles.field}>
          <span className={styles.label}>API key</span>
          <input
            className={styles.input}
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={draft.apiKey ?? ''}
            placeholder={keyPlaceholder}
            onChange={(e) => set('apiKey', e.target.value)}
          />
          {preset?.getKeyUrl && (
            <button type="button" className={styles.link} onClick={() => void ipc.openExternal(preset.getKeyUrl ?? '')}>
              Get key
            </button>
          )}
        </label>
        {models.length > 0 && (
          <label className={styles.field}>
            <span className={styles.label}>Model to test</span>
            <select
              className={styles.select}
              value={draft.testModel ?? ''}
              onChange={(e) => set('testModel', e.target.value || null)}
            >
              {models.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.id}
                </option>
              ))}
            </select>
          </label>
        )}
        <label className={styles.field}>
          <span className={styles.label}>Requests at once</span>
          <input
            className={styles.input}
            type="number"
            min={1}
            max={64}
            value={draft.concurrency}
            onChange={(e) => set('concurrency', Number(e.target.value))}
          />
        </label>
        <label className={`${styles.check} ${styles.fieldWide}`}>
          <input
            type="checkbox"
            checked={draft.dataPolicyWarning}
            onChange={(e) => set('dataPolicyWarning', e.target.checked)}
          />
          <span>This provider&apos;s free tier may use prompts for training.</span>
        </label>
        {editing?.key.present && (
          <label className={`${styles.check} ${styles.fieldWide}`}>
            <input type="checkbox" checked={draft.removeKey} onChange={(e) => set('removeKey', e.target.checked)} />
            <span>Remove the saved key from the Keychain.</span>
          </label>
        )}
      </div>

      <div className={styles.actions}>
        <button type="button" className={styles.small} disabled={busy} onClick={() => fetchModels.mutate()}>
          {fetchModels.isPending ? 'Fetching…' : 'Fetch models'}
        </button>
        <button type="button" className={styles.small} disabled={busy} onClick={() => test.mutate()}>
          {test.isPending ? 'Testing…' : 'Test connection'}
        </button>
        <button type="submit" className={styles.save} disabled={busy}>
          Save <span className="visually-hidden">provider</span>
        </button>
        {editing && (
          <button type="button" className={styles.small} onClick={onDone}>
            Cancel
          </button>
        )}
        {editing && (
          <button
            type="button"
            className={styles.danger}
            disabled={busy}
            onClick={() => (confirmDelete ? remove.mutate() : setConfirmDelete(true))}
          >
            {confirmDelete ? 'Confirm remove' : 'Remove provider'}
          </button>
        )}
        {!editing && (
          <p className={styles.presetNote}>
            Presets: Anthropic, OpenAI, OpenRouter, Google Gemini, Groq, Mistral, Ollama, LM Studio
          </p>
        )}
      </div>
      {result && (
        <p role="status" className={`${styles.result} ${result.ok ? styles.resultOk : styles.resultBad}`}>
          {result.text}
        </p>
      )}
    </form>
  );
}
