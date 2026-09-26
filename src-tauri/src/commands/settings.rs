//! Settings screen commands: providers, task models, prices, sources, app settings.
//!
//! Database locks are never held across an `.await`: each async command reads
//! what it needs, releases the lock, calls the network, then writes back.

use serde::{Deserialize, Serialize};
use tauri::State;

use super::CmdResult;
use crate::llm::cost::Price;
use crate::llm::presets::{self, Preset};
use crate::llm::store::{self, Assignment, PriceRow, Task};
use crate::llm::{ModelInfo, RetryPolicy};
use crate::providers::{self, ProviderDraft, ProviderSummary, TestOutcome};
use crate::settings::{self, AppSettings};
use crate::sources::keytest::{self, Credentials, SourceTestResult};
use crate::sources::{self, SourceStatus};
use crate::state::AppState;

/// Interactive checks retry less than background jobs.
fn interactive_retry() -> RetryPolicy {
    RetryPolicy {
        max_retries: 1,
        ..RetryPolicy::default()
    }
}

#[tauri::command]
pub async fn list_presets() -> Vec<Preset> {
    presets::all()
}

#[tauri::command]
pub async fn list_providers(state: State<'_, AppState>) -> CmdResult<Vec<ProviderSummary>> {
    Ok(providers::list(&*state.app_db()?, state.secrets.as_ref())?)
}

#[tauri::command]
pub async fn check_base_url(url: String) -> CmdResult<Option<String>> {
    Ok(providers::check_base_url(&url)?)
}

/// Fetch models for the form. Saved providers also cache the list.
#[tauri::command]
pub async fn fetch_models(
    state: State<'_, AppState>,
    draft: ProviderDraft,
) -> CmdResult<Vec<ModelInfo>> {
    let conn = providers::conn_for(
        &*state.app_db()?,
        state.secrets.as_ref(),
        &draft,
        interactive_retry(),
    )?;
    let models = providers::fetch_models(state.http.clone(), conn).await?;
    if let Some(id) = &draft.id {
        store::set_models(&*state.app_db()?, id, &models)?;
    }
    Ok(models)
}

#[tauri::command]
pub async fn test_provider(
    state: State<'_, AppState>,
    draft: ProviderDraft,
) -> CmdResult<TestOutcome> {
    let conn = providers::conn_for(
        &*state.app_db()?,
        state.secrets.as_ref(),
        &draft,
        interactive_retry(),
    )?;
    let outcome = providers::run_test(state.http.clone(), conn).await;
    if let Some(id) = &draft.id {
        providers::record_test(&*state.app_db()?, id, &outcome)?;
    }
    Ok(outcome)
}

#[tauri::command]
pub async fn save_provider(
    state: State<'_, AppState>,
    draft: ProviderDraft,
) -> CmdResult<ProviderSummary> {
    Ok(providers::save(
        &*state.app_db()?,
        state.secrets.as_ref(),
        &draft,
    )?)
}

#[tauri::command]
pub async fn delete_provider(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    Ok(providers::delete(
        &*state.app_db()?,
        state.secrets.as_ref(),
        &id,
    )?)
}

