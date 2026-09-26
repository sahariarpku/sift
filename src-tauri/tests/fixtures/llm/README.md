# LLM fixtures

| File | Provenance |
| --- | --- |
| `freellm_models.json` | Real `GET /v1/models` response from a local FreeLLM gateway (github.com/Devansh-365/freellm), 2026-09-24, trimmed to the first 6 models. |
| `freellm_chat_auto.json` | Real `POST /v1/chat/completions` response from the same gateway with `model: "auto"`, 2026-09-24. The gateway also sent `X-Routed-Via: openrouter/dots-studio/dots-3-note-preview:free`. |
| `anthropic_messages_json.json` | Shape from the Messages API reference (platform.claude.com/docs, `POST /v1/messages`). Replace with a recorded response once an Anthropic key is available. |
| `anthropic_models.json` | Shape from the Models API reference (`GET /v1/models`: `data`, `has_more`, `first_id`, `last_id`, `capabilities`). Replace with a recorded response once a key is available. |
| `ollama_models.json` | Shape from docs.ollama.com/api/openai-compatibility (`/v1/models`: `owned_by` defaults to `library`). Replace with a recorded response from a local Ollama. |

No fixture contains an API key.
