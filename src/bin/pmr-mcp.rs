//! Local stdio MCP server. stdout is reserved for protocol messages.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use hippocampus::{
    PersistenceResult, SqliteMemoryStore, UserId,
    chat::{ChatConfig, ChatMemoryService, memory_tools},
};
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt, model::*, service::RequestContext,
    transport::stdio,
};
use serde_json::{Value, json};
use tokio::sync::watch;

type PrepareResult = Result<CallToolResult, String>;
type PrepareJobs = HashMap<String, (Instant, watch::Receiver<Option<PrepareResult>>, String)>;

fn recall_prepare(response: &CallToolResult) -> bool {
    let Some(body) = response.structured_content.as_ref() else {
        return false;
    };
    // Same new-path shapes as validate_v3_prepare_response, including the v1 adapter.
    body.get("source").is_some()
        || (body["remember"]["result"].get("source").is_some()
            && body["recall"]["result"].get("request_id").is_some())
}

fn clear_recall_prepares(jobs: &mut PrepareJobs) {
    jobs.retain(|_, (_, receiver, _)| {
        let value = receiver.borrow();
        let Some(Ok(response)) = value.as_ref() else {
            return true;
        };
        !recall_prepare(response)
    });
}

#[derive(Clone)]
struct Server {
    service: Arc<Mutex<ChatMemoryService>>,
    prepares: Arc<Mutex<PrepareJobs>>,
}

