//! Provider management behind the Settings screen: create, edit, delete,
//! fetch models and test. Keys go to the keychain; summaries carry only a hint.

use std::time::Duration;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::llm::presets::{self, Preset};
use crate::llm::store::{self, ProviderRecord};
use crate::llm::{
    build_provider, LlmError, ModelInfo, Protocol, ProviderConn, ProviderOptions, RetryPolicy,
    TestResult,
};
use crate::secrets::{self, SecretStatus, SecretStore};

/// What the add/edit form sends. `api_key` is only ever frontend -> Rust.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDraft {
    pub id: Option<String>,
    pub preset: String,
    pub display_name: String,
    pub base_url: String,
    /// A newly typed key. Empty or absent keeps the saved key.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub remove_key: bool,
    #[serde(default)]
    pub data_policy_warning: bool,
    #[serde(default = "default_concurrency")]
    pub concurrency: u32,
    /// Model for Test connection; defaults to the first listed model.
    #[serde(default)]
    pub test_model: Option<String>,
}

fn default_concurrency() -> u32 {
    4
}

impl std::fmt::Debug for ProviderDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderDraft")
            .field("id", &self.id)
            .field("preset", &self.preset)
            .field("display_name", &self.display_name)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    Connected,
    Failed,
    /// A local server (Ollama, LM Studio) that is not answering.
    NotRunning,
    Unreachable,
    Untested,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSummary {
    pub id: String,
    pub preset: String,
    pub display_name: String,
    pub protocol: Protocol,
    pub base_url: String,
    /// Base URL without the scheme, for the list ("openrouter.ai/api/v1").
    pub host_label: String,
    pub key: SecretStatus,
    pub requires_key: bool,
    pub local: bool,
    pub models: Vec<ModelInfo>,
    pub data_policy_warning: bool,
    pub concurrency: u32,
    pub status: ProviderStatus,
    pub status_message: Option<String>,
    pub url_warning: Option<String>,
    pub get_key_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestOutcome {
    pub ok: bool,
    pub status: ProviderStatus,
    pub message: String,
    pub result: Option<TestResult>,
}

pub fn key_ref(id: &str) -> String {
    format!("provider:{id}")
}

fn is_local_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h == "localhost" || h.ends_with(".localhost") || h == "::1" || h.starts_with("127.")
}

/// Validates a base URL. Returns a warning for plain-HTTP remote hosts (§11).
pub fn check_base_url(url: &str) -> Result<Option<String>> {
    let parsed = reqwest::Url::parse(url.trim()).map_err(|_| {
        Error::Invalid("Enter a full base URL, for example https://api.example.com/v1".into())
    })?;
    let host = parsed.host_str().unwrap_or_default().to_owned();
    if host.is_empty() {
        return Err(Error::Invalid("The base URL has no host.".into()));
    }
    match parsed.scheme() {
        "https" => Ok(None),
        "http" if is_local_host(&host) => Ok(None),
        "http" => Ok(Some(format!(
            "{host} is reached over plain http, so your key and prompts would travel unencrypted. Use https for remote servers."
        ))),
        other => Err(Error::Invalid(format!("Unsupported URL scheme {other}. Use https."))),
    }
}

fn preset_for(id: &str) -> Result<Preset> {
    presets::find(id).ok_or_else(|| Error::Invalid(format!("Unknown preset {id}.")))
}

fn new_key(draft: &ProviderDraft) -> Option<&str> {
    draft
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
}

