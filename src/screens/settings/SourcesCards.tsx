import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { errorMessage, ipc, type AppSettings, type SourceStatus, type SourceTestResult } from '../../lib/ipc';
import styles from './Settings.module.css';

function Result({ r }: { r: SourceTestResult }) {
  return (
    <p role="status" className={`${styles.result} ${r.ok ? styles.resultOk : styles.resultBad}`}>
      {r.message}
    </p>
  );
}

export function DatabasesCard() {
  const sources = useQuery({ queryKey: ['sources'], queryFn: ipc.listSources });
  const [open, setOpen] = useState<string | null>(null);
  const list = (sources.data ?? []).filter((s) => s.auth !== 'email');

  return (
    <section id="databases" className={styles.card} aria-labelledby="databases-title">
      <h2 id="databases-title" className={styles.cardTitle}>
        Databases
      </h2>
      {sources.isError && <p className={styles.warnText}>{errorMessage(sources.error)}</p>}
      <ul className={styles.list} aria-label="Databases">
        {list.map((s) => (
          <li key={s.id}>
            <SourceRow source={s} open={open === s.id} onToggle={() => setOpen(open === s.id ? null : s.id)} />
          </li>
        ))}
      </ul>
    </section>
  );
}

function SourceRow({ source: s, open, onToggle }: { source: SourceStatus; open: boolean; onToggle: () => void }) {
  const qc = useQueryClient();
  const [key, setKey] = useState('');
  const [token, setToken] = useState('');
  const [result, setResult] = useState<SourceTestResult | null>(null);

  const save = useMutation({
    mutationFn: () => ipc.setSourceKey(s.id, key || null, token || null),
    onSuccess: (list) => {
      qc.setQueryData(['sources'], list);
      setKey('');
      setToken('');
      setResult(null);
    },
    onError: (e) => setResult({ ok: false, status: null, message: errorMessage(e) }),
  });
  const remove = useMutation({
    mutationFn: () => ipc.deleteSourceKey(s.id),
    onSuccess: (list) => qc.setQueryData(['sources'], list),
  });
  const test = useMutation({
    mutationFn: () => ipc.testSource(s.id),
    onSuccess: setResult,
    onError: (e) => setResult({ ok: false, status: null, message: errorMessage(e) }),
  });

  const needsKey = s.auth !== 'none';
  const note = s.key.present && s.key.hint ? `${s.note} · ${s.key.hint}` : s.note;

  return (
    <>
      <div className={styles.row}>
        <div className={styles.rowMain}>
          <p className={styles.rowName}>{s.name}</p>
          <p className={styles.rowNote}>{note}</p>
        </div>
        {s.getKeyUrl && (
          <button type="button" className={styles.link} onClick={() => void ipc.openExternal(s.getKeyUrl ?? '')}>
            Get key <span className="visually-hidden">for {s.name}</span>
          </button>
        )}
        {s.ready && <span className={`${styles.badge} ${styles.badgeOk}`}>Ready</span>}
        <button type="button" className={styles.small} aria-expanded={open} onClick={onToggle}>
          {!needsKey ? 'Test' : s.key.present ? 'Edit' : 'Add'}{' '}
          <span className="visually-hidden">{s.name}</span>
        </button>
      </div>
      {open && (
        <div className={styles.sourceForm}>
          {needsKey && (
            <label className={styles.field}>
              <span className={styles.label}>{s.name} API key</span>
              <input
                className={styles.input}
                type="password"
                autoComplete="off"
                value={key}
                placeholder={s.key.present ? `${s.key.hint ?? 'Key saved'}. Paste a new one to replace it.` : 'Paste key'}
                onChange={(e) => setKey(e.target.value)}
              />
            </label>
          )}
          {s.auth === 'key_and_inst_token' && (
            <label className={styles.field}>
              <span className={styles.label}>Institution token (optional)</span>
              <input
                className={styles.input}
                type="password"
                autoComplete="off"
                value={token}
                placeholder={s.instToken?.present ? `${s.instToken.hint ?? 'Token saved'}` : 'Only needed off campus'}
                onChange={(e) => setToken(e.target.value)}
              />
            </label>
          )}
          <div className={styles.actions}>
            {needsKey && (
              <button
                type="button"
                className={styles.save}
                disabled={save.isPending || (key.trim() === '' && token.trim() === '')}
                onClick={() => save.mutate()}
              >
                Save <span className="visually-hidden">{s.name} key</span>
              </button>
            )}
            <button type="button" className={styles.small} disabled={test.isPending} onClick={() => test.mutate()}>
              {test.isPending ? 'Testing…' : 'Test'}{' '}
              <span className="visually-hidden">{s.name}</span>
            </button>
            {s.key.present && (
              <button type="button" className={styles.danger} onClick={() => remove.mutate()}>
                Remove key
              </button>
            )}
          </div>
          {result && <Result r={result} />}
        </div>
      )}
    </>
  );
}

