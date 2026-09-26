import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { errorMessage, ipc, type AppSettings, type McpStatus } from '../../lib/ipc';
import styles from './Settings.module.css';

type Outcome = { ok: boolean; message: string } | null;

function Result({ r }: { r: NonNullable<Outcome> }) {
  return (
    <p role="status" className={`${styles.result} ${r.ok ? styles.resultOk : styles.resultBad}`}>
      {r.message}
    </p>
  );
}

function useSave(label: string, set: (o: Outcome) => void) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (s: AppSettings) => ipc.updateSettings(s),
    onSuccess: (s) => {
      qc.setQueryData(['settings'], s);
      void qc.invalidateQueries({ queryKey: ['mcp'] });
      set({ ok: true, message: `${label} saved.` });
    },
    onError: (e) => set({ ok: false, message: errorMessage(e) }),
  });
}

const blank = (v: string) => (v.trim() === '' ? null : v.trim());

export function AutomationCard({ settings }: { settings: AppSettings }) {
  const [bypass, setBypass] = useState(settings.bypassByDefault);
  const [result, setResult] = useState<Outcome>(null);
  const save = useSave('Automation', setResult);

  return (
    <section id="automation" className={styles.card} aria-labelledby="automation-title">
      <h2 id="automation-title" className={styles.cardTitle}>
        Automation
      </h2>
      <form
        className={styles.toggles}
        onSubmit={(e) => {
          e.preventDefault();
          save.mutate({ ...settings, bypassByDefault: bypass });
        }}
      >
        <label className={styles.check}>
          <input type="checkbox" checked={bypass} onChange={(e) => setBypass(e.target.checked)} />
          <span>
            <strong>Start new reviews in bypass mode</strong>
            <span className={styles.hint} style={{ display: 'block' }}>
              The review runs to the end without stopping for you. The three gates are skipped. Records the agents
              still cannot settle after a second pass are carried forward to full text, full texts go by the
              agents&apos; votes, papers no automatic route finds are marked not retrieved, and flagged extractions are
              left out of synthesis. Every bypassed step is logged and disclosed, and the review stays Unverified
              permanently. You can turn bypass on or off for one review from its top bar.
            </span>
          </span>
        </label>
        <div className={styles.actions}>
          <button type="submit" className={styles.save} disabled={save.isPending}>
            Save <span className="visually-hidden">automation</span>
          </button>
        </div>
        {result && <Result r={result} />}
      </form>
    </section>
  );
}

export function AuthorCard({ settings }: { settings: AppSettings }) {
  const [name, setName] = useState(settings.authorName ?? '');
  const [affiliation, setAffiliation] = useState(settings.authorAffiliation ?? '');
  const [funding, setFunding] = useState(settings.fundingStatement ?? '');
  const [interests, setInterests] = useState(settings.competingInterests ?? '');
  const [result, setResult] = useState<Outcome>(null);
  const save = useSave('Author details', setResult);

  return (
    <section id="author" className={styles.card} aria-labelledby="author-title">
      <h2 id="author-title" className={styles.cardTitle}>
        Author details
      </h2>
      <p className={styles.cardIntro}>
        Used on the APA title page and for PRISMA items 25 and 26. Sift never writes these for you.
      </p>
      <form
        className={styles.toggles}
        onSubmit={(e) => {
          e.preventDefault();
          save.mutate({
            ...settings,
            authorName: blank(name),
            authorAffiliation: blank(affiliation),
            fundingStatement: blank(funding),
            competingInterests: blank(interests),
          });
        }}
      >
        <label className={styles.field}>
          <span className={styles.label}>Author name</span>
          <input className={styles.input} value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label className={styles.field}>
          <span className={styles.label}>Affiliation</span>
          <input className={styles.input} value={affiliation} onChange={(e) => setAffiliation(e.target.value)} />
        </label>
        <label className={styles.field}>
          <span className={styles.label}>Funding statement (item 25)</span>
          <textarea
            className={styles.input}
            rows={3}
            value={funding}
            placeholder="This review received no specific funding."
            onChange={(e) => setFunding(e.target.value)}
          />
        </label>
        <label className={styles.field}>
          <span className={styles.label}>Competing interests (item 26)</span>
          <textarea
            className={styles.input}
            rows={3}
            value={interests}
            placeholder="The author declares no competing interests."
            onChange={(e) => setInterests(e.target.value)}
          />
          <span className={styles.hint}>New reviews copy these in. Edit them per review at sign-off.</span>
        </label>
        <div className={styles.actions}>
          <button type="submit" className={styles.save} disabled={save.isPending}>
            Save <span className="visually-hidden">author details</span>
          </button>
        </div>
        {result && <Result r={result} />}
      </form>
    </section>
  );
}

