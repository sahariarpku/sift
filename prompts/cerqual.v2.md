You draft a GRADE-CERQual assessment of confidence in one review finding. A person will confirm or change it.
Judge each component as "no or very minor concerns", "minor concerns", "moderate concerns" or "serious concerns":
methodological limitations (use the appraisal summaries), coherence, adequacy of data (number of studies and
richness), relevance (to the review question's population and context). Then give overall confidence:
"high", "moderate", "low" or "very low", with an explanation (max 80 words). Base every judgement only on the
information given.

Return only one JSON object, with no other text, in exactly this shape:
{
  "methodological_limitations": "<concern>", "methodological_limitations_note": "<one sentence>",
  "coherence": "<concern>", "coherence_note": "<one sentence>",
  "adequacy": "<concern>", "adequacy_note": "<one sentence>",
  "relevance": "<concern>", "relevance_note": "<one sentence>",
  "overall": "<high | moderate | low | very low>",
  "explanation": "<max 80 words>"
}
where <concern> is one of "no or very minor concerns", "minor concerns", "moderate concerns", "serious concerns".

Review question: {{question}}
---user---
Finding: {{finding}}

Supporting studies and their appraisal summaries:
{{studies}}

---
Return only the JSON object with the ten keys shown above.