/// Builds a connection from the form, using the saved key when no new key was typed.
pub fn conn_for(
    app: &Connection,
    secrets: &dyn SecretStore,
    draft: &ProviderDraft,
    retry: RetryPolicy,
) -> Result<ProviderConn> {
    let preset = preset_for(&draft.preset)?;
    check_base_url(&draft.base_url)?;
    let existing = match &draft.id {
        Some(id) => store::get(app, id)?,
        None => None,
    };
    let api_key = match (new_key(draft), draft.remove_key, &existing) {
        (Some(k), _, _) => Some(k.to_owned()),
        (None, true, _) => None,
        (None, false, Some(rec)) => match &rec.api_key_ref {
            Some(r) => secrets.get(r)?,
            None => None,
        },
        (None, false, None) => None,
    };
    let options = existing
        .as_ref()
        .map(|r| r.options.clone())
        .unwrap_or_else(|| preset.options());
    let test_model = draft
        .test_model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .or_else(|| {
            existing
                .as_ref()
                .and_then(|r| r.models.first().map(|m| m.id.clone()))
        });
    Ok(ProviderConn {
        protocol: preset.protocol,
        base_url: draft.base_url.trim().to_owned(),
        api_key,
        options,
        test_model,
        retry,
        timeout: Duration::from_secs(60),
    })
}

pub fn save(
    app: &Connection,
    secrets: &dyn SecretStore,
    draft: &ProviderDraft,
) -> Result<ProviderSummary> {
    let preset = preset_for(&draft.preset)?;
    let name = draft.display_name.trim();
    if name.is_empty() {
        return Err(Error::Invalid("Give the provider a display name.".into()));
    }
    check_base_url(&draft.base_url)?;
    if !(1..=64).contains(&draft.concurrency) {
        return Err(Error::Invalid(
            "Concurrency must be between 1 and 64.".into(),
        ));
    }
    let existing = match &draft.id {
        Some(id) => Some(
            store::get(app, id)?
                .ok_or_else(|| Error::Invalid("That provider no longer exists.".into()))?,
        ),
        None => None,
    };
    let id = draft.id.clone().unwrap_or_else(crate::reviews::new_id);
    let kref = key_ref(&id);

    let has_saved_key = existing.as_ref().is_some_and(|r| r.api_key_ref.is_some());
    let will_have_key = new_key(draft).is_some() || (has_saved_key && !draft.remove_key);
    if preset.requires_key && !will_have_key {
        return Err(Error::Invalid(format!(
            "{} needs an API key.",
            preset.display_name
        )));
    }

    if let Some(k) = new_key(draft) {
        secrets.set(&kref, k)?;
    } else if draft.remove_key {
        secrets.delete(&kref)?;
    }

    let rec = ProviderRecord {
        id: id.clone(),
        preset: preset.id.clone(),
        display_name: name.to_owned(),
        protocol: preset.protocol,
        base_url: draft.base_url.trim().trim_end_matches('/').to_owned(),
        api_key_ref: will_have_key.then(|| kref.clone()),
        models: existing
            .as_ref()
            .map(|r| r.models.clone())
            .unwrap_or_default(),
        data_policy_warning: draft.data_policy_warning,
        concurrency: draft.concurrency,
        options: existing
            .as_ref()
            .map(|r| r.options.clone())
            .unwrap_or_else(|| preset.options()),
        last_test_status: None,
        last_test_message: None,
        last_test_at: None,
    };
    store::upsert(app, &rec)?;
    // Changing the address or key invalidates the last test.
    let endpoint_changed = existing
        .as_ref()
        .is_some_and(|e| e.base_url != rec.base_url);
    if endpoint_changed || new_key(draft).is_some() || draft.remove_key {
        app.execute(
            "UPDATE provider SET last_test_status = NULL, last_test_message = NULL, last_test_at = NULL WHERE id = ?1",
            [&id],
        )?;
    }
    let saved = store::get(app, &id)?.ok_or_else(|| Error::Invalid("save failed".into()))?;
    summary(&saved, secrets)
}

pub fn delete(app: &Connection, secrets: &dyn SecretStore, id: &str) -> Result<()> {
    secrets.delete(&key_ref(id))?;
    store::delete(app, id)
}

