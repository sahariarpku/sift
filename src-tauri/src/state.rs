//! Process-wide state held by Tauri.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;
use tokio::sync::Notify;

use crate::error::{Error, Result};
use crate::jobs::{Registry, Worker};
use crate::llm::service::LlmService;
use crate::pipeline::env::Env;
use crate::secrets::{KeychainStore, SecretStore};
use crate::{db, reviews};

pub struct AppState {
    pub reviews_dir: PathBuf,
    app_db: Mutex<Connection>,
    registry: Arc<Registry>,
    /// One running worker per open review file, keyed by review id.
    workers: Mutex<HashMap<String, Arc<Notify>>>,
    pub secrets: Arc<dyn SecretStore>,
    /// One HTTP client for every outbound call (all network access is in Rust).
    pub http: reqwest::Client,
    pub llm: Arc<LlmService>,
    /// What background jobs can reach.
    pub env: Arc<Env>,
    /// The MCP server and its AI bridge (see `mcp`).
    pub mcp: crate::mcp::McpRuntime,
}

impl AppState {
    pub fn open(data_dir: PathBuf) -> Result<Self> {
        let app_db = db::open_app(&data_dir.join("app.db"))?;
        let http = crate::llm::http_client().map_err(|e| Error::Invalid(e.to_string()))?;
        let secrets: Arc<dyn SecretStore> = Arc::new(KeychainStore::default());
        let llm = Arc::new(LlmService::new(http.clone()));
        let env = Arc::new(Env {
            app_db_path: data_dir.join("app.db"),
            secrets: secrets.clone(),
            http: http.clone(),
            llm: llm.clone(),
            source_bases: HashMap::new(),
            job_concurrency: 12,
            live: Default::default(),
        });
        Ok(Self {
            reviews_dir: data_dir.join("reviews"),
            app_db: Mutex::new(app_db),
            registry: Arc::new(Registry::standard()),
            workers: Mutex::new(HashMap::new()),
            secrets,
            llm,
            env,
            http,
            mcp: Default::default(),
        })
    }

    pub fn app_db(&self) -> Result<MutexGuard<'_, Connection>> {
        self.app_db
            .lock()
            .map_err(|_| Error::Task("app.db lock poisoned".into()))
    }

    /// Starts the worker for a review if it is not running yet, and wakes it.
    pub fn ensure_worker(&self, review_id: &str) -> Result<()> {
        let path = reviews::path_of(&*self.app_db()?, review_id)?;
        let mut workers = self
            .workers
            .lock()
            .map_err(|_| Error::Task("worker map lock poisoned".into()))?;
        let waker = workers.entry(review_id.to_owned()).or_insert_with(|| {
            let worker = Worker::new(
                path,
                review_id.to_owned(),
                self.registry.clone(),
                self.env.clone(),
            );
            let waker = worker.waker();
            tauri::async_runtime::spawn(worker.run_forever());
            waker
        });
        waker.notify_one();
        Ok(())
    }

    /// Starts workers for every known review. Called once at launch, which is
    /// when interrupted jobs from the previous session are recovered.
    pub fn start_all_workers(&self) -> Result<()> {
        let ids: Vec<String> = reviews::list(&*self.app_db()?)?
            .into_iter()
            .map(|r| r.id)
            .collect();
        for id in ids {
            self.ensure_worker(&id)?;
        }
        Ok(())
    }
}
