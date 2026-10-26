//! Standalone loopback-only Ollama embedding client; not wired into production R2.
use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const DEFAULT_BATCH: usize = 16;
pub const DIMENSIONS: usize = 1024;
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub const SEGMENT_VERSION: &str = "lexical-768chars-128overlap-v1";
pub const QUERY_INSTRUCTION: &str =
    "Given a personal memory query, retrieve relevant passages that answer the query";

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub model: Model,
    pub base_url: String,
    pub batch: usize,
    /// One total network deadline, including tags, show and digest verification.
    pub query_timeout_ms: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            model: Model::Qwen,
            base_url: "http://127.0.0.1:11434".into(),
            batch: DEFAULT_BATCH,
            query_timeout_ms: 10_000,
        }
    }
}
impl Config {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
    pub fn validate(&self) -> Result<()> {
        endpoint(&self.base_url)?;
        if !(1..=DEFAULT_BATCH).contains(&self.batch)
            || !(1..=360_000).contains(&self.query_timeout_ms)
        {
            return Err("embedding_config_bounds".into());
        }
        Ok(())
    }
}

/// Runtime permission is stricter than the separately authorized AF175 experiment.
pub fn runtime_network_allowed() -> bool {
    runtime_policy(&std::env::var("HIPPOCAMPUS_MODEL_NETWORK").unwrap_or_default())
}
fn runtime_policy(mode: &str) -> bool {
    mode == "live"
}

pub(crate) struct RequestControl {
    deadline: Instant,
    stopped: Arc<AtomicBool>,
}
impl RequestControl {
    pub(crate) fn new(timeout_ms: u64, stopped: Arc<AtomicBool>) -> Self {
        Self {
            deadline: Instant::now() + Duration::from_millis(timeout_ms),
            stopped,
        }
    }
    pub(crate) fn check(&self) -> Result<Duration> {
        if self.stopped.load(Ordering::SeqCst) {
            return Err("embedding_cancelled".into());
        }
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| "embedding_deadline_exhausted".into())
    }
}

pub(crate) trait Provider: Send + Sync {
    fn resolve(&self, model: Model, control: &RequestControl) -> Result<Binding>;
    fn embed(
        &self,
        binding: &Binding,
        inputs: &[String],
        control: &RequestControl,
    ) -> Result<Batch>;
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Model {
    #[serde(rename = "qwen3-embedding:0.6b")]
    Qwen,
    #[serde(rename = "bge-m3")]
    Bge,
}
impl Model {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Qwen => "qwen3-embedding:0.6b",
            Self::Bge => "bge-m3",
        }
    }
    fn matches(self, tag: &str) -> bool {
        tag == self.tag() || (self == Self::Bge && tag == "bge-m3:latest")
    }
    pub fn query(self, text: &str) -> String {
        match self {
            Self::Qwen => format!("Instruct: {QUERY_INSTRUCTION}\nQuery:{text}"),
            Self::Bge => text.into(),
        }
    }
}

/// Reuse the actual lexical splitter without exporting/changing the lexical module.
pub fn segments(text: &str) -> Vec<(usize, usize)> {
    crate::lexical::segments(text)
}

