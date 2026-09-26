import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { errorMessage, ipc, type AppSettings } from '../../lib/ipc';
import styles from './Settings.module.css';

/** Optional local pre-screen with Laya-MLX (Apple Silicon, runs offline). */
export function LocalModelCard({ settings }: { settings: AppSettings }) {
  const qc = useQueryClient();
  const status = useQuery({ queryKey: ['laya-status'], queryFn: ipc.layaStatus });
  const [python, setPython] = useState(settings.layaPython ?? '');
  const [result, setResult] = useState<{ ok: boolean; message: string } | null>(null);

  const save = useMutation({
    mutationFn: () => ipc.updateSettings({ ...settings, layaPython: python }),
    onSuccess: (s) => {
      qc.setQueryData(['settings'], s);
      void qc.invalidateQueries({ queryKey: ['laya-status'] });
      setResult({ ok: true, message: 'Saved.' });
    },
    onError: (e) => setResult({ ok: false, message: errorMessage(e) }),
  });
  const test = useMutation({
    mutationFn: ipc.layaTest,
    onSuccess: (t) =>
      setResult({
        ok: t.ok,
        message: t.ok ? `${t.message} Loaded in ${t.loadMs} ms, answered in ${t.answerMs} ms.` : t.message,
      }),
    onError: (e) => setResult({ ok: false, message: errorMessage(e) }),
  });

  const s = status.data;
  return (
    <section id="local-model" className={styles.card} aria-labelledby="local-title">
      <h2 id="local-title" className={styles.cardTitle}>
        Local pre-screen
      </h2>
      <p className={styles.hint}>
        Laya-MLX is a small decision model that runs on this Mac. It scores each record against the inclusion
        criteria to order screening, and can give low-ranked records one AI screener instead of two. It never
        excludes a record by itself. Choose it per review on the Start screen.
      </p>
      <p role="status" className={`${styles.result} ${s?.available ? styles.resultOk : styles.resultBad}`}>
        {status.isPending
          ? 'Checking for Laya-MLX…'
          : status.isError
            ? errorMessage(status.error)
            : s &&
              `${s.message}${s.version ? ` laya-mlx ${s.version}` : ''}${s.python ? ` at ${s.python}` : ''}. Model ${s.model}${
                s.modelDownloaded ? ' downloaded' : ' not downloaded'
              }.`}
      </p>
      <form
        className={styles.toggles}
        onSubmit={(e) => {
          e.preventDefault();
          save.mutate();
        }}
      >
        <label className={styles.field}>
          <span className={styles.label}>Python interpreter (optional)</span>
          <input
            className={`${styles.input} ${styles.mono}`}
            value={python}
            placeholder={s?.python ?? '/opt/homebrew/bin/python3'}
            spellCheck={false}
            onChange={(e) => setPython(e.target.value)}
          />
          <span className={styles.hint}>Leave empty to detect it. It needs laya-mlx installed (pip install laya-mlx).</span>
        </label>
        <div className={styles.actions}>
          <button type="submit" className={styles.save} disabled={save.isPending}>
            Save <span className="visually-hidden">local pre-screen</span>
          </button>
          <button
            type="button"
            className={styles.small}
            disabled={test.isPending || !s?.available}
            onClick={() => test.mutate()}
          >
            {test.isPending ? 'Testing…' : 'Test Laya'}
          </button>
        </div>
        {result && (
          <p role="status" className={`${styles.result} ${result.ok ? styles.resultOk : styles.resultBad}`}>
            {result.message}
          </p>
        )}
      </form>
    </section>
  );
}