#[tauri::command]
pub async fn list_task_assignments(state: State<'_, AppState>) -> CmdResult<Vec<Assignment>> {
    Ok(store::assignments(&*state.app_db()?)?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentInput {
    task: String,
    provider_id: Option<String>,
    model_id: Option<String>,
}

#[tauri::command]
pub async fn set_task_assignment(
    state: State<'_, AppState>,
    input: AssignmentInput,
) -> CmdResult<Vec<Assignment>> {
    let task = Task::parse(&input.task)
        .ok_or_else(|| crate::error::Error::Invalid(format!("Unknown task {}", input.task)))?;
    let app = state.app_db()?;
    let target = match (&input.provider_id, &input.model_id) {
        (Some(p), Some(m)) => Some((p.as_str(), m.as_str())),
        _ => None,
    };
    store::set_assignment(&app, task, target)?;
    Ok(store::assignments(&app)?)
}

#[tauri::command]
pub async fn list_prices(state: State<'_, AppState>) -> CmdResult<Vec<PriceRow>> {
    Ok(store::prices(&*state.app_db()?)?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceInput {
    provider_id: String,
    model_id: String,
    price: Option<Price>,
}

#[tauri::command]
pub async fn set_price(state: State<'_, AppState>, input: PriceInput) -> CmdResult<Vec<PriceRow>> {
    let app = state.app_db()?;
    store::set_price(&app, &input.provider_id, &input.model_id, input.price)?;
    Ok(store::prices(&app)?)
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> CmdResult<AppSettings> {
    Ok(settings::load(&*state.app_db()?)?)
}

#[tauri::command]
pub async fn update_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    settings: AppSettings,
) -> CmdResult<AppSettings> {
    let saved = settings::save(&*state.app_db()?, settings)?;
    // Start, move or stop the MCP server to match.
    crate::mcp::sync(&app)?;
    Ok(saved)
}

/// The MCP server's address, token and bridge queue, for Settings. The token
/// is a local capability the user pastes into their assistant's config.
#[tauri::command]
pub async fn mcp_status(app: tauri::AppHandle) -> CmdResult<crate::mcp::McpStatus> {
    Ok(crate::mcp::status(&app)?)
}

/// Replaces the MCP token; connected assistants must be given the new one.
#[tauri::command]
pub async fn regenerate_mcp_token(app: tauri::AppHandle) -> CmdResult<crate::mcp::McpStatus> {
    Ok(crate::mcp::regenerate(&app)?)
}

#[tauri::command]
pub async fn list_sources(state: State<'_, AppState>) -> CmdResult<Vec<SourceStatus>> {
    let email = settings::load(&*state.app_db()?)?.unpaywall_email;
    Ok(sources::statuses(state.secrets.as_ref(), email.as_deref())?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceKeyInput {
    id: String,
    key: Option<String>,
    inst_token: Option<String>,
}

/// Saves a source key (and Scopus institution token). Blank values are ignored.
#[tauri::command]
pub async fn set_source_key(
    state: State<'_, AppState>,
    input: SourceKeyInput,
) -> CmdResult<Vec<SourceStatus>> {
    let info = sources::find(&input.id)
        .ok_or_else(|| crate::error::Error::Invalid(format!("Unknown source {}", input.id)))?;
    let clean = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    if let Some(k) = clean(&input.key) {
        state.secrets.set(&sources::key_ref(info.id), &k)?;
    }
    if let Some(t) = clean(&input.inst_token) {
        state.secrets.set(&sources::inst_token_ref(info.id), &t)?;
    }
    list_sources(state).await
}

#[tauri::command]
pub async fn delete_source_key(
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<Vec<SourceStatus>> {
    state.secrets.delete(&sources::key_ref(&id))?;
    state.secrets.delete(&sources::inst_token_ref(&id))?;
    list_sources(state).await
}

#[tauri::command]
pub async fn test_source(state: State<'_, AppState>, id: String) -> CmdResult<SourceTestResult> {
    let creds = Credentials {
        key: state.secrets.get(&sources::key_ref(&id))?,
        inst_token: state.secrets.get(&sources::inst_token_ref(&id))?,
        email: settings::load(&*state.app_db()?)?.unpaywall_email,
    };
    Ok(keytest::test_source(&state.http, &id, &creds, None).await)
}

/// Opens a Get key or documentation page in the user's browser. Only URLs
/// from the built-in presets and sources catalogue are allowed.
#[tauri::command]
pub async fn open_external(url: String) -> CmdResult<()> {
    let allowed = presets::all()
        .into_iter()
        .filter_map(|p| p.get_key_url)
        .any(|u| u == url)
        || sources::SOURCES
            .iter()
            .any(|s| s.get_key_url == Some(url.as_str()) || s.docs_url == url);
    if !allowed {
        return Err(crate::error::Error::Invalid(
            "That link is not in the app's list of known pages.".into(),
        )
        .into());
    }
    open_in_browser(&url).map_err(|e| crate::error::Error::Io(e).into())
}

/// Opens a URL or folder built by the app itself (library proxy link, export
/// folder). Never called with text from the frontend or from a web page.
pub(crate) fn open_url_unchecked(target: &str) -> std::io::Result<()> {
    open_in_browser(target)
}

fn open_in_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(url).spawn().map(|_| ())
}

/// Whether the optional local pre-screen (Laya-MLX) can run on this Mac.
#[tauri::command]
pub async fn laya_status(state: State<'_, AppState>) -> CmdResult<crate::local::laya::LayaStatus> {
    let configured = settings::load(&*state.app_db()?)?.laya_python;
    Ok(crate::local::laya::detect(configured.as_deref(), crate::local::laya::DEFAULT_MODEL).await)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayaTest {
    pub ok: bool,
    pub message: String,
    pub load_ms: u64,
    pub answer_ms: u64,
}

/// Loads Laya and asks one sample question, timing both.
#[tauri::command]
pub async fn laya_test(state: State<'_, AppState>) -> CmdResult<LayaTest> {
    use crate::local::laya::{self, Laya};
    let (configured, dir) = {
        let app = state.app_db()?;
        (
            settings::load(&app)?.laya_python,
            state.env.app_db_path.parent().map(|p| p.join("laya")),
        )
    };
    let status = laya::detect(configured.as_deref(), laya::DEFAULT_MODEL).await;
    let (Some(python), true, Some(dir)) = (status.python.clone(), status.available, dir) else {
        return Ok(LayaTest {
            ok: false,
            message: status.message,
            load_ms: 0,
            answer_ms: 0,
        });
    };
    let t = std::time::Instant::now();
    let mut helper =
        match Laya::start(std::path::Path::new(&python), &dir, laya::DEFAULT_MODEL).await {
            Ok(h) => h,
            Err(e) => {
                return Ok(LayaTest {
                    ok: false,
                    message: e.to_string(),
                    load_ms: 0,
                    answer_ms: 0,
                })
            }
        };
    let load_ms = t.elapsed().as_millis() as u64;
    let t = std::time::Instant::now();
    let q = serde_json::json!({"x": {"type": "noul", "instructions": "Does this record study university students?"}});
    let answer = helper
        .ask(
            "Title: Chinese international students' experiences at UK universities",
            &q,
        )
        .await;
    let answer_ms = t.elapsed().as_millis() as u64;
    Ok(match answer {
        Ok(a) => LayaTest {
            ok: true,
            message: format!(
                "Connected: laya-mlx {} answered P(true) = {:.2}.",
                helper.version,
                a["x"]["noul"].as_f64().unwrap_or(f64::NAN)
            ),
            load_ms,
            answer_ms,
        },
        Err(e) => LayaTest {
            ok: false,
            message: e.to_string(),
            load_ms,
            answer_ms,
        },
    })
}
