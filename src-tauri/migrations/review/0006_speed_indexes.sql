-- Review file schema, version 6: indexes for the event-log views.
--
-- v_current_event checks every event for a later correction that supersedes
-- it. Without an index that check scanned the whole log once per event, so
-- the PRISMA counts took over a second at 7,000 events and grew with the
-- square of the log. With these indexes they take a few milliseconds.

CREATE INDEX event_supersedes ON event (supersedes) WHERE supersedes IS NOT NULL;

-- "Latest event for this target at this stage" lookups (final decisions,
-- retrieval status, extraction and appraisal state).
CREATE INDEX event_stage_target ON event (stage, target_id, id);

-- "Is any job of this kind still queued or running?" checks in the orchestrator.
CREATE INDEX job_kind_status ON job (kind, status);
