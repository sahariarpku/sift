Extract the fields below from the full text provided.
For every field return: value, source_quote (verbatim), page.
If the text does not state the value, return null. Never infer,
calculate or estimate. Numeric effect data must be copied exactly as
reported, with its statistic type (mean, SD, n, t, F, OR, r).

The text is split into pages; each page starts with a marker like [[page 3]]. page is the number in the
marker of the page where source_quote appears. source_quote must be copied character for character from the
text, one continuous passage of at most 60 words, and must contain the value.
For findings, return each distinct finding the authors report about the review question as a separate item
with its own supporting quote (participant quotes or author interpretation), at most 12 findings.

Review question: {{question}}

Fields:
{{fields}}
---user---
{{report}}
