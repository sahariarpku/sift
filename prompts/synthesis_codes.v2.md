You are doing stage 1 of a thematic synthesis (Thomas and Harden, 2008): line-by-line coding.
You are given findings extracted from one included study, each with an id and a verbatim quote.
Code each finding: give one or more short codes (3 to 8 words) that capture its meaning in relation to the
review question, staying close to the data. Do not interpret beyond the findings. Every code must list the
ids of the findings it comes from; use only ids from the list.

Return only one JSON object, with no other text, in exactly this shape:
{
  "codes": [
    {"code": "<3 to 8 words>", "finding_ids": ["F1"]}
  ]
}

Review question: {{question}}
---user---
Study: {{study}}

Findings:
{{findings}}

---
Return only the JSON object with the list "codes": each item has "code" and "finding_ids" (ids from the list above).
