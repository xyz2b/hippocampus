//! DeepSeek adapters using the OpenAI-compatible Responses API.
//! The existing OpenAi type names describe the wire protocol and remain source-compatible.
//!
//! The API response is constrained by JSON Schema and then validated again here before it is
//! converted into domain types. Values owned by the runtime (`owner_id`, `observed_at`, query
//! `at`, and retrieval `limit`) never come from the model.

use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fmt;
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::UtcOffset;

use crate::ModelBudgetConfig;
use crate::model_budget::{ModelBudget, ModelStage};

#[path = "model_provider.rs"]
pub mod model_provider;
use model_provider::{ProviderConfig, ProviderProtocol};

#[path = "write_models.rs"]
pub mod write_models;

#[path = "recall_models.rs"]
pub mod recall_models;

#[cfg(test)]
#[path = "eval_replay_tests.rs"]
mod eval_replay_tests;

#[cfg(test)]
type TestTransport =
    dyn Fn(reqwest::blocking::Request) -> Result<(u16, String), OpenAiAdapterError> + Send + Sync;

use crate::{
    Domain, Entity, MemoryType, MemoryValueKind, Predicate, PredicateDefinition, Timestamp, UserId,
};

/// Ark inference endpoint shared by memory extraction and query understanding.
pub const DEEPSEEK_MODEL: &str = "ep-20260923215039-vxgxg";

/// Responses wire values; endpoint/model support is checked by the provider.
/// `omit` removes the reasoning option, leaving the remote model's own default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Omit,
    None,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    Xhigh,
    Max,
}

impl ReasoningEffort {
    pub fn apply(self, request: &mut Value) {
        if self == Self::Omit {
            request.as_object_mut().unwrap().remove("reasoning");
        } else {
            request["reasoning"] = json!({"effort": self});
        }
    }

    pub(crate) fn bind(self, binding: &mut Value) {
        // Absent means medium in existing checkpoints; keep those bytes unchanged.
        if self != Self::Medium {
            binding["reasoning_effort"] = json!(self);
        }
    }
}

/// Production Responses request construction, also used by offline stage evaluations.
pub fn structured_request(
    stage: ModelStage,
    schema_name: &str,
    instructions: &str,
    input: Value,
    schema: Value,
    config: &ModelBudgetConfig,
) -> Value {
    json!({
        "model": DEEPSEEK_MODEL,
        "instructions": format!(
            "Return exactly one complete, compact JSON object matching the supplied JSON Schema. \
             Do not emit Markdown, commentary, or whitespace padding. \
             Stop immediately after the closing brace. \
             Preserve whitespace inside string values verbatim.\n\n{instructions}"
        ),
        "input": input.to_string(),
        // Least sampling randomness: a recorded eval set stays representative.
        "temperature": 0,
        "reasoning": { "effort": "medium" },
        "max_output_tokens": stage.output_tokens(config),
        "store": false,
        "text": { "format": { "type": "json_schema", "name": schema_name,
            "strict": true, "schema": schema } }
    })
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ElapsedUnit {
    Day,
    Week,
    Month,
    Year,
}

#[derive(Debug)]
pub enum OpenAiAdapterError {
    /// `DEEPSEEK_BASE_URL` is absent or empty.
    MissingBaseUrl,
    /// `DEEPSEEK_API_KEY` is absent or empty.
    MissingApiKey,
    /// Adapter configuration is internally inconsistent.
    InvalidContext(String),
    /// The model network gate rejected the request before any socket was opened.
    NetworkDisabled,
    /// A local deployment has no pinned tokenizer/template capacity proof.
    ContextUnproven,
    /// The conservative input plus output bound exceeds the deployed window.
    ContextExceeded,
    /// Missing authorization, exhausted allowance, or a failed durable budget write.
    Budget(String),
    /// The API request could not be completed.
    Http(String),
    /// DeepSeek returned a non-success response.
    Api { status: u16, message: String },
    /// The model explicitly refused the request.
    Refusal(String),
    /// The response stopped before producing complete structured output.
    Incomplete(String),
    /// The Responses API envelope did not contain usable output text.
    InvalidResponse(String),
    /// Structured output was valid JSON but invalid for the V1 domain model.
    InvalidStructuredOutput(String),
}

impl fmt::Display for OpenAiAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingBaseUrl => write!(formatter, "DEEPSEEK_BASE_URL is not set"),
            Self::MissingApiKey => write!(formatter, "DEEPSEEK_API_KEY is not set"),
            Self::InvalidContext(message) => {
                write!(formatter, "invalid adapter context: {message}")
            }
            Self::NetworkDisabled => write!(
                formatter,
                "model network disabled; PMR_MODEL_NETWORK=live explicitly enables provider calls"
            ),
            Self::ContextUnproven => write!(formatter, "model_context_unproven"),
            Self::ContextExceeded => write!(formatter, "model_context_exceeded"),
            Self::Budget(message) => write!(formatter, "model budget: {message}"),
            Self::Http(message) => write!(formatter, "DeepSeek request failed: {message}"),
            Self::Api { status, message } => {
                write!(formatter, "DeepSeek API returned HTTP {status}: {message}")
            }
            Self::Refusal(message) => write!(formatter, "DeepSeek refused the request: {message}"),
            Self::Incomplete(message) => {
                write!(formatter, "DeepSeek response incomplete: {message}")
            }
            Self::InvalidResponse(message) => {
                write!(formatter, "invalid DeepSeek response: {message}")
            }
            Self::InvalidStructuredOutput(message) => {
                write!(formatter, "invalid structured output: {message}")
            }
        }
    }
}

impl std::error::Error for OpenAiAdapterError {}

fn http_error(error: reqwest::Error) -> OpenAiAdapterError {
    use std::error::Error;
    // reqwest 的顶层 Display 隐藏了连接/TLS 等原因；移除 URL 后保留错误链。
    let error = error.without_url();
    let mut message = error.to_string();
    let mut cause = error.source();
    while let Some(source) = cause {
        message.push_str(&format!(": {source}"));
        cause = source.source();
    }
    OpenAiAdapterError::Http(message)
}

/// Shared protocol headers; secrets never appear in requests' persisted identity.
pub fn provider_http_request(
    http: &Client,
    url: &str,
    key: &str,
    protocol: ProviderProtocol,
) -> reqwest::blocking::RequestBuilder {
    let request = http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(
            reqwest::header::USER_AGENT,
            model_provider::MODEL_USER_AGENT,
        );
    match protocol {
        ProviderProtocol::AnthropicMessages => request
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        _ if !key.is_empty() => request.bearer_auth(key),
        _ => request,
    }
}

/// Offline permits literal loopback fixtures only: no DNS, proxy, or redirect escape.
fn model_network_allowed(mode: &str, url: &reqwest::Url) -> Result<bool, OpenAiAdapterError> {
    match mode {
        "deny" => Ok(false),
        "offline" => Ok(url
            .host_str()
            .and_then(|host| {
                host.trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .ok()
            })
            .is_some_and(|ip| ip.is_loopback())),
        "live" => Ok(true),
        _ => Err(OpenAiAdapterError::InvalidContext(
            "PMR_MODEL_NETWORK must be deny, offline, or live".into(),
        )),
    }
}

/// Minimal blocking client shared by both adapters.
#[derive(Clone)]
pub struct OpenAiClient {
    /// Responses endpoint derived from `DEEPSEEK_BASE_URL`.
    responses_url: String,
    /// Kept private so debug output cannot accidentally expose the secret.
    api_key: String,
    provider: Option<ProviderConfig>,
    role_providers: BTreeMap<String, ProviderConfig>,
    default_base_url: Option<String>,
    /// Reused blocking HTTPS client with a bounded request timeout.
    http: Client,
    network_allowed: bool,
    // Clones share counts; every physical send consumes a durable reservation.
    call_count: Arc<AtomicU64>,
    budget: Option<ModelBudget>,
    accounting: Arc<Mutex<Vec<Value>>>,
    #[cfg(test)]
    transport: Option<Arc<TestTransport>>,
}

