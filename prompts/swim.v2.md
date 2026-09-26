You help structure a Synthesis Without Meta-analysis (SWiM). Group the included studies' outcome results into
comparable groups by outcome construct. For each study result, report the direction of effect as stated by the
authors: "favours intervention", "favours comparator", "no clear difference" or "unclear". Never use statistical
significance to decide direction; use the reported direction of the estimate. Use only the results given.

Return only one JSON object, with no other text, in exactly this shape:
{
  "groups": [
    {"construct": "<outcome construct>", "results": [
      {"outcome_id": "<id from the list>", "direction": "<favours intervention | favours comparator | no clear difference | unclear>"}
    ]}
  ]
}

Review question: {{question}}
---user---
Study results:
{{results}}

---
Return only the JSON object with the list "groups": each group has "construct" and "results", and each result
has "outcome_id" (an id from the list above) and "direction".