pub fn endpoint(base: &str) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(base).map_err(|_| "embedding_endpoint_invalid")?;
    if !matches!(url.scheme(), "http" | "https")
        || !matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err("embedding_endpoint_not_loopback_root".into());
    }
    url.set_path("/");
    Ok(url)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub model: Model,
    pub digest: String,
    pub dimensions: usize,
    pub query_format: String,
    pub document_format: String,
    pub segment_version: String,
}
impl Binding {
    pub fn from_tags(model: Model, tags: &Value) -> Result<Self> {
        let entries: Vec<_> = tags["models"]
            .as_array()
            .ok_or("embedding_tags_shape")?
            .iter()
            .filter(|m| m["name"].as_str().is_some_and(|s| model.matches(s)))
            .collect();
        if entries.len() != 1 {
            return Err("embedding_model_missing_or_ambiguous".into());
        }
        let entry = entries[0];
        if entry.get("remote_host").is_some_and(|v| !v.is_null())
            || entry.get("remote_model").is_some_and(|v| !v.is_null())
        {
            return Err("embedding_cloud_model_forbidden".into());
        }
        let digest = entry["digest"].as_str().ok_or("embedding_digest_missing")?;
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("embedding_digest_invalid".into());
        }
        Ok(Self {
            model,
            digest: digest.to_ascii_lowercase(),
            dimensions: DIMENSIONS,
            query_format: if model == Model::Qwen {
                format!("Instruct: {QUERY_INSTRUCTION}\nQuery:{{query}}")
            } else {
                "{query}".into()
            },
            document_format: "{source_body_segment}".into(),
            segment_version: SEGMENT_VERSION.into(),
        })
    }
    pub fn check_show(&self, show: &Value) -> Result<()> {
        if show.get("remote_host").is_some_and(|v| !v.is_null())
            || show.get("remote_model").is_some_and(|v| !v.is_null())
        {
            return Err("embedding_cloud_model_forbidden".into());
        }
        let info = show["model_info"]
            .as_object()
            .ok_or("embedding_model_info_missing")?;
        let dimensions: Vec<_> = info
            .iter()
            .filter(|(key, _)| key.ends_with(".embedding_length"))
            .map(|(_, value)| value.as_u64())
            .collect();
        if dimensions != [Some(self.dimensions as u64)] {
            return Err("embedding_show_dimension".into());
        }
        if !show["capabilities"]
            .as_array()
            .is_some_and(|caps| caps.iter().any(|c| c == "embedding"))
        {
            return Err("embedding_capability_missing".into());
        }
        Ok(())
    }
    pub fn request(&self, inputs: &[String]) -> Result<Value> {
        if self.dimensions != DIMENSIONS
            || inputs.is_empty()
            || inputs.len() > DEFAULT_BATCH
            || inputs
                .iter()
                .any(|s| s.trim().is_empty() || s.len() > 16 * 1024)
        {
            return Err("embedding_input_bounds".into());
        }
        Ok(json!({"model":self.model.tag(),"input":inputs,"truncate":false,"keep_alive":"5m"}))
    }
    pub fn fingerprint(&self, inputs: &[String]) -> Result<String> {
        Ok(crate::openai::recall_models::fingerprint(
            &json!({"binding":self,"request":self.request(inputs)?}),
        ))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    pub vectors: Vec<Vec<f32>>,
    pub input_tokens: Option<u64>,
    pub total_duration_ns: Option<u64>,
    pub load_duration_ns: Option<u64>,
    pub elapsed_ms: f64,
}

pub fn normalize(vector: &[f64], dimensions: usize) -> Result<Vec<f32>> {
    if vector.len() != dimensions || vector.iter().any(|v| !v.is_finite()) {
        return Err("embedding_dimension_or_nonfinite".into());
    }
    let norm = vector.iter().map(|v| v * v).sum::<f64>().sqrt();
    if !norm.is_finite() || norm == 0.0 {
        return Err("embedding_zero_or_invalid_norm".into());
    }
    let result: Vec<_> = vector.iter().map(|v| (v / norm) as f32).collect();
    check_normalized(&result, dimensions)?;
    Ok(result)
}
pub fn check_normalized(vector: &[f32], dimensions: usize) -> Result<()> {
    let norm = vector.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
    if vector.len() != dimensions
        || vector.iter().any(|v| !v.is_finite())
        || (norm - 1.0).abs() > 0.0001
    {
        return Err("embedding_cached_vector_invalid".into());
    }
    Ok(())
}
pub fn bounded_json(reader: impl Read, max_bytes: usize) -> Result<Value> {
    let mut bytes = Vec::new();
    reader
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "embedding_response_read")?;
    if bytes.len() > max_bytes {
        return Err("embedding_response_too_large".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "embedding_response_json".into())
}
pub fn decode_response(
    binding: &Binding,
    count: usize,
    response: &Value,
    elapsed_ms: f64,
) -> Result<Batch> {
    if !elapsed_ms.is_finite() || elapsed_ms < 0.0 {
        return Err("embedding_elapsed_invalid".into());
    }
    if response["truncated"] == true
        || response["input_truncated"] == true
        || response.get("error").is_some()
    {
        return Err("embedding_truncated_or_error".into());
    }
    if !response["model"]
        .as_str()
        .is_some_and(|s| binding.model.matches(s))
    {
        return Err("embedding_response_model".into());
    }
    let arrays = response["embeddings"]
        .as_array()
        .ok_or("embedding_response_batch")?;
    if count == 0 || arrays.len() != count {
        return Err("embedding_response_batch".into());
    }
    let vectors = arrays
        .iter()
        .map(|row| {
            let values: Vec<f64> = row
                .as_array()
                .ok_or("embedding_response_vector")?
                .iter()
                .map(|v| v.as_f64().ok_or("embedding_non_numeric"))
                .collect::<std::result::Result<_, _>>()?;
            normalize(&values, binding.dimensions)
        })
        .collect::<Result<_>>()?;
    let number = |field: &str| -> Result<Option<u64>> {
        match response.get(field) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_u64()
                .map(Some)
                .ok_or_else(|| "embedding_usage_invalid".into()),
        }
    };
    Ok(Batch {
        vectors,
        input_tokens: number("prompt_eval_count")?,
        total_duration_ns: number("total_duration")?,
        load_duration_ns: number("load_duration")?,
        elapsed_ms,
    })
}