impl OpenAiClient {
    pub(crate) fn network_allowed(&self) -> bool {
        self.network_allowed
    }

    /// Builds a client from an API base URL and key.
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Self, OpenAiAdapterError> {
        let base_url = base_url.into();
        if base_url.trim().is_empty() {
            return Err(OpenAiAdapterError::MissingBaseUrl);
        }
        let responses_url = format!("{}/responses", base_url.trim().trim_end_matches('/'));
        let parsed_url = reqwest::Url::parse(&responses_url)
            .map_err(|error| OpenAiAdapterError::InvalidContext(error.to_string()))?;
        if !matches!(parsed_url.scheme(), "http" | "https") {
            return Err(OpenAiAdapterError::InvalidContext(
                "DEEPSEEK_BASE_URL must use http or https".to_owned(),
            ));
        }

        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(OpenAiAdapterError::MissingApiKey);
        }
        let mode = env::var("PMR_MODEL_NETWORK").unwrap_or_else(|_| "offline".into());
        let network_allowed = model_network_allowed(&mode, &parsed_url)?;
        let mut builder = Client::builder()
            // Long intake extractions can generate tens of thousands of tokens; 180 s cut them off.
            .timeout(Duration::from_secs(
                env::var("PMR_MODEL_TIMEOUT_SECS")
                    .ok()
                    .and_then(|secs| secs.parse().ok())
                    .unwrap_or(600),
            ))
            // Redirects and hidden retries must not bypass the gate or attempt counter.
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never());
        if mode != "live" {
            builder = builder.no_proxy();
        }
        let http = builder.build().map_err(http_error)?;
        Ok(Self {
            responses_url,
            api_key: api_key.trim().to_owned(),
            provider: None,
            role_providers: BTreeMap::new(),
            default_base_url: None,
            http,
            network_allowed,
            call_count: Arc::new(AtomicU64::new(0)),
            budget: None,
            accounting: Arc::new(Mutex::new(Vec::new())),
            #[cfg(test)]
            transport: None,
        })
    }

    /// Explicit protocols never use the offline loopback-fixture exception.
    pub fn new_for_provider(
        base: &str,
        key: &str,
        provider: ProviderConfig,
    ) -> Result<Self, OpenAiAdapterError> {
        provider.validate()?;
        let endpoint = provider.endpoint(base)?;
        let mut client = Self::new(base, if key.is_empty() { "local-no-key" } else { key })?;
        client.responses_url = endpoint;
        client.api_key = key.trim().to_owned();
        if key.is_empty() && provider.protocol != ProviderProtocol::OpenaiChat {
            return Err(OpenAiAdapterError::MissingApiKey);
        }
        client.network_allowed = env::var("PMR_MODEL_NETWORK").as_deref() == Ok("live");
        client.provider = Some(provider);
        Ok(client)
    }

    pub(crate) fn with_role_providers(
        mut self,
        providers: BTreeMap<String, ProviderConfig>,
        default_base: Option<String>,
    ) -> Self {
        self.role_providers = providers;
        self.default_base_url = default_base;
        self
    }

    pub(crate) fn for_stage(&self, stage: ModelStage) -> Result<Self, OpenAiAdapterError> {
        self.for_stage_with(stage, |name| env::var(name).ok())
    }
    fn for_stage_with(
        &self,
        stage: ModelStage,
        variable: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, OpenAiAdapterError> {
        if self.role_providers.is_empty() {
            return Ok(self.clone());
        }
        let config = self.role_providers.get(stage.name()).cloned();
        let routed = if let Some(config) = config {
            let base = variable(&config.base_url_env).ok_or(OpenAiAdapterError::MissingBaseUrl)?;
            let key = config
                .api_key_env
                .as_ref()
                .map(|name| variable(name).ok_or(OpenAiAdapterError::MissingApiKey))
                .transpose()?
                .unwrap_or_default();
            Self::new_for_provider(&base, &key, config)?
        } else {
            let base = self
                .default_base_url
                .clone()
                .or_else(|| variable("DEEPSEEK_BASE_URL"))
                .ok_or(OpenAiAdapterError::MissingBaseUrl)?;
            Self::new(
                base,
                variable("DEEPSEEK_API_KEY").ok_or(OpenAiAdapterError::MissingApiKey)?,
            )?
        };
        let mut client = self.clone();
        client.responses_url = routed.responses_url;
        client.api_key = routed.api_key;
        client.provider = routed.provider;
        client.network_allowed = routed.network_allowed;
        #[cfg(test)]
        if client.transport.is_some() {
            client.network_allowed = self.network_allowed;
        }
        Ok(client)
    }

    pub(crate) fn provider_binding(
        &self,
        model: &str,
        effort: ReasoningEffort,
        stage: ModelStage,
        schema: Value,
    ) -> Result<Option<Value>, OpenAiAdapterError> {
        if self.provider.is_none() && self.role_providers.is_empty() {
            return Ok(None);
        }
        if let Some(config) = &self.provider {
            config.check_model(model, effort)?;
        }
        let protocol = self
            .provider
            .as_ref()
            .map_or(ProviderProtocol::OpenaiResponses, |p| p.protocol);
        let schema = if protocol == ProviderProtocol::AnthropicMessages {
            model_provider::anthropic_schema(&schema)?
        } else {
            schema
        };
        let output = self
            .budget
            .as_ref()
            .ok_or_else(|| OpenAiAdapterError::Budget("missing budget".into()))?
            .output_tokens(stage);
        Ok(Some(
            json!({"adapter":model_provider::ADAPTER_VERSION,"protocol":protocol,
            "endpoint":self.responses_url,"model":model,"revision":self.provider.as_ref().and_then(|p| p.model_revision.as_ref()),
            "effort":effort,"thinking":self.provider.as_ref().and_then(|p| p.thinking),"schema":schema,"output_tokens":output,
            "context_tokens":self.provider.as_ref().and_then(|p| p.context_tokens),"context_proof":self.provider.as_ref().and_then(|p| p.context_proof.as_ref())}),
        ))
    }

    /// Cache identity needs endpoints, not credentials for stages that may never run.
    pub(crate) fn stage_provider_binding(
        &self,
        stage: ModelStage,
        model: &str,
        effort: ReasoningEffort,
        schema: Value,
    ) -> Result<Option<Value>, OpenAiAdapterError> {
        self.stage_provider_binding_with(stage, model, effort, schema, |name| env::var(name).ok())
    }
    fn stage_provider_binding_with(
        &self,
        stage: ModelStage,
        model: &str,
        effort: ReasoningEffort,
        schema: Value,
        variable: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Value>, OpenAiAdapterError> {
        if self.role_providers.is_empty() {
            return self.provider_binding(model, effort, stage, schema);
        }
        let mut client = self.clone();
        client.provider = self.role_providers.get(stage.name()).cloned();
        client.responses_url = if let Some(config) = &client.provider {
            config.endpoint(
                &variable(&config.base_url_env).ok_or(OpenAiAdapterError::MissingBaseUrl)?,
            )?
        } else {
            let base = self
                .default_base_url
                .clone()
                .or_else(|| variable("DEEPSEEK_BASE_URL"))
                .ok_or(OpenAiAdapterError::MissingBaseUrl)?;
            format!("{}/responses", base.trim().trim_end_matches('/'))
        };
        client.provider_binding(model, effort, stage, schema)
    }

    /// Reads `DEEPSEEK_BASE_URL` and `DEEPSEEK_API_KEY` without logging the secret.
    pub fn from_env() -> Result<Self, OpenAiAdapterError> {
        let base_url =
            env::var("DEEPSEEK_BASE_URL").map_err(|_| OpenAiAdapterError::MissingBaseUrl)?;
        let api_key =
            env::var("DEEPSEEK_API_KEY").map_err(|_| OpenAiAdapterError::MissingApiKey)?;
        let client = Self::new(base_url, api_key)?;
        let keys = [
            "PMR_MODEL_BUDGET_DB",
            "PMR_MODEL_BUDGET_OWNER",
            "PMR_MODEL_BUDGET_CONFIG",
            "PMR_MODEL_OPERATION_ID",
        ];
        let values = keys.map(|key| env::var(key).ok());
        if values.iter().all(Option::is_none) {
            return Ok(client);
        }
        let [Some(path), Some(owner), Some(config), Some(operation)] = values else {
            return Err(OpenAiAdapterError::Budget(
                "all PMR_MODEL_BUDGET_DB/OWNER/CONFIG and PMR_MODEL_OPERATION_ID are required"
                    .into(),
            ));
        };
        let config = serde_json::from_str(&config)
            .map_err(|e| OpenAiAdapterError::Budget(format!("invalid budget config: {e}")))?;
        client.with_budget(path, owner, config, operation)
    }

    /// Binds explicit durable authorization; construction performs no ledger write or model call.
    pub fn with_budget(
        mut self,
        path: impl AsRef<Path>,
        owner: impl Into<String>,
        config: ModelBudgetConfig,
        operation_id: impl Into<String>,
    ) -> Result<Self, OpenAiAdapterError> {
        self.budget = Some(ModelBudget::new(
            path.as_ref(),
            owner.into(),
            config,
            operation_id.into(),
        )?);
        Ok(self)
    }

    /// Sets a separate 1..3 write-operation allowance within the same durable authorization.
    /// Use max_operation_calls=2 and this limit=3 for a shared foreground/background group.
    pub fn with_background_operation_limit(
        mut self,
        limit: u64,
    ) -> Result<Self, OpenAiAdapterError> {
        self.budget = Some(
            self.budget
                .take()
                .ok_or_else(|| {
                    OpenAiAdapterError::Budget(
                        "explicit persistent model budget is required".into(),
                    )
                })?
                .with_background_operation_limit(limit)?,
        );
        Ok(self)
    }

    /// Accounting results remain visible even when optional diagnostic tracing is disabled.
    pub fn model_budget_report(&self) -> Vec<Value> {
        self.accounting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Number of send attempts for this client and its clones, not billable generations.
    pub fn model_call_count(&self) -> u64 {
        self.call_count.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn structured_output(
        &self,
        stage: ModelStage,
        schema_name: &str,
        instructions: &str,
        input: Value,
        schema: Value,
    ) -> Result<String, OpenAiAdapterError> {
        self.structured_output_for_model(
            DEEPSEEK_MODEL,
            stage,
            schema_name,
            instructions,
            input,
            schema,
        )
    }

    #[cfg(test)]
    fn structured_output_for_model(
        &self,
        model: &str,
        stage: ModelStage,
        schema_name: &str,
        instructions: &str,
        input: Value,
        schema: Value,
    ) -> Result<String, OpenAiAdapterError> {
        self.structured_output_for_model_with_effort(
            model,
            ReasoningEffort::Medium,
            stage,
            schema_name,
            instructions,
            input,
            schema,
        )
    }

    fn structured_output_for_model_with_effort(
        &self,
        model: &str,
        effort: ReasoningEffort,
        stage: ModelStage,
        schema_name: &str,
        instructions: &str,
        input: Value,
        schema: Value,
    ) -> Result<String, OpenAiAdapterError> {
        if !self.network_allowed {
            return Err(OpenAiAdapterError::NetworkDisabled);
        }
        let budget = self.budget.as_ref().ok_or_else(|| {
            OpenAiAdapterError::Budget("explicit persistent model budget is required".into())
        })?;
        let mut request = structured_request(
            stage,
            schema_name,
            instructions,
            input,
            schema,
            &budget.config,
        );
        request["max_output_tokens"] = json!(budget.output_tokens(stage));
        request["model"] = json!(model);
        effort.apply(&mut request);
        let original_schema = request["text"]["format"]["schema"].clone();
        let request = model_provider::provider_request(self.provider.as_ref(), &request).map_err(|error| {
            crate::diagnostics::record("model_check", || json!({"stage":schema_name,"phase":"provider_preflight","status":"failed","error":error.to_string(),"sent":false}));
            error
        })?;
        self.send_response(
            stage,
            schema_name,
            request,
            true,
            self.provider.as_ref().map(|_| original_schema),
        )
    }

    /// Sends one plain-text E2 reader or judge request through the same durable budget gate.
    pub fn evaluation_text(
        &self,
        stage: ModelStage,
        prompt: &str,
    ) -> Result<String, OpenAiAdapterError> {
        if !matches!(stage, ModelStage::Reader | ModelStage::Judge) || prompt.trim().is_empty() {
            return Err(OpenAiAdapterError::InvalidContext(
                "evaluation_text requires reader/judge and a nonempty prompt".into(),
            ));
        }
        if self.provider.is_some() || !self.role_providers.is_empty() {
            return Err(OpenAiAdapterError::InvalidContext(
                "plain-text reader/judge only supports the default Responses adapter".into(),
            ));
        }
        if !self.network_allowed {
            return Err(OpenAiAdapterError::NetworkDisabled);
        }
        let budget = self.budget.as_ref().ok_or_else(|| {
            OpenAiAdapterError::Budget("explicit persistent model budget is required".into())
        })?;
        let request = json!({
            "model": DEEPSEEK_MODEL,
            "input": prompt,
            "temperature": 0,
            "reasoning": { "effort": "medium" },
            "max_output_tokens": stage.output_tokens(&budget.config),
            "store": false,
            "text": { "format": { "type": "text" } }
        });
        self.send_response(stage, stage.name(), request, false, None)
    }

    /// Suggests soft search text once; the caller must apply all trusted retrieval rules itself.
    fn send_response(
        &self,
        stage: ModelStage,
        schema_name: &str,
        request: Value,
        expects_json: bool,
        original_schema: Option<Value>,
    ) -> Result<String, OpenAiAdapterError> {
        if !self.network_allowed {
            return Err(OpenAiAdapterError::NetworkDisabled);
        }
        let budget = self.budget.as_ref().ok_or_else(|| {
            OpenAiAdapterError::Budget("explicit persistent model budget is required".into())
        })?;
        let body = serde_json::to_vec(&request)
            .map_err(|e| OpenAiAdapterError::InvalidContext(e.to_string()))?;
        // Build and validate the HTTP request before consuming authorization; never retry a send.
        let request_timeout = budget.timeout(stage);
        let protocol = self
            .provider
            .as_ref()
            .map_or(ProviderProtocol::OpenaiResponses, |p| p.protocol);
        let http_request =
            provider_http_request(&self.http, &self.responses_url, &self.api_key, protocol)
                .timeout(request_timeout)
                .body(body.clone())
                .build()
                .map_err(http_error)?;
        let reservation = budget.reserve(stage, &body)?;
        let attempt_id = &reservation.id;
        let started = std::time::Instant::now();
        crate::diagnostics::record("model_start", || {
            json!({"call_id":attempt_id,"stage":schema_name,
            "attempt":1,"request":request,"timeout_ms":request_timeout.as_millis(),"budget_group":budget.config.group_id,
            "provider":reqwest::Url::parse(&self.responses_url).ok().and_then(|u| u.host_str().map(str::to_owned))})
        });
        let call_id = self.call_count.fetch_add(1, Ordering::Relaxed) + 1;
        if env::var_os("PMR_OPENAI_TRACE").is_some() {
            eprintln!(
                "[openai-attempt] {}",
                json!({"client_attempt":call_id,"schema":schema_name})
            );
        }
        let mut http_status = None;
        let mut diagnostic_response = Value::Null;
        let mut usage = None;
        let result = (|| {
            let execute = |request| {
                let response = self.http.execute(request).map_err(http_error)?;
                Ok::<_, OpenAiAdapterError>((
                    response.status(),
                    response.text().map_err(http_error)?,
                ))
            };
            #[cfg(test)]
            let (status, body) = if let Some(transport) = &self.transport {
                let (status, body) = transport(http_request)?;
                let status = reqwest::StatusCode::from_u16(status)
                    .map_err(|e| OpenAiAdapterError::Http(e.to_string()))?;
                (status, body)
            } else {
                execute(http_request)?
            };
            #[cfg(not(test))]
            let (status, body) = execute(http_request)?;
            http_status = Some(status.as_u16());
            let envelope = serde_json::from_str::<Value>(&body).ok();
            // Usage accounting is unconditional; PMR_TRACE_DIR never controls the budget ledger.
            usage = envelope
                .as_ref()
                .and_then(|e| model_provider::normalized_usage(protocol, e));
            if crate::diagnostics::enabled()
                && let Some(envelope) = &envelope
            {
                diagnostic_response = if protocol != ProviderProtocol::OpenaiResponses {
                    json!({"id":envelope.get("id"),"model":envelope.get("model"),"usage":usage,
                        "stop_reason":envelope.get("stop_reason"),"output_text":model_provider::provider_output(protocol, envelope).ok(),
                        "observability":"business_message_allowlist; reasoning bodies excluded"})
                } else {
                    json!({"id":envelope.get("id"),"model":envelope.get("model"),
                    "status":envelope.get("status"),"usage":envelope.get("usage"),
                    "incomplete_details":envelope.get("incomplete_details"),"error":envelope.get("error"),
                    "output":envelope.get("output").and_then(Value::as_array).map(|items| items.iter()
                        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
                        .map(|item| json!({"type":"message","content":item.get("content").and_then(Value::as_array).map(|parts| parts.iter()
                            .filter(|part| matches!(part.get("type").and_then(Value::as_str),Some("output_text"|"refusal")))
                            .cloned().collect::<Vec<_>>())})).collect::<Vec<_>>()),
                    "observability":"business_message_allowlist; reasoning bodies excluded"})
                };
            }
            if !status.is_success() {
                let message = envelope
                    .as_ref()
                    .and_then(|e| e.pointer("/error/message"))
                    .and_then(Value::as_str)
                    .unwrap_or("API returned a non-JSON error body")
                    .to_owned();
                return Err(OpenAiAdapterError::Api {
                    status: status.as_u16(),
                    message,
                });
            }
            let envelope = envelope.ok_or_else(|| {
                OpenAiAdapterError::InvalidResponse("response is not JSON".into())
            })?;
            if env::var_os("PMR_OPENAI_TRACE").is_some() {
                eprintln!(
                    "[openai-response] {}",
                    json!({"schema":schema_name,"response_id":envelope.get("id"),
                    "model":envelope.get("model"),"status":envelope.get("status"),"reasoning":envelope.get("reasoning"),
                    "usage":envelope.get("usage"),"elapsed_ms":started.elapsed().as_millis()})
                );
            }
            let output = model_provider::provider_output(protocol, &envelope)?;
            if let Some(schema) = &original_schema {
                let parsed: Value = serde_json::from_str(&output).map_err(|_| {
                    OpenAiAdapterError::InvalidStructuredOutput("invalid JSON".into())
                })?;
                model_provider::check_output(schema, &parsed)?;
            }
            if env::var_os("PMR_OPENAI_TRACE_CONTENT").is_some() {
                eprintln!(
                    "[openai-output] {}",
                    json!({"schema":schema_name,"output":output})
                );
            }
            Ok(output)
        })();
        let outcome = match &result {
            Ok(_) => "completed",
            Err(OpenAiAdapterError::Incomplete(_)) => "incomplete",
            Err(_) => "failed",
        };
        let settlement = reservation.settle(outcome, usage.as_ref());
        let accounting = match &settlement {
            Ok(value) => value.clone(),
            Err(error) => {
                json!({"call_id":attempt_id,"usage_status":"unknown","settlement":"failed",
                "output_reservation_retained":true,"error":error.to_string()})
            }
        };
        self.accounting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(accounting.clone());
        crate::diagnostics::record("model_end", || {
            json!({"call_id":attempt_id,"status":outcome,
            "local_outcome":match &result {
                Ok(_) => "completed",
                Err(OpenAiAdapterError::Incomplete(reason)) if matches!(reason.as_str(), "max_output_tokens" | "length") => "output_limit",
                Err(OpenAiAdapterError::Incomplete(_)) => "incomplete",
                Err(OpenAiAdapterError::Http(message)) if message.contains("timeout") || message.contains("timed out") => "timeout",
                Err(OpenAiAdapterError::InvalidResponse(_)) => "decode_failed",
                Err(_) => "transport_failed",
            },
            "http_status":http_status,"response":diagnostic_response,"accounting":accounting,
            "parsed_result":if expects_json {result.as_ref().ok().and_then(|text| serde_json::from_str::<Value>(text).ok())} else {None},
            "parsing_status":if expects_json {match &result {Ok(text) if serde_json::from_str::<Value>(text).is_ok() => "valid_json",Ok(_) => "invalid_json",Err(_) => "not_run"}} else {"not_applicable"},
            "error":result.as_ref().err().map(ToString::to_string),"duration_ms":started.elapsed().as_millis()})
        });
        // Accounting failure is a visible operation failure, even if the provider returned valid output.
        settlement?;
        result
    }
}

pub(crate) fn elapsed_count(from: time::Date, to: time::Date, unit: ElapsedUnit) -> Option<i64> {
    let days = (to - from).whole_days();
    let months = i64::from(to.year() - from.year()) * 12 + i64::from(to.month() as u8)
        - i64::from(from.month() as u8)
        - i64::from(to.day() < from.day());
    (days >= 0).then_some(match unit {
        ElapsedUnit::Day => days,
        ElapsedUnit::Week => (days + 3) / 7,
        ElapsedUnit::Month => months,
        ElapsedUnit::Year => months / 12,
    })
}

/// Runtime-owned values and the only entities the model may resolve to.
#[derive(Clone, Debug)]
pub struct OpenAiAdapterContext {
    /// Memory store owner; injected locally into every output object.
    pub owner_id: UserId,
    /// Entity catalog available for subject, value, and query resolution.
    pub known_entities: Vec<Entity>,
    /// Optional controlled semantic slots shared by extraction and retrieval queries.
    pub known_predicates: Vec<PredicateDefinition>,
    /// Optional per-predicate value types; omitted predicates remain unconstrained.
    pub predicate_value_kinds: Vec<(Predicate, Vec<MemoryValueKind>)>,
    /// Optional semantic categories for each predicate; omitted predicates retain all categories.
    pub predicate_memory_types: Vec<(Predicate, Vec<MemoryType>)>,
    /// Optional controlled domain names shared by memory scope and query context.
    pub known_domains: Vec<Domain>,
    /// Current logical time; used as `observed_at` and query `at`.
    pub now: Timestamp,
    /// User-local UTC offset used for calendar boundaries such as “next month”.
    pub utc_offset_seconds: i32,
}

impl OpenAiAdapterContext {
    /// Validates ownership and entity identifiers once before any API request.
    pub fn new(
        owner_id: UserId,
        known_entities: Vec<Entity>,
        now: Timestamp,
    ) -> Result<Self, OpenAiAdapterError> {
        let mut ids = HashSet::new();
        for entity in &known_entities {
            if entity.owner_id != owner_id {
                return Err(OpenAiAdapterError::InvalidContext(format!(
                    "entity {} belongs to another owner",
                    entity.id
                )));
            }
            if entity.id.0.trim().is_empty() || entity.canonical_name.trim().is_empty() {
                return Err(OpenAiAdapterError::InvalidContext(
                    "entity id and canonical name must not be empty".to_owned(),
                ));
            }
            if !ids.insert(entity.id.clone()) {
                return Err(OpenAiAdapterError::InvalidContext(format!(
                    "duplicate entity id {}",
                    entity.id
                )));
            }
        }
        Ok(Self {
            owner_id,
            known_entities,
            known_predicates: Vec::new(),
            predicate_value_kinds: Vec::new(),
            predicate_memory_types: Vec::new(),
            known_domains: Vec::new(),
            now,
            utc_offset_seconds: 0,
        })
    }

    /// Sets the user-local UTC offset after validating the platform-supported range.
    pub fn with_utc_offset_seconds(mut self, seconds: i32) -> Result<Self, OpenAiAdapterError> {
        UtcOffset::from_whole_seconds(seconds)
            .map_err(|error| OpenAiAdapterError::InvalidContext(error.to_string()))?;
        self.utc_offset_seconds = seconds;
        Ok(self)
    }

    /// Installs the semantic catalog used by both JSON Schema and local validation.
    pub fn with_semantic_catalog(
        mut self,
        predicates: Vec<PredicateDefinition>,
        domains: Vec<Domain>,
    ) -> Result<Self, OpenAiAdapterError> {
        let mut predicate_names = HashSet::new();
        for definition in &predicates {
            if definition.predicate.0.trim().is_empty() {
                return Err(OpenAiAdapterError::InvalidContext(
                    "predicate names must not be empty".to_owned(),
                ));
            }
            if !predicate_names.insert(definition.predicate.clone()) {
                return Err(OpenAiAdapterError::InvalidContext(format!(
                    "duplicate predicate {}",
                    definition.predicate
                )));
            }
        }
        let mut domain_names = HashSet::new();
        for domain in &domains {
            if domain.0.trim().is_empty() {
                return Err(OpenAiAdapterError::InvalidContext(
                    "domain names must not be empty".to_owned(),
                ));
            }
            if !domain_names.insert(domain.clone()) {
                return Err(OpenAiAdapterError::InvalidContext(format!(
                    "duplicate domain {domain}"
                )));
            }
        }
        self.known_predicates = predicates;
        self.known_domains = domains;
        Ok(self)
    }

    /// Adds value-type contracts without changing the existing cardinality-only catalog API.
    pub fn with_predicate_value_kinds(
        mut self,
        contracts: Vec<(Predicate, Vec<MemoryValueKind>)>,
    ) -> Result<Self, OpenAiAdapterError> {
        self.validate_predicate_types(&contracts)?;
        if contracts.iter().any(|(predicate, kinds)| {
            predicate.0 == "fact" && kinds.as_slice() != [MemoryValueKind::Text]
        }) {
            return Err(OpenAiAdapterError::InvalidContext(
                "generic fact requires text values".into(),
            ));
        }
        self.predicate_value_kinds = contracts;
        Ok(self)
    }

    /// Constrains semantic categories without changing the existing predicate/cardinality API.
    pub fn with_predicate_memory_types(
        mut self,
        contracts: Vec<(Predicate, Vec<MemoryType>)>,
    ) -> Result<Self, OpenAiAdapterError> {
        self.validate_predicate_types(&contracts)?;
        self.predicate_memory_types = contracts;
        Ok(self)
    }

    fn contains_predicate(&self, id: &str) -> bool {
        self.known_predicates.is_empty()
            || self.known_predicates.iter().any(|p| p.predicate.0 == id)
    }

    fn validate_predicate_types<T>(
        &self,
        contracts: &[(Predicate, Vec<T>)],
    ) -> Result<(), OpenAiAdapterError> {
        let mut seen = HashSet::new();
        for (predicate, kinds) in contracts {
            if predicate.0.trim().is_empty()
                || !self.contains_predicate(&predicate.0)
                || kinds.is_empty()
                || !seen.insert(predicate.clone())
            {
                return Err(OpenAiAdapterError::InvalidContext(
                    "type contracts require unique allowed predicates and non-empty type lists"
                        .to_owned(),
                ));
            }
        }
        Ok(())
    }
}

fn extract_output_text(envelope: &Value) -> Result<String, OpenAiAdapterError> {
    if envelope.get("status").and_then(Value::as_str) == Some("incomplete") {
        let reason = envelope
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown reason");
        return Err(OpenAiAdapterError::Incomplete(reason.to_owned()));
    }

    let output = envelope
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| OpenAiAdapterError::InvalidResponse("missing output array".to_owned()))?;
    let mut output_text = String::new();
    for item in output {
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in content {
            match part.get("type").and_then(Value::as_str) {
                Some("output_text") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        output_text.push_str(text);
                    }
                }
                Some("refusal") => {
                    let message = part
                        .get("refusal")
                        .and_then(Value::as_str)
                        .unwrap_or("no refusal reason returned");
                    return Err(OpenAiAdapterError::Refusal(message.to_owned()));
                }
                _ => {}
            }
        }
    }
    if !output_text.is_empty() {
        return Ok(output_text);
    }
    Err(OpenAiAdapterError::InvalidResponse(
        "missing output_text content".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_fixture(protocol: ProviderProtocol) -> ProviderConfig {
        ProviderConfig {
            protocol,
            base_url_env: "TEST_PROVIDER_BASE".into(),
            api_key_env: Some("TEST_PROVIDER_KEY".into()),
            model_revision: Some("fixture-model-revision".into()),
            thinking: if protocol == ProviderProtocol::OpenaiChat {
                Some(false)
            } else {
                None
            },
            context_tokens: if protocol == ProviderProtocol::OpenaiChat {
                Some(65536)
            } else {
                None
            },
            context_proof: Some(model_provider::ContextProof {
                tokenizer_revision: "fixture-tokenizer".into(),
                template_token_bound: 64,
                byte_level_bpe: true,
            }),
        }
    }
    #[test]
    fn explicit_protocols_share_durable_budget_and_never_need_unselected_credentials() {
        let path = env::temp_dir().join(format!(
            "provider-budget-{}.sqlite3",
            crate::diagnostics::new_id()
        ));
        crate::SqliteMemoryStore::open(&path, UserId::from("fixture")).unwrap();
        let mut client = OpenAiClient::new("https://unused.invalid/v1", "unused")
            .unwrap()
            .with_budget(
                &path,
                "fixture",
                ModelBudgetConfig {
                    group_id: "mixed".into(),
                    max_calls: 3,
                    ..Default::default()
                },
                "one-operation",
            )
            .unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let sent = calls.clone();
        client.network_allowed = true;
        client.transport = Some(Arc::new(move |r| {
            assert_eq!(
                r.headers()[reqwest::header::USER_AGENT],
                model_provider::MODEL_USER_AGENT
            );
            let body: Value =
                serde_json::from_slice(r.body().unwrap().as_bytes().unwrap()).unwrap();
            let schema = crate::write_models::object(&[("count", crate::write_models::index())]);
            let reply = match r.url().path() {
                "/v1/responses" => {
                    assert_eq!(body["max_output_tokens"], 32768);
                    assert!(body.get("temperature").is_none());
                    json!({"status":"completed","usage":{"input_tokens":2,"output_tokens":3},"output":[{"content":[{"type":"output_text","text":"{\"count\":1}"}]}]})
                }
                "/v1/messages" => {
                    assert_eq!(r.headers()["anthropic-version"], "2023-06-01");
                    assert_eq!(r.headers()["x-api-key"], "fixture-key");
                    assert!(r.headers().get(reqwest::header::AUTHORIZATION).is_none());
                    assert_eq!(body["max_tokens"], 16384);
                    assert_eq!(
                        body["output_config"]["format"]["schema"],
                        model_provider::anthropic_schema(&schema).unwrap()
                    );
                    json!({"stop_reason":"end_turn","usage":{"input_tokens":2,"cache_read_input_tokens":4,"output_tokens":8},
                        "content":[{"type":"thinking","thinking":"never record this"},{"type":"text","text":"{\"count\":1}"}]})
                }
                "/v1/chat/completions" => {
                    assert_eq!(body["max_tokens"], 16384);
                    assert_eq!(body["reasoning_effort"], "none");
                    json!({"choices":[{"finish_reason":"stop","message":{"content":"{\"count\":1}","reasoning":"never record this"}}],"usage":{"prompt_tokens":9,"completion_tokens":10}})
                }
                _ => panic!("unexpected endpoint"),
            };
            sent.lock().unwrap().push(body);
            Ok((200, reply.to_string()))
        }));
        client = client.with_role_providers(
            [
                (
                    "w1".into(),
                    provider_fixture(ProviderProtocol::OpenaiResponses),
                ),
                (
                    "w3".into(),
                    provider_fixture(ProviderProtocol::AnthropicMessages),
                ),
                ("w4".into(), provider_fixture(ProviderProtocol::OpenaiChat)),
            ]
            .into(),
            None,
        );
        let resolve = |name: &str| match name {
            "TEST_PROVIDER_BASE" => Some("https://fixture.invalid/v1".into()),
            "TEST_PROVIDER_KEY" => Some("fixture-key".into()),
            _ => panic!("unselected credential lookup: {name}"),
        };
        for (stage, model, effort) in [
            (ModelStage::W1, "gpt-6.1-sol", ReasoningEffort::Medium),
            (ModelStage::W3, "claude-opus-5-5", ReasoningEffort::Medium),
            (ModelStage::W4, "qwen3.8:27b-q4_K_M", ReasoningEffort::Omit),
        ] {
            let routed = client.for_stage_with(stage, resolve).unwrap();
            assert_eq!(
                routed
                    .structured_output_for_model_with_effort(
                        model,
                        effort,
                        stage,
                        "test",
                        "fixed instruction",
                        json!({"text":"虚构"}),
                        crate::write_models::object(&[("count", crate::write_models::index())])
                    )
                    .unwrap(),
                "{\"count\":1}"
            );
        }
        assert_eq!(client.model_call_count(), 3);
        assert_eq!(calls.lock().unwrap().len(), 3);
        let ledger = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(ledger.query_row("SELECT count(*) FROM model_calls WHERE operation_id='one-operation' AND usage_status='known'",[],|r|r.get::<_,i64>(0)).unwrap(),3);
        assert_eq!(client.model_budget_report()[1]["input_tokens"], 6);
        assert_eq!(client.model_budget_report()[1]["output_tokens"], 8);
        let policy: String = ledger
            .query_row("SELECT policy_json FROM model_budget_groups", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(!policy.contains("provider"));
        drop(ledger);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn provider_preflight_rejections_leave_no_reservation_or_send() {
        let path = env::temp_dir().join(format!(
            "provider-reject-{}.sqlite3",
            crate::diagnostics::new_id()
        ));
        crate::SqliteMemoryStore::open(&path, UserId::from("fixture")).unwrap();
        let mut client = OpenAiClient::new("https://fixture.invalid/v1", "fixture-key")
            .unwrap()
            .with_budget(
                &path,
                "fixture",
                ModelBudgetConfig {
                    group_id: "rejections".into(),
                    max_calls: 1,
                    ..Default::default()
                },
                "operation",
            )
            .unwrap();
        client.network_allowed = true;
        client.transport = Some(Arc::new(|_| panic!("preflight must not send")));
        client.provider = Some(provider_fixture(ProviderProtocol::OpenaiResponses));
        assert!(matches!(
            client.structured_output_for_model_with_effort(
                "gpt-6.1-sol",
                ReasoningEffort::Minimal,
                ModelStage::W1,
                "test",
                "test",
                json!({}),
                json!({"type":"object"})
            ),
            Err(OpenAiAdapterError::InvalidContext(_))
        ));
        client.provider = Some(provider_fixture(ProviderProtocol::OpenaiChat));
        client.provider.as_mut().unwrap().context_tokens = Some(1);
        assert!(matches!(
            client.structured_output_for_model_with_effort(
                "qwen3.8:27b-q4_K_M",
                ReasoningEffort::Omit,
                ModelStage::W1,
                "test",
                "test",
                json!({}),
                json!({"type":"object"})
            ),
            Err(OpenAiAdapterError::ContextExceeded)
        ));
        client.provider.as_mut().unwrap().context_proof = None;
        assert!(matches!(
            client.structured_output_for_model_with_effort(
                "qwen3.8:27b-q4_K_M",
                ReasoningEffort::Omit,
                ModelStage::W1,
                "test",
                "test",
                json!({}),
                json!({"type":"object"})
            ),
            Err(OpenAiAdapterError::ContextUnproven)
        ));
        assert_eq!(client.model_call_count(), 0);
        assert!(client.model_budget_report().is_empty());
        let ledger = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            ledger
                .query_row("SELECT count(*) FROM model_calls", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(ledger);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn mixed_role_bindings_include_default_endpoint_without_reading_unused_keys() {
        let path = env::temp_dir().join(format!(
            "provider-identity-{}.sqlite3",
            crate::diagnostics::new_id()
        ));
        crate::SqliteMemoryStore::open(&path, UserId::from("fixture")).unwrap();
        let client = OpenAiClient::new("https://unused.invalid/v1", "unused")
            .unwrap()
            .with_budget(
                &path,
                "fixture",
                ModelBudgetConfig {
                    group_id: "identity".into(),
                    max_calls: 2,
                    ..Default::default()
                },
                "operation",
            )
            .unwrap()
            .with_role_providers(
                [(
                    "r1".into(),
                    provider_fixture(ProviderProtocol::OpenaiResponses),
                )]
                .into(),
                None,
            );
        let identity = |stage, endpoint: &str| {
            client.stage_provider_binding_with(stage,
            if matches!(stage, ModelStage::R1) { "gpt-6.1-sol" } else { DEEPSEEK_MODEL }, ReasoningEffort::Medium,
            json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
            |name| match name {
                "TEST_PROVIDER_BASE" | "DEEPSEEK_BASE_URL" => Some(endpoint.into()),
                _ => panic!("cache binding must not resolve a key: {name}"),
            }).unwrap().unwrap()
        };
        let r1 = identity(ModelStage::R1, "https://gpt.invalid/v1");
        assert_eq!(r1["endpoint"], "https://gpt.invalid/v1/responses");
        let r4 = identity(ModelStage::R4, "https://deepseek.invalid/v1");
        assert_eq!(r4["protocol"], "openai_responses");
        assert_ne!(r4, identity(ModelStage::R4, "https://different.invalid/v1"));
        assert_eq!(client.model_call_count(), 0);
        assert!(client.model_budget_report().is_empty());
        if env::var("PMR_MODEL_NETWORK").as_deref() != Ok("live") {
            let local = OpenAiClient::new_for_provider(
                "http://127.0.0.1:11434/v1",
                "",
                provider_fixture(ProviderProtocol::OpenaiChat),
            )
            .unwrap();
            assert!(!local.network_allowed());
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn explicit_protocol_failures_consume_once_and_preserve_unknown_usage() {
        let path = env::temp_dir().join(format!(
            "provider-failures-{}.sqlite3",
            crate::diagnostics::new_id()
        ));
        crate::SqliteMemoryStore::open(&path, UserId::from("fixture")).unwrap();
        let schema = crate::write_models::object(&[("count", crate::write_models::index())]);
        for protocol in [
            ProviderProtocol::OpenaiResponses,
            ProviderProtocol::AnthropicMessages,
            ProviderProtocol::OpenaiChat,
        ] {
            for kind in ["extra", "refusal", "truncated", "empty", "http", "timeout"] {
                let config = provider_fixture(protocol);
                let mut client = OpenAiClient::new("https://fixture.invalid/v1", "fixture-key")
                    .unwrap()
                    .with_budget(
                        &path,
                        "fixture",
                        ModelBudgetConfig {
                            group_id: "failures".into(),
                            max_calls: 100,
                            ..Default::default()
                        },
                        format!("{protocol:?}:{kind}"),
                    )
                    .unwrap();
                client.responses_url = config.endpoint("https://fixture.invalid/v1").unwrap();
                client.provider = Some(config);
                client.network_allowed = true;
                client.transport = Some(Arc::new(move |_| {
                    if kind == "timeout" {
                        return Err(OpenAiAdapterError::Http("request timeout".into()));
                    }
                    if kind == "http" {
                        return Ok((
                            429,
                            json!({"error":{"message":"fixture rate limit"}}).to_string(),
                        ));
                    }
                    let text = if kind == "extra" {
                        "{\"count\":0,\"extra\":true}"
                    } else {
                        ""
                    };
                    let reply = match protocol {
                        ProviderProtocol::OpenaiResponses => match kind {
                            "refusal" => {
                                json!({"status":"completed","output":[{"content":[{"type":"refusal","refusal":"fixture"}]}]})
                            }
                            "truncated" => {
                                json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}})
                            }
                            _ => {
                                json!({"status":"completed","output":[{"content":[{"type":"output_text","text":text}]}]})
                            }
                        },
                        ProviderProtocol::AnthropicMessages => {
                            json!({"stop_reason":match kind {"refusal"=>"refusal","truncated"=>"max_tokens",_=>"end_turn"},"content":[{"type":"text","text":text}]})
                        }
                        ProviderProtocol::OpenaiChat => {
                            json!({"choices":[{"finish_reason":match kind {"refusal"=>"content_filter","truncated"=>"length",_=>"stop"},"message":{"content":text,"reasoning":"not final"}}]})
                        }
                    };
                    Ok((200, reply.to_string()))
                }));
                let (model, effort) = match protocol {
                    ProviderProtocol::OpenaiResponses => ("gpt-6.1-sol", ReasoningEffort::Medium),
                    ProviderProtocol::AnthropicMessages => {
                        ("claude-opus-5-5", ReasoningEffort::Medium)
                    }
                    ProviderProtocol::OpenaiChat => ("qwen3.8:27b-q4_K_M", ReasoningEffort::Omit),
                };
                let failure = client
                    .structured_output_for_model_with_effort(
                        model,
                        effort,
                        ModelStage::W3,
                        "test",
                        "fixed instruction",
                        json!({}),
                        schema.clone(),
                    )
                    .unwrap_err();
                match kind {
                    "extra" => assert!(matches!(
                        failure,
                        OpenAiAdapterError::InvalidStructuredOutput(_)
                    )),
                    "refusal" => assert!(matches!(failure, OpenAiAdapterError::Refusal(_))),
                    "truncated" => assert!(matches!(failure, OpenAiAdapterError::Incomplete(_))),
                    "empty" => assert!(matches!(failure, OpenAiAdapterError::InvalidResponse(_))),
                    "http" => assert!(matches!(
                        failure,
                        OpenAiAdapterError::Api { status: 429, .. }
                    )),
                    _ => assert!(matches!(failure, OpenAiAdapterError::Http(_))),
                };
                assert_eq!(client.model_call_count(), 1);
                assert_eq!(client.model_budget_report()[0]["usage_status"], "unknown");
                assert_eq!(
                    client.model_budget_report()[0]["output_reservation_retained"],
                    true
                );
                assert!(matches!(
                    client.structured_output_for_model_with_effort(
                        model,
                        effort,
                        ModelStage::W3,
                        "test",
                        "fixed instruction",
                        json!({}),
                        schema.clone()
                    ),
                    Err(OpenAiAdapterError::Budget(_))
                ));
                assert_eq!(client.model_call_count(), 1);
            }
        }
        let db = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM model_calls", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            18
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> Value {
        use std::io::{BufRead, BufReader, Read};
        let mut reader = BufReader::new(stream);
        let mut length = None;
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
        let mut body = vec![0; length.unwrap()];
        reader.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[test]
    fn offline_gate_blocks_provider_and_redirect_without_sending() {
        use std::io::Write;
        use std::net::TcpListener;
        for address in [
            "https://api.deepseek.com",
            "http://localhost",
            "http://127.0.0.1.example.com",
        ] {
            let url = reqwest::Url::parse(address).unwrap();
            assert!(!model_network_allowed("offline", &url).unwrap());
            assert!(model_network_allowed("live", &url).unwrap());
            assert!(model_network_allowed("typo", &url).is_err());
        }
        for address in ["http://127.0.0.1", "http://[::1]"] {
            let url = reqwest::Url::parse(address).unwrap();
            assert!(model_network_allowed("offline", &url).unwrap());
            assert!(!model_network_allowed("deny", &url).unwrap());
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let unauthorized = OpenAiClient::new(
            format!("http://{}", listener.local_addr().unwrap()),
            "fixture",
        )
        .unwrap();
        assert!(matches!(
            unauthorized.structured_output(
                ModelStage::Reader,
                "test",
                "test",
                json!({}),
                json!({})
            ),
            Err(OpenAiAdapterError::Budget(_))
        ));
        assert_eq!(unauthorized.model_call_count(), 0);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        let ledger = crate::model_budget::tests::TestLedger::new();
        let mut client = ledger.client(format!("http://{}", listener.local_addr().unwrap()));
        client.network_allowed = false;
        assert!(matches!(
            client.structured_output(ModelStage::Reader, "test", "test", json!({}), json!({})),
            Err(OpenAiAdapterError::NetworkDisabled)
        ));
        assert_eq!(client.model_call_count(), 0);
        assert!(!ledger.path.exists());
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );

        // A loopback fixture cannot redirect to another socket (or a paid provider).
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let address = target.local_addr().unwrap();
        listener.set_nonblocking(false).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            read_http_request(&mut stream);
            write!(stream, "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{address}/responses\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        client.network_allowed = true;
        assert!(matches!(
            client.structured_output(ModelStage::Reader, "test", "test", json!({}), json!({})),
            Err(OpenAiAdapterError::Api { status: 307, .. })
        ));
        server.join().unwrap();
        assert_eq!(client.model_call_count(), 1);
        assert_eq!(
            target.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn evaluation_text_uses_distinct_budgeted_stages_and_respects_deny() {
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for (prompt, tokens, reply) in
                [("reader prompt", 800, "33"), ("judge prompt", 10, "yes")]
            {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let request = read_http_request(&mut stream);
                assert_eq!(request["model"], DEEPSEEK_MODEL);
                assert_eq!(request["input"], prompt);
                assert_eq!(request["reasoning"]["effort"], "medium");
                assert_eq!(request["temperature"], 0);
                assert_eq!(request["text"]["format"]["type"], "text");
                assert_eq!(request["max_output_tokens"], tokens);
                let body =
                    json!({"status":"completed","usage":{"input_tokens":4,"output_tokens":1},
                    "output":[{"type":"message","content":[{"type":"output_text","text":reply}]}]})
                    .to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let ledger = crate::model_budget::tests::TestLedger::new();
        for (stage, group, prompt, expected, cap) in [
            (ModelStage::Reader, "e2-reader", "reader prompt", "33", 800),
            (ModelStage::Judge, "e2-judge", "judge prompt", "yes", 10),
        ] {
            let config = ModelBudgetConfig {
                group_id: group.into(),
                max_calls: 1,
                extraction_output_tokens: if matches!(stage, ModelStage::Reader) {
                    cap
                } else {
                    4096
                },
                review_output_tokens: if matches!(stage, ModelStage::Judge) {
                    cap
                } else {
                    4096
                },
                ..Default::default()
            };
            let mut client = OpenAiClient::new(format!("http://{address}/v1"), "fixture")
                .unwrap()
                .with_budget(&ledger.path, "fixture", config, group)
                .unwrap();
            client.network_allowed = true;
            assert_eq!(client.evaluation_text(stage, prompt).unwrap(), expected);
            assert_eq!(client.model_call_count(), 1);
            assert_eq!(client.model_budget_report()[0]["stage"], stage.name());
            assert!(matches!(
                client.evaluation_text(stage, prompt),
                Err(OpenAiAdapterError::Budget(_))
            ));
            assert_eq!(client.model_call_count(), 1);
        }
        server.join().unwrap();
        let db = rusqlite::Connection::open(&ledger.path).unwrap();
        let stages: Vec<String> = db
            .prepare("SELECT stage FROM model_calls ORDER BY stage")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(stages, ["judge", "reader"]);
        drop(db);

        let denied_ledger = crate::model_budget::tests::TestLedger::new();
        let mut denied = denied_ledger.client(format!("http://{address}/v1"));
        denied.network_allowed = false;
        assert!(matches!(
            denied.evaluation_text(ModelStage::Reader, "reader prompt"),
            Err(OpenAiAdapterError::NetworkDisabled)
        ));
        assert_eq!(denied.model_call_count(), 0);
        assert!(!denied_ledger.path.exists());
    }

    #[test]
    fn physical_sends_never_retry_connection_or_http_failures() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;

        // 真正经过 blocking Client；TLS 对端在握手中 EOF，HTTP 对端可成功、报错或断开。
        for (scheme, status, expected_attempts, stage, output_tokens) in [
            ("https", None, 1, ModelStage::Reader, 4096),
            ("http", Some(200), 1, ModelStage::Reader, 4096),
            ("http", Some(503), 1, ModelStage::Reader, 4096),
            ("http", None, 1, ModelStage::Reader, 4096),
            ("http", Some(200), 1, ModelStage::Reader, 8192),
            ("http", Some(200), 1, ModelStage::Judge, 8192),
            ("http", Some(200), 1, ModelStage::Reader, 16384),
            ("http", Some(200), 1, ModelStage::Judge, 16384),
            ("http", Some(200), 1, ModelStage::Reader, 32768),
            ("http", Some(200), 1, ModelStage::Judge, 32768),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let (stop, stopped) = mpsc::channel();
            let server = std::thread::spawn(move || {
                let mut attempts = 0;
                loop {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            attempts += 1;
                            stream.set_nonblocking(false).unwrap();
                            stream
                                .set_read_timeout(Some(Duration::from_secs(2)))
                                .unwrap();
                            if scheme == "https" {
                                stream.read_exact(&mut [0; 1]).unwrap();
                            } else {
                                let request = read_http_request(&mut stream);
                                assert_eq!(request["model"], "ep-20260923215039-vxgxg");
                                assert_eq!(request["reasoning"]["effort"], "medium");
                                assert_eq!(request["temperature"], 0);
                                assert_eq!(request["max_output_tokens"], output_tokens);
                            }
                            if let Some(status) = status {
                                let body = r#"{"output":[{"type":"message","content":[{"type":"output_text","text":"{}"}]}]}"#;
                                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(error) => panic!("test server: {error}"),
                    }
                    if stopped.recv_timeout(Duration::from_millis(5)).is_ok() {
                        return attempts;
                    }
                }
            });
            let ledger = crate::model_budget::tests::TestLedger::new();
            let client = ledger
                .client(format!("{scheme}://{address}/v1"))
                .with_budget(
                    &ledger.path,
                    "fixture",
                    ModelBudgetConfig {
                        group_id: "fixture".into(),
                        max_calls: 1,
                        extraction_output_tokens: output_tokens,
                        review_output_tokens: output_tokens,
                        ..Default::default()
                    },
                    "operation",
                )
                .unwrap();
            let result = client.structured_output(stage, "test", "test", json!({}), json!({}));
            stop.send(()).unwrap();
            assert_eq!(client.model_call_count(), expected_attempts);
            assert_eq!(client.clone().model_call_count(), expected_attempts);
            assert_eq!(
                server.join().unwrap(),
                expected_attempts,
                "{scheme} {status:?}"
            );
            match status {
                Some(200) => assert_eq!(result.unwrap(), "{}"),
                Some(503) => assert!(matches!(
                    result,
                    Err(OpenAiAdapterError::Api { status: 503, .. })
                )),
                _ => assert!(result.is_err(), "{scheme} {status:?}: {result:?}"),
            }
        }
    }

    #[test]
    fn client_builds_responses_endpoint_from_base_url() {
        let direct = OpenAiClient::new("https://api.deepseek.com/", "test-key").unwrap();
        assert_eq!(direct.responses_url, "https://api.deepseek.com/responses");
        let client = OpenAiClient::new("https://example.test/openai/v1/", " test-key ").unwrap();
        assert_eq!(
            client.responses_url,
            "https://example.test/openai/v1/responses"
        );
        assert_eq!(client.api_key, "test-key");
        assert!(matches!(
            OpenAiClient::new("", "test-key"),
            Err(OpenAiAdapterError::MissingBaseUrl)
        ));
        assert!(matches!(
            OpenAiClient::new("https://api.deepseek.com", " "),
            Err(OpenAiAdapterError::MissingApiKey)
        ));
        assert!(OpenAiClient::new("file:///tmp/openai/v1", "test-key").is_err());
        // 错误请求在 builder 阶段失败，不发网络请求；底层原因不能再被顶层 Display 吞掉。
        let error = client.http.get("http://").send().unwrap_err();
        let message = http_error(error).to_string();
        assert!(message.contains("empty host"), "{message}");
    }

    #[test]
    fn responses_envelope_handles_text_refusal_and_incomplete_status() {
        let response = json!({
            "status": "completed",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "{\"candidates\":[]}"}]
            }]
        });
        assert_eq!(
            extract_output_text(&response).unwrap(),
            "{\"candidates\":[]}"
        );

        let refusal = json!({
            "status": "completed",
            "output": [{"content": [{"type": "refusal", "refusal": "cannot comply"}]}]
        });
        assert!(matches!(
            extract_output_text(&refusal),
            Err(OpenAiAdapterError::Refusal(_))
        ));

        let incomplete = json!({
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "output": []
        });
        assert!(matches!(
            extract_output_text(&incomplete),
            Err(OpenAiAdapterError::Incomplete(_))
        ));
    }
}
