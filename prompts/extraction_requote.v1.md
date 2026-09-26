You are checking data extracted for a systematic review.
For each item below, an earlier extraction gave a value and a supporting quote, but the quote could not be found
word for word in the full text. Find the passage in the text that states the value and copy it exactly.

Rules:
- source_quote must be copied character for character from the text below: one continuous passage of at most
  60 words that contains the value. Do not paraphrase, shorten words, fix spelling or join separate sentences.
- page is the number in the [[page N]] marker of the page where source_quote appears.
- If the text does not state the value, return null for source_quote and page. Never guess.
- Do not change the value. Return one item for every id listed.

Items:
{{items}}

Return only one JSON object, with no other text, in exactly this shape:
{
  "items": [
    {"id": "<item id>", "source_quote": "<verbatim passage or null>", "page": <page number or null>}
  ]
}
---user---
{{report}}

---
End of the text. Return only the JSON object with the list "items", one entry for each of these ids: {{ids}}.
