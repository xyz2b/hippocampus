//! Wire protocol capabilities. This layer checks shape and capacity, never assertion semantics.
use super::{OpenAiAdapterError as Error, ReasoningEffort};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const ADAPTER_VERSION: &str = "structured-provider.v1";
pub const ANTHROPIC_SIMPLIFIED_VERSION: &str = "anthropic-native-simplified.v1";
pub const ANTHROPIC_NOPATTERN_VERSION: &str = "anthropic-native-simplified-nopattern.v1";
pub const MODEL_USER_AGENT: &str = "hippocampus/0.1";
pub const PROMPT_SCHEMA_VERSION: &str = "prompt-schema.v1";
pub const PROMPT_SCHEMA_INSTRUCTION: &str = "Output exactly one JSON object conforming to the following JSON Schema. Do not output prose, Markdown fences, or any text before or after the object.\nJSON Schema:\n";
pub const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
pub const DASHSCOPE_MODEL: &str = "qwen3.7-flash-2026-07-15";
pub const DASHSCOPE_QWEN38_MODEL: &str = "qwen3.8-flash";
pub const DASHSCOPE_QWEN38_CONTEXT_REVISION: &str = "qwen3.8-flash;dashscope-text-context.v1;alias";
pub const DASHSCOPE_CONTEXT_REVISION: &str = "qwen3.7-flash-2026-07-15;dashscope-text-context.v1";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredMode {
    #[default]
    NativeSchema,
    PromptSchema,
    /// Evaluation only; canonical output shape is unchanged.
    NativeSimplified,
    /// Evaluation only; removed nonblank constraints are enforced on canonical output.
    NativeSimplifiedNopattern,
}
impl StructuredMode {
    fn is_native(&self) -> bool {
        *self == Self::NativeSchema
    }
    fn rewrite_version(self) -> Option<&'static str> {
        match self {
            Self::NativeSimplified => Some(ANTHROPIC_SIMPLIFIED_VERSION),
            Self::NativeSimplifiedNopattern => Some(ANTHROPIC_NOPATTERN_VERSION),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    #[default]
    OpenaiResponses,
    AnthropicMessages,
    OpenaiChat,
}
impl ProviderProtocol {
    pub fn path(self) -> &'static str {
        match self {
            Self::OpenaiResponses => "responses",
            Self::AnthropicMessages => "v1/messages",
            Self::OpenaiChat => "chat/completions",
        }
    }
}

/// Deployment attestation: a pinned byte-level tokenizer and a verified template token bound.
/// No chars/4 heuristic, remote token-count request, or guessed template allowance is used.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextProof {
    pub tokenizer_revision: String,
    pub template_token_bound: u64,
    pub byte_level_bpe: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub protocol: ProviderProtocol,
    // Keep existing native provider serialization and recording identities byte-for-byte.
    #[serde(default, skip_serializing_if = "StructuredMode::is_native")]
    pub structured_mode: StructuredMode,
    pub base_url_env: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_proof: Option<ContextProof>,
}
impl ProviderConfig {
    fn is_dashscope(&self) -> bool {
        self.protocol == ProviderProtocol::OpenaiChat
            && matches!(
                self.model_revision.as_deref(),
                Some(DASHSCOPE_CONTEXT_REVISION | DASHSCOPE_QWEN38_CONTEXT_REVISION)
            )
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.structured_mode.rewrite_version().is_some()
            && self.protocol != ProviderProtocol::AnthropicMessages
        {
            return Err(Error::InvalidContext(
                "simplified schema modes only support Anthropic evaluation".into(),
            ));
        }
        if self.structured_mode == StructuredMode::PromptSchema
            && self.protocol != ProviderProtocol::AnthropicMessages
        {
            return Err(Error::InvalidContext(
                "prompt_schema only supports Anthropic evaluation".into(),
            ));
        }
        let env_name = |s: &str| {
            !s.is_empty()
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                && !s.as_bytes()[0].is_ascii_digit()
        };
        if !env_name(&self.base_url_env) || self.api_key_env.as_ref().is_some_and(|s| !env_name(s))
        {
            return Err(Error::InvalidContext(
                "provider environment variable name is invalid".into(),
            ));
        }
        if self.context_tokens == Some(0)
            || self
                .model_revision
                .as_ref()
                .is_some_and(|s| s.trim().is_empty())
        {
            return Err(Error::InvalidContext(
                "provider revision/capacity is invalid".into(),
            ));
        }
        Ok(())
    }

    pub fn require_native_schema(&self) -> Result<(), Error> {
        if self.structured_mode.rewrite_version().is_some() {
            return Err(Error::InvalidContext(
                "simplified schema modes are limited to Anthropic schema evaluation".into(),
            ));
        }
        if self.structured_mode != StructuredMode::NativeSchema {
            return Err(Error::InvalidContext(
                "prompt_schema is limited to the Opus evaluation profile".into(),
            ));
        }
        Ok(())
    }

    pub fn structured_mode_binding(&self) -> Option<Value> {
        if let Some(version) = self.structured_mode.rewrite_version() {
            return Some(json!({"mode":self.structured_mode,
                "rewrite_version":version}));
        }
        (self.structured_mode == StructuredMode::PromptSchema).then(|| {
            json!({"mode":self.structured_mode,"instruction_version":PROMPT_SCHEMA_VERSION,
                "instruction":PROMPT_SCHEMA_INSTRUCTION})
        })
    }

    pub fn check_model(&self, model: &str, effort: ReasoningEffort) -> Result<(), Error> {
        self.validate()?;
        let supported = match self.protocol {
            ProviderProtocol::OpenaiResponses if matches!(model, "gpt-6.1-sol" | "gpt-6-luna") => {
                self.thinking.is_none()
                    && matches!(
                        effort,
                        ReasoningEffort::Low | ReasoningEffort::Medium | ReasoningEffort::High
                    )
            }
            ProviderProtocol::OpenaiResponses => {
                self.thinking.is_none() && model.starts_with("ep-")
            }
            ProviderProtocol::AnthropicMessages => {
                matches!(
                    model,
                    "claude-opus-5-5" | "claude-sonnet-5-5" | "claude-haiku-5-5"
                ) && self.thinking != Some(false)
                    && matches!(
                        effort,
                        ReasoningEffort::Low | ReasoningEffort::Medium | ReasoningEffort::High
                    )
            }
            ProviderProtocol::OpenaiChat if model == DASHSCOPE_QWEN38_MODEL => {
                self.model_revision.as_deref() == Some(DASHSCOPE_QWEN38_CONTEXT_REVISION)
                    && self.api_key_env.is_some()
                    && self.thinking.is_some()
                    && effort == ReasoningEffort::Omit
            }
            ProviderProtocol::OpenaiChat if model == DASHSCOPE_MODEL => {
                self.model_revision.as_deref() == Some(DASHSCOPE_CONTEXT_REVISION)
                    && self.api_key_env.is_some()
                    && matches!(
                        (self.thinking, effort),
                        (Some(true), ReasoningEffort::Medium)
                            | (Some(false), ReasoningEffort::Omit)
                    )
            }
            ProviderProtocol::OpenaiChat => {
                matches!(
                    model,
                    "qwen3.5:9b-q8_0" | "qwen3.8:27b-q4_K_M" | "qwen3.8:27b-q4_K_M-ctx64k"
                ) && self.thinking.is_some()
                    && effort == ReasoningEffort::Omit
            }
        };
        if !supported {
            return Err(Error::InvalidContext(
                "provider/model/effort/thinking combination is unsupported".into(),
            ));
        }
        Ok(())
    }

