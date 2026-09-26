//! Sift core: everything that touches disk, keys or the network lives here.

pub mod agents;
pub mod commands;
pub mod db;
pub mod dedup;
pub mod error;
pub mod events;
pub mod jobs;
pub mod llm;
pub mod local;
pub mod mcp;
pub mod pipeline;
pub mod providers;
pub mod query;
pub mod records;
pub mod report;
pub mod reviews;
pub mod secrets;
pub mod settings;
pub mod sources;
pub mod state;
pub mod stats;
#[cfg(test)]
pub(crate) mod test_http;
pub mod time;
pub mod validate;

use tauri::Manager;

/// Working name. Change it here and in `src/lib/app.ts`.
pub const APP_NAME: &str = "Sift";

pub fn run() {
    // Relay mode for MCP clients that launch a command (see `mcp::stdio`).
    if std::env::args().any(|a| a == "--mcp-stdio") {
        std::process::exit(mcp::stdio::run());
    }
    let result = tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let state = state::AppState::open(data_dir)?;
            state.start_all_workers()?;
            app.manage(state);
            if let Err(e) = mcp::sync(app.handle()) {
                eprintln!("MCP server: {e}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::reviews::create_review,
            commands::reviews::list_reviews,
            commands::reviews::enqueue_noop_job,
            commands::reviews::list_jobs,
            commands::reviews::review_spend,
            commands::settings::list_presets,
            commands::settings::laya_status,
            commands::settings::laya_test,
            commands::pipeline::retry_queue_with_agents,
            commands::pipeline::retryable_count,
            commands::pipeline::retry_retrieval,
            commands::pipeline::open_upload_batch,
            commands::pipeline::scan_downloads,
            commands::pipeline::read_download,
            commands::pipeline::match_download_text,
            commands::pipeline::assign_download,
            commands::pipeline::import_from_zotero,
            commands::pipeline::review_activity,
            commands::pipeline::open_report_pdf,
            commands::pipeline::retrieval_live,
            commands::settings::list_providers,
            commands::settings::check_base_url,
            commands::settings::fetch_models,
            commands::settings::test_provider,
            commands::settings::save_provider,
            commands::settings::delete_provider,
            commands::settings::list_task_assignments,
            commands::settings::set_task_assignment,
            commands::settings::list_prices,
            commands::settings::set_price,
            commands::settings::get_settings,
            commands::settings::update_settings,
            commands::settings::mcp_status,
            commands::settings::regenerate_mcp_token,
            commands::settings::list_sources,
            commands::settings::set_source_key,
            commands::settings::delete_source_key,
            commands::settings::test_source,
            commands::settings::open_external,
            commands::pipeline::start_review,
            commands::pipeline::get_protocol,
            commands::pipeline::save_protocol,
            commands::pipeline::set_review_options,
            commands::pipeline::lock_protocol,
            commands::pipeline::review_progress,
            commands::pipeline::pause_review,
            commands::pipeline::resume_review,
            commands::pipeline::retry_failed_jobs,
            commands::pipeline::dismiss_failed_jobs,
            commands::pipeline::skip_citation_chasing,
            commands::pipeline::skip_stage,
            commands::pipeline::resume_stage,
            commands::pipeline::get_prisma,
            commands::pipeline::prisma_svg,
            commands::pipeline::quality_checks,
            commands::pipeline::decision_log,
            commands::pipeline::list_search_runs,
            commands::pipeline::import_records,
            commands::pipeline::calibration,
            commands::pipeline::get_record,
            commands::pipeline::calibration_judge,
            commands::pipeline::calibration_finish,
            commands::pipeline::skip_gate,
            commands::pipeline::set_bypass,
            commands::pipeline::get_bypass,
            commands::pipeline::hand_screening_to_me,
            commands::pipeline::human_queue,
            commands::pipeline::human_decide,
            commands::pipeline::list_reports,
            commands::pipeline::upload_report_pdf,
            commands::pipeline::batch_upload_match,
            commands::pipeline::batch_match_staged,
            commands::pipeline::batch_assign_staged,
            commands::pipeline::mark_not_retrieved,
            commands::pipeline::open_via_proxy,
            commands::pipeline::pending_pdf_texts,
            commands::pipeline::read_report_pdf,
            commands::pipeline::store_page_texts,
            commands::pipeline::report_pages,
            commands::pipeline::list_studies,
            commands::pipeline::study_extraction,
            commands::pipeline::resolve_value,
            commands::pipeline::list_appraisals,
            commands::pipeline::synthesis_items,
            commands::pipeline::confirm_certainty,
            commands::pipeline::get_manuscript,
            commands::pipeline::get_checklist,
            commands::pipeline::get_disclosure,
            commands::pipeline::gate3_requirements,
            commands::pipeline::set_author_text,
            commands::pipeline::confirm_links,
            commands::pipeline::sign_off,
            commands::pipeline::export_file,
            commands::pipeline::save_export,
            commands::pipeline::reveal_exports,
        ])
        .run(tauri::generate_context!());
    if let Err(e) = result {
        eprintln!("{APP_NAME} failed to start: {e}");
        std::process::exit(1);
    }
}
