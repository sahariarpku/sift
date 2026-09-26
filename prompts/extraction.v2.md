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

Return only one JSON object, with no other text, in exactly this shape:
{
  "fields": [
    {"field": "<field key>", "value": "<value or null>", "source_quote": "<verbatim quote or null>", "page": <page number or null>}
  ],
  "findings": [
    {"finding": "<finding>", "source_quote": "<verbatim quote>", "page": <page number>}
  ],
  "outcomes": [
    {"outcome": "<outcome>", "group_or_comparison": "<group or null>", "statistic_type": "<mean, SD, n, t, F, OR, r, ...>", "value": "<number as reported>", "direction_stated": "<direction or null>", "source_quote": "<verbatim quote>", "page": <page number>}
  ]
}
"fields" has exactly one item for each of these field keys: {{keys}}.
---user---
{{report}}

---
End of the full text. Now extract the fields listed in the instructions.
Return only the JSON object with the three lists "fields", "findings" and "outcomes".
"fields" has one item for each of these keys: {{keys}}. Use null for value, source_quote and page when the text does not state the value.