fn network_policy(mode: &str) -> Result<()> {
    match mode {
        "offline" | "live" => Ok(()), // endpoint() still limits both modes to loopback
        "deny" => Err("embedding_network_disabled".into()),
        _ => Err("embedding_network_policy_invalid".into()),
    }
}

pub struct Ollama {
    base: reqwest::Url,
    client: Client,
    runtime: bool,
    #[cfg(test)]
    transport: Option<Arc<dyn Fn(&str, Option<&Value>, Duration) -> Result<Value> + Send + Sync>>,
}
impl Ollama {
    pub fn new(base: &str) -> Result<Self> {
        let base = endpoint(base)?;
        network_policy(
            &std::env::var("HIPPOCAMPUS_MODEL_NETWORK").unwrap_or_else(|_| "offline".into()),
        )?;
        let host = base.host_str().unwrap();
        let address =
            SocketAddr::from((Ipv4Addr::LOCALHOST, base.port_or_known_default().unwrap()));
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .resolve(host, address)
            .timeout(Duration::from_secs(360))
            .user_agent("hippocampus-local-embedding/af175")
            .build()
            .map_err(|_| "embedding_client")?;
        Ok(Self {
            base,
            client,
            runtime: false,
            #[cfg(test)]
            transport: None,
        })
    }
    pub(crate) fn runtime(base: &str) -> Result<Self> {
        let mut client = Self::new(base)?;
        client.runtime = true;
        Ok(client)
    }
    fn call(&self, path: &str, body: Option<&Value>, control: &RequestControl) -> Result<Value> {
        let remaining = control.check()?;
        if self.runtime && !runtime_network_allowed() {
            return Err("embedding_network_disabled".into());
        }
        network_policy(
            &std::env::var("HIPPOCAMPUS_MODEL_NETWORK").unwrap_or_else(|_| "offline".into()),
        )?;
        #[cfg(test)]
        if let Some(transport) = &self.transport {
            let result = transport(path, body, remaining)?;
            control.check()?;
            return Ok(result);
        }
        let url = self.base.join(path).map_err(|_| "embedding_url")?;
        let request = match body {
            Some(body) => self.client.post(url).json(body),
            None => self.client.get(url),
        };
        let response = request
            .timeout(remaining)
            .send()
            .map_err(|_| "embedding_transport")?;
        if !response.status().is_success() {
            return Err(format!("embedding_http_{}", response.status().as_u16()));
        }
        let result = bounded_json(
            response,
            if path == "api/embed" {
                MAX_RESPONSE_BYTES
            } else {
                1024 * 1024
            },
        )?;
        control.check()?;
        Ok(result)
    }
    pub fn resolve(&self, model: Model) -> Result<Binding> {
        Provider::resolve(
            self,
            model,
            &RequestControl::new(360_000, Arc::new(AtomicBool::new(false))),
        )
    }
    fn verify_digest(&self, binding: &Binding, control: &RequestControl) -> Result<()> {
        let current = Binding::from_tags(binding.model, &self.call("api/tags", None, control)?)?;
        if &current != binding {
            return Err("embedding_weights_changed".into());
        }
        Ok(())
    }
    pub fn embed(&self, binding: &Binding, inputs: &[String]) -> Result<Batch> {
        Provider::embed(
            self,
            binding,
            inputs,
            &RequestControl::new(360_000, Arc::new(AtomicBool::new(false))),
        )
    }
}
impl Provider for Ollama {
    fn resolve(&self, model: Model, control: &RequestControl) -> Result<Binding> {
        let binding = Binding::from_tags(model, &self.call("api/tags", None, control)?)?;
        binding.check_show(&self.call(
            "api/show",
            Some(&json!({"model":model.tag()})),
            control,
        )?)?;
        Ok(binding)
    }
    fn embed(
        &self,
        binding: &Binding,
        inputs: &[String],
        control: &RequestControl,
    ) -> Result<Batch> {
        let request = binding.request(inputs)?;
        let start = Instant::now();
        self.verify_digest(binding, control)?;
        let response = self.call("api/embed", Some(&request), control)?;
        self.verify_digest(binding, control)?;
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        decode_response(binding, inputs.len(), &response, elapsed_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> Binding {
        Binding::from_tags(
            Model::Qwen,
            &json!({"models":[{"name":Model::Qwen.tag(),"digest":"a".repeat(64)}]}),
        )
        .unwrap()
    }
    #[test]
    fn runtime_config_defaults_bounds_and_strict_gate() {
        let c = Config::default();
        assert!(!c.enabled);
        assert_eq!(c.batch, 16);
        assert_eq!(c.model, Model::Qwen);
        c.validate().unwrap();
        for mode in ["", "offline", "deny", "unexpected"] {
            assert!(!runtime_policy(mode));
        }
        assert!(runtime_policy("live"));
        for config in [
            Config {
                batch: 0,
                ..c.clone()
            },
            Config {
                batch: 17,
                ..c.clone()
            },
            Config {
                query_timeout_ms: 0,
                ..c.clone()
            },
            Config {
                query_timeout_ms: 360_001,
                ..c.clone()
            },
            Config {
                base_url: "http://cloud.example".into(),
                ..c
            },
        ] {
            assert!(config.validate().is_err());
        }
        assert!(serde_json::from_value::<Config>(json!({"enabled":true,"fallback":true})).is_err());
    }
    #[test]
    fn shared_deadline_includes_metadata_and_digest_requests_and_stops_next_request() {
        let mut client = Ollama::new("http://127.0.0.1:11434").unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        client.transport = Some(Arc::new(move |path, _, remaining| {
            observed.fetch_add(1, Ordering::SeqCst);
            assert!(remaining <= Duration::from_millis(30));
            if path == "api/tags" {
                Ok(json!({"models":[{"name":Model::Qwen.tag(),"digest":"a".repeat(64)}]}))
            } else {
                std::thread::sleep(Duration::from_millis(35));
                Ok(
                    json!({"capabilities":["embedding"],"model_info":{"qwen.embedding_length":DIMENSIONS}}),
                )
            }
        }));
        let control = RequestControl::new(30, Arc::new(AtomicBool::new(false)));
        assert_eq!(
            Provider::resolve(&client, Model::Qwen, &control).unwrap_err(),
            "embedding_deadline_exhausted"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            Provider::embed(&client, &binding(), &["正文".into()], &control).unwrap_err(),
            "embedding_deadline_exhausted"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let stop = Arc::new(AtomicBool::new(true));
        assert_eq!(
            Provider::resolve(&client, Model::Qwen, &RequestControl::new(100, stop)).unwrap_err(),
            "embedding_cancelled"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn loopback_rejects_cloud_forwarding_credentials_and_host_tricks() {
        assert!(network_policy("deny").is_err());
        assert!(network_policy("unknown").is_err());
        assert!(network_policy("offline").is_ok());
        assert!(network_policy("live").is_ok());
        for url in ["http://127.0.0.1:11434", "http://localhost:11434/"] {
            assert!(endpoint(url).is_ok());
        }
        for url in [
            "https://ollama.com",
            "http://localhost.evil.com",
            "http://127.0.0.2",
            "http://[::1]",
            "http://u:p@localhost",
            "http://localhost/proxy",
            "http://localhost/?target=cloud",
            "http://localhost/#x",
        ] {
            assert!(endpoint(url).is_err(), "{url}");
        }
    }
    #[test]
    fn formats_truncation_and_weight_binding_are_fixed() {
        let b = binding();
        let inputs = vec![b.model.query("虚构的查询")];
        assert!(inputs[0].starts_with("Instruct: "));
        assert_eq!(Model::Bge.query("虚构的查询"), "虚构的查询");
        assert_eq!(b.request(&inputs).unwrap()["truncate"], false);
        let fp = b.fingerprint(&inputs).unwrap();
        let mut changed = b.clone();
        changed.digest = "b".repeat(64);
        assert_ne!(fp, changed.fingerprint(&inputs).unwrap());
        changed = b.clone();
        changed.query_format.push('x');
        assert_ne!(fp, changed.fingerprint(&inputs).unwrap());
        changed = b.clone();
        changed.document_format.push('x');
        assert_ne!(fp, changed.fingerprint(&inputs).unwrap());
        changed = b.clone();
        changed.segment_version.push('x');
        assert_ne!(fp, changed.fingerprint(&inputs).unwrap());
        assert_ne!(fp, b.fingerprint(&["另一个文本".into()]).unwrap());
        changed = b.clone();
        changed.dimensions = 128;
        assert!(changed.request(&inputs).is_err());
    }
    #[test]
    fn fake_response_rejects_truncation_dimension_count_nonfinite_and_zero() {
        let mut b = binding();
        b.dimensions = 2;
        let good = json!({"model":b.model.tag(),"embeddings":[[3.0,4.0]],"prompt_eval_count":7});
        let result = decode_response(&b, 1, &good, 1.0).unwrap();
        assert_eq!(result.vectors[0], [0.6, 0.8]);
        assert_eq!(result.input_tokens, Some(7));
        for key in ["truncated", "input_truncated"] {
            let mut bad = good.clone();
            bad[key] = json!(true);
            assert!(decode_response(&b, 1, &bad, 0.0).is_err());
        }
        assert!(decode_response(&b, 2, &good, 0.0).is_err());
        assert!(decode_response(&b, 1, &good, f64::NAN).is_err());
        assert!(decode_response(&b, 1, &good, -1.0).is_err());
        for vector in [json!([0, 0]), json!([1]), json!([1, "NaN"])] {
            let mut bad = good.clone();
            bad["embeddings"] = json!([vector]);
            assert!(decode_response(&b, 1, &bad, 0.0).is_err());
        }
        assert!(normalize(&[f64::NAN, 1.0], 2).is_err());
        assert!(normalize(&[f64::INFINITY, 1.0], 2).is_err());
        assert!(bounded_json(&b"{\"embeddings\":[NaN]}"[..], 100).is_err());
        assert!(bounded_json(&b"{\"x\":123}"[..], 2).is_err());
    }
    #[test]
    fn metadata_refuses_missing_remote_and_wrong_dimension_models() {
        assert!(Binding::from_tags(Model::Bge, &json!({"models":[]})).is_err());
        assert!(Binding::from_tags(Model::Qwen,&json!({"models":[{"name":Model::Qwen.tag(),"digest":"a".repeat(64),"remote_host":"cloud"}]})).is_err());
        let b = binding();
        assert!(
            b.check_show(
                &json!({"capabilities":["embedding"],"model_info":{"qwen3.embedding_length":1024}})
            )
            .is_ok()
        );
        assert!(
            b.check_show(
                &json!({"capabilities":["embedding"],"model_info":{"qwen3.embedding_length":128}})
            )
            .is_err()
        );
    }
    #[test]
    fn splitter_is_utf8_safe_with_real_lexical_overlap() {
        let text = "汉字".repeat(900);
        let ranges = segments(&text);
        assert_eq!(ranges[0], (0, 768 * 3));
        assert_eq!(ranges[1].0, (768 - 128) * 3);
        for (start, end) in ranges {
            assert!(text.get(start..end).is_some());
        }
    }
}

#[derive(Clone, Debug)]
pub struct QueryVector {
    pub binding: Binding,
    pub vector: Vec<f32>,
}
impl QueryVector {
    pub(crate) fn check(&self, model: Model) -> Result<()> {
        if self.binding.model != model {
            return Err("embedding_query_model".into());
        }
        crate::embedding_index::Binding::new(self.binding.clone())
            .map_err(|_| "embedding_query_binding")?;
        check_normalized(&self.vector, self.binding.dimensions)
    }
}
/// One query embedding, with a single total deadline for discovery and digest checks.
pub fn query(config: &Config, text: &str) -> Result<QueryVector> {
    config.validate()?;
    if !config.enabled || !runtime_network_allowed() {
        return Err("embedding_network_disabled".into());
    }
    let control = RequestControl::new(config.query_timeout_ms, Arc::new(AtomicBool::new(false)));
    let provider = Ollama::runtime(&config.base_url)?;
    let binding = Provider::resolve(&provider, config.model, &control)?;
    let batch = Provider::embed(&provider, &binding, &[config.model.query(text)], &control)?;
    let vector = batch
        .vectors
        .into_iter()
        .next()
        .ok_or("embedding_batch_count")?;
    let result = QueryVector { binding, vector };
    result.check(config.model)?;
    Ok(result)
}
