//! One MCP worker; SQLite tasks are authoritative, bounded notifications only wake a scan.
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
        // Do not hold MCP shutdown open for an already sent HTTP request. Its durable lease and
        // reservation recover after restart; a missing result is failed, never resent.
        self.stop();
    }
}

impl ChatMemoryService {
    /// Start once, before wrapping the service in its mutex. Off/default legacy modes stay inert.
    pub fn start_background(&mut self) -> PersistenceResult<Option<BackgroundWorker>> {
        if self.config.enrichment_mode != EnrichmentMode::Deferred
            || self.config.model_budget.is_none()
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

    /// One durable new-path task; explicit CLI retry and legacy drain keep their own selection.
    pub fn run_background_once(&mut self) -> PersistenceResult<bool> {
        if self.config.enrichment_mode != EnrichmentMode::Deferred
            || self.config.model_budget.is_none()
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
                    && source.task.accepted_mode.as_deref() == Some("deferred")
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
    pub(crate) fn inject_model_client(&mut self, client: OpenAiClient) {
        self.model_client_override = Some(client);
    }
}