pub fn summary(rec: &ProviderRecord, secrets: &dyn SecretStore) -> Result<ProviderSummary> {
    let preset = presets::find(&rec.preset);
    let key = match &rec.api_key_ref {
        Some(r) => secrets::status(secrets, r)?,
        None => SecretStatus {
            present: false,
            hint: None,
        },
    };
    let status = match rec.last_test_status.as_deref() {
        Some("ok") => ProviderStatus::Connected,
        Some("not_running") => ProviderStatus::NotRunning,
        Some("unreachable") => ProviderStatus::Unreachable,
        Some(_) => ProviderStatus::Failed,
        None => ProviderStatus::Untested,
    };
    let host_label = rec
        .base_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned();
    Ok(ProviderSummary {
        id: rec.id.clone(),
        preset: rec.preset.clone(),
        display_name: rec.display_name.clone(),
        protocol: rec.protocol,
        base_url: rec.base_url.clone(),
        host_label,
        key,
        requires_key: preset.as_ref().is_some_and(|p| p.requires_key),
        local: preset.as_ref().is_some_and(|p| p.local),
        models: rec.models.clone(),
        data_policy_warning: rec.data_policy_warning,
        concurrency: rec.concurrency,
        status,
        status_message: rec.last_test_message.clone(),
        url_warning: check_base_url(&rec.base_url).ok().flatten(),
        get_key_url: preset.and_then(|p| p.get_key_url),
    })
}

pub fn list(app: &Connection, secrets: &dyn SecretStore) -> Result<Vec<ProviderSummary>> {
    store::list(app)?
        .iter()
        .map(|r| summary(r, secrets))
        .collect()
}

fn is_local_url(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(is_local_host))
        .unwrap_or(false)
}

/// Runs the provider's `test()` and classifies the result for the list badge.
pub async fn run_test(client: reqwest::Client, conn: ProviderConn) -> TestOutcome {
    let local = is_local_url(&conn.base_url);
    match build_provider(conn, client).test().await {
        Ok(result) => TestOutcome {
            ok: true,
            status: ProviderStatus::Connected,
            message: result.message.clone(),
            result: Some(result),
        },
        Err(e) => {
            let status = match (&e, local) {
                (LlmError::Unreachable { .. }, true) => ProviderStatus::NotRunning,
                (LlmError::Unreachable { .. }, false) => ProviderStatus::Unreachable,
                _ => ProviderStatus::Failed,
            };
            let message = match status {
                ProviderStatus::NotRunning => {
                    format!("Not running. Start the local server, then test again. ({e})")
                }
                _ => e.to_string(),
            };
            TestOutcome {
                ok: false,
                status,
                message,
                result: None,
            }
        }
    }
}

/// Stores a test outcome on a saved provider.
pub fn record_test(app: &Connection, id: &str, outcome: &TestOutcome) -> Result<()> {
    let status = match outcome.status {
        ProviderStatus::Connected => "ok",
        ProviderStatus::NotRunning => "not_running",
        ProviderStatus::Unreachable => "unreachable",
        _ => "failed",
    };
    store::set_test_result(app, id, status, &outcome.message)
}

pub async fn fetch_models(
    client: reqwest::Client,
    conn: ProviderConn,
) -> std::result::Result<Vec<ModelInfo>, LlmError> {
    build_provider(conn, client).list_models().await
}