impl Server {
    async fn prepare(
        &self,
        arguments: Value,
        wait: Duration,
    ) -> Result<CallToolResponse, ErrorData> {
        if arguments.to_string().len() > 65_536 {
            return Err(ErrorData::invalid_params(
                "tool arguments exceed 64 KiB",
                None,
            ));
        }
        let key = hippocampus::recall_models::fingerprint(&arguments);
        let (mut result, operation_id) = {
            let mut jobs = self
                .prepares
                .lock()
                .map_err(|_| ErrorData::internal_error("prepare queue lock poisoned", None))?;
            // ponytail: bounded process-local continuations; after restart the persisted turn receipt prevents duplicate writes.
            jobs.retain(|_, (started, receiver, _)| {
                receiver.borrow().is_none() || started.elapsed() < Duration::from_secs(600)
            });
            if let Some((_, receiver, operation_id)) = jobs.get(&key) {
                (receiver.clone(), operation_id.clone())
            } else {
                if jobs.len() >= 32 {
                    return Err(ErrorData::internal_error(
                        "prepare queue is full; retry later",
                        None,
                    ));
                }
                let (sender, receiver) = watch::channel(None);
                let operation_id = hippocampus::diagnostics::new_id();
                jobs.insert(
                    key.clone(),
                    (Instant::now(), receiver.clone(), operation_id.clone()),
                );
                let worker_id = operation_id.clone();
                let service = self.service.clone();
                let prepares = self.prepares.clone();
                let worker_arguments = arguments.clone();
                tokio::spawn(async move {
                    let publisher = sender.clone();
                    let value = tokio::task::spawn_blocking(move || {
                        // Accepted prepare work continues across client timeouts; its status is retrieved by repeating the same arguments.
                        let value = ChatMemoryService::call_shared(
                            &service,
                            "memory_prepare",
                            worker_arguments.clone(),
                            || false,
                            &worker_id,
                        )
                        .map_err(|error| error.to_string());
                        let validation = if value.as_ref().is_ok_and(recall_prepare) {
                            let operation = service
                                .lock()
                                .map_err(|_| "memory service lock poisoned")?
                                .operation();
                            Some(operation.connect().map_err(|e| e.to_string())?)
                        } else {
                            None
                        };
                        // Serialize publication with forget's cache purge; no HTTP runs under these locks.
                        let _jobs = prepares.lock().map_err(|_| "prepare queue lock poisoned")?;
                        let value = value.map(|value| match validation {
                            Some(service) => {
                                service.validate_prepare_response(&worker_arguments, value)
                            }
                            None => value,
                        });
                        publisher.send_replace(Some(value));
                        Ok::<_, String>(())
                    })
                    .await
                    .unwrap_or_else(|_| Err("memory worker failed".to_owned()));
                    if let Err(error) = value {
                        sender.send_replace(Some(Err(error)));
                    }
                });
                (receiver, operation_id)
            }
        };
        let response = match tokio::time::timeout(wait, async {
            loop {
                if let Some(value) = result.borrow().clone() {
                    return value;
                }
                result
                    .changed()
                    .await
                    .map_err(|_| "memory worker stopped".to_owned())?;
            }
        })
        .await
        {
            Ok(value) => {
                self.prepares
                    .lock()
                    .map_err(|_| ErrorData::internal_error("prepare queue lock poisoned", None))?
                    .remove(&key);
                let value = value.map_err(|error| ErrorData::internal_error(error, None))?;
                let service = self.service.clone();
                Ok(tokio::task::spawn_blocking(move || {
                    let service = service.lock().map_err(|_| {
                        ErrorData::internal_error("memory service lock poisoned", None)
                    })?;
                    Ok::<_, ErrorData>(service.validate_prepare_response(&arguments, value))
                })
                .await
                .map_err(|_| {
                    ErrorData::internal_error("prepare validation worker failed", None)
                })??)
            }
            Err(_) => Ok(CallToolResult::structured(json!({
                "status": "in_progress",
                "next_action": "Call memory_prepare again with exactly the same arguments to wait for the result. This resumes the existing operation; do not create new IDs or call memory_remember. Do not report success or failure until remember and recall results arrive.",
                "global_context": {"status":"unavailable", "error":"Operation is still running; reuse previously loaded global requirements."}
            }))),
        };
        hippocampus::diagnostics::record_for(
            &operation_id,
            "rpc_response",
            || json!({"response":response.as_ref().ok(),"error":response.as_ref().err()}),
        );
        response.map(Into::into)
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "hippocampus",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(include_str!("../../docs/personal-memory-instructions.md"))
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if request.is_some_and(|request| request.cursor.is_some()) {
            return Err(ErrorData::invalid_params(
                "tools/list has no continuation cursor",
                None,
            ));
        }
        Ok(ListToolsResult::with_all_items(memory_tools()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        memory_tools().into_iter().find(|tool| tool.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if self.get_tool(&request.name).is_none() {
            return Err(ErrorData::invalid_params("unknown memory tool", None));
        }
        if request.name == "memory_prepare" {
            if context.ct.is_cancelled() {
                return Err(ErrorData::internal_error(
                    "request cancelled before prepare started",
                    None,
                ));
            }
            return self
                .prepare(
                    Value::Object(request.arguments.unwrap_or_default()),
                    Duration::from_secs(45),
                )
                .await;
        }
        let service = self.service.clone();
        let prepares = self.prepares.clone();
        let result = tokio::task::spawn_blocking(move || {
            let value = ChatMemoryService::call_shared(
                &service,
                &request.name,
                Value::Object(request.arguments.unwrap_or_default()),
                || context.ct.is_cancelled(),
                &hippocampus::diagnostics::new_id(),
            )
            .map_err(|error| error.to_string())?;
            if request.name == "memory_forget"
                && value
                    .structured_content
                    .as_ref()
                    .is_some_and(|v| v["removed"] == true)
            {
                let mut jobs = prepares.lock().map_err(|_| "prepare queue lock poisoned")?;
                clear_recall_prepares(&mut jobs);
            }
            Ok::<_, String>(value)
        })
        .await
        .map_err(|_| ErrorData::internal_error("memory worker failed", None))?;
        Ok(result
            .map_err(|error| ErrorData::internal_error(error, None))?
            .into())
    }
}

fn main() -> PersistenceResult<()> {
    let mut args = std::env::args().skip(1);
    let mut db = None;
    let mut owner = "local-user".to_owned();
    let mut owner_explicit = false;
    let mut config = ChatConfig::default();
    let mut config_explicit = false;
    let mut drain = false;
    let mut max_jobs = None;
    let mut enrich_source = None;
    let mut tool_call = None;
    let mut arguments_file = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                eprintln!(
                    "pmr-mcp [--db PATH] [--owner ID] [--config JSON_FILE] [--drain-enrichment --max-jobs N | --enrich-source SOURCE_ID]\nDefault DB: $HOME/.local/share/personal-memory-runtime/memory.sqlite3\nDefault owner: local-user. Default timezone: UTC+08:00.\n--print-default-config prints the editable config and exits.\nretrieval_mode: legacy/local/assisted; enrichment_mode: inline/deferred/off.\n--call memory_remember|memory_recall|memory_prepare|memory_validate_answer --arguments JSON_FILE runs one CLI tool call (explicit --db/--owner). recall_pipeline: legacy (default)/recall_v3; request recall_version=recall.v3 or answer_contract.v1. recall.v3 needs a stable request_id; prepare defaults to session/turn. offline=true and bootstrap make no recall model calls. MCP online recall.v3 uses independent operation connections and shared request replay; snapshot changes fail without automatic retry. Recall and write pipelines are independent.\nwrite_pipeline: legacy (default)/w4_runtime_v1. New sources bind their version at capture; w4_runtime_v1 requires off/deferred and capture_source=true, runs automatically in MCP deferred mode, and supports these CLI batch commands. Existing sources keep their bound version.\nDeferred capture returns after raw storage; one MCP worker scans/reclaims durable new-version tasks outside the service lock. CLI --call does not start a worker; drain runs only in deferred mode. Explicit source processing can handle an off/failed source even when the current mode is off. Both batch commands require --db, --owner, --config, and a finite model_budget.\nlocal recall and bootstrap are offline; assisted makes at most one budgeted model request and falls back to local. local+off prepare requires capture_source=true and no model credentials.\nModel calls require DEEPSEEK_BASE_URL, DEEPSEEK_API_KEY, an explicit persistent model_budget, and the network gate. PMR_MODEL_NETWORK=offline (default) permits literal loopback fixtures only; live enables provider calls. background_operation_limit (1..3) explicitly separates the background operation cap from max_operation_calls; both use the same total model_budget. No automatic model retries."
                );
                return Ok(());
            }
            "--print-default-config" => {
                println!("{}", serde_json::to_string_pretty(&ChatConfig::default())?);
                return Ok(());
            }
            "--db" => db = Some(PathBuf::from(args.next().ok_or("--db requires a path")?)),
            "--owner" => {
                owner = args.next().ok_or("--owner requires an ID")?;
                owner_explicit = true;
            }
            "--config" => {
                config_explicit = true;
                config = serde_json::from_slice(&std::fs::read(
                    args.next().ok_or("--config requires a path")?,
                )?)?
            }
            "--call" if tool_call.is_none() => {
                let name = args.next().ok_or("--call requires a tool name")?;
                if !matches!(
                    name.as_str(),
                    "memory_remember"
                        | "memory_recall"
                        | "memory_prepare"
                        | "memory_validate_answer"
                ) {
                    return Err(
                        "--call supports memory_remember/memory_recall/memory_prepare/memory_validate_answer"
                            .into(),
                    );
                }
                tool_call = Some(name);
            }
            "--arguments" if arguments_file.is_none() => {
                arguments_file = Some(PathBuf::from(
                    args.next().ok_or("--arguments requires a JSON file")?,
                ));
            }
            "--drain-enrichment" if !drain => drain = true,
            "--max-jobs" if max_jobs.is_none() => {
                max_jobs = Some(
                    args.next()
                        .ok_or("--max-jobs requires a count")?
                        .parse::<usize>()?,
                );
            }
            "--enrich-source" if enrich_source.is_none() => {
                enrich_source = Some(args.next().ok_or("--enrich-source requires an ID")?);
            }
            _ => return Err(format!("unknown argument {arg}; use --help").into()),
        }
    }
    config.validate()?;
    if tool_call.is_some() || arguments_file.is_some() {
        if tool_call.is_none()
            || arguments_file.is_none()
            || drain
            || max_jobs.is_some()
            || enrich_source.is_some()
        {
            return Err(
                "choose --call TOOL --arguments JSON_FILE or a batch enrichment command".into(),
            );
        }
        if db.is_none() || !owner_explicit {
            return Err("CLI tool calls require explicit --db and --owner".into());
        }
    }
    if drain || max_jobs.is_some() || enrich_source.is_some() {
        if drain == enrich_source.is_some() || drain != max_jobs.is_some() {
            return Err(
                "choose --drain-enrichment --max-jobs N or --enrich-source SOURCE_ID".into(),
            );
        }
        if db.is_none() || !owner_explicit || !config_explicit || config.model_budget.is_none() {
            return Err(
                "batch enrichment requires explicit --db, --owner, --config with model_budget"
                    .into(),
            );
        }
        if max_jobs.is_some_and(|count| !(1..=100).contains(&count)) {
            return Err("--max-jobs must be between 1 and 100".into());
        }
    }
    let db = match db {
        Some(path) => path,
        None => PathBuf::from(std::env::var_os("HOME").ok_or("HOME is unavailable; provide --db")?)
            .join(".local/share/personal-memory-runtime/memory.sqlite3"),
    };
    let mut service = ChatMemoryService::new(SqliteMemoryStore::open(db, UserId(owner))?, config)?;
    if let Some(name) = tool_call {
        // CLI 不持 MCP 全局锁；调用共用版本选择、冻结输出和请求预算。
        let arguments = serde_json::from_slice(&std::fs::read(arguments_file.unwrap())?)?;
        let result = service.call(&name, arguments, || false)?;
        println!("{}", serde_json::to_string(&result)?);
        return Ok(());
    }
    if drain || enrich_source.is_some() {
        // CLI model calls run before the MCP service is wrapped in its global mutex (J6).
        let result = if let Some(source_id) = enrich_source {
            service.enrich_source(&source_id)?
        } else {
            service.drain_enrichment(max_jobs.unwrap())?
        };
        println!("{}", serde_json::to_string(&result)?);
        return Ok(());
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let _background = service.start_background()?;
            let server = Server {
                service: Arc::new(Mutex::new(service)),
                prepares: Arc::new(Mutex::new(HashMap::new())),
            }
            .serve(stdio())
            .await?;
            server.waiting().await?;
            Ok(())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn new_prepare_timeout_resumes_one_accepted_job_without_a_model_send() {
        let root = std::env::temp_dir().join(format!(
            "mcp-prepare-{}",
            hippocampus::diagnostics::new_id()
        ));
        let path = root.join("memory.sqlite3");
        let service = ChatMemoryService::new(
            SqliteMemoryStore::open(&path, "prepare-timeout".into()).unwrap(),
            serde_json::from_value(json!({"recall_pipeline":"recall_v3","enrichment_mode":"off"}))
                .unwrap(),
        )
        .unwrap();
        let server = Server {
            service: Arc::new(Mutex::new(service)),
            prepares: Arc::new(Mutex::new(HashMap::new())),
        };
        let args = json!({"user_turn":{"text":"我在学习陶艺。","session_id":"timeout","turn_id":"one","capture_source":true},"offline":true});
        // Holding the entry lock is an event barrier: the accepted worker cannot finish before the zero-duration wait expires.
        let held = server.service.lock().unwrap();
        let pending = server.prepare(args.clone(), Duration::ZERO).await.unwrap();
        let CallToolResponse::Complete(pending) = pending else {
            panic!("expected a tool result")
        };
        assert_eq!(pending.structured_content.unwrap()["status"], "in_progress");
        assert_eq!(server.prepares.lock().unwrap().len(), 1);
        assert!(
            server
                .prepares
                .lock()
                .unwrap()
                .keys()
                .all(|key| key.starts_with("sha256:") && !key.contains("陶艺"))
        );
        drop(held);
        let resumed = server.prepare(args, Duration::from_secs(5)).await.unwrap();
        let CallToolResponse::Complete(resumed) = resumed else {
            panic!("expected a tool result")
        };
        let body = resumed.structured_content.unwrap();
        assert_eq!(body["source"]["status"], "stored");
        assert_eq!(body["recall"]["version"], "recall.v3");
        assert_eq!(body["recall"]["semantic_judgment"], "skipped");
        assert!(server.prepares.lock().unwrap().is_empty());
        let store = SqliteMemoryStore::open_operation(&path, "prepare-timeout".into()).unwrap();
        assert_eq!(store.read().unwrap().sources.len(), 1);
        let connection = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM model_calls", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(connection);
        drop(store);
        drop(server);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deletion_purges_new_completed_prepare_copies_but_keeps_legacy_and_running_jobs() {
        let mut jobs = PrepareJobs::new();
        let mut senders = Vec::new();
        for (key, body) in [
            (
                "new",
                Some(
                    json!({"source":{"status":"stored"},"enrichment":{},"recall":{"version":"recall.v3","current_message":"原话"}}),
                ),
            ),
            (
                "adapter",
                Some(
                    json!({"remember":{"result":{"source":{}}},"recall":{"result":{"request_id":"id","text":"原话"}}}),
                ),
            ),
            (
                "legacy",
                Some(
                    json!({"remember":{"result":{"source":{}}},"recall":{"result":{"answer_contract":{"version":1}}}}),
                ),
            ),
            ("running", None),
        ] {
            let (sender, receiver) =
                watch::channel(body.map(|body| Ok(CallToolResult::structured(body))));
            senders.push(sender);
            jobs.insert(key.into(), (Instant::now(), receiver, key.into()));
        }
        clear_recall_prepares(&mut jobs);
        assert_eq!(jobs.len(), 2);
        assert!(jobs.contains_key("legacy") && jobs.contains_key("running"));
    }
}
