-- Review file schema, version 3: derived views. Every PRISMA count comes from
-- here, computed from the event log and the record/report tables (principle 1).
--
-- Event conventions:
--   ta_screen / ft_screen: screener_a and screener_b write individual decisions;
--     the final decision is written by 'resolver' (both screeners agreed, a code rule),
--     'arbiter' (model) or 'human'. An arbiter 'uncertain' sends the item to the human queue.
--   search: 'run' and 'superseded' on search_run targets.
--   dedup: 'duplicate' on the removed record (reason_code = rule, rationale = kept record id).
--   retrieval: open_access | proxy | uploaded | not_retrieved on report targets.

-- Events not superseded by a later correction.
CREATE VIEW v_current_event AS
SELECT e.* FROM event e
WHERE NOT EXISTS (SELECT 1 FROM event s WHERE s.supersedes = e.id);

-- Search runs that count: not seed-set lookups, not superseded by a broadened rerun.
CREATE VIEW v_current_run AS
SELECT r.* FROM search_run r
WHERE r.source NOT LIKE 'seed%'
  AND NOT EXISTS (
    SELECT 1 FROM v_current_event e
    WHERE e.stage = 'search' AND e.target_type = 'search_run' AND e.target_id = r.id AND e.decision = 'superseded');

CREATE VIEW v_identified AS
SELECT rec.*, rec.rowid AS rec_order, run.source AS run_source FROM record rec JOIN v_current_run run ON run.id = rec.search_run_id;

CREATE VIEW v_duplicate AS
SELECT DISTINCT e.target_id AS record_id FROM v_current_event e
WHERE e.stage = 'dedup' AND e.decision = 'duplicate' AND e.target_type = 'record';

CREATE VIEW v_screenable AS
SELECT i.* FROM v_identified i WHERE i.id NOT IN (SELECT record_id FROM v_duplicate);

-- Latest final decision per target at a screening stage.
CREATE VIEW v_final_decision AS
SELECT e.stage, e.target_id, e.decision, e.reason_code, e.actor_id, e.id AS event_id
FROM v_current_event e
WHERE e.stage IN ('ta_screen', 'ft_screen')
  AND e.actor_id IN ('resolver', 'arbiter', 'human')
  AND e.decision IN ('include', 'exclude')
  AND e.id = (
    SELECT MAX(x.id) FROM v_current_event x
    WHERE x.stage = e.stage AND x.target_id = e.target_id
      AND x.actor_id IN ('resolver', 'arbiter', 'human') AND x.decision IN ('include', 'exclude'));

CREATE VIEW v_latest_retrieval AS
SELECT e.target_id AS report_id, e.decision
FROM v_current_event e
WHERE e.stage = 'retrieval'
  AND e.id = (SELECT MAX(x.id) FROM v_current_event x WHERE x.stage = 'retrieval' AND x.target_id = e.target_id);

CREATE VIEW v_included_report AS
SELECT rep.*, rep.rowid AS rep_order FROM report rep
JOIN v_final_decision f ON f.stage = 'ft_screen' AND f.target_id = rep.id AND f.decision = 'include';

CREATE VIEW v_prisma_sources AS
SELECT run_source AS source, COUNT(*) AS n FROM v_identified GROUP BY run_source;

CREATE VIEW v_prisma_ft_reasons AS
SELECT COALESCE(f.reason_code, 'unspecified') AS reason_code, COUNT(*) AS n
FROM v_final_decision f JOIN report rep ON rep.id = f.target_id
WHERE f.stage = 'ft_screen' AND f.decision = 'exclude'
GROUP BY COALESCE(f.reason_code, 'unspecified');

-- One row: every PRISMA 2020 box.
CREATE VIEW v_prisma_counts AS
SELECT
  (SELECT COUNT(*) FROM v_identified WHERE run_source NOT IN ('citation_chasing', 'manual_import')) AS identified_databases,
  (SELECT COUNT(*) FROM v_identified WHERE run_source IN ('citation_chasing', 'manual_import')) AS identified_other,
  (SELECT COUNT(*) FROM v_identified i JOIN v_duplicate d ON d.record_id = i.id) AS duplicates_removed,
  (SELECT COUNT(*) FROM v_screenable) AS records_screened,
  (SELECT COUNT(*) FROM v_screenable s JOIN v_final_decision f
     ON f.stage = 'ta_screen' AND f.target_id = s.id AND f.decision = 'exclude') AS records_excluded,
  (SELECT COUNT(*) FROM v_screenable s JOIN v_final_decision f
     ON f.stage = 'ta_screen' AND f.target_id = s.id AND f.decision = 'include') AS reports_sought,
  (SELECT COUNT(*) FROM v_latest_retrieval WHERE decision = 'not_retrieved') AS reports_not_retrieved,
  (SELECT COUNT(*) FROM v_latest_retrieval WHERE decision IN ('open_access', 'proxy', 'uploaded')) AS reports_assessed,
  (SELECT COUNT(*) FROM v_final_decision WHERE stage = 'ft_screen' AND decision = 'exclude') AS reports_excluded,
  (SELECT COUNT(DISTINCT study_id) FROM v_included_report WHERE study_id IS NOT NULL) AS studies_included,
  (SELECT COUNT(*) FROM v_included_report) AS reports_included;