function Copyable({ label, text, secret = false }: { label: string; text: string; secret?: boolean }) {
  const [shown, setShown] = useState(!secret);
  const [copied, setCopied] = useState(false);
  const copy = () => {
    navigator.clipboard.writeText(text).then(
      () => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1500);
      },
      () => setShown(true),
    );
  };
  return (
    <div className={styles.field}>
      <span className={styles.label}>{label}</span>
      <pre className={styles.code}>{shown ? text : `${'•'.repeat(24)}${text.slice(-4)}`}</pre>
      <div className={styles.codeActions}>
        {secret && (
          <button type="button" className={styles.small} onClick={() => setShown(!shown)}>
            {shown ? 'Hide' : 'Show'} <span className="visually-hidden">{label}</span>
          </button>
        )}
        <button type="button" className={styles.small} onClick={copy}>
          {copied ? 'Copied' : 'Copy'} <span className="visually-hidden">{label}</span>
        </button>
      </div>
    </div>
  );
}

function statusLine(s: McpStatus): { ok: boolean; text: string } {
  if (s.error) return { ok: false, text: s.error };
  if (!s.enabled) return { ok: false, text: 'Off. Turn it on to let an assistant connect.' };
  if (!s.running) return { ok: false, text: 'Starting.' };
  const poll = s.bridge.lastPollSecs;
  const seen =
    poll === null ? 'No assistant has asked for work yet.' : `An assistant asked for work ${poll} s ago.`;
  return { ok: true, text: `Running at ${s.url}. ${seen}` };
}

