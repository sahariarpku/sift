import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useUi } from "../../app/store";
import { errorMessage } from "../../lib/ipc";
import { Icon } from "../../components/Icon";
import { review, type ChasingState, type Progress } from "../../lib/review";
import { STAGES, type StageId } from "../../lib/stages";
import { Header, haltText, pct, time, type ScreenProps } from "./common";
import styles from "./review.module.css";
import { KEEPS, StageControl } from "./StageControl";

const STATUS_WORD = {
  done: "Done",
  running: "Running",
  waiting: "Needs you",
  pending: "Not started",
  skipped: "Stopped early",
} as const;

/**
 * The whole pipeline at a glance. Each stage opens on click; the stage that
 * is working now can be skipped from here, keeping what it already did.
 */
function Pipeline({
  reviewId,
  progress,
}: {
  reviewId: string;
  progress: Progress;
}) {
  const go = useUi((s) => s.go);
  const [confirming, setConfirming] = useState(false);
  const stages = STAGES.filter((s) => s.id !== "overview").map((s) => ({
    ...s,
    state: progress.stages.find((p) => p.id === s.id),
  }));
  const active = stages.find(
    (s) => s.state?.status === "running" || s.state?.status === "waiting",
  );
  const done = stages.filter(
    (s) => s.state?.status === "done" || s.state?.status === "skipped",
  ).length;
  const halt = progress.halt;
  return (
    <section className={`${styles.card} ${styles.pipelineCard}`} aria-labelledby="pipeline-title">
      <div className={styles.cardHead}>
        <h2 id="pipeline-title" className={styles.cardTitle}>
          Pipeline
        </h2>
        <span className={`${styles.muted} ${styles.small}`}>
          {done} of {stages.length} stages finished
        </span>
      </div>
      <ol className={styles.stepper}>
        {stages.map((s) => {
          const status = s.state?.status ?? "pending";
          return (
            <li key={s.id} className={`${styles.step} ${styles[`step_${status}`] ?? ""}`}>
              <button
                type="button"
                className={styles.stepButton}
                onClick={() => go({ name: "review", reviewId, stage: s.id })}
              >
                <span className={styles.stepNode} aria-hidden="true">
                  {status === "done" && <Icon name="check" size={12} />}
                </span>
                <span className={styles.stepLabel}>{s.label}</span>
                <span className={styles.stepStatus}>{STATUS_WORD[status]}</span>
              </button>
            </li>
          );
        })}
      </ol>
      {active && (
        <div className={styles.now}>
          <div className={styles.nowText}>
            <p className={styles.nowEyebrow}>Now · {active.label}</p>
            <p className={styles.nowDetail}>
              {halt.kind === "done" ? "Signed off" : halt.detail}
            </p>
          </div>
          <div className={styles.headerActions}>
            {active.state?.skippable && !confirming && KEEPS[active.id] && (
              <button
                type="button"
                className={styles.quiet}
                onClick={() => setConfirming(true)}
              >
                <Icon name="skip" size={14} />
                Skip {active.label.toLowerCase()}
              </button>
            )}
            <button
              type="button"
              className={styles.button}
              onClick={() => go({ name: "review", reviewId, stage: active.id })}
            >
              Open {active.label.toLowerCase()}
              <Icon name="arrow" size={15} />
            </button>
          </div>
        </div>
      )}
      {active && (
        <StageControl
          reviewId={reviewId}
          stage={active.id}
          progress={progress}
          part="panel"
          confirming={confirming}
          setConfirming={setConfirming}
        />
      )}
      {progress.skips.length > 0 && (
        <ul className={styles.stoppedList}>
          {progress.skips.map((k) => (
            <li key={k.eventId}>
              <span className={`${styles.badge} ${styles.badgeMuted}`}>Stopped early</span>{" "}
              <span className={styles.small}>{k.rationale}</span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** Where to send the user for a waiting halt. */
function targetFor(detail: string): StageId | null {
  if (detail.startsWith("Gate 1")) return "protocol";
  if (
    detail.startsWith("Gate 2") ||
    detail.includes("screening decision") ||
    detail.startsWith("Screen the remaining")
  )
    return "screening";
  if (detail.includes("full texts") || detail.includes("upload"))
    return "fulltext";
  if (detail.startsWith("Gate 3")) return "report";
  return null;
}

function Value({
  value,
  warn = false,
}: {
  value: string | null;
  warn?: boolean;
}) {
  if (value == null)
    return <span className={`${styles.small} ${styles.muted}`}>Pending</span>;
  return (
    <span className={`${styles.metricValue} ${warn ? styles.metricWarn : ""}`}>
      {value}
    </span>
  );
}

/**
 * Citation searching is optional in PRISMA 2020 but must be reported. The
 * author may stop it before its records are screened; the skip is written to
 * the decision log and disclosed under information sources (item 6).
 */
function ChasingBanner({
  reviewId,
  chasing,
  onDone,
}: {
  reviewId: string;
  chasing: ChasingState;
  onDone: () => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const skip = useMutation({
    mutationFn: () => review.skipCitationChasing(reviewId),
    onSuccess: onDone,
  });
  if (!chasing.canSkip) return null;
  const left = chasing.records - chasing.screened;
  const what =
    chasing.status === "screening"
      ? `Citation searching found ${chasing.records} new records; ${left} still need title and abstract screening before extraction can start.`
      : "Citation searching of the included studies is running or about to run. Its new records must be screened before extraction can start.";
  return (
    <div
      className={styles.banner}
      role="region"
      aria-label="Citation searching"
    >
      <span>
        {what}
        {confirming && (
          <>
            {" "}
            Skipping stops it here. Its records are left out of screening and
            the flow diagram, and the report states that citation searching was
            started and stopped before screening. The studies already included
            go straight on to extraction and synthesis. This cannot be undone.
          </>
        )}
        {skip.isError && (
          <span className={styles.error}> {errorMessage(skip.error)}</span>
        )}
      </span>
      <span className={styles.bannerActions}>
        {confirming ? (
          <>
            <button
              type="button"
              className={`${styles.button} ${styles.primary}`}
              onClick={() => skip.mutate()}
              disabled={skip.isPending}
            >
              Skip and continue to extraction
            </button>
            <button
              type="button"
              className={styles.button}
              onClick={() => setConfirming(false)}
              disabled={skip.isPending}
            >
              Keep citation searching
            </button>
          </>
        ) : (
          <button
            type="button"
            className={styles.button}
            onClick={() => setConfirming(true)}
          >
            Skip citation searching
          </button>
        )}
      </span>
    </div>
  );
}

export function Overview({ reviewId, question, progress }: ScreenProps) {
  const go = useUi((s) => s.go);
  const qc = useQueryClient();
  const svg = useQuery({
    queryKey: ["prismaSvg", reviewId],
    queryFn: () => review.prismaSvg(reviewId),
  });
  const quality = useQuery({
    queryKey: ["quality", reviewId],
    queryFn: () => review.quality(reviewId),
  });
  const log = useQuery({
    queryKey: ["log", reviewId],
    queryFn: () => review.log(reviewId, 60),
  });
  const refresh = () =>
    void qc.invalidateQueries({ queryKey: ["progress", reviewId] });
  const pause = useMutation({
    mutationFn: () => review.pause(reviewId),
    onSuccess: refresh,
  });
  const resume = useMutation({
    mutationFn: () => review.resume(reviewId),
    onSuccess: refresh,
  });
  const retry = useMutation({
    mutationFn: () => review.retryFailed(reviewId),
    onSuccess: refresh,
  });
  const dismiss = useMutation({
    mutationFn: () => review.dismissFailed(reviewId),
    onSuccess: refresh,
  });
  const exportCsv = useMutation({
    mutationFn: () => review.exportFile(reviewId, "csv"),
  });

  const halt = progress?.halt;
  const paused = halt?.kind === "paused";
  const waitingTarget =
    halt?.kind === "waiting" ? targetFor(halt.detail) : null;
  const q = quality.data;

  return (
    <>
      <Header
        eyebrow={halt?.kind === "done" ? "Overview · Signed off" : paused ? "Overview · Paused" : `Overview · ${haltText(halt).length > 60 ? "Running" : haltText(halt)}`}
        title={question}
        actions={
          paused ? (
            <button
              type="button"
              className={styles.primary}
              onClick={() => resume.mutate()}
              disabled={resume.isPending}
            >
              <Icon name="play" size={15} />
              Resume run
            </button>
          ) : (
            <button
              type="button"
              className={styles.button}
              onClick={() => pause.mutate()}
              disabled={pause.isPending || halt?.kind === "done"}
            >
              <Icon name="pause" size={15} />
              Pause run
            </button>
          )
        }
      />

      {paused && progress?.pause && (
        <div className={`${styles.banner} ${styles.bannerWarn}`} role="status">
          <span>
            Paused. {progress.pause.reason}
            {progress.pause.rateLimited ? " It resumes on its own." : ""}
          </span>
        </div>
      )}
      {progress && progress.failed > 0 && (
        <div className={`${styles.banner} ${styles.bannerWarn}`} role="alert">
          <span>
            {progress.failed} jobs failed. Their errors are in the decision log
            and job list.
          </span>
          <span className={styles.bannerActions}>
            <button
              type="button"
              className={styles.button}
              onClick={() => retry.mutate()}
            >
              Retry
            </button>
            <button
              type="button"
              className={styles.button}
              onClick={() => dismiss.mutate()}
              title="Only failed searches and downloads can be set aside; this is disclosed in the report."
            >
              Continue without
            </button>
          </span>
        </div>
      )}
      {progress?.chasing && (
        <ChasingBanner
          reviewId={reviewId}
          chasing={progress.chasing}
          onDone={() => {
            refresh();
            void qc.invalidateQueries({ queryKey: ["prismaSvg", reviewId] });
            void qc.invalidateQueries({ queryKey: ["log", reviewId] });
          }}
        />
      )}
      {halt?.kind === "waiting" && (
        <div className={`${styles.banner} ${styles.bannerWarn}`} role="status">
          <span>
            <strong>Your turn.</strong> {halt.detail}
          </span>
          {waitingTarget && (
            <button
              type="button"
              className={styles.primary}
              onClick={() =>
                go({ name: "review", reviewId, stage: waitingTarget })
              }
            >
              Open
              <Icon name="arrow" size={15} />
            </button>
          )}
        </div>
      )}

      {progress && <Pipeline reviewId={reviewId} progress={progress} />}

      <div className={styles.grid}>
        <section className={styles.card} aria-labelledby="flow-title">
          <div className={styles.cardHead}>
            <h2 id="flow-title" className={styles.cardTitle}>
              PRISMA 2020 flow
            </h2>
            <span className={`${styles.muted} ${styles.small}`}>
              Built live from the decision log
            </span>
          </div>
          {/* The SVG is generated in Rust from v_prisma_counts with escaped text. */}
          <div
            className={styles.flow}
            dangerouslySetInnerHTML={{ __html: svg.data ?? "" }}
          />
        </section>

        <div className={styles.column}>
          <section className={styles.card} aria-labelledby="quality-title">
            <h2 id="quality-title" className={styles.cardTitle}>
              Quality checks
            </h2>
            <div className={styles.metric}>
              <span>Screening recall (Gate 2)</span>
              <Value
                value={
                  q?.gate2Outcome === "skip"
                    ? "Skipped"
                    : q?.gate2 && q.gate2.judged < q.gate2.sampled
                      ? `${q.gate2.judged} of ${q.gate2.sampled} screened`
                      : q?.gate2?.recall != null
                        ? pct(q.gate2.recall)
                        : null
                }
              />
            </div>
            <div className={styles.metric}>
              <span>Agreement, screener A vs B</span>
              <Value
                value={q?.kappaAb != null ? `κ ${q.kappaAb.toFixed(2)}` : null}
              />
            </div>
            <div className={styles.metric}>
              <span>Extraction quotes verified</span>
              <Value
                value={q?.quoteRate != null ? pct(q.quoteRate) : null}
                warn={q?.quoteRate != null && q.quoteRate < 0.98}
              />
            </div>
            <div className={styles.metric}>
              <span>Seed studies retrieved</span>
              <Value
                value={
                  q?.seedFound ? `${q.seedFound[0]} of ${q.seedFound[1]}` : null
                }
              />
            </div>
            <div className={styles.metric}>
              <span>Drift on re-screening</span>
              <Value
                value={q?.drift != null ? pct(q.drift) : null}
                warn={q?.drift != null && q.drift > 0.1}
              />
            </div>
            {q?.quoteRate != null && q.quoteRate < 0.98 && (
              <p className={`${styles.small} ${styles.metricWarn}`}>
                Below 98%: check the flagged extractions carefully.
              </p>
            )}
            {q?.drift != null && q.drift > 0.1 && (
              <p className={`${styles.small} ${styles.metricWarn}`}>
                More than 10% of re-screened decisions changed.
              </p>
            )}
          </section>

          <section className={styles.card} aria-labelledby="log-title">
            <div className={styles.cardHead}>
              <h2 id="log-title" className={styles.cardTitle}>
                Decision log
              </h2>
              <button
                type="button"
                className={styles.link}
                onClick={() => exportCsv.mutate()}
              >
                Export CSV
              </button>
            </div>
            {exportCsv.data && (
              <p className={`${styles.small} ${styles.muted}`}>
                Saved to {exportCsv.data}
              </p>
            )}
            {exportCsv.isError && (
              <p className={styles.error}>{errorMessage(exportCsv.error)}</p>
            )}
            <ul className={styles.log}>
              {(log.data ?? []).map((e) => (
                <li key={e.id} className={styles.logItem}>
                  <span className={styles.logTime}>{time(e.time)}</span>
                  <span className={styles.logText}>
                    {e.actor} · {e.decision.replace(/_/g, " ")}
                    {e.target ? (
                      <>
                        {" · "}
                        <strong>
                          {e.target.length > 70
                            ? `${e.target.slice(0, 70)}…`
                            : e.target}
                        </strong>
                      </>
                    ) : null}
                    {e.detail ? (
                      <span className={styles.muted}>
                        {" "}
                        ·{" "}
                        {e.detail.length > 140
                          ? `${e.detail.slice(0, 140)}…`
                          : e.detail}
                      </span>
                    ) : null}
                  </span>
                </li>
              ))}
              {log.data?.length === 0 && (
                <li className={styles.muted}>No decisions yet.</li>
              )}
            </ul>
          </section>
        </div>
      </div>
    </>
  );
}
