//! Independent enrichment and derived-index workers; bounded notifications only wake a scan.
use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{RecvTimeoutError, SyncSender, sync_channel},
};
use std::thread::JoinHandle;
use std::time::Duration;

pub struct BackgroundWorker {
    stopped: Arc<AtomicBool>,
    wake: SyncSender<()>,
    thread: Option<JoinHandle<()>>,
    #[cfg(test)]
    idle: std::sync::mpsc::Receiver<()>,
}

impl BackgroundWorker {
    #[cfg(test)]
    pub(crate) fn wait_idle(&self) {
        self.idle.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    /// Wait only when the caller knows any outstanding model response has been released.
    pub fn stop_and_join(mut self) -> std::thread::Result<()> {
        self.stop();
        self.thread.take().unwrap().join()
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = self.wake.try_send(());
    }
}

impl Drop for BackgroundWorker {
    fn drop(&mut self) {
        // Do not hold MCP shutdown open for a sent HTTP request. W4 reservations and the
        // derived index's separately rate-limited lease recover under their respective rules.
        self.stop();
    }
}

impl ChatMemoryService {
    /// Independent derived index: no W-stage mode, credentials or generative budget.
    pub fn start_embedding_background(&mut self) -> PersistenceResult<Option<BackgroundWorker>> {
        if !self.config.embedding.enabled || !crate::embedding::runtime_network_allowed() {
            return Ok(None);
        }
        let stopped = Arc::new(AtomicBool::new(false));
        let config = self.config.embedding.clone();
        let stop = Arc::clone(&stopped);
        self.spawn_embedding_with(
            move || crate::embedding_index::Runner::runtime(config, stop),
            stopped,
        )
        .map(Some)
    }

    #[cfg(test)]
    pub(crate) fn spawn_embedding(
        &mut self,
        runner: crate::embedding_index::Runner,
        stopped: Arc<AtomicBool>,
    ) -> PersistenceResult<BackgroundWorker> {
        self.spawn_embedding_with(move || Ok(runner), stopped)
    }

