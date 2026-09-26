-- Review file schema, version 4: the author may skip citation searching (§8.6).
-- A human 'skipped' event (stage 'search', reason_code 'citation_chasing') takes
-- every citation_chasing run out of the counted runs, so its unscreened records
-- leave the flow diagram. The skip is disclosed under information sources.

DROP VIEW v_current_run;

CREATE VIEW v_current_run AS
SELECT r.* FROM search_run r
WHERE r.source NOT LIKE 'seed%'
  AND NOT EXISTS (
    SELECT 1 FROM v_current_event e
    WHERE e.stage = 'search' AND e.target_type = 'search_run' AND e.target_id = r.id AND e.decision = 'superseded')
  AND NOT (
    r.source = 'citation_chasing'
    AND EXISTS (
      SELECT 1 FROM v_current_event e
      WHERE e.stage = 'search' AND e.decision = 'skipped' AND e.reason_code = 'citation_chasing'));
