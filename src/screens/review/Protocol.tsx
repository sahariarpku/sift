import { useEffect, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useUi } from '../../app/store';
import { errorMessage } from '../../lib/ipc';
import { review, SOURCE_NAMES, type Criterion, type Framework, type ProtocolBody, type QuestionType, type TestableAt } from '../../lib/review';
import { Header, haltText, type ScreenProps } from './common';
import styles from './review.module.css';

const TYPES: QuestionType[] = ['intervention', 'exposure', 'experience', 'policy', 'mixed'];
const FRAMEWORKS: Framework[] = ['PICO', 'PEO', 'SPIDER', 'PICo', 'SPICE'];
const TESTABLE: [TestableAt, string][] = [
  ['title_abstract', 'Title and abstract'],
  ['full_text', 'Full text'],
  ['both', 'Both'],
];

const lines = (s: string) => s.split('\n').map((x) => x.trim()).filter(Boolean);
const csv = (s: string) => s.split(',').map((x) => x.trim()).filter(Boolean);

export function Protocol({ reviewId, question, progress }: ScreenProps) {
  const go = useUi((s) => s.go);
  const qc = useQueryClient();
  const view = useQuery({ queryKey: ['protocol', reviewId], queryFn: () => review.protocol(reviewId), refetchInterval: (q) => (q.state.data?.draft ? false : 2000) });
  const [body, setBody] = useState<ProtocolBody | null>(null);
  const [dirty, setDirty] = useState(false);
  const [confirmSkip, setConfirmSkip] = useState(false);

  const source = view.data?.locked?.body ?? view.data?.draft?.body ?? null;
  useEffect(() => {
    if (source && !dirty) setBody(structuredClone(source));
  }, [source, dirty]);

  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ['protocol', reviewId] });
    void qc.invalidateQueries({ queryKey: ['progress', reviewId] });
  };
  const save = useMutation({
    mutationFn: (b: ProtocolBody) => review.saveProtocol(reviewId, b),
    onSuccess: () => {
      setDirty(false);
      refresh();
    },
  });
  const lock = useMutation({
    mutationFn: async (reviewed: boolean) => {
      if (body && dirty) await review.saveProtocol(reviewId, body);
      return review.lock(reviewId, reviewed);
    },
    onSuccess: () => {
      setDirty(false);
      refresh();
      go({ name: 'review', reviewId, stage: 'overview' });
    },
  });
  const retry = useMutation({ mutationFn: () => review.retryFailed(reviewId), onSuccess: refresh });

  const locked = view.data?.locked ?? null;
  const edit = (f: (b: ProtocolBody) => void) => {
    if (!body || locked) return;
    const next = structuredClone(body);
    f(next);
    setBody(next);
    setDirty(true);
  };

  if (!view.data?.draft && !locked) {
    return (
      <>
        <Header eyebrow="Gate 1 of 3 · Protocol" title={question} />
        <section className={styles.card}>
          <p className={styles.reading}>{haltText(progress?.halt)}.</p>
          <p className={styles.muted}>
            The protocol agent classifies the question, chooses the framework and drafts criteria and search concepts. Nothing is searched until
            you lock the protocol.
          </p>
          {progress && progress.failed > 0 && (
            <button type="button" className={styles.button} onClick={() => retry.mutate()}>
              Retry the draft
            </button>
          )}
          {progress?.halt.kind === 'paused' && <p className={styles.error}>{progress.halt.detail}</p>}
        </section>
      </>
    );
  }
  if (!body) return null;

  const criteriaEditor = (kind: 'inclusion_criteria' | 'exclusion_criteria', prefix: string) => (
    <>
      {body[kind].map((c: Criterion, i: number) => (
        <div key={`${kind}-${i}`} style={{ marginBottom: 14, paddingBottom: 12, borderBottom: '1px solid var(--color-line)' }}>
          <div className={styles.row} style={{ marginBottom: 6 }}>
          <input
            aria-label={`${prefix} criterion code ${i + 1}`}
            className={`${styles.input} ${styles.mono}`}
            style={{ width: 150 }}
            value={c.code}
            disabled={!!locked}
            onChange={(e) => edit((b) => void (b[kind][i]!.code = e.target.value.toUpperCase()))}
          />
          <select
            aria-label={`${prefix} criterion ${i + 1} is testable from`}
            className={styles.select}
            style={{ width: 170 }}
            value={c.testable_at}
            disabled={!!locked}
            onChange={(e) => edit((b) => void (b[kind][i]!.testable_at = e.target.value as TestableAt))}
          >
            {TESTABLE.map(([v, l]) => (
              <option key={v} value={v}>
                {l}
              </option>
            ))}
          </select>
          {!locked && (
            <button type="button" className={styles.dangerLink} onClick={() => edit((b) => void b[kind].splice(i, 1))}>
              Remove
            </button>
          )}
          </div>
          <textarea
            aria-label={`${prefix} criterion text ${i + 1}`}
            className={styles.textarea}
            style={{ minHeight: 52 }}
            value={c.text}
            disabled={!!locked}
            onChange={(e) => edit((b) => void (b[kind][i]!.text = e.target.value))}
          />
        </div>
      ))}
      {!locked && (
        <button
          type="button"
          className={styles.link}
          onClick={() => edit((b) => void b[kind].push({ code: `${prefix}-NEW`, text: '', testable_at: 'both' }))}
        >
          Add {prefix === 'IN' ? 'inclusion' : 'exclusion'} criterion
        </button>
      )}
    </>
  );

  return (
    <>
      <Header
        eyebrow={locked ? `Protocol · version ${locked.version} locked ${locked.lockedAt?.slice(0, 10) ?? ''}` : 'Gate 1 of 3 · Review the protocol'}
        title={question}
      />
      {!locked && (
        <div className={styles.banner}>
          <span>Check the framework, criteria and search strings. Edits are logged. Locking creates version 1 and starts the search.</span>
        </div>
      )}
      <div className={styles.grid}>
        <div className={styles.column}>
          <section className={styles.card} aria-labelledby="framework-title">
            <h2 id="framework-title" className={styles.cardTitle}>
              Framework
            </h2>
            <div className={styles.row} style={{ margin: '14px 0' }}>
              <label className={styles.field} style={{ flex: 1 }}>
                <span className={styles.label}>Question type</span>
                <select className={styles.select} value={body.question_type} disabled={!!locked} onChange={(e) => edit((b) => void (b.question_type = e.target.value as QuestionType))}>
                  {TYPES.map((t) => (
                    <option key={t}>{t}</option>
                  ))}
                </select>
              </label>
              <label className={styles.field} style={{ flex: 1 }}>
                <span className={styles.label}>Framing</span>
                <select className={styles.select} value={body.framework} disabled={!!locked} onChange={(e) => edit((b) => void (b.framework = e.target.value as Framework))}>
                  {FRAMEWORKS.map((t) => (
                    <option key={t}>{t}</option>
                  ))}
                </select>
              </label>
              <label className={styles.field} style={{ flex: 1 }}>
                <span className={styles.label}>Searching</span>
                <select className={styles.select} value={body.search_framework} disabled={!!locked} onChange={(e) => edit((b) => void (b.search_framework = e.target.value as Framework))}>
                  {FRAMEWORKS.map((t) => (
                    <option key={t}>{t}</option>
                  ))}
                </select>
              </label>
            </div>
            {body.framework_elements.map((el, i) => (
              <label key={i} className={styles.field}>
                <span className={styles.label}>{el.element}</span>
                <input className={styles.input} value={el.description} disabled={!!locked} onChange={(e) => edit((b) => void (b.framework_elements[i]!.description = e.target.value))} />
              </label>
            ))}
          </section>

          <section className={styles.card} aria-labelledby="criteria-title">
            <h2 id="criteria-title" className={styles.cardTitle}>
              Eligibility criteria
            </h2>
            <p className={`${styles.small} ${styles.muted}`}>Every criterion must be testable from the title and abstract or from the full text.</p>
            <h3 className={styles.label}>Include</h3>
            {criteriaEditor('inclusion_criteria', 'IN')}
            <h3 className={styles.label} style={{ marginTop: 18 }}>
              Exclude
            </h3>
            {criteriaEditor('exclusion_criteria', 'EX')}
          </section>

          <section className={styles.card} aria-labelledby="concepts-title">
            <h2 id="concepts-title" className={styles.cardTitle}>
              Search concepts
            </h2>
            <p className={`${styles.small} ${styles.muted}`}>Concepts are combined with AND; terms within a concept with OR. Use * for truncation.</p>
            {body.concepts.map((c, i) => (
              <div key={i} style={{ marginTop: 14 }}>
                <label className={styles.field}>
                  <span className={styles.label}>Concept {i + 1}</span>
                  <input className={styles.input} value={c.name} disabled={!!locked} onChange={(e) => edit((b) => void (b.concepts[i]!.name = e.target.value))} />
                </label>
                <div className={styles.row} style={{ alignItems: 'flex-start' }}>
                  <label className={styles.field} style={{ flex: 2 }}>
                    <span className={styles.label}>Terms, one per line</span>
                    <textarea
                      className={`${styles.textarea} ${styles.mono}`}
                      value={c.synonyms.join('\n')}
                      disabled={!!locked}
                      onChange={(e) => edit((b) => void (b.concepts[i]!.synonyms = lines(e.target.value)))}
                    />
                  </label>
                  <label className={styles.field} style={{ flex: 1 }}>
                    <span className={styles.label}>ERIC Thesaurus terms</span>
                    <textarea
                      className={`${styles.textarea} ${styles.mono}`}
                      value={c.controlled_vocab.join('\n')}
                      disabled={!!locked}
                      onChange={(e) => edit((b) => void (b.concepts[i]!.controlled_vocab = lines(e.target.value)))}
                    />
                  </label>
                </div>
              </div>
            ))}
          </section>
        </div>

        <div className={styles.column}>
          <section className={styles.card} aria-labelledby="scope-title">
            <h2 id="scope-title" className={styles.cardTitle}>
              Scope
            </h2>
            <div className={styles.row} style={{ marginTop: 12 }}>
              <label className={styles.field} style={{ flex: 1 }}>
                <span className={styles.label}>From year</span>
                <input className={styles.input} type="number" value={body.date_range.from} disabled={!!locked} onChange={(e) => edit((b) => void (b.date_range.from = Number(e.target.value)))} />
              </label>
              <label className={styles.field} style={{ flex: 1 }}>
                <span className={styles.label}>To year</span>
                <input className={styles.input} type="number" value={body.date_range.to} disabled={!!locked} onChange={(e) => edit((b) => void (b.date_range.to = Number(e.target.value)))} />
              </label>
            </div>
            <label className={styles.field}>
              <span className={styles.label}>Languages</span>
              <input className={styles.input} value={body.languages.join(', ')} disabled={!!locked} onChange={(e) => edit((b) => void (b.languages = csv(e.target.value)))} />
            </label>
            <label className={styles.field}>
              <span className={styles.label}>Study designs</span>
              <input className={styles.input} value={body.study_designs.join(', ')} disabled={!!locked} onChange={(e) => edit((b) => void (b.study_designs = csv(e.target.value)))} />
            </label>
            <label className={styles.field}>
              <span className={styles.label}>Planned synthesis</span>
              <textarea className={styles.textarea} value={body.planned_synthesis} disabled={!!locked} onChange={(e) => edit((b) => void (b.planned_synthesis = e.target.value))} />
            </label>
            {body.open_questions.length > 0 && (
              <>
                <h3 className={styles.label}>Open questions for you</h3>
                <ul className={styles.small}>
                  {body.open_questions.map((q, i) => (
                    <li key={i}>{q}</li>
                  ))}
                </ul>
              </>
            )}
          </section>

          <section className={styles.card} aria-labelledby="strings-title">
            <h2 id="strings-title" className={styles.cardTitle}>
              Search strings
            </h2>
            {dirty && <p className={`${styles.small} ${styles.muted}`}>Save your changes to update the strings.</p>}
            {(view.data?.previews ?? []).map(([src, r]) => (
              <div key={src} style={{ marginTop: 14 }}>
                <h3 className={styles.label}>{SOURCE_NAMES[src] ?? src}</h3>
                <p className={styles.mono} style={{ overflowWrap: 'anywhere', margin: '6px 0' }}>
                  {r.query}
                  {r.params.map(([k, v]) => ` ${k}=${v}`).join('')}
                </p>
                {r.simplifications.map((s, i) => (
                  <p key={i} className={`${styles.small} ${styles.muted}`} style={{ margin: '2px 0' }}>
                    Note: {s}
                  </p>
                ))}
              </div>
            ))}
            {view.data?.options.seedCheck && (
              <p className={`${styles.small} ${styles.muted}`} style={{ marginTop: 16 }}>
                Seed-set check: after locking, a narrow search finds up to 10 clearly relevant papers that both screeners confirm. If the full
                search misses any of them, the search is broadened and rerun, up to 3 times.
              </p>
            )}
          </section>

          {!locked && (
            <section className={styles.card}>
              <div className={styles.row}>
                <button type="button" className={styles.button} disabled={!dirty || save.isPending} onClick={() => save.mutate(body)}>
                  Save changes
                </button>
                <button type="button" className={styles.primary} disabled={lock.isPending} onClick={() => lock.mutate(true)}>
                  Lock protocol and run search
                </button>
              </div>
              {(save.isError || lock.isError) && <p className={styles.error}>{errorMessage(save.error ?? lock.error)}</p>}
              <p className={`${styles.small} ${styles.muted}`} style={{ marginTop: 16 }}>
                {confirmSkip ? (
                  <>
                    Skipping marks this review unverified for good.{' '}
                    <button type="button" className={styles.dangerLink} onClick={() => lock.mutate(false)}>
                      Lock without reviewing
                    </button>{' '}
                    <button type="button" className={styles.link} onClick={() => setConfirmSkip(false)}>
                      Cancel
                    </button>
                  </>
                ) : (
                  <button type="button" className={styles.link} onClick={() => setConfirmSkip(true)}>
                    Skip Gate 1
                  </button>
                )}
              </p>
            </section>
          )}
        </div>
      </div>
    </>
  );
}