    pub fn endpoint(&self, base: &str) -> Result<String, Error> {
        // Responses/Chat bases are complete API prefixes, including any version path.
        // Anthropic uses an SDK-style root; accept an explicit /v1 without duplicating it.
        let base = base.trim().trim_end_matches('/');
        let path = if self.is_dashscope() && base.ends_with("/compatible-mode") {
            "v1/chat/completions"
        } else if self.protocol == ProviderProtocol::AnthropicMessages && base.ends_with("/v1") {
            "messages"
        } else {
            self.protocol.path()
        };
        let endpoint = format!("{base}/{path}");
        let url = reqwest::Url::parse(&endpoint)
            .map_err(|_| Error::InvalidContext("invalid provider URL".into()))?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::InvalidContext(
                "provider URL must be http(s) without credentials/query/fragment".into(),
            ));
        }
        if self.is_dashscope()
            && (url.scheme() != "https"
                || !url.host_str().is_some_and(|h| h.ends_with(".aliyuncs.com"))
                || url.path() != "/compatible-mode/v1/chat/completions")
        {
            return Err(Error::InvalidContext(
                "DashScope requires its official HTTPS compatible-mode endpoint".into(),
            ));
        }
        Ok(endpoint)
    }

    pub fn check_context(&self, request: &Value, output: u64) -> Result<(), Error> {
        if self.is_dashscope() {
            // Official Qwen3.7 snapshot / Qwen3.8 alias limits, checked 2026-10-08: context 1,000,000;
            // thinking input 983,616; output 131,072. Cloud overflow is an error,
            // unlike Ollama's possible truncation. This is a byte prescreen, not
            // an attestation of an unpublished cloud tokenizer/template.
            // https://help.aliyun.com/zh/model-studio/qwen3-7-flash
            // https://help.aliyun.com/zh/model-studio/qwen3-8-flash
            // https://help.aliyun.com/zh/model-studio/error-code
            if self.context_tokens != Some(1_000_000)
                || self.context_proof.is_some()
                || request["model"]
                    != if self.model_revision.as_deref() == Some(DASHSCOPE_QWEN38_CONTEXT_REVISION)
                    {
                        DASHSCOPE_QWEN38_MODEL
                    } else {
                        DASHSCOPE_MODEL
                    }
                || !request["messages"].as_array().is_some_and(|m| {
                    m.len() == 2
                        && m[0]["role"] == "system"
                        && m[1]["role"] == "user"
                        && m.iter().all(|m| m["content"].is_string())
                })
            {
                return Err(Error::ContextUnproven);
            }
            let bytes = serde_json::to_vec(request)
                .map_err(|e| Error::InvalidContext(e.to_string()))?
                .len() as u64;
            // DashScope documents up to ten completion tokens of rounding error.
            if output == 0
                || output > 131_072
                || bytes > 983_616
                || bytes
                    .checked_add(output)
                    .and_then(|n| n.checked_add(10))
                    .is_none_or(|n| n > 1_000_000)
            {
                return Err(Error::ContextExceeded);
            }
            return Ok(());
        }
        // Remote structured APIs reject overflow; local Chat may silently truncate, so it is mandatory there.
        if self.context_tokens.is_none() && self.protocol != ProviderProtocol::OpenaiChat {
            return Ok(());
        }
        let proof = self
            .context_proof
            .as_ref()
            .filter(|p| p.byte_level_bpe && !p.tokenizer_revision.trim().is_empty())
            .filter(|_| {
                self.model_revision
                    .as_ref()
                    .is_some_and(|r| !r.trim().is_empty())
            })
            .ok_or(Error::ContextUnproven)?;
        let capacity = self.context_tokens.ok_or(Error::ContextUnproven)?;
        // Serialized bytes overcount text, escaping, schema and protocol framing for byte BPE.
        // Special tokens and any deployment template text are covered by the attested template bound.
        let input = serde_json::to_vec(request)
            .map_err(|e| Error::InvalidContext(e.to_string()))?
            .len() as u64;
        let total = input
            .checked_add(proof.template_token_bound)
            .and_then(|n| n.checked_add(output));
        if total.is_none_or(|n| n > capacity) {
            return Err(Error::ContextExceeded);
        }
        Ok(())
    }
}

/// Convert a canonical Responses request without modifying its domain payload or original schema.
pub fn provider_request(
    config: Option<&ProviderConfig>,
    canonical: &Value,
) -> Result<Value, Error> {
    let Some(config) = config else {
        return Ok(canonical.clone());
    };
    let effort = canonical
        .pointer("/reasoning/effort")
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()
        .map_err(|_| Error::InvalidContext("invalid effort".into()))?
        .unwrap_or(ReasoningEffort::Omit);
    let model = canonical["model"]
        .as_str()
        .ok_or_else(|| Error::InvalidContext("missing model".into()))?;
    config.check_model(model, effort)?;
    let tokens = canonical["max_output_tokens"]
        .as_u64()
        .ok_or_else(|| Error::InvalidContext("missing output cap".into()))?;
    let schema = &canonical["text"]["format"]["schema"];
    let mut request = match config.protocol {
        ProviderProtocol::OpenaiResponses => {
            if canonical["instructions"]
                .as_str()
                .is_none_or(|s| s.is_empty())
            {
                return Err(Error::InvalidContext("instructions required".into()));
            }
            let mut r = canonical.clone();
            if matches!(model, "gpt-6.1-sol" | "gpt-6-luna") {
                r.as_object_mut().unwrap().remove("temperature");
            }
            r
        }
        ProviderProtocol::AnthropicMessages => {
            let mut r = json!({"model":model,"system":canonical["instructions"],
            "messages":[{"role":"user","content":canonical["input"]}],"max_tokens":tokens,
            "thinking":{"type":"adaptive"},"output_config":{"effort":effort}});
            if config.structured_mode == StructuredMode::PromptSchema {
                let instructions = canonical["instructions"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| Error::InvalidContext("instructions required".into()))?;
                // Validate the supported keyword subset without altering the schema in the prompt.
                anthropic_schema(schema)?;
                r["system"] = json!(format!(
                    "{instructions}\n\n[{PROMPT_SCHEMA_VERSION}]\n{PROMPT_SCHEMA_INSTRUCTION}{schema}"
                ));
            } else {
                let schema = match config.structured_mode {
                    StructuredMode::NativeSimplified => anthropic_simplified_schema(schema)?,
                    StructuredMode::NativeSimplifiedNopattern => {
                        anthropic_simplified_nopattern_schema(schema)?
                    }
                    _ => anthropic_schema(schema)?,
                };
                r["output_config"]["format"] = json!({"type":"json_schema","schema":schema});
            }
            r
        }
        ProviderProtocol::OpenaiChat => json!({"model":model,"messages":[
            {"role":"system","content":canonical["instructions"]},{"role":"user","content":canonical["input"]}],
            "max_tokens":tokens,"temperature":0,"stream":false,
            "reasoning_effort":if config.thinking == Some(false) {"none"} else {"medium"},
            "response_format":{"type":"json_schema","json_schema":{"name":canonical["text"]["format"]["name"],"strict":true,"schema":schema}}}),
    };
    if config.model_revision.as_deref() == Some(DASHSCOPE_QWEN38_CONTEXT_REVISION) {
        // Qwen3.8 disallows reasoning_effort together with thinking_budget.
        request.as_object_mut().unwrap().remove("reasoning_effort");
        request["enable_thinking"] = json!(config.thinking.unwrap());
    }
    if config.is_dashscope() {
        // max_tokens excludes thought tokens on Qwen; this cap includes both.
        request.as_object_mut().unwrap().remove("max_tokens");
        request["max_completion_tokens"] = json!(tokens);
        if config.thinking == Some(true) {
            request["thinking_budget"] = json!(tokens / 2);
        }
    }
    config.check_context(&request, tokens)?;
    Ok(request)
}