export function FullTextCard({ settings }: { settings: AppSettings }) {
  const qc = useQueryClient();
  const [email, setEmail] = useState(settings.unpaywallEmail ?? '');
  const [proxy, setProxy] = useState(settings.libraryProxyPrefix ?? '');
  const [institutional, setInstitutional] = useState(settings.institutionalAccess);
  const [result, setResult] = useState<SourceTestResult | null>(null);

  const save = useMutation({
    mutationFn: () =>
      ipc.updateSettings({ ...settings, unpaywallEmail: email, libraryProxyPrefix: proxy, institutionalAccess: institutional }),
    onSuccess: (s) => {
      qc.setQueryData(['settings'], s);
      void qc.invalidateQueries({ queryKey: ['sources'] });
      setResult({ ok: true, status: null, message: 'Saved.' });
    },
    onError: (e) => setResult({ ok: false, status: null, message: errorMessage(e) }),
  });
  const test = useMutation({
    mutationFn: () => ipc.testSource('unpaywall'),
    onSuccess: setResult,
    onError: (e) => setResult({ ok: false, status: null, message: errorMessage(e) }),
  });

  return (
    <section id="full-text" className={styles.card} aria-labelledby="fulltext-title">
      <h2 id="fulltext-title" className={styles.cardTitle}>
        Full-text access
      </h2>
      <form
        className={styles.toggles}
        onSubmit={(e) => {
          e.preventDefault();
          save.mutate();
        }}
      >
        <label className={styles.field}>
          <span className={styles.label}>Email for Unpaywall (no key needed)</span>
          <input className={styles.input} type="email" value={email} onChange={(e) => setEmail(e.target.value)} />
        </label>
        <label className={styles.field}>
          <span className={styles.label}>Library proxy prefix</span>
          <input
            className={`${styles.input} ${styles.mono}`}
            value={proxy}
            placeholder="https://[library proxy]/login?url="
            spellCheck={false}
            onChange={(e) => setProxy(e.target.value)}
          />
          <span className={styles.hint}>
            Your library&apos;s website lists this; it lets paywalled PDFs download through your institution.
          </span>
        </label>
        <label className={styles.field} style={{ flexDirection: 'row', gap: 8, alignItems: 'flex-start' }}>
          <input type="checkbox" checked={institutional} onChange={(e) => setInstitutional(e.target.checked)} />
          <span>
            <span className={styles.label}>I use my institution&apos;s network or VPN</span>
            <span className={styles.hint} style={{ display: 'block' }}>
              Sift follows each DOI to the publisher and downloads the PDF the page links to, if your institution
              subscribes. Requests say they come from Sift and are spaced out. Publishers that block automated access
              are left for you to download in your browser. Add Elsevier and Wiley keys under Databases for their
              official download APIs.
            </span>
          </span>
        </label>
        <div className={styles.actions}>
          <button type="submit" className={styles.save} disabled={save.isPending}>
            Save <span className="visually-hidden">full-text access</span>
          </button>
          <button
            type="button"
            className={styles.small}
            disabled={test.isPending || !settings.unpaywallEmail}
            onClick={() => test.mutate()}
          >
            Test Unpaywall
          </button>
        </div>
        {result && <Result r={result} />}
      </form>
    </section>
  );
}