export function McpCard({ settings }: { settings: AppSettings }) {
  const qc = useQueryClient();
  const status = useQuery({ queryKey: ['mcp'], queryFn: ipc.mcpStatus, refetchInterval: 3000 });
  const [enabled, setEnabled] = useState(settings.mcpEnabled);
  const [port, setPort] = useState(String(settings.mcpPort));
  const [result, setResult] = useState<Outcome>(null);
  const [confirming, setConfirming] = useState(false);
  const save = useSave('MCP server', setResult);
  const regenerate = useMutation({
    mutationFn: ipc.regenerateMcpToken,
    onSuccess: (s) => {
      qc.setQueryData(['mcp'], s);
      setResult({ ok: true, message: 'New token made. Update it in every assistant you connected.' });
    },
    onError: (e) => setResult({ ok: false, message: errorMessage(e) }),
  });

  const s = status.data;
  const line = s ? statusLine(s) : null;
  const command = s?.stdioCommand?.replace(/ --mcp-stdio$/, '') ?? null;
  const desktopConfig = command
    ? JSON.stringify({ mcpServers: { sift: { command, args: ['--mcp-stdio'] } } }, null, 2)
    : null;

  return (
    <section id="assistants" className={styles.card} aria-labelledby="mcp-title">
      <h2 id="mcp-title" className={styles.cardTitle}>
        Claude or ChatGPT (MCP)
      </h2>
      <p className={styles.cardIntro}>
        Lets an assistant you already use run Sift through the Model Context Protocol: start reviews, watch progress,
        pause, skip stages, and answer the app&apos;s model calls in place of an API key. It cannot make your human
        decisions for you.
      </p>
      <form
        className={styles.toggles}
        onSubmit={(e) => {
          e.preventDefault();
          const n = Number(port);
          if (!Number.isInteger(n) || n < 1024 || n > 65535) {
            setResult({ ok: false, message: 'Port must be a whole number from 1024 to 65535.' });
            return;
          }
          save.mutate({ ...settings, mcpEnabled: enabled, mcpPort: n });
        }}
      >
        <label className={styles.check}>
          <input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} />
          <span>
            <strong>Run the MCP server</strong>
            <span className={styles.hint} style={{ display: 'block' }}>
              Listens on this computer only (127.0.0.1) and needs the token below.
            </span>
          </span>
        </label>
        <label className={styles.field}>
          <span className={styles.label}>Port</span>
          <input
            className={`${styles.input} ${styles.mono}`}
            inputMode="numeric"
            value={port}
            onChange={(e) => setPort(e.target.value)}
            style={{ maxWidth: 140 }}
          />
        </label>
        <div className={styles.actions}>
          <button type="submit" className={styles.save} disabled={save.isPending}>
            Save <span className="visually-hidden">MCP server</span>
          </button>
        </div>
        {result && <Result r={result} />}
      </form>

      {status.isError && <p className={styles.warnText}>{errorMessage(status.error)}</p>}
      {s && line && (
        <div className={styles.toggles} style={{ marginTop: 18 }}>
          <p role="status" className={`${styles.result} ${line.ok ? styles.resultOk : styles.resultBad}`}>
            {line.text}
            {s.bridge.waiting + s.bridge.claimed > 0 &&
              ` ${s.bridge.waiting} model call${s.bridge.waiting === 1 ? '' : 's'} waiting, ${s.bridge.claimed} being answered.`}
          </p>

          <Copyable label="Token" text={s.token} secret />
          <div className={styles.codeActions}>
            {confirming ? (
              <>
                <span className={styles.hint}>Assistants using the old token stop working.</span>
                <button
                  type="button"
                  className={styles.small}
                  disabled={regenerate.isPending}
                  onClick={() => {
                    setConfirming(false);
                    regenerate.mutate();
                  }}
                >
                  Make new token
                </button>
                <button type="button" className={styles.small} onClick={() => setConfirming(false)}>
                  Cancel
                </button>
              </>
            ) : (
              <button type="button" className={styles.small} onClick={() => setConfirming(true)}>
                New token
              </button>
            )}
          </div>

          <h3 className={styles.formTitle}>Connect Claude Code</h3>
          <Copyable
            label="Run in a terminal"
            text={`claude mcp add --transport http sift ${s.url} --header "Authorization: Bearer ${s.token}"`}
            secret
          />

          {desktopConfig && (
            <>
              <h3 className={styles.formTitle}>Connect Claude Desktop</h3>
              <Copyable label="Add to claude_desktop_config.json" text={desktopConfig} />
              <span className={styles.hint}>Sift must be open while Claude Desktop uses it.</span>
            </>
          )}

          <h3 className={styles.formTitle}>Connect ChatGPT</h3>
          <p className={styles.hint}>
            ChatGPT connectors only reach public HTTPS addresses, so it cannot see this server directly. Expose{' '}
            <code className={styles.mono}>{s.url}</code> through an HTTPS tunnel you control and add the token as a
            bearer header. Anyone with the tunnel address and token can operate Sift, so close the tunnel when you are
            done.
          </p>

          <h3 className={styles.formTitle}>Use the assistant as the AI</h3>
          <p className={styles.hint}>
            Under AI models, add the provider &ldquo;Claude or ChatGPT (via MCP)&rdquo;
            {s.port !== 31480 && (
              <>
                {' '}
                with base URL <code className={styles.mono}>{s.bridgeUrl}</code>
              </>
            )}
            , then assign it to the tasks you want the assistant to do. Each model call waits until the assistant
            takes it with <code className={styles.mono}>sift_next_task</code> and answers with{' '}
            <code className={styles.mono}>sift_submit_task</code>. The report discloses which assistant answered.
          </p>
        </div>
      )}
    </section>
  );
}
