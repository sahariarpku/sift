-- Review file schema, version 5: the author may skip any stage after Gate 1
-- and keep what it already did. A skip is a human 'skipped' event on the
-- review (reason_code 'stage_skip') at the stage's event stage; a later
-- 'resumed' event supersedes it.
--
-- Under a skip the flow diagram counts only what the stage finished:
--   ta_screen skipped: records screened = records with a final decision;
--     the rest are 'records not screened' (screening stopped by the author).
--   ft_screen skipped: reports assessed = retrieved reports with a final
--     decision; retrieved reports without one are 'awaiting classification';
--     reports sought but never retrieved are 'not retrieved'.

CREATE VIEW v_stage_skip AS
SELECT e.stage, e.id AS event_id, e.created_at, e.rationale FROM v_current_event e
WHERE e.target_type = 'review' AND e.decision = 'skipped' AND e.reason_code = 'stage_skip';

DROP VIEW v_prisma_counts;

CREATE VIEW v_prisma_counts AS
WITH
  skip AS (
    SELECT
      EXISTS(SELECT 1 FROM v_stage_skip WHERE stage = 'ta_screen') AS ta,
      EXISTS(SELECT 1 FROM v_stage_skip WHERE stage = 'ft_screen') AS ft),
  n AS (
    SELECT
      (SELECT COUNT(*) FROM v_screenable) AS screenable,
      (SELECT COUNT(*) FROM v_screenable s JOIN v_final_decision f
         ON f.stage = 'ta_screen' AND f.target_id = s.id) AS ta_decided,
      (SELECT COUNT(*) FROM v_screenable s JOIN v_final_decision f
         ON f.stage = 'ta_screen' AND f.target_id = s.id AND f.decision = 'include') AS sought,
      (SELECT COUNT(*) FROM v_latest_retrieval WHERE decision = 'not_retrieved') AS not_retrieved,
      (SELECT COUNT(*) FROM v_latest_retrieval WHERE decision IN ('open_access', 'proxy', 'uploaded')) AS retrieved,
      (SELECT COUNT(*) FROM v_latest_retrieval l JOIN v_final_decision f
         ON f.stage = 'ft_screen' AND f.target_id = l.report_id
       WHERE l.decision IN ('open_access', 'proxy', 'uploaded')) AS ft_decided)
SELECT
  (SELECT COUNT(*) FROM v_identified WHERE run_source NOT IN ('citation_chasing', 'manual_import')) AS identified_databases,
  (SELECT COUNT(*) FROM v_identified WHERE run_source IN ('citation_chasing', 'manual_import')) AS identified_other,
  (SELECT COUNT(*) FROM v_identified i JOIN v_duplicate d ON d.record_id = i.id) AS duplicates_removed,
  CASE WHEN skip.ta THEN n.ta_decided ELSE n.screenable END AS records_screened,
  CASE WHEN skip.ta THEN n.screenable - n.ta_decided ELSE 0 END AS records_not_screened,
  (SELECT COUNT(*) FROM v_screenable s JOIN v_final_decision f
     ON f.stage = 'ta_screen' AND f.target_id = s.id AND f.decision = 'exclude') AS records_excluded,
  n.sought AS reports_sought,
  CASE WHEN skip.ft THEN MAX(n.sought - n.retrieved, 0) ELSE n.not_retrieved END AS reports_not_retrieved,
  CASE WHEN skip.ft THEN n.ft_decided ELSE n.retrieved END AS reports_assessed,
  CASE WHEN skip.ft THEN n.retrieved - n.ft_decided ELSE 0 END AS reports_awaiting,
  (SELECT COUNT(*) FROM v_final_decision WHERE stage = 'ft_screen' AND decision = 'exclude') AS reports_excluded,
  (SELECT COUNT(DISTINCT study_id) FROM v_included_report WHERE study_id IS NOT NULL) AS studies_included,
  (SELECT COUNT(*) FROM v_included_report) AS reports_included
FROM skip, n;