pub fn options_for_preset(id: &str) -> Option<ProviderOptions> {
    presets::find(id).map(|p| p.options())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::secrets::MemoryStore;
    use crate::test_http::{MockServer, Reply};
    use serde_json::json;

    fn draft(preset: &str, name: &str, base: &str, key: Option<&str>) -> ProviderDraft {
        ProviderDraft {
            id: None,
            preset: preset.into(),
            display_name: name.into(),
            base_url: base.into(),
            api_key: key.map(str::to_owned),
            remove_key: false,
            data_policy_warning: false,
            concurrency: 4,
            test_model: None,
        }
    }

    fn openai_chat(model: &str) -> Reply {
        Reply::json(
            200,
            json!({"model": model, "choices": [{"finish_reason": "stop", "message": {"role": "assistant", "content": "OK"}}],
                   "usage": {"prompt_tokens": 9, "completion_tokens": 1}})
            .to_string(),
        )
    }

    /// The Phase 1 exit criterion: add Anthropic, OpenRouter and a local
    /// Ollama provider and get a successful test from each (mocked HTTP).
    #[tokio::test]
    async fn anthropic_openrouter_and_ollama_can_be_added_and_tested() {
        let dir = tempfile::tempdir().unwrap();
        let app = db::open_app(&dir.path().join("app.db")).unwrap();
        let secrets = MemoryStore::default();
        let client = reqwest::Client::new();

        let anthropic =
            MockServer::routes(vec![
                (
                    Some("GET"),
                    "/v1/models",
                    vec![Reply::json(
                        200,
                        include_str!("../tests/fixtures/llm/anthropic_models.json"),
                    )],
                ),
                (
                    Some("POST"),
                    "/v1/messages",
                    vec![Reply::json(200, json!({
                "model": "claude-sonnet-5", "content": [{"type": "text", "text": "OK"}],
                "stop_reason": "end_turn", "usage": {"input_tokens": 12, "output_tokens": 1}
            }).to_string())],
                ),
            ])
            .await;
        let openrouter = MockServer::routes(vec![
            (
                Some("GET"),
                "/api/v1/models",
                vec![Reply::json(
                    200,
                    r#"{"data":[{"id":"anthropic/claude-sonnet-5","name":"Claude Sonnet 5"}]}"#,
                )],
            ),
            (
                Some("POST"),
                "/api/v1/chat/completions",
                vec![openai_chat("anthropic/claude-sonnet-5")],
            ),
        ])
        .await;
        let ollama = MockServer::routes(vec![
            (
                Some("GET"),
                "/v1/models",
                vec![Reply::json(
                    200,
                    include_str!("../tests/fixtures/llm/ollama_models.json"),
                )],
            ),
            (
                Some("POST"),
                "/v1/chat/completions",
                vec![openai_chat("llama3.2:latest")],
            ),
        ])
        .await;

        let cases = [
            draft(
                "anthropic",
                "Anthropic",
                anthropic.base(),
                Some("sk-ant-api03-test-key-4Kf2"),
            ),
            draft(
                "openrouter",
                "OpenRouter",
                &format!("{}/api/v1", openrouter.base()),
                Some("sk-or-v1-test-key-9aQ1"),
            ),
            draft(
                "ollama",
                "Ollama (on this computer)",
                &format!("{}/v1", ollama.base()),
                None,
            ),
        ];
        for d in cases {
            let saved = save(&app, &secrets, &d).unwrap();
            assert_eq!(saved.status, ProviderStatus::Untested);

            // Fetch models, cache them, then test using the first one.
            let edit = ProviderDraft {
                id: Some(saved.id.clone()),
                api_key: None,
                ..d.clone()
            };
            let conn = conn_for(&app, &secrets, &edit, RetryPolicy::fast()).unwrap();
            let models = fetch_models(client.clone(), conn).await.unwrap();
            store::set_models(&app, &saved.id, &models).unwrap();

            let conn = conn_for(&app, &secrets, &edit, RetryPolicy::fast()).unwrap();
            let outcome = run_test(client.clone(), conn).await;
            assert!(outcome.ok, "{}: {}", d.display_name, outcome.message);
            record_test(&app, &saved.id, &outcome).unwrap();
        }

        let all = list(&app, &secrets).unwrap();
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|p| p.status == ProviderStatus::Connected));
        assert_eq!(all[0].key.hint.as_deref(), Some("key ending 4Kf2"));
        assert_eq!(all[1].key.hint.as_deref(), Some("key ending 9aQ1"));
        assert!(!all[2].key.present && all[2].local);

        // The keys went where they belong and nowhere else.
        assert_eq!(
            anthropic.requests()[1].header("x-api-key"),
            Some("sk-ant-api03-test-key-4Kf2")
        );
        assert_eq!(
            openrouter.requests()[1].header("authorization"),
            Some("Bearer sk-or-v1-test-key-9aQ1")
        );
        assert!(ollama
            .requests()
            .iter()
            .all(|r| r.header("authorization").is_none()));
        // Summaries never carry a key.
        let json = serde_json::to_string(&all).unwrap();
        assert!(!json.contains("sk-ant") && !json.contains("sk-or"));
    }

    #[tokio::test]
    async fn stopped_local_server_shows_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let app = db::open_app(&dir.path().join("app.db")).unwrap();
        let secrets = MemoryStore::default();
        let mut d = draft("ollama", "Ollama", "http://127.0.0.1:9/v1", None);
        d.test_model = Some("llama3.2".into());
        let saved = save(&app, &secrets, &d).unwrap();
        let conn = conn_for(
            &app,
            &secrets,
            &ProviderDraft {
                id: Some(saved.id.clone()),
                ..d
            },
            RetryPolicy::fast(),
        )
        .unwrap();
        let outcome = run_test(reqwest::Client::new(), conn).await;
        assert_eq!(outcome.status, ProviderStatus::NotRunning);
        record_test(&app, &saved.id, &outcome).unwrap();
        assert_eq!(
            list(&app, &secrets).unwrap()[0].status,
            ProviderStatus::NotRunning
        );
    }

    #[test]
    fn keys_are_kept_replaced_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let app = db::open_app(&dir.path().join("app.db")).unwrap();
        let secrets = MemoryStore::default();
        let s = save(
            &app,
            &secrets,
            &draft(
                "custom",
                "Gateway",
                "http://127.0.0.1:31415/v1",
                Some("first-key-000001"),
            ),
        )
        .unwrap();
        let kref = key_ref(&s.id);
        assert_eq!(
            secrets.get(&kref).unwrap().as_deref(),
            Some("first-key-000001")
        );

        let mut edit = draft("custom", "Gateway", "http://127.0.0.1:31415/v1", Some(""));
        edit.id = Some(s.id.clone());
        save(&app, &secrets, &edit).unwrap();
        assert_eq!(
            secrets.get(&kref).unwrap().as_deref(),
            Some("first-key-000001"),
            "blank keeps the key"
        );

        edit.api_key = Some("second-key-000002".into());
        save(&app, &secrets, &edit).unwrap();
        assert_eq!(
            secrets.get(&kref).unwrap().as_deref(),
            Some("second-key-000002")
        );

        edit.api_key = None;
        edit.remove_key = true;
        let s = save(&app, &secrets, &edit).unwrap();
        assert!(!s.key.present);
        assert!(secrets.get(&kref).unwrap().is_none());

        delete(&app, &secrets, &s.id).unwrap();
        assert!(list(&app, &secrets).unwrap().is_empty());
    }

    #[test]
    fn validation() {
        let dir = tempfile::tempdir().unwrap();
        let app = db::open_app(&dir.path().join("app.db")).unwrap();
        let secrets = MemoryStore::default();
        let err = save(
            &app,
            &secrets,
            &draft("anthropic", "Anthropic", "https://api.anthropic.com", None),
        )
        .unwrap_err();
        assert!(err.to_string().contains("needs an API key"));
        assert!(save(
            &app,
            &secrets,
            &draft("custom", " ", "https://x.org/v1", None)
        )
        .is_err());
        assert!(save(
            &app,
            &secrets,
            &draft("nope", "X", "https://x.org/v1", None)
        )
        .is_err());
        assert!(save(&app, &secrets, &draft("custom", "X", "not a url", None)).is_err());
    }

    #[test]
    fn plain_http_is_allowed_locally_and_warned_remotely() {
        assert_eq!(check_base_url("http://localhost:11434/v1").unwrap(), None);
        assert_eq!(check_base_url("http://127.0.0.1:31415/v1").unwrap(), None);
        assert_eq!(
            check_base_url("https://openrouter.ai/api/v1").unwrap(),
            None
        );
        assert!(check_base_url("http://gateway.example.edu/v1")
            .unwrap()
            .unwrap()
            .contains("unencrypted"));
        assert!(check_base_url("ftp://x.org").is_err());
    }

    #[test]
    fn draft_debug_hides_key() {
        let d = draft("custom", "G", "https://x.org", Some("super-secret-key"));
        assert!(!format!("{d:?}").contains("super-secret"));
    }
}