    fn spawn_embedding_with(
        &mut self,
        create: impl FnOnce() -> PersistenceResult<crate::embedding_index::Runner> + Send + 'static,
        stopped: Arc<AtomicBool>,
    ) -> PersistenceResult<BackgroundWorker> {
        if self.embedding_wake.is_some() {
            return Err("embedding worker already started".into());
        }
        let (wake, receiver) = sync_channel(1);
        let operation = self.operation();
        let stop = Arc::clone(&stopped);
        #[cfg(test)]
        let (idle_sender, idle) = sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("pmr-embedding".into())
            .spawn(move || {
                // Build/drop reqwest's blocking client on this thread, outside MCP's Tokio runtime.
                let runner = match create() {
                    Ok(runner) => runner,
                    Err(_) => {
                        eprintln!("embedding worker: local client unavailable");
                        return;
                    }
                };
                let mut service = match operation.connect() {
                    Ok(service) => service,
                    Err(_) => {
                        eprintln!("embedding worker: operation connection unavailable");
                        return;
                    }
                };
                while !stop.load(Ordering::SeqCst) {
                    if runner.once(&mut service.store).is_err() {
                        eprintln!("embedding worker: cache scan/execution failed");
                    }
                    #[cfg(test)]
                    let _ = idle_sender.try_send(());
                    // Durable cadence/backoff still applies after a wake or another process's claim.
                    match receiver.recv_timeout(Duration::from_secs(5)) {
                        Ok(()) | Err(RecvTimeoutError::Timeout) => (),
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;
        self.embedding_wake = Some(wake.clone());
        Ok(BackgroundWorker {
            stopped,
            wake,
            thread: Some(thread),
            #[cfg(test)]
            idle,
        })
    }

    /// Explicit bounded CLI maintenance; does not start a resident worker or reserve model calls.
    pub fn drain_embeddings(&mut self, max_batches: usize) -> PersistenceResult<Value> {
        if !(1..=100).contains(&max_batches) {
            return Err("max_batches must be 1..100".into());
        }
        if !self.config.embedding.enabled || !crate::embedding::runtime_network_allowed() {
            return Ok(
                json!({"status":"disabled", "attempted_batches":0, "stats":self.store.embedding_stats()?}),
            );
        }
        crate::embedding_index::Runner::runtime(
            self.config.embedding.clone(),
            Arc::new(AtomicBool::new(false)),
        )?
        .drain(&mut self.store, max_batches)
    }

    /// Start once before the service mutex. Off or missing budget keeps the worker inert.
    pub fn start_background(&mut self) -> PersistenceResult<Option<BackgroundWorker>> {
        if self.config.enrichment_mode == EnrichmentMode::Off || self.config.model_budget.is_none()
        {
            return Ok(None);
        }
        if self.background_wake.is_some() {
            return Err("background worker already started".into());
        }
        let (wake, receiver) = sync_channel(1);
        self.background_wake = Some(wake.clone());
        let operation = self.operation();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        #[cfg(test)]
        let (idle_sender, idle) = sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("pmr-enrichment".into())
            .spawn(move || {
                let mut service = match operation.connect() {
                    Ok(service) => service,
                    Err(_) => {
                        eprintln!("background worker: operation connection unavailable");
                        return;
                    }
                };
                loop {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    match service.run_background_once() {
                        Ok(true) => continue,
                        Ok(false) => (),
                        Err(error) if error.to_string().starts_with("default Pro model is unavailable:") => {
                            eprintln!("background worker: default Pro model is unavailable; set DEEPSEEK_PRO_MODEL or configure write_models explicitly; no model request was sent");
                        }
                        Err(_) => eprintln!("background worker: task scan/execution failed"),
                    }
                    #[cfg(test)]
                    let _ = idle_sender.try_send(());
                    // ponytail: one five-second scan, no per-source in-memory queue or worker pool.
                    match receiver.recv_timeout(Duration::from_secs(5)) {
                        Ok(()) | Err(RecvTimeoutError::Timeout) => (),
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;
        Ok(Some(BackgroundWorker {
            stopped,
            wake,
            thread: Some(thread),
            #[cfg(test)]
            idle,
        }))
    }

    /// One durable new-path task; explicit CLI processing keeps its own selection.
    pub fn run_background_once(&mut self) -> PersistenceResult<bool> {
        if self.config.enrichment_mode == EnrichmentMode::Off || self.config.model_budget.is_none()
        {
            return Ok(false);
        }
        let state = self.store.read()?;
        let now = unix_seconds()?;
        let mut candidates: Vec<_> = state
            .sources
            .iter()
            .filter(|(_, source)| {
                source.request.is_some()
                    && source.task.processing_version.as_deref() == Some(W4_PROCESSING_VERSION)
                    && matches!(
                        source.task.accepted_mode.as_deref(),
                        Some("deferred" | "inline")
                    )
                    && matches!(
                        source.enrichment.as_str(),
                        "queued" | "running" | "stale_requires_review"
                    )
                    && source.task.lease_until.is_none_or(|until| until < now)
            })
            .collect();
        candidates.sort_by_key(|(_, source)| source.order_sequence());
        let mut next = None;
        for (key, source) in candidates {
            if source.enrichment == "stale_requires_review" {
                // A newer source can mark even an entirely committed round stale before summary.
                // Reconcile only that crash window, never retry an unsubmitted stale decision.
                if source.task.round.is_none() {
                    continue;
                }
                let progress = w4_progress(&self.store, key)?;
                if !progress.plan_ready
                    || !progress.remaining.is_empty()
                    || !progress.needs_rejudge.is_empty()
                {
                    continue;
                }
            }
            next = Some((key, source));
            break;
        }
        let Some((key, source)) = next else {
            return Ok(false);
        };
        let result = self.run_background_source(key);
        if let Err(error) = result {
            let reason = error.to_string();
            if reason.starts_with("claim_lost") || reason == "source_deleted" {
                return Ok(true);
            }
            // Failures before a claim (e.g. an invalidated checkpoint) cannot use pause_attempt.
            // Recheck the source/round and absence of a live executor under the write lock.
            let code = if reason.starts_with("needs_rejudge") {
                "needs_rejudge"
            } else {
                "attempt_failed"
            };
            let recorded = self.store.transaction(|state| {
                let Some(current) = state.sources.get_mut(key) else {
                    return Ok(false);
                };
                if current.request == source.request
                    && current.task.generation == source.task.generation
                    && current.task.processing_version == source.task.processing_version
                    && (current.task.round == source.task.round
                        || (source.task.round.is_none()
                            && current.task.round == Some(0)
                            && current.task.operation_id.as_deref()
                                == Some(format!("w4:{}:0", source.id).as_str())))
                    && current.task.attempt_id == source.task.attempt_id
                    && matches!(current.enrichment.as_str(), "queued" | "running")
                    && current
                        .task
                        .lease_until
                        .is_none_or(|until| until < unix_seconds().unwrap_or(0))
                {
                    current.enrichment = if code == "needs_rejudge" {
                        "needs_rejudge"
                    } else {
                        "failed"
                    }
                    .into();
                    current.error = Some(code.into());
                    current.task.attempt_id = None;
                    current.task.lease_until = None;
                    current.task.claim_revision = None;
                    return Ok(true);
                }
                Ok(false)
            })?;
            if !recorded {
                return Err(error);
            }
        }
        Ok(true)
    }

    fn run_background_source(&mut self, key: &str) -> PersistenceResult<Value> {
        let source = &self.store.read()?.sources[key];
        if source.task.round.is_some() && w4_progress(&self.store, key)?.plan_ready {
            // Frozen plans/receipts require no model credentials, input assembly or model sends.
            crate::enrichment::run_source(
                &mut self.store,
                key,
                crate::enrichment::RuntimeInputBudget::default(),
                |_, _| Err("saved plan unexpectedly requested upstream".into()),
                |_, _, _| Err("saved plan unexpectedly requested W4".into()),
            )
        } else {
            self.process_w4_source(key)
        }
    }

    #[cfg(test)]
    pub(crate) fn inject_embedding_query(
        &mut self,
        query: Arc<
            dyn Fn(&str) -> crate::embedding::Result<crate::embedding::QueryVector> + Send + Sync,
        >,
    ) {
        self.config.embedding.enabled = true;
        self.embedding_query = Some(query);
    }

    #[cfg(test)]
    pub(crate) fn inject_model_client(&mut self, client: OpenAiClient) {
        self.model_client_override = Some(client);
    }
}

#[cfg(test)]
mod embedding_lifecycle_tests {
    use super::*;

    #[tokio::test]
    async fn embedding_client_lifecycle_is_outside_the_mcp_async_runtime_without_requests() {
        let root =
            std::env::temp_dir().join(format!("embedding-async-{}", crate::diagnostics::new_id()));
        let mut service = ChatMemoryService::new(
            SqliteMemoryStore::open(root.join("memory.sqlite3"), "worker".into()).unwrap(),
            ChatConfig {
                enrichment_mode: EnrichmentMode::Off,
                ..Default::default()
            },
        )
        .unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let worker = service
            .spawn_embedding_with(
                move || {
                    assert_eq!(std::thread::current().name(), Some("pmr-embedding"));
                    // Disabled runner builds/drops a real blocking client but never resolves or embeds.
                    crate::embedding_index::Runner::runtime(
                        crate::embedding::Config::default(),
                        stop,
                    )
                },
                stopped,
            )
            .unwrap();
        worker.wait_idle();
        worker.stop_and_join().unwrap();
        let calls = service.store.embedding_stats().unwrap();
        assert_eq!(calls.metadata_calls, 0);
        assert_eq!(calls.embedding_calls, 0);
        drop(service);
        std::fs::remove_dir_all(root).unwrap();
    }
}
