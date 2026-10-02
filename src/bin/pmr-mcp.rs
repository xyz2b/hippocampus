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

#[derive(Clone)]
struct Server {
    service: Arc<Mutex<ChatMemoryService>>,
    prepares: Arc<Mutex<PrepareJobs>>,
}

impl Server {
    async fn prepare(&self, arguments: Value) -> Result<CallToolResponse, ErrorData> {
        let key = arguments.to_string();
        if key.len() > 65_536 {
            return Err(ErrorData::invalid_params(
                "tool arguments exceed 64 KiB",
                None,
            ));
        }
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
                let worker_arguments = arguments.clone();
                tokio::spawn(async move {
                    let value = tokio::task::spawn_blocking(move || {
                        let mut service = service
                            .lock()
                            .map_err(|_| "memory service lock poisoned".to_owned())?;
                        // Accepted prepare work continues across client timeouts; its status is retrieved by repeating the same arguments.
                        Ok(service.call_with_operation(
                            "memory_prepare",
                            worker_arguments,
                            || false,
                            &worker_id,
                        ))
                    })
                    .await
                    .unwrap_or_else(|_| Err("memory worker failed".to_owned()));
                    sender.send_replace(Some(value));
                });
                (receiver, operation_id)
            }
        };
        let response = match tokio::time::timeout(Duration::from_secs(45), async {
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
                .prepare(Value::Object(request.arguments.unwrap_or_default()))
                .await;
        }
        let service = self.service.clone();
        let result = tokio::task::spawn_blocking(move || {
            // ponytail: serial calls per local server; use per-owner workers if multi-user throughput is needed.
            let mut service = service.lock().map_err(|_| "memory service lock poisoned")?;
            Ok::<_, &str>(service.call_with_global_context(
                &request.name,
                Value::Object(request.arguments.unwrap_or_default()),
                || context.ct.is_cancelled(),
            ))
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
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                eprintln!(
                    "pmr-mcp [--db PATH] [--owner ID] [--config JSON_FILE] [--drain-enrichment --max-jobs N | --enrich-source SOURCE_ID]\nDefault DB: $HOME/.local/share/personal-memory-runtime/memory.sqlite3\nDefault owner: local-user. Default timezone: UTC+08:00.\n--print-default-config prints the editable config and exits.\nretrieval_mode: legacy/local/assisted; enrichment_mode: inline/deferred/off.\nwrite_pipeline: legacy (default)/w4_runtime_v1. New sources bind their version at capture; w4_runtime_v1 requires off/deferred and capture_source=true, and runs only through these CLI batch commands. Existing sources keep their bound version.\nDeferred capture queues authorized sources without model calls; drain runs only in deferred mode. Explicit source processing can handle an off/failed source even when the current mode is off. Both batch commands require --db, --owner, --config, and a finite model_budget.\nlocal recall and bootstrap are offline; assisted makes at most one budgeted model request and falls back to local. local+off prepare requires capture_source=true and no model credentials.\nModel calls require DEEPSEEK_BASE_URL, DEEPSEEK_API_KEY, an explicit persistent model_budget, and the network gate. PMR_MODEL_NETWORK=offline (default) permits literal loopback fixtures only; live enables provider calls. No automatic model retries."
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
