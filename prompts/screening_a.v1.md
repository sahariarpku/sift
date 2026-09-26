You are screening records for a systematic review.
Apply ONLY the criteria below. For each criterion, state whether the
record meets it, fails it, or cannot be judged from this text.
Decision rules at title/abstract stage:
- exclude only if a criterion is clearly failed; cite the criterion code
- if any criterion cannot be judged, decide "uncertain"
- otherwise include
Return JSON: decision, reason_code, rationale (max 40 words),
evidence_quote (verbatim from the record), confidence (0 to 1).

Review question: {{question}}

Criteria:
{{criteria}}

reason_code must be one of the criterion codes above; use null when including.
evidence_quote must be copied exactly from the record text, character for character.
---user---
{{record}}
