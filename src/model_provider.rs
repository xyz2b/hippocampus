//! Wire protocol capabilities. This layer checks shape and capacity, never assertion semantics.
use super::{OpenAiAdapterError as Error, ReasoningEffort};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const ADAPTER_VERSION: &str = "structured-provider.v1";
pub const MODEL_USER_AGENT: &str = "hippocampus/0.1";

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
    pub fn validate(&self) -> Result<(), Error> {
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

    pub fn check_model(&self, model: &str, effort: ReasoningEffort) -> Result<(), Error> {
        self.validate()?;
        let supported = match self.protocol {
            ProviderProtocol::OpenaiResponses if model == "gpt-6.1-sol" => {
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
                model == "claude-opus-5-5"
                    && self.thinking != Some(false)
                    && matches!(
                        effort,
                        ReasoningEffort::Low | ReasoningEffort::Medium | ReasoningEffort::High
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
        let path = if self.protocol == ProviderProtocol::AnthropicMessages && base.ends_with("/v1")
        {
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
        Ok(endpoint)
    }

    pub fn check_context(&self, request: &Value, output: u64) -> Result<(), Error> {
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
    let request = match config.protocol {
        ProviderProtocol::OpenaiResponses => {
            if canonical["instructions"]
                .as_str()
                .is_none_or(|s| s.is_empty())
            {
                return Err(Error::InvalidContext("instructions required".into()));
            }
            let mut r = canonical.clone();
            if model == "gpt-6.1-sol" {
                r.as_object_mut().unwrap().remove("temperature");
            }
            r
        }
        ProviderProtocol::AnthropicMessages => {
            json!({"model":model,"system":canonical["instructions"],
            "messages":[{"role":"user","content":canonical["input"]}],"max_tokens":tokens,
            "thinking":{"type":"adaptive"},"output_config":{"effort":effort,
                "format":{"type":"json_schema","schema":anthropic_schema(schema)?}}})
        }
        ProviderProtocol::OpenaiChat => json!({"model":model,"messages":[
            {"role":"system","content":canonical["instructions"]},{"role":"user","content":canonical["input"]}],
            "max_tokens":tokens,"temperature":0,"stream":false,
            "reasoning_effort":if config.thinking == Some(false) {"none"} else {"medium"},
            "response_format":{"type":"json_schema","json_schema":{"name":canonical["text"]["format"]["name"],"strict":true,"schema":schema}}}),
    };
    config.check_context(&request, tokens)?;
    Ok(request)
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
}