/// An error envelope contains only fixed classifications, bounded header IDs and numeric usage.
/// Provider prose can echo private input or credentials; it is never retained or regex-redacted.
pub fn safe_http_error(
    status: u16,
    body: &[u8],
    request_id: Option<&str>,
    protocol: ProviderProtocol,
) -> Value {
    let parsed = (body.len() <= MAX_ERROR_BODY_BYTES)
        .then(|| serde_json::from_slice::<Value>(body).ok())
        .flatten();
    let code = if body.len() > MAX_ERROR_BODY_BYTES {
        "provider_error_body_too_large"
    } else if parsed.is_none() {
        "provider_error_non_json"
    } else if parsed
        .as_ref()
        .and_then(|e| e.pointer("/error/message"))
        .and_then(Value::as_str)
        .is_some_and(|s| s.contains("The compiled grammar is too large"))
    {
        "schema_grammar_too_large"
    } else {
        "provider_error_unclassified"
    };
    let mut detail = json!({"http_status":status,"code":code});
    if let Some(kind) = parsed
        .as_ref()
        .and_then(|e| e.pointer("/error/type"))
        .and_then(Value::as_str)
        .filter(|s| {
            matches!(
                *s,
                "invalid_request_error"
                    | "authentication_error"
                    | "permission_error"
                    | "not_found_error"
                    | "request_too_large"
                    | "rate_limit_error"
                    | "api_error"
                    | "overloaded_error"
                    | "server_error"
                    | "insufficient_quota"
            )
        })
    {
        detail["error_type"] = json!(kind);
    }
    if let Some(id) = request_id.filter(|s| {
        s.strip_prefix("req_").is_some_and(|suffix| {
            (8..=64).contains(&suffix.len())
                && suffix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    }) {
        detail["request_id"] = json!(id);
    }
    let usage = parsed
        .as_ref()
        .and_then(|e| normalized_usage(protocol, e))
        .and_then(|u| {
            let mut result = json!({"input_tokens":u["input_tokens"].as_u64()?,
                "output_tokens":u["output_tokens"].as_u64()?});
            for (field, key) in [
                ("input_tokens_details", "cached_tokens"),
                ("output_tokens_details", "reasoning_tokens"),
            ] {
                if let Some(n) = u[field][key].as_u64() {
                    result[field] = json!({key:n});
                }
            }
            Some(result)
        });
    json!({"status":"failed","provider_http_error":detail,"usage":usage})
}

/// Share bounded error-body handling between production and both evaluation send callbacks.
pub fn read_http_response(
    mut response: reqwest::blocking::Response,
    protocol: ProviderProtocol,
) -> Result<(u16, Value), Error> {
    use std::io::Read;
    let status = response.status().as_u16();
    let request_id = response
        .headers()
        .get("request-id")
        .or_else(|| response.headers().get("x-request-id"))
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let mut body = Vec::new();
    if !(200..300).contains(&status) {
        return Ok((
            status,
            read_http_error(response, status, request_id.as_deref(), protocol),
        ));
    }
    response
        .read_to_end(&mut body)
        .map_err(|_| Error::Http("response body read failed".into()))?;
    let envelope = serde_json::from_slice(&body)
        .map_err(|_| Error::InvalidResponse("response is not JSON".into()))?;
    Ok((status, envelope))
}

fn read_http_error(
    reader: impl std::io::Read,
    status: u16,
    request_id: Option<&str>,
    protocol: ProviderProtocol,
) -> Value {
    use std::io::Read;
    let mut body = Vec::new();
    let read = reader
        .take((MAX_ERROR_BODY_BYTES + 1) as u64)
        .read_to_end(&mut body);
    let mut envelope = safe_http_error(status, &body, request_id, protocol);
    if read.is_err() {
        envelope["provider_http_error"]["code"] = json!("provider_error_body_unreadable");
        envelope["usage"] = Value::Null;
    }
    envelope
}

pub fn anthropic_schema(schema: &Value) -> Result<Value, Error> {
    let object = schema
        .as_object()
        .ok_or_else(|| Error::InvalidContext("schema must be an object".into()))?;
    let mut result = object.clone();
    for (key, value) in object {
        match key.as_str() {
            "minimum" | "maximum" if schema["type"] == "integer" => {
                result.remove(key);
            }
            "type"
            | "required"
            | "additionalProperties"
            | "enum"
            | "const"
            | "pattern"
            | "description"
            | "title" => {}
            "properties" => {
                let properties = value
                    .as_object()
                    .ok_or_else(|| Error::InvalidContext("invalid properties".into()))?;
                result.insert(
                    key.clone(),
                    Value::Object(
                        properties
                            .iter()
                            .map(|(k, v)| Ok((k.clone(), anthropic_schema(v)?)))
                            .collect::<Result<_, Error>>()?,
                    ),
                );
            }
            "items" => {
                result.insert(key.clone(), anthropic_schema(value)?);
            }
            "anyOf" => {
                result.insert(
                    key.clone(),
                    Value::Array(
                        value
                            .as_array()
                            .ok_or_else(|| Error::InvalidContext("invalid anyOf".into()))?
                            .iter()
                            .map(anthropic_schema)
                            .collect::<Result<_, _>>()?,
                    ),
                );
            }
            _ => {
                return Err(Error::InvalidContext(format!(
                    "unsupported Anthropic schema keyword: {key}"
                )));
            }
        }
    }
    Ok(Value::Object(result))
}

/// Shape-preserving, evaluation-only rewrite. Only schema nodes are visited;
/// an enum, description, or application value can never become a reference.
pub fn anthropic_simplified_schema(schema: &Value) -> Result<Value, Error> {
    use std::collections::BTreeMap;
    fn children(node: &mut Value, mut visit: impl FnMut(&mut Value)) {
        if let Some(fields) = node.get_mut("properties").and_then(Value::as_object_mut) {
            for child in fields.values_mut() {
                visit(child);
            }
        }
        if let Some(child) = node.get_mut("items") {
            visit(child);
        }
        if let Some(branches) = node.get_mut("anyOf").and_then(Value::as_array_mut) {
            for child in branches {
                visit(child);
            }
        }
    }
    fn flatten(node: &mut Value) {
        children(node, flatten);
        if let Some(branches) = node.get_mut("anyOf").and_then(Value::as_array_mut) {
            let mut flat = Vec::new();
            for branch in std::mem::take(branches) {
                if branch.as_object().is_some_and(|o| o.len() == 1)
                    && let Some(inner) = branch["anyOf"].as_array()
                {
                    flat.extend(inner.iter().cloned());
                } else {
                    flat.push(branch);
                }
            }
            *branches = flat;
        }
    }
    fn collect(node: &Value, repeated: &mut BTreeMap<String, (Value, usize)>) {
        if node["type"] == "object" {
            let entry = repeated
                .entry(node.to_string())
                .or_insert((node.clone(), 0));
            entry.1 += 1;
        }
        // Reuse the schema-only traversal, without modifying the input.
        children(&mut node.clone(), |child| collect(child, repeated));
    }
    fn render(node: &Value, names: &BTreeMap<String, String>, defining: bool) -> Value {
        if !defining && let Some(name) = names.get(&node.to_string()) {
            return json!({"$ref":format!("#/definitions/{name}")});
        }
        let mut result = node.clone();
        children(&mut result, |child| *child = render(child, names, false));
        result
    }
    let mut flat = anthropic_schema(schema)?;
    flatten(&mut flat);
    let mut repeated = BTreeMap::new();
    collect(&flat, &mut repeated);
    let names: BTreeMap<_, _> = repeated
        .iter()
        .filter(|(_, (_, count))| *count > 1)
        .enumerate()
        .map(|(i, (key, _))| (key.clone(), format!("d{i}")))
        .collect();
    let mut result = render(&flat, &names, true);
    if !names.is_empty() {
        result["definitions"] = json!(
            names
                .iter()
                .map(|(key, name)| { (name.clone(), render(&repeated[key].0, &names, true)) })
                .collect::<BTreeMap<_, _>>()
        );
    }
    // Internal definitions are strictly smaller subtrees, hence nonrecursive.
    // Check that invariant before handing the schema to a provider.
    expand_anthropic_simplified_schema(&result)?;
    Ok(result)
}

/// Both schemas and the rewrite identity belong to the recording, not model input.
pub fn anthropic_simplified_binding(original: &Value) -> Result<Value, Error> {
    Ok(
        json!({"mode":"native_simplified","rewrite_version":ANTHROPIC_SIMPLIFIED_VERSION,
        "original_schema":original,"wire_schema":anthropic_simplified_schema(original)?}),
    )
}

/// AF238 only: remove exactly the canonical nonblank pattern, never arbitrary regex constraints.
pub fn anthropic_simplified_nopattern_schema(original: &Value) -> Result<Value, Error> {
    fn remove(node: &mut Value) {
        if node["pattern"] == r"^[\s\S]*\S[\s\S]*$" {
            node.as_object_mut().unwrap().remove("pattern");
        }
        for key in ["properties", "definitions"] {
            if let Some(fields) = node.get_mut(key).and_then(Value::as_object_mut) {
                for child in fields.values_mut() {
                    remove(child);
                }
            }
        }
        if let Some(child) = node.get_mut("items") {
            remove(child);
        }
        if let Some(branches) = node.get_mut("anyOf").and_then(Value::as_array_mut) {
            for child in branches {
                remove(child);
            }
        }
    }
    let mut wire = anthropic_simplified_schema(original)?;
    remove(&mut wire);
    Ok(wire)
}

pub fn anthropic_simplified_nopattern_binding(original: &Value) -> Result<Value, Error> {
    Ok(
        json!({"mode":"native_simplified_nopattern","rewrite_version":ANTHROPIC_NOPATTERN_VERSION,
        "original_schema":original,"wire_schema":anthropic_simplified_nopattern_schema(original)?}),
    )
}

/// Resolve only acyclic local definitions generated by the evaluation rewrite.
/// No response coercion or reverse mapping is performed.
pub fn expand_anthropic_simplified_schema(schema: &Value) -> Result<Value, Error> {
    fn expand(
        node: &Value,
        definitions: &serde_json::Map<String, Value>,
        stack: &mut Vec<String>,
        remaining: &mut usize,
        depth: usize,
    ) -> Result<Value, Error> {
        let invalid = || Error::InvalidContext("invalid simplified schema reference".into());
        *remaining = remaining.checked_sub(1).ok_or_else(invalid)?;
        if depth > 64 {
            return Err(invalid());
        }
        let object = node.as_object().ok_or_else(invalid)?;
        if let Some(reference) = object.get("$ref") {
            let name = reference
                .as_str()
                .and_then(|s| s.strip_prefix("#/definitions/"))
                .filter(|s| !s.is_empty() && !s.contains(['/', '~']))
                .ok_or_else(invalid)?;
            if object.len() != 1 || stack.iter().any(|s| s == name) {
                return Err(invalid());
            }
            let target = definitions.get(name).ok_or_else(invalid)?;
            stack.push(name.into());
            let result = expand(target, definitions, stack, remaining, depth + 1);
            stack.pop();
            return result;
        }
        if object.contains_key("definitions") {
            return Err(invalid());
        }
        let mut result = node.clone();
        if let Some(fields) = result.get_mut("properties").and_then(Value::as_object_mut) {
            for child in fields.values_mut() {
                *child = expand(child, definitions, stack, remaining, depth + 1)?;
            }
        }
        if let Some(child) = result.get_mut("items") {
            *child = expand(child, definitions, stack, remaining, depth + 1)?;
        }
        if let Some(branches) = result.get_mut("anyOf").and_then(Value::as_array_mut) {
            for child in branches {
                *child = expand(child, definitions, stack, remaining, depth + 1)?;
            }
        }
        // Fail closed for unsupported keywords, even when an unused definition contains them.
        anthropic_schema(&result)?;
        Ok(result)
    }
    let mut root = schema.clone();
    let definitions = root
        .as_object_mut()
        .ok_or_else(|| Error::InvalidContext("schema must be an object".into()))?
        .remove("definitions")
        .unwrap_or_else(|| json!({}));
    let definitions = definitions
        .as_object()
        .ok_or_else(|| Error::InvalidContext("invalid simplified definitions".into()))?;
    let mut remaining = 50_000;
    for (name, node) in definitions {
        if name.is_empty() || name.contains(['/', '~']) {
            return Err(Error::InvalidContext(
                "invalid simplified definition name".into(),
            ));
        }
        expand(
            node,
            definitions,
            &mut vec![name.clone()],
            &mut remaining,
            0,
        )?;
    }
    expand(&root, definitions, &mut Vec::new(), &mut remaining, 0)
}

pub fn normalized_usage(protocol: ProviderProtocol, envelope: &Value) -> Option<Value> {
    let u = envelope.get("usage")?;
    if protocol == ProviderProtocol::OpenaiResponses {
        return Some(u.clone());
    }
    let (input, output) = match protocol {
        ProviderProtocol::AnthropicMessages => (
            u["input_tokens"]
                .as_u64()?
                .checked_add(
                    u.get("cache_creation_input_tokens")
                        .map_or(Some(0), Value::as_u64)?,
                )?
                .checked_add(
                    u.get("cache_read_input_tokens")
                        .map_or(Some(0), Value::as_u64)?,
                )?,
            u["output_tokens"].as_u64()?,
        ),
        ProviderProtocol::OpenaiChat => (
            u["prompt_tokens"].as_u64()?,
            u["completion_tokens"].as_u64()?,
        ),
        _ => unreachable!(),
    };
    let mut result = json!({"input_tokens":input,"output_tokens":output});
    if protocol == ProviderProtocol::OpenaiChat {
        if let Some(n) = u
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .filter(|n| *n <= input)
        {
            result["input_tokens_details"] = json!({"cached_tokens":n});
        }
    }
    let thinking = if protocol == ProviderProtocol::AnthropicMessages {
        u.pointer("/output_tokens_details/thinking_tokens")
    } else {
        u.pointer("/completion_tokens_details/reasoning_tokens")
    };
    if let Some(t) = thinking.and_then(Value::as_u64).filter(|n| *n <= output) {
        result["output_tokens_details"] = json!({"reasoning_tokens":t});
    }
    Some(result)
}

pub fn provider_output(protocol: ProviderProtocol, envelope: &Value) -> Result<String, Error> {
    match protocol {
        ProviderProtocol::OpenaiResponses => {
            let mut envelope = envelope.clone();
            if let Some(items) = envelope["output"].as_array_mut() {
                items.retain(|item| item["type"].is_null() || item["type"] == "message");
            }
            nonempty(super::extract_output_text(&envelope)?)
        }
        ProviderProtocol::AnthropicMessages => {
            match envelope["stop_reason"].as_str() {
                Some("refusal") => return Err(Error::Refusal("provider_refusal".into())),
                Some("max_tokens") => return Err(Error::Incomplete("max_output_tokens".into())),
                Some("end_turn") => {}
                _ => {
                    return Err(Error::InvalidResponse(
                        "unexpected Anthropic stop_reason".into(),
                    ));
                }
            }
            let content = envelope["content"]
                .as_array()
                .ok_or_else(|| Error::InvalidResponse("missing content".into()))?;
            if content.iter().any(|p| {
                !matches!(
                    p["type"].as_str(),
                    Some("text" | "thinking" | "redacted_thinking")
                )
            }) {
                return Err(Error::InvalidResponse("unexpected content block".into()));
            }
            nonempty(
                content
                    .iter()
                    .filter(|p| p["type"] == "text")
                    .filter_map(|p| p["text"].as_str())
                    .collect(),
            )
        }
        ProviderProtocol::OpenaiChat => {
            let choices = envelope["choices"]
                .as_array()
                .filter(|c| c.len() == 1)
                .ok_or_else(|| Error::InvalidResponse("expected one choice".into()))?;
            let c = &choices[0];
            if !c["message"]["refusal"].is_null() || c["finish_reason"] == "content_filter" {
                return Err(Error::Refusal("provider_refusal".into()));
            }
            if c["finish_reason"] == "length" {
                return Err(Error::Incomplete("max_output_tokens".into()));
            }
            if c["finish_reason"] != "stop" || !c["message"]["tool_calls"].is_null() {
                return Err(Error::InvalidResponse("unexpected chat completion".into()));
            }
            nonempty(c["message"]["content"].as_str().unwrap_or_default().into())
        }
    }
}
/// Evaluation-only excerpts: never parse these fragments as a model answer.
pub fn incomplete_diagnostics(protocol: ProviderProtocol, envelope: &Value) -> Option<Value> {
    let (content, reasoning) = match protocol {
        ProviderProtocol::OpenaiChat => {
            let choices = envelope["choices"].as_array().filter(|c| c.len() == 1)?;
            let choice = &choices[0];
            if choice["finish_reason"] != "length" || !choice["message"]["refusal"].is_null() {
                return None;
            }
            (
                choice["message"]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                choice["message"]["reasoning_content"]
                    .as_str()
                    .map(str::to_owned),
            )
        }
        ProviderProtocol::OpenaiResponses => {
            if envelope["status"] != "incomplete"
                || envelope["incomplete_details"]["reason"] != "max_output_tokens"
            {
                return None;
            }
            let messages = envelope["output"].as_array()?;
            let mut text = String::new();
            for message in messages.iter().filter(|m| m["type"] == "message") {
                for part in message["content"].as_array()? {
                    if part["type"] == "refusal" {
                        return None;
                    }
                    if part["type"] == "output_text" {
                        text.push_str(part["text"].as_str().unwrap_or_default());
                    }
                }
            }
            (text, None) // Encrypted/internal Responses reasoning is never captured.
        }
        ProviderProtocol::AnthropicMessages => {
            if envelope["stop_reason"] != "max_tokens" {
                return None;
            }
            let content = envelope["content"].as_array()?;
            let text = content
                .iter()
                .filter(|p| p["type"] == "text")
                .filter_map(|p| p["text"].as_str())
                .collect::<String>();
            (text, None) // Keep diagnostics limited to final text; no thinking blocks.
        }
    };
    fn head(s: &str, bytes: usize) -> &str {
        let mut end = s.len().min(bytes);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
    fn tail(s: &str, bytes: usize) -> &str {
        let mut start = s.len().saturating_sub(bytes);
        while !s.is_char_boundary(start) {
            start += 1;
        }
        &s[start..]
    }
    let mut result = json!({"diagnostic_only":true,"scoring_eligible":false,
        "output_utf8_bytes":content.len(),"output_head":head(&content,2048),"output_tail":tail(&content,2048),
        "limits_utf8_bytes":{"output_head":2048,"output_tail":2048,"reasoning_tail":1024}});
    if let Some(reasoning) = reasoning {
        result["reasoning_utf8_bytes"] = json!(reasoning.len());
        result["reasoning_tail"] = json!(tail(&reasoning, 1024));
    }
    Some(result)
}
fn nonempty(text: String) -> Result<String, Error> {
    if text.trim().is_empty() {
        Err(Error::InvalidResponse("empty final content".into()))
    } else {
        Ok(text)
    }
}

/// Mechanical schema subset used by this runtime, including properties dropped on the wire.
pub fn check_output(schema: &Value, output: &Value) -> Result<(), Error> {
    fn matches(s: &Value, v: &Value) -> bool {
        if let Some(branches) = s["anyOf"].as_array() {
            return branches.iter().any(|s| matches(s, v));
        }
        if s.get("const").is_some_and(|c| c != v)
            || s["enum"].as_array().is_some_and(|e| !e.contains(v))
        {
            return false;
        }
        match s["type"].as_str() {
            Some("null") => v.is_null(),
            Some("boolean") => v.is_boolean(),
            Some("string") => v
                .as_str()
                .is_some_and(|v| s.get("pattern").is_none() || !v.trim().is_empty()),
            Some("integer") => {
                (v.is_i64() || v.is_u64())
                    && v.as_f64().is_some_and(|n| {
                        s["minimum"].as_f64().is_none_or(|min| n >= min)
                            && s["maximum"].as_f64().is_none_or(|max| n <= max)
                    })
            }
            Some("array") => v
                .as_array()
                .is_some_and(|a| a.iter().all(|v| matches(&s["items"], v))),
            Some("object") => v.as_object().is_some_and(|o| {
                let properties = s["properties"].as_object();
                s["required"].as_array().is_none_or(|r| {
                    r.iter()
                        .all(|k| k.as_str().is_some_and(|k| o.contains_key(k)))
                }) && o.iter().all(|(k, v)| {
                    properties
                        .and_then(|p| p.get(k))
                        .map_or(s["additionalProperties"] != false, |s| matches(s, v))
                })
            }),
            _ => false,
        }
    }
    if matches(schema, output) {
        Ok(())
    } else {
        Err(Error::InvalidStructuredOutput("schema_shape".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModelBudgetConfig, ModelStage, structured_request};
    fn config(protocol: ProviderProtocol) -> ProviderConfig {
        ProviderConfig {
            protocol,
            structured_mode: Default::default(),
            base_url_env: "FIXTURE_BASE".into(),
            api_key_env: None,
            model_revision: None,
            thinking: None,
            context_tokens: None,
            context_proof: None,
        }
    }
    fn request() -> Value {
        structured_request(
            ModelStage::W3,
            "test",
            "fixed instruction",
            json!({"message":"虚构内容"}),
            crate::write_models::object(&[("count", crate::write_models::index())]),
            &ModelBudgetConfig::default(),
        )
    }
    fn dashscope(thinking: bool) -> ProviderConfig {
        let mut cfg = config(ProviderProtocol::OpenaiChat);
        cfg.api_key_env = Some("QWEN_API_KEY".into());
        cfg.thinking = Some(thinking);
        cfg.model_revision = Some(DASHSCOPE_CONTEXT_REVISION.into());
        cfg.context_tokens = Some(1_000_000);
        cfg
    }
    #[test]
    fn dashscope_snapshot_caps_auth_endpoint_and_usage_are_explicit() {
        for thinking in [true, false] {
            let cfg = dashscope(thinking);
            let mut canonical = request();
            canonical["model"] = json!(DASHSCOPE_MODEL);
            canonical["max_output_tokens"] = json!(32768);
            if thinking {
                canonical["reasoning"] = json!({"effort":"medium"});
            } else {
                canonical.as_object_mut().unwrap().remove("reasoning");
            }
            let wire = provider_request(Some(&cfg), &canonical).unwrap();
            assert_eq!(
                wire["reasoning_effort"],
                if thinking { "medium" } else { "none" }
            );
            assert_eq!(wire["max_completion_tokens"], 32768);
            if thinking {
                assert_eq!(wire["thinking_budget"], 16384);
                canonical["max_output_tokens"] = json!(16384);
                let w3 = provider_request(Some(&cfg), &canonical).unwrap();
                assert_eq!(w3["thinking_budget"], 8192);
                assert_eq!(w3["max_completion_tokens"], 16384);
            } else {
                assert!(wire.get("thinking_budget").is_none());
            }
            assert!(wire.get("max_tokens").is_none());
            assert_eq!(
                wire["response_format"]["json_schema"]["schema"],
                canonical["text"]["format"]["schema"]
            );
            assert_eq!(wire["response_format"]["json_schema"]["strict"], true);
            let endpoint = "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions";
            for base in [
                "https://dashscope.aliyuncs.com/compatible-mode/",
                "https://dashscope.aliyuncs.com/compatible-mode/v1",
            ] {
                assert_eq!(cfg.endpoint(base).unwrap(), endpoint);
            }
            for base in [
                "https://ws-fixture.cn-beijing.maas.aliyuncs.com/compatible-mode",
                "https://ws-fixture.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
            ] {
                assert_eq!(
                    cfg.endpoint(base).unwrap(),
                    "https://ws-fixture.cn-beijing.maas.aliyuncs.com/compatible-mode/v1/chat/completions"
                );
            }
            for base in [
                "http://127.0.0.1:11434/v1",
                "http://ws-fixture.cn-beijing.maas.aliyuncs.com/compatible-mode",
                "http://dashscope.aliyuncs.com/compatible-mode",
                "https://evil.aliyuncs.com.example.com/compatible-mode",
                "https://aliyuncs.com.evil.com/compatible-mode",
                "https://dashscope.aliyuncs.com/v1",
                "https://proxy.invalid/compatible-mode",
                "https://dashscope.aliyuncs.com/compatible-mode?secret=x",
                "https://ws-fixture.cn-beijing.maas.aliyuncs.com/compatible-mode#fragment",
                "https://user:password@ws-fixture.cn-beijing.maas.aliyuncs.com/compatible-mode",
            ] {
                assert!(cfg.endpoint(base).is_err());
            }
            assert!(
                cfg.check_model("qwen3.7-flash", ReasoningEffort::Medium)
                    .is_err()
            );
            assert!(
                cfg.check_model("qwen3.7-flash-2099-01-01", ReasoningEffort::Medium)
                    .is_err()
            );
            let mut invalid = cfg.clone();
            invalid.api_key_env = None;
            assert!(
                invalid
                    .check_model(
                        DASHSCOPE_MODEL,
                        if thinking {
                            ReasoningEffort::Medium
                        } else {
                            ReasoningEffort::Omit
                        }
                    )
                    .is_err()
            );
        }
        let response = json!({"choices":[{"finish_reason":"stop","message":{"content":"{}","reasoning_content":"ignored thought"}}],
            "usage":{"prompt_tokens":11,"completion_tokens":8,"completion_tokens_details":{"reasoning_tokens":6}}});
        assert_eq!(
            provider_output(ProviderProtocol::OpenaiChat, &response).unwrap(),
            "{}"
        );
        assert_eq!(
            normalized_usage(ProviderProtocol::OpenaiChat, &response).unwrap(),
            json!({"input_tokens":11,"output_tokens":8,"output_tokens_details":{"reasoning_tokens":6}})
        );
        let no_thought = json!({"usage":{"prompt_tokens":11,"completion_tokens":2}});
        assert!(
            normalized_usage(ProviderProtocol::OpenaiChat, &no_thought)
                .unwrap()
                .get("output_tokens_details")
                .is_none()
        );
    }
    #[test]
    fn dashscope_context_rejects_overflow_unknown_binding_and_non_text_without_truncation() {
        let cfg = dashscope(true);
        let mut canonical = request();
        canonical["model"] = json!(DASHSCOPE_MODEL);
        canonical["reasoning"] = json!({"effort":"medium"});
        let wire = provider_request(Some(&cfg), &canonical).unwrap();
        let mut big = wire.clone();
        big["messages"][1]["content"] = json!("汉".repeat(400000));
        assert!(matches!(
            cfg.check_context(&big, 32768),
            Err(Error::ContextExceeded)
        ));
        assert!(cfg.check_context(&wire, u64::MAX).is_err());
        let mut unknown = cfg.clone();
        unknown.context_tokens = None;
        assert!(matches!(
            unknown.check_context(&wire, 32768),
            Err(Error::ContextUnproven)
        ));
        unknown = cfg.clone();
        unknown.context_tokens = Some(2_000_000);
        assert!(matches!(
            unknown.check_context(&wire, 32768),
            Err(Error::ContextUnproven)
        ));
        let mut multimedia = wire.clone();
        multimedia["messages"][1]["content"] =
            json!([{"type":"image_url","image_url":{"url":"https://example.invalid/image"}}]);
        assert!(matches!(
            cfg.check_context(&multimedia, 32768),
            Err(Error::ContextUnproven)
        ));
        assert_eq!(wire["messages"][1]["content"], canonical["input"]);
        let mut local = config(ProviderProtocol::OpenaiChat);
        local.thinking = Some(false);
        assert!(matches!(
            local.check_context(&wire, 32768),
            Err(Error::ContextUnproven)
        ));
    }
    #[test]
    fn prompt_schema_preserves_original_schemas_and_checks_the_expanded_request() {
        let mut cfg = config(ProviderProtocol::AnthropicMessages);
        let native = serde_json::to_value(&cfg).unwrap();
        assert!(native.get("structured_mode").is_none());
        assert_eq!(
            serde_json::from_value::<ProviderConfig>(native).unwrap(),
            cfg
        );
        cfg.structured_mode = StructuredMode::PromptSchema;
        assert!(cfg.require_native_schema().is_err());
        for schema in [
            crate::write_models::w1_schema(),
            crate::write_models::w3_schema(),
            crate::write_models::w4_schema(),
            crate::recall_models::r1_schema(),
            crate::recall_models::r4_schema(),
        ] {
            let mut canonical = request();
            canonical["model"] = json!("claude-opus-5-5");
            canonical["text"]["format"]["schema"] = schema.clone();
            let before = canonical.clone();
            let wire = provider_request(Some(&cfg), &canonical).unwrap();
            let system = wire["system"].as_str().unwrap();
            assert!(system.starts_with(&format!(
                "{}\n\n[{PROMPT_SCHEMA_VERSION}]\n",
                canonical["instructions"].as_str().unwrap()
            )));
            let appended = system.split_once(PROMPT_SCHEMA_INSTRUCTION).unwrap().1;
            assert_eq!(serde_json::from_str::<Value>(appended).unwrap(), schema);
            assert!(wire["output_config"].get("format").is_none());
            assert_eq!(wire["output_config"]["effort"], "medium");
            assert_eq!(wire["thinking"], json!({"type":"adaptive"}));
            assert_eq!(wire["max_tokens"], canonical["max_output_tokens"]);
            assert_eq!(wire["messages"][0]["content"], canonical["input"]);
            assert_eq!(canonical, before);
            assert_eq!(
                cfg.structured_mode_binding().unwrap()["instruction_version"],
                PROMPT_SCHEMA_VERSION
            );
            cfg.model_revision = Some("fixture-model".into());
            cfg.context_proof = Some(ContextProof {
                tokenizer_revision: "fixture-tokenizer".into(),
                template_token_bound: 64,
                byte_level_bpe: true,
            });
            let expanded_bytes = serde_json::to_vec(&wire).unwrap().len() as u64;
            cfg.context_tokens =
                Some(expanded_bytes + 64 + wire["max_tokens"].as_u64().unwrap() - 1);
            assert!(matches!(
                provider_request(Some(&cfg), &canonical),
                Err(Error::ContextExceeded)
            ));
            cfg.context_tokens = None;
        }
        for protocol in [
            ProviderProtocol::OpenaiResponses,
            ProviderProtocol::OpenaiChat,
        ] {
            cfg.protocol = protocol;
            assert!(cfg.validate().is_err());
        }
    }
    #[test]
    fn local_schema_accepts_valid_json_and_rejects_unrepaired_outputs() {
        let schema = crate::write_models::object(&[
            ("kind", crate::write_models::choices(&["ok"])),
            ("count", crate::write_models::index()),
            ("quote", crate::write_models::string()),
        ]);
        let valid = r#"{"kind":"ok","count":0,"quote":"虚构"}"#;
        check_output(&schema, &serde_json::from_str(valid).unwrap()).unwrap();
        for text in [
            format!("```json\n{valid}\n```"),
            format!("Here is the output: {valid}"),
            format!("{valid} done"),
            format!("{valid}{valid}"),
            "{".into(),
        ] {
            assert!(serde_json::from_str::<Value>(&text).is_err());
        }
        for bad in [
            json!({"kind":"ok","count":0,"quote":"虚构","extra":true}),
            json!({"kind":"bad","count":0,"quote":"虚构"}),
            json!({"kind":"ok","count":-1,"quote":"虚构"}),
            json!({"kind":"ok","count":0,"quote":" \n"}),
            json!({"kind":"ok","count":0}),
        ] {
            assert!(check_output(&schema, &bad).is_err());
        }
    }
    #[test]
    fn http_errors_keep_only_safe_classifications_ids_and_numeric_usage() {
        let protocol = ProviderProtocol::AnthropicMessages;
        let body = json!({"error":{"type":"invalid_request_error",
            "message":"The compiled grammar is too large. PRIVATE_TEXT Bearer secret-key"},
            "request_id":"PRIVATE_TEXT", "usage":{"input_tokens":3,"output_tokens":0,
                "private":"PRIVATE_TEXT"}});
        let safe = safe_http_error(
            400,
            body.to_string().as_bytes(),
            Some("req_12345678"),
            protocol,
        );
        assert_eq!(
            safe["provider_http_error"],
            json!({"http_status":400,
            "code":"schema_grammar_too_large","error_type":"invalid_request_error","request_id":"req_12345678"})
        );
        assert_eq!(safe["usage"], json!({"input_tokens":3,"output_tokens":0}));
        assert!(!safe.to_string().contains("PRIVATE_TEXT"));
        assert!(!safe.to_string().contains("secret-key"));
        for id in [
            "PRIVATE_TEXT",
            "Bearer secret-key",
            "req_生日",
            "req_12345678\nsecret",
            &format!("req_{}", "a".repeat(65)),
        ] {
            let body = json!({"error":{"type":"PRIVATE_TEXT", "message":"PRIVATE_TEXT secret-key"},
                "usage":{"input_tokens":3,"output_tokens":0,"private":"PRIVATE_TEXT"}});
            let safe = safe_http_error(
                401,
                body.to_string().as_bytes(),
                Some(id),
                ProviderProtocol::OpenaiResponses,
            );
            assert_eq!(
                safe["provider_http_error"],
                json!({"http_status":401,"code":"provider_error_unclassified"})
            );
            assert_eq!(safe["usage"], json!({"input_tokens":3,"output_tokens":0}));
            assert!(!safe.to_string().contains("PRIVATE_TEXT"));
            assert!(!safe.to_string().contains("secret-key"));
        }
        let non_json = safe_http_error(502, "<html>私有原话</html>".as_bytes(), None, protocol);
        assert_eq!(
            non_json["provider_http_error"]["code"],
            "provider_error_non_json"
        );
        assert!(non_json["usage"].is_null());
        let bytes = vec![b'x'; MAX_ERROR_BODY_BYTES * 2];
        let mut cursor = std::io::Cursor::new(&bytes);
        let limited = read_http_error(&mut cursor, 400, None, protocol);
        assert_eq!(cursor.position(), (MAX_ERROR_BODY_BYTES + 1) as u64);
        assert_eq!(
            limited["provider_http_error"]["code"],
            "provider_error_body_too_large"
        );
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("PRIVATE_TEXT secret-key"))
            }
        }
        let broken = read_http_error(Broken, 503, None, protocol);
        assert_eq!(
            broken["provider_http_error"]["code"],
            "provider_error_body_unreadable"
        );
        assert!(!broken.to_string().contains("PRIVATE_TEXT"));
    }
    #[test]
    fn provider_endpoints_handle_roots_and_api_prefixes_without_duplication() {
        for (protocol, base, expected) in [
            (
                ProviderProtocol::AnthropicMessages,
                "https://fixture.invalid",
                "https://fixture.invalid/v1/messages",
            ),
            (
                ProviderProtocol::AnthropicMessages,
                "https://fixture.invalid/",
                "https://fixture.invalid/v1/messages",
            ),
            (
                ProviderProtocol::AnthropicMessages,
                "https://fixture.invalid/v1/",
                "https://fixture.invalid/v1/messages",
            ),
            (
                ProviderProtocol::AnthropicMessages,
                "https://fixture.invalid/proxy",
                "https://fixture.invalid/proxy/v1/messages",
            ),
            (
                ProviderProtocol::OpenaiResponses,
                "https://fixture.invalid",
                "https://fixture.invalid/responses",
            ),
            (
                ProviderProtocol::OpenaiResponses,
                "https://fixture.invalid/v1/",
                "https://fixture.invalid/v1/responses",
            ),
            (
                ProviderProtocol::OpenaiResponses,
                "https://ark.cn-beijing.volces.com/api/v3/",
                "https://ark.cn-beijing.volces.com/api/v3/responses",
            ),
            (
                ProviderProtocol::OpenaiChat,
                "https://fixture.invalid/",
                "https://fixture.invalid/chat/completions",
            ),
            (
                ProviderProtocol::OpenaiChat,
                "http://127.0.0.1:11434/v1/",
                "http://127.0.0.1:11434/v1/chat/completions",
            ),
        ] {
            assert_eq!(config(protocol).endpoint(base).unwrap(), expected);
        }
    }
    #[test]
    fn all_five_anthropic_schemas_only_drop_integer_bounds() {
        fn without_bounds(v: &mut Value) {
            match v {
                Value::Object(o) => {
                    o.remove("minimum");
                    o.remove("maximum");
                    for v in o.values_mut() {
                        without_bounds(v);
                    }
                }
                Value::Array(a) => {
                    for v in a {
                        without_bounds(v);
                    }
                }
                _ => {}
            }
        }
        for schema in [
            crate::write_models::w1_schema(),
            crate::write_models::w3_schema(),
            crate::write_models::w4_schema(),
            crate::recall_models::r1_schema(),
            crate::recall_models::r4_schema(),
        ] {
            let original = schema.clone();
            let actual = anthropic_schema(&schema).unwrap();
            let mut expected = schema.clone();
            without_bounds(&mut expected);
            assert_eq!(actual, expected);
            assert_eq!(schema, original);
        }
        for bad in [
            json!({"type":"integer","multipleOf":2}),
            json!({"type":"number","minimum":0}),
            json!({"type":"string","minLength":1}),
            json!({"type":"array","items":{"type":"string"},"maxItems":3}),
        ] {
            assert!(anthropic_schema(&bad).is_err());
        }
    }
    #[test]
    fn capabilities_and_wire_shapes_are_explicit() {
        let original = request();
        assert_eq!(provider_request(None, &original).unwrap(), original);
        let mut gpt = original.clone();
        gpt["model"] = json!("gpt-6.1-sol");
        let cfg = config(ProviderProtocol::OpenaiResponses);
        let r = provider_request(Some(&cfg), &gpt).unwrap();
        assert_eq!(r["instructions"], original["instructions"]);
        assert!(r.get("temperature").is_none());
        gpt["reasoning"]["effort"] = json!("minimal");
        assert!(provider_request(Some(&cfg), &gpt).is_err());
        let mut anthropic = config(ProviderProtocol::AnthropicMessages);
        let mut r = original.clone();
        r["model"] = json!("claude-opus-5-5");
        let converted = provider_request(Some(&anthropic), &r).unwrap();
        assert_eq!(converted["system"], original["instructions"]);
        assert_eq!(converted["messages"][0]["content"], original["input"]);
        assert_eq!(converted["max_tokens"], original["max_output_tokens"]);
        assert!(converted.get("temperature").is_none());
        assert!(
            converted["output_config"]["format"]["schema"]["properties"]["count"]
                .get("minimum")
                .is_none()
        );
        anthropic.thinking = Some(false);
        assert!(provider_request(Some(&anthropic), &r).is_err());
        let mut qwen = config(ProviderProtocol::OpenaiChat);
        qwen.thinking = Some(false);
        r["model"] = json!("qwen3.5:9b-q8_0");
        ReasoningEffort::Omit.apply(&mut r);
        assert!(matches!(
            provider_request(Some(&qwen), &r),
            Err(Error::ContextUnproven)
        ));
        qwen.model_revision = Some("fixture-digest".into());
        qwen.context_tokens = Some(131072);
        qwen.context_proof = Some(ContextProof {
            tokenizer_revision: "fixture-tokenizer".into(),
            template_token_bound: 64,
            byte_level_bpe: true,
        });
        let off = provider_request(Some(&qwen), &r).unwrap();
        assert_eq!(off["reasoning_effort"], "none");
        qwen.thinking = Some(true);
        let on = provider_request(Some(&qwen), &r).unwrap();
        assert_eq!(on["reasoning_effort"], "medium");
        assert_eq!(on["messages"], off["messages"]);
        assert!(!on.to_string().contains("fixture-digest"));
        qwen.context_tokens = Some(1);
        assert!(matches!(
            provider_request(Some(&qwen), &r),
            Err(Error::ContextExceeded)
        ));
        ReasoningEffort::Low.apply(&mut r);
        assert!(provider_request(Some(&qwen), &r).is_err());
    }
    #[test]
    fn qwen_ctx64k_alias_preserves_thinking_and_rejects_unknown_aliases() {
        let mut qwen = config(ProviderProtocol::OpenaiChat);
        qwen.model_revision = Some("fixture-alias-digest".into());
        qwen.context_tokens = Some(65536);
        qwen.context_proof = Some(ContextProof {
            tokenizer_revision: "fixture-alias-tokenizer".into(),
            template_token_bound: 256,
            byte_level_bpe: true,
        });
        let mut canonical = request();
        canonical["model"] = json!("qwen3.8:27b-q4_K_M-ctx64k");
        ReasoningEffort::Omit.apply(&mut canonical);
        for thinking in [false, true] {
            qwen.thinking = Some(thinking);
            let wire = provider_request(Some(&qwen), &canonical).unwrap();
            assert_eq!(wire["model"], canonical["model"]);
            assert_eq!(
                wire["reasoning_effort"],
                if thinking { "medium" } else { "none" }
            );
            for unknown in [
                "qwen3.8:27b-q4_K_M-ctx32k",
                "qwen3.8:27b-q4_K_M-ctx64k-unknown",
            ] {
                assert!(qwen.check_model(unknown, ReasoningEffort::Omit).is_err());
            }
            assert!(
                qwen.check_model(canonical["model"].as_str().unwrap(), ReasoningEffort::Low)
                    .is_err()
            );
        }
        qwen.thinking = None;
        assert!(provider_request(Some(&qwen), &canonical).is_err());
    }
    #[test]
    fn incomplete_chat_diagnostics_are_utf8_bounded_and_never_final_output() {
        let body = format!("HEAD{}TAIL", "汉".repeat(5000));
        let reasoning = format!("{}END", "🚀".repeat(1000));
        let mut response = json!({"choices":[{"finish_reason":"length","message":{"content":body,"reasoning_content":reasoning}}]});
        let diagnostic = incomplete_diagnostics(ProviderProtocol::OpenaiChat, &response).unwrap();
        assert_eq!(diagnostic["diagnostic_only"], true);
        assert_eq!(diagnostic["scoring_eligible"], false);
        assert_eq!(diagnostic["output_utf8_bytes"], body.len());
        assert!(
            diagnostic["output_head"]
                .as_str()
                .unwrap()
                .starts_with("HEAD")
        );
        assert!(
            diagnostic["output_tail"]
                .as_str()
                .unwrap()
                .ends_with("TAIL")
        );
        assert!(
            diagnostic["reasoning_tail"]
                .as_str()
                .unwrap()
                .ends_with("END")
        );
        for (key, max) in [
            ("output_head", 2048),
            ("output_tail", 2048),
            ("reasoning_tail", 1024),
        ] {
            assert!(diagnostic[key].as_str().unwrap().len() <= max);
        }
        assert!(matches!(
            provider_output(ProviderProtocol::OpenaiChat, &response),
            Err(Error::Incomplete(_))
        ));
        assert!(incomplete_diagnostics(ProviderProtocol::OpenaiResponses, &response).is_none());
        response["choices"][0]["message"]
            .as_object_mut()
            .unwrap()
            .remove("reasoning_content");
        assert!(
            incomplete_diagnostics(ProviderProtocol::OpenaiChat, &response)
                .unwrap()
                .get("reasoning_tail")
                .is_none()
        );
        response["choices"][0]["finish_reason"] = json!("stop");
        assert!(incomplete_diagnostics(ProviderProtocol::OpenaiChat, &response).is_none());
        response["choices"][0]["finish_reason"] = json!("length");
        response["choices"][0]["message"]["refusal"] = json!("refused");
        assert!(incomplete_diagnostics(ProviderProtocol::OpenaiChat, &response).is_none());
    }
    #[test]
    fn output_protocols_reject_refusal_truncation_and_empty_without_reading_thoughts() {
        let responses = json!({"status":"completed","output":[{"content":[{"type":"output_text","text":"{}"}]}]});
        let anthropic = json!({"stop_reason":"end_turn","content":[{"type":"thinking","thinking":"private"},{"type":"text","text":"{}"}]});
        let chat = json!({"choices":[{"finish_reason":"stop","message":{"content":"{}","reasoning":"private"}}]});
        for (protocol, ok) in [
            (ProviderProtocol::OpenaiResponses, responses),
            (ProviderProtocol::AnthropicMessages, anthropic),
            (ProviderProtocol::OpenaiChat, chat),
        ] {
            assert_eq!(provider_output(protocol, &ok).unwrap(), "{}");
            let (truncated, refused, empty) = match protocol {
                ProviderProtocol::OpenaiResponses => (
                    json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}),
                    json!({"output":[{"content":[{"type":"refusal","refusal":"no"}]}]}),
                    json!({"output":[]}),
                ),
                ProviderProtocol::AnthropicMessages => (
                    json!({"stop_reason":"max_tokens"}),
                    json!({"stop_reason":"refusal"}),
                    json!({"stop_reason":"end_turn","content":[{"type":"thinking","thinking":"{}"}]}),
                ),
                ProviderProtocol::OpenaiChat => (
                    json!({"choices":[{"finish_reason":"length"}]}),
                    json!({"choices":[{"finish_reason":"content_filter"}]}),
                    json!({"choices":[{"finish_reason":"stop","message":{"content":"","reasoning":"{}"}}]}),
                ),
            };
            assert!(matches!(
                provider_output(protocol, &truncated),
                Err(Error::Incomplete(_))
            ));
            assert!(matches!(
                provider_output(protocol, &refused),
                Err(Error::Refusal(_))
            ));
            assert!(matches!(
                provider_output(protocol, &empty),
                Err(Error::InvalidResponse(_))
            ));
        }
    }
    #[test]
    fn original_mechanical_schema_and_usage_remain_authoritative() {
        let schema = &request()["text"]["format"]["schema"];
        check_output(schema, &json!({"count":0})).unwrap();
        for invalid in [
            json!({"count":-1}),
            json!({"count":0,"extra":true}),
            json!({}),
            json!({"count":"0"}),
        ] {
            assert!(check_output(schema, &invalid).is_err());
        }
        let a=normalized_usage(ProviderProtocol::AnthropicMessages,&json!({"usage":{"input_tokens":3,
            "cache_creation_input_tokens":4,"cache_read_input_tokens":5,"output_tokens":11,"output_tokens_details":{"thinking_tokens":9}}})).unwrap();
        assert_eq!(a["input_tokens"], 12);
        assert_eq!(a["output_tokens"], 11);
        assert_eq!(a["output_tokens_details"]["reasoning_tokens"], 9);
        assert_eq!(
            normalized_usage(
                ProviderProtocol::OpenaiChat,
                &json!({"usage":{"prompt_tokens":4,"completion_tokens":7}})
            )
            .unwrap(),
            json!({"input_tokens":4,"output_tokens":7})
        );
        assert!(
            normalized_usage(
                ProviderProtocol::AnthropicMessages,
                &json!({"usage":{"input_tokens":1}})
            )
            .is_none()
        );
        assert!(normalized_usage(ProviderProtocol::AnthropicMessages,&json!({"usage":{"input_tokens":u64::MAX,"cache_read_input_tokens":1,"output_tokens":2}})).is_none());
    }
    #[test]
    fn af163_new_models_keep_native_protocol_effort_and_usage() {
        for (model, protocol) in [
            ("gpt-6-luna", ProviderProtocol::OpenaiResponses),
            ("claude-sonnet-5-5", ProviderProtocol::AnthropicMessages),
            ("claude-haiku-5-5", ProviderProtocol::AnthropicMessages),
        ] {
            let cfg = config(protocol);
            let mut canonical = request();
            canonical["model"] = json!(model);
            ReasoningEffort::Medium.apply(&mut canonical);
            let wire = provider_request(Some(&cfg), &canonical).unwrap();
            assert_eq!(wire["model"], model);
            cfg.require_native_schema().unwrap();
            assert!(
                cfg.check_model("unknown-new-model", ReasoningEffort::Medium)
                    .is_err()
            );
            if protocol == ProviderProtocol::OpenaiResponses {
                assert_eq!(wire["instructions"], canonical["instructions"]);
                assert!(wire.get("temperature").is_none());
                let mut missing = canonical.clone();
                missing.as_object_mut().unwrap().remove("instructions");
                assert!(provider_request(Some(&cfg), &missing).is_err());
                let envelope = json!({"usage":{"input_tokens":12,"output_tokens":20,"output_tokens_details":{"reasoning_tokens":7}}});
                assert_eq!(
                    normalized_usage(protocol, &envelope),
                    Some(envelope["usage"].clone())
                );
            } else {
                assert_eq!(wire["system"], canonical["instructions"]);
                assert_eq!(wire["output_config"]["format"]["type"], "json_schema");
                assert_eq!(wire["output_config"]["effort"], "medium");
                assert!(
                    wire["system"]
                        .as_str()
                        .unwrap()
                        .contains(PROMPT_SCHEMA_INSTRUCTION)
                        == false
                );
                let usage=normalized_usage(protocol,&json!({"usage":{"input_tokens":10,"cache_read_input_tokens":2,"cache_creation_input_tokens":3,"output_tokens":20,"output_tokens_details":{"thinking_tokens":7}}})).unwrap();
                assert_eq!(usage["input_tokens"], 15);
                assert_eq!(usage["output_tokens"], 20);
                assert_eq!(usage["output_tokens_details"]["reasoning_tokens"], 7);
            }
        }
    }
    #[test]
    fn af163_incomplete_diagnostics_bound_cloud_text_without_exposing_thinking() {
        let text = "测".repeat(3000);
        for (protocol, envelope) in [
            (
                ProviderProtocol::OpenaiResponses,
                json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[
                {"type":"reasoning","summary":[{"text":"PRIVATE_THINKING"}]},
                {"type":"message","content":[{"type":"output_text","text":text}]}]}),
            ),
            (
                ProviderProtocol::AnthropicMessages,
                json!({"stop_reason":"max_tokens","content":[{"type":"thinking","thinking":"PRIVATE_THINKING"},{"type":"text","text":text}]}),
            ),
        ] {
            let fragments = incomplete_diagnostics(protocol, &envelope).unwrap();
            assert_eq!(fragments["diagnostic_only"], true);
            assert_eq!(fragments["scoring_eligible"], false);
            assert_eq!(fragments["output_utf8_bytes"], 9000);
            for key in ["output_head", "output_tail"] {
                assert!(fragments[key].as_str().unwrap().len() <= 2048);
            }
            assert!(!fragments.to_string().contains("PRIVATE_THINKING"));
            assert!(matches!(
                provider_output(protocol, &envelope),
                Err(Error::Incomplete(_))
            ));
            let mut completed = envelope.clone();
            completed["status"] = json!("completed");
            completed["stop_reason"] = json!("end_turn");
            assert!(incomplete_diagnostics(protocol, &completed).is_none());
        }
    }
    #[test]
    fn af166_qwen38_alias_uses_enable_thinking_and_budget_without_effort() {
        for thinking in [true, false] {
            let mut cfg = dashscope(thinking);
            cfg.model_revision = Some(DASHSCOPE_QWEN38_CONTEXT_REVISION.into());
            for cap in [32768, 16384] {
                let mut canonical = request();
                canonical["model"] = json!(DASHSCOPE_QWEN38_MODEL);
                canonical["max_output_tokens"] = json!(cap);
                ReasoningEffort::Omit.apply(&mut canonical);
                let wire = provider_request(Some(&cfg), &canonical).unwrap();
                assert_eq!(wire["enable_thinking"], thinking);
                assert!(wire.get("reasoning_effort").is_none());
                assert!(wire.get("max_tokens").is_none());
                assert_eq!(wire["max_completion_tokens"], cap);
                if thinking {
                    assert_eq!(wire["thinking_budget"], cap / 2);
                } else {
                    assert!(wire.get("thinking_budget").is_none());
                }
                assert_eq!(wire["response_format"]["json_schema"]["strict"], true);
            }
            assert_eq!(
                cfg.endpoint("https://ws-example.cn-beijing.maas.aliyuncs.com/compatible-mode")
                    .unwrap(),
                "https://ws-example.cn-beijing.maas.aliyuncs.com/compatible-mode/v1/chat/completions"
            );
            assert!(
                cfg.endpoint("http://dashscope.aliyuncs.com/compatible-mode")
                    .is_err()
            );
            assert!(
                cfg.check_model("qwen3.8-flash-2026-10-08", ReasoningEffort::Omit)
                    .is_err()
            );
            assert!(
                cfg.check_model(DASHSCOPE_QWEN38_MODEL, ReasoningEffort::Medium)
                    .is_err()
            );
            let mut wrong = cfg.clone();
            wrong.model_revision = Some(DASHSCOPE_CONTEXT_REVISION.into());
            assert!(
                wrong
                    .check_model(DASHSCOPE_QWEN38_MODEL, ReasoningEffort::Omit)
                    .is_err()
            );
            assert!(
                cfg.check_model(DASHSCOPE_MODEL, ReasoningEffort::Omit)
                    .is_err()
            );
        }
        let usage=normalized_usage(ProviderProtocol::OpenaiChat,&json!({"usage":{"prompt_tokens":10,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":3},"completion_tokens_details":{"reasoning_tokens":7}}})).unwrap();
        assert_eq!(
            usage,
            json!({"input_tokens":10,"input_tokens_details":{"cached_tokens":3},"output_tokens":20,"output_tokens_details":{"reasoning_tokens":7}})
        );
    }
}
