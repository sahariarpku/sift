//! What background jobs can reach: app settings, keys, HTTP, the LLM service.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::Connection;

use crate::error::Result;
use crate::llm::service::LlmService;
use crate::secrets::SecretStore;

pub struct Env {
    pub app_db_path: PathBuf,
    pub secrets: Arc<dyn SecretStore>,
    pub http: reqwest::Client,
    pub llm: Arc<LlmService>,
    /// Source id -> base URL. Empty in the app; tests point sources at mock servers.
    pub source_bases: HashMap<String, String>,
    /// Jobs a review may run at once.
    pub job_concurrency: usize,
    /// What full-text retrieval is doing right now (for the Full text screen).
    pub live: Arc<crate::pipeline::live::LiveRetrieval>,
}

impl Env {
    pub fn app_db(&self) -> Result<Connection> {
        crate::db::open_app(&self.app_db_path)
    }

    pub fn source_base(&self, id: &str) -> Option<&str> {
        self.source_bases.get(id).map(String::as_str)
    }

    /// An environment with in-memory keys and fast retries, for tests and tools.
    pub fn detached(app_db_path: PathBuf) -> Arc<Self> {
        Self::detached_with(
            app_db_path,
            Arc::new(crate::secrets::MemoryStore::default()),
            HashMap::new(),
        )
    }

    pub fn detached_with(
        app_db_path: PathBuf,
        secrets: Arc<dyn SecretStore>,
        source_bases: HashMap<String, String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            app_db_path,
            secrets,
            // The app's client (it identifies itself as Sift; some servers
            // refuse requests without a User-Agent).
            http: crate::llm::http_client().unwrap_or_default(),
            llm: Arc::new(
                LlmService::new(reqwest::Client::new()).with_retry(crate::llm::RetryPolicy::fast()),
            ),
            source_bases,
            job_concurrency: 4,
            live: Default::default(),
        })
    }
}
