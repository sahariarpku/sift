import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useUi } from '../app/store';
import { APP_NAME } from '../lib/app';
import { errorMessage, ipc } from '../lib/ipc';
import { review, type PrescreenMode, type ReviewOptions } from '../lib/review';
import { Icon } from '../components/Icon';
import ui from '../components/ui.module.css';
import styles from './Start.module.css';

const YEARS: [string, number, number][] = [
  ['2010 to 2026', 2010, 2026],
  ['2015 to 2026', 2015, 2026],
  ['2000 to 2026', 2000, 2026],
  ['2020 to 2026', 2020, 2026],
  ['Any year', 1900, 2026],
];
const LANGS: [string, string[]][] = [
  ['English', ['English']],
  ['English and Chinese', ['English', 'Chinese']],
  ['Any language', []],
];
const TYPES: [string, string][] = [
  ['detect', 'Detect automatically'],
  ['experience', 'Lived experience (qualitative)'],
  ['intervention', 'Intervention effect'],
  ['exposure', 'Exposure association'],
  ['policy', 'Policy or service evaluation'],
  ['mixed', 'Mixed'],
];
const PRESCREEN: [PrescreenMode, string][] = [
  ['off', 'Off'],
  ['safety_net', 'Safety net'],
  ['speed', 'Speed'],
];
const PRESCREEN_HELP: Record<PrescreenMode, string> = {
  off: 'Every record gets two AI screeners.',
  safety_net:
    'Laya ranks records on this Mac. Top-ranked records that both screeners exclude go to the arbiter.',
  speed:
    'As the safety net, and the lowest-ranked 30% get one AI screener; if it does not exclude, the second one runs.',
};

const DBS: [string, string][] = [
  ['eric', 'ERIC'],
  ['openalex', 'OpenAlex'],
  ['core', 'CORE'],
  ['semantic_scholar', 'Semantic Scholar'],
];

