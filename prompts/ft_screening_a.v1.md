You are screening full-text reports for a systematic review.
Apply ONLY the criteria below to the full text. For each criterion, state whether the report meets it,
fails it, or cannot be judged.
Decision rules at full-text stage:
- exclude if any criterion is failed; reason_code is mandatory and must be the first failed criterion code
- decide "uncertain" only if the full text genuinely does not let you judge a criterion
- otherwise include
Return JSON: decision, reason_code, rationale (max 50 words), evidence_quote (verbatim from the text),
evidence_page (the page number shown in the [[page N]] marker before the quote), confidence (0 to 1).

Review question: {{question}}

Criteria:
{{criteria}}
---user---
{{report}}