export function SpendingCard({ settings, reviewId }: { settings: AppSettings; reviewId: string | null }) {
  const qc = useQueryClient();
  const [cap, setCap] = useState(settings.spendingCapUsd?.toString() ?? '');
  const [error, setError] = useState<string | null>(null);
  const spend = useQuery({
    queryKey: ['spend', reviewId],
    queryFn: () => ipc.reviewSpend(reviewId ?? ''),
    enabled: reviewId !== null,
  });
  const providers = useQuery({ queryKey: ['providers'], queryFn: ipc.listProviders });
  const assignments = useQuery({ queryKey: ['assignments'], queryFn: ipc.listTaskAssignments });
  const prices = useQuery({ queryKey: ['prices'], queryFn: ipc.listPrices });

  const save = useMutation({
    mutationFn: () => {
      const trimmed = cap.trim().replace(/^\$/, '');
      const value = trimmed === '' ? null : Number(trimmed);
      if (value !== null && Number.isNaN(value)) throw new Error('Enter an amount in US dollars.');
      return ipc.updateSettings({ ...settings, spendingCapUsd: value });
    },
    onSuccess: (s) => {
      qc.setQueryData(['settings'], s);
      setError(null);
    },
    onError: (e) => setError(errorMessage(e)),
  });

  // One price row per distinct provider + model in use.
  const inUse = new Map<string, { providerId: string; modelId: string }>();
  for (const a of assignments.data ?? []) {
    if (a.providerId && a.modelId) inUse.set(`${a.providerId}/${a.modelId}`, { providerId: a.providerId, modelId: a.modelId });
  }
  const providerName = (id: string) => providers.data?.find((p) => p.id === id)?.displayName ?? id;

  return (
    <section id="spending" className={styles.card} aria-labelledby="spending-title">
      <h2 id="spending-title" className={styles.cardTitle}>
        Spending
      </h2>
      {reviewId && (
        <div className={styles.spend}>
          <span>This review so far</span>
          <span className={styles.amount}>${(spend.data ?? 0).toFixed(2)}</span>
        </div>
      )}
      <form
        className={styles.field}
        onSubmit={(e) => {
          e.preventDefault();
          save.mutate();
        }}
      >
        <label className={styles.field}>
          <span className={styles.label}>Pause the run when a review reaches (US dollars)</span>
          <input className={styles.input} inputMode="decimal" value={cap} onChange={(e) => setCap(e.target.value)} />
        </label>
        <div className={styles.actions}>
          <button type="submit" className={styles.save} disabled={save.isPending}>
            Save <span className="visually-hidden">spending cap</span>
          </button>
        </div>
        {error && <p className={styles.warnText}>{error}</p>}
      </form>

      {inUse.size > 0 && (
        <>
          <hr className={styles.divider} />
          <p className={styles.hint}>
            Prices per million tokens, used to estimate cost. Check your provider&apos;s price list.
          </p>
          <table className={styles.prices}>
            <thead>
              <tr>
                <th scope="col">Model</th>
                <th scope="col">Input $</th>
                <th scope="col">Output $</th>
              </tr>
            </thead>
            <tbody>
              {[...inUse.values()].map((m) => (
                <PriceEditor
                  key={`${m.providerId}/${m.modelId}`}
                  label={`${providerName(m.providerId)} · ${m.modelId}`}
                  providerId={m.providerId}
                  modelId={m.modelId}
                  current={prices.data?.find((p) => p.providerId === m.providerId && p.modelId === m.modelId)?.price ?? null}
                />
              ))}
            </tbody>
          </table>
        </>
      )}
    </section>
  );
}

function PriceEditor(props: {
  label: string;
  providerId: string;
  modelId: string;
  current: { inputPerMtok: number; outputPerMtok: number } | null;
}) {
  const qc = useQueryClient();
  const [input, setInput] = useState(props.current?.inputPerMtok.toString() ?? '');
  const [output, setOutput] = useState(props.current?.outputPerMtok.toString() ?? '');
  const save = useMutation({
    mutationFn: () =>
      ipc.setPrice(
        props.providerId,
        props.modelId,
        input === '' && output === '' ? null : { inputPerMtok: Number(input || 0), outputPerMtok: Number(output || 0) },
      ),
    onSuccess: (rows) => qc.setQueryData(['prices'], rows),
  });
  return (
    <tr>
      <td className={styles.mono}>{props.label}</td>
      <td>
        <input
          className={styles.input}
          inputMode="decimal"
          aria-label={`Input price for ${props.label}`}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onBlur={() => save.mutate()}
        />
      </td>
      <td>
        <input
          className={styles.input}
          inputMode="decimal"
          aria-label={`Output price for ${props.label}`}
          value={output}
          onChange={(e) => setOutput(e.target.value)}
          onBlur={() => save.mutate()}
        />
      </td>
    </tr>
  );
}