export function Start() {
  const go = useUi((s) => s.go);
  const qc = useQueryClient();
  const [question, setQuestion] = useState('');
  const [years, setYears] = useState(0);
  const [langs, setLangs] = useState(0);
  const [type, setType] = useState('detect');
  const [dbs, setDbs] = useState<string[]>(['eric', 'openalex', 'core']);
  const [prescreen, setPrescreen] = useState<PrescreenMode>('off');
  const [carry, setCarry] = useState(false);
  const laya = useQuery({ queryKey: ['laya-status'], queryFn: ipc.layaStatus });
  const reviews = useQuery({ queryKey: ['reviews'], queryFn: ipc.listReviews });
  const sources = useQuery({ queryKey: ['sources'], queryFn: ipc.listSources });
  const assignments = useQuery({ queryKey: ['assignments'], queryFn: ipc.listTaskAssignments });
  const ready = (id: string) => sources.data?.find((s) => s.id === id)?.ready ?? id !== 'core';
  const chosen = dbs.filter(ready);
  const unassigned = (assignments.data ?? []).filter((a) => !a.providerId).map((a) => a.label);

  const create = useMutation({
    mutationFn: () => {
      const [, from, to] = YEARS[years] ?? YEARS[0]!;
      const options: ReviewOptions = {
        yearFrom: from,
        yearTo: to,
        languages: LANGS[langs]?.[1] ?? ['English'],
        reviewType: type,
        sources: chosen,
        maxRecordsPerSource: 1500,
        citationChasing: true,
        seedCheck: true,
        prescreen: {
          mode: laya.data?.available ? prescreen : 'off',
          speedFraction: 0.3,
          netFraction: 0.1,
          model: laya.data?.model ?? 'aac6fef/laya-mlx',
        },
        uncertainToFullText: carry,
        retrievalDeadlineHours: 168,
      };
      return review.start(question, options);
    },
    onSuccess: (r) => {
      void qc.invalidateQueries({ queryKey: ['reviews'] });
      go({ name: 'review', reviewId: r.id, stage: 'protocol' });
    },
  });

  return (
    <div className={styles.page}>
      <header className={styles.topbar}>
        <span className={styles.brand}>
          <span className={styles.mark} aria-hidden="true">
            S
          </span>
          {APP_NAME}
        </span>
        <nav className={styles.topnav} aria-label="App">
          <a href="#my-reviews" className={styles.navLink}>
            My reviews
          </a>
          <button type="button" className={`${ui.secondary} ${styles.navButton}`} onClick={() => go({ name: 'settings', from: null })}>
            <Icon name="settings" size={16} />
            Settings
          </button>
        </nav>
      </header>

      <main className={styles.main}>
        <div className={styles.hero}>
          <p className={ui.eyebrow}>New systematic review</p>
          <h1 className={styles.title}>What question should the evidence answer?</h1>
          <p className={styles.lede}>
            Ask it once. {APP_NAME} drafts the protocol, searches, screens, extracts, appraises and writes a PRISMA 2020
            report, pausing only where your judgement is needed.
          </p>
        </div>

        {unassigned.length > 0 && (
          <p className={styles.warn} role="status">
            <Icon name="alert" size={16} className={styles.warnIcon} />
            <span>
              No AI model is set for: {unassigned.join(', ')}.{' '}
              <button type="button" className={styles.inlineLink} onClick={() => go({ name: 'settings', from: null })}>
                Choose models in Settings
              </button>
              .
            </span>
          </p>
        )}

        <form
          className={styles.composer}
          onSubmit={(e) => {
            e.preventDefault();
            create.mutate();
          }}
        >
          <label className={styles.label} htmlFor="question">
            Research question or topic
          </label>
          <textarea
            id="question"
            className={styles.question}
            value={question}
            onChange={(e) => setQuestion(e.target.value)}
            placeholder="For example: How do international students in UK higher education experience belonging, and what supports it?"
            aria-describedby="question-help"
          />
          <p id="question-help" className={styles.help}>
            {APP_NAME} detects the question type and chooses the framework. Nothing is searched until you approve the protocol.
          </p>

          <div className={styles.section}>
            <p className={styles.sectionTitle} aria-hidden="true">
              Scope
            </p>
            <div className={styles.selects}>
              <label className={styles.selectField}>
                <span className={styles.label}>Publication years</span>
                <select className={styles.select} value={years} onChange={(e) => setYears(Number(e.target.value))}>
                  {YEARS.map(([l], i) => (
                    <option key={l} value={i}>
                      {l}
                    </option>
                  ))}
                </select>
              </label>
              <label className={styles.selectField}>
                <span className={styles.label}>Languages</span>
                <select className={styles.select} value={langs} onChange={(e) => setLangs(Number(e.target.value))}>
                  {LANGS.map(([l], i) => (
                    <option key={l} value={i}>
                      {l}
                    </option>
                  ))}
                </select>
              </label>
              <label className={styles.selectField}>
                <span className={styles.label}>Review type</span>
                <select className={styles.select} value={type} onChange={(e) => setType(e.target.value)}>
                  {TYPES.map(([v, l]) => (
                    <option key={v} value={v}>
                      {l}
                    </option>
                  ))}
                </select>
              </label>
              <label className={styles.selectField}>
                <span className={styles.label}>Local pre-screen (Laya)</span>
                <select
                  className={styles.select}
                  value={laya.data?.available ? prescreen : 'off'}
                  disabled={!laya.data?.available}
                  onChange={(e) => setPrescreen(e.target.value as PrescreenMode)}
                  aria-describedby="prescreen-help"
                >
                  {PRESCREEN.map(([v, l]) => (
                    <option key={v} value={v}>
                      {l}
                    </option>
                  ))}
                </select>
              </label>
            </div>
            <p id="prescreen-help" className={styles.help}>
              {laya.data?.available ? PRESCREEN_HELP[prescreen] : `Local pre-screen unavailable: ${laya.data?.message ?? 'checking…'}`}
            </p>

            <label className={styles.check}>
              <input type="checkbox" checked={carry} onChange={(e) => setCarry(e.target.checked)} />
              <span>
                Send records the AI still cannot decide at title and abstract to full-text screening, instead of asking
                you. Disclosed in the report.
              </span>
            </label>
          </div>

          <fieldset className={`${styles.section} ${styles.chips}`}>
            <legend className={styles.sectionTitle}>Databases</legend>
            {DBS.map(([id, name]) =>
              ready(id) ? (
                <label key={id} className={`${styles.chip} ${dbs.includes(id) ? styles.chipOn : ''}`}>
                  <input
                    type="checkbox"
                    className="visually-hidden"
                    checked={dbs.includes(id)}
                    onChange={(e) => setDbs(e.target.checked ? [...dbs, id] : dbs.filter((d) => d !== id))}
                  />
                  <span className={styles.chipBox} aria-hidden="true">
                    {dbs.includes(id) && <Icon name="check" size={12} />}
                  </span>
                  {name}
                </label>
              ) : (
                <button key={id} type="button" className={`${styles.chip} ${styles.chipAdd}`} onClick={() => go({ name: 'settings', from: null })}>
                  + {name} (add key)
                </button>
              ),
            )}
          </fieldset>

          <div className={styles.actions}>
            <button type="submit" className={`${ui.primary} ${styles.submit}`} disabled={question.trim() === '' || chosen.length === 0 || create.isPending}>
              {create.isPending ? 'Drafting protocol' : 'Draft protocol'}
              <Icon name="arrow" size={17} />
            </button>
            <span className={styles.note}>About 2 minutes. You review it before anything is searched.</span>
          </div>
          {create.isError && (
            <p role="alert" className={ui.error}>
              {errorMessage(create.error)}
            </p>
          )}
        </form>

        <section className={styles.how} aria-labelledby="how-title">
          <h2 id="how-title" className={styles.howTitle}>
            Where you come in
          </h2>
          <p className={styles.howText}>
            Everything between the gates runs on its own. Any stage can be skipped; the work already done is kept and the
            skip is disclosed.
          </p>
          <ol className={styles.gates}>
            <li>
              <span className={styles.gateNo} aria-hidden="true">
                1
              </span>
              <span className="visually-hidden">Gate 1: </span>
              <strong>Approve the protocol</strong>
              <span className={styles.note}>15 to 30 minutes</span>
            </li>
            <li>
              <span className={styles.gateNo} aria-hidden="true">
                2
              </span>
              <span className="visually-hidden">Gate 2: </span>
              <strong>Screen a 100-record sample</strong>
              <span className={styles.note}>1 to 2 hours</span>
            </li>
            <li>
              <span className={styles.gateNo} aria-hidden="true">
                3
              </span>
              <span className="visually-hidden">Gate 3: </span>
              <strong>Sign off the report</strong>
              <span className={styles.note}>2 to 4 hours</span>
            </li>
          </ol>
        </section>

        <section id="my-reviews" className={styles.reviews} aria-labelledby="my-reviews-title">
          <h2 id="my-reviews-title" className={styles.howTitle}>
            My reviews
          </h2>
          {reviews.isError && <p className={ui.error}>{errorMessage(reviews.error)}</p>}
          {reviews.data?.length === 0 && <p className={styles.note}>No reviews yet.</p>}
          <ul className={styles.list}>
            {reviews.data?.map((r) => (
              <li key={r.id}>
                <button type="button" className={styles.item} onClick={() => go({ name: 'review', reviewId: r.id, stage: 'overview' })}>
                  <span className={styles.itemQuestion}>{r.question}</span>
                  <span className={styles.meta}>
                    <span className={r.verification === 'verified' ? styles.pillOn : styles.pill}>
                      {r.verification === 'verified' ? 'Verified' : 'Unverified'}
                    </span>
                    Created {new Date(r.createdAt).toLocaleDateString([], { day: 'numeric', month: 'short', year: 'numeric' })}
                  </span>
                  <Icon name="arrow" size={18} className={styles.itemArrow} />
                </button>
              </li>
            ))}
          </ul>
        </section>
      </main>
    </div>
  );
}
