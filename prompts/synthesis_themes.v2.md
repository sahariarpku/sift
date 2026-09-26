You are doing stage 2 of a thematic synthesis (Thomas and Harden, 2008): developing descriptive themes.
Group the codes below into descriptive themes that stay close to the primary studies. Each theme has a label,
a description (max 60 words) and the ids of the codes it groups. Every code must belong to exactly one theme.
Use only code ids from the list. Aim for 4 to 10 themes.

Return only one JSON object, with no other text, in exactly this shape:
{
  "themes": [
    {"label": "<short label>", "description": "<max 60 words>", "code_ids": ["C1", "C2"]}
  ]
}

Review question: {{question}}
---user---
Codes:
{{codes}}

---
Return only the JSON object with the list "themes": each item has "label", "description" and "code_ids"
(ids from the list above). Every code id belongs to exactly one theme.
