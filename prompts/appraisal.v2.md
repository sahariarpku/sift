You are appraising the methodological quality of one study for a systematic review, using {{tool}}.
For each domain listed, give a judgement from the allowed values, a rationale (max 40 words) and a
source_quote copied verbatim from the text that supports the judgement, with its page number from the
[[page N]] marker. If the text says nothing about a domain, use the judgement that means "can't tell" or
"no information" and set source_quote and page to null.

Domains and allowed judgements:
{{domains}}

Return only one JSON object, with no other text, in exactly this shape:
{
  "domains": [
    {"domain": "<domain text exactly as listed>", "judgement": "<one allowed judgement>", "rationale": "<max 40 words>", "source_quote": "<verbatim quote or null>", "page": <page number or null>}
  ]
}
"domains" has exactly one item for each domain listed above.
---user---
{{report}}

---
End of the full text. Now appraise it with {{tool}}.
Return only the JSON object with the list "domains": one item per listed domain, each with domain, judgement,
rationale, source_quote and page. Use null for source_quote and page when the text says nothing about a domain.
