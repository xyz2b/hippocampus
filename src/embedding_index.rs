//! Derived source vectors. Cache transactions never save the business snapshot.
use std::collections::BTreeMap;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{PersistenceResult, SourceRecord, SqliteMemoryStore, UserId, embedding};

pub const NORMALIZATION_VERSION: &str = "unit-l2-f64-to-f32-v1";
pub const BLOB_VERSION: &str = "f32-le-v1";

/// Keep cache encoding/versioning out of the existing experimental request fingerprints.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub embedding: embedding::Binding,
    pub normalization_version: String,
    pub blob_version: String,
}

impl Binding {
    pub fn new(embedding: embedding::Binding) -> PersistenceResult<Self> {
        let binding = Self {
            embedding,
            normalization_version: NORMALIZATION_VERSION.into(),
            blob_version: BLOB_VERSION.into(),
        };
        binding.check()?;
        Ok(binding)
    }

    fn check(&self) -> PersistenceResult<()> {
        let canonical = embedding::Binding::from_tags(
            self.embedding.model,
            &serde_json::json!({"models":[{
                "name": self.embedding.model.tag(), "digest": self.embedding.digest
            }]}),
        )?;
        if self.embedding != canonical
            || self.normalization_version != NORMALIZATION_VERSION
            || self.blob_version != BLOB_VERSION
        {
            return Err("embedding_index_binding_invalid".into());
        }
        Ok(())
    }

    pub fn fingerprint(&self) -> PersistenceResult<String> {
        self.check()?;
        Ok(crate::openai::recall_models::fingerprint(
            &serde_json::to_value(self)?,
        ))
    }
}

/// A persisted binding generation rejects late writes, including switch-away-and-back.
#[derive(Clone, Debug)]
pub struct BindingToken {
    owner: UserId,
    binding: Binding,
    generation: i64,
    lease: Option<Lease>,
}

impl BindingToken {
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub index: usize,
    pub start: usize,
    pub end: usize,
    pub content_hash: String,
    pub text: String,
}

/// Captured under a read transaction; rechecked under the write lock after embedding.
#[derive(Clone, Debug)]
pub struct SourceInput {
    token: BindingToken,
    pub source_id: String,
    pub content_hash: String,
    pub segments: Vec<Segment>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CachedSegment {
    pub index: usize,
    pub start: usize,
    pub end: usize,
    pub vector: Vec<f32>,
}

pub(crate) fn create_schema(db: &Connection) -> PersistenceResult<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS source_vector_bindings (
            owner_id TEXT PRIMARY KEY,
            binding TEXT NOT NULL,
            generation INTEGER NOT NULL CHECK(generation > 0)
        );
        CREATE TABLE IF NOT EXISTS source_vectors (
            owner_id TEXT NOT NULL,
            source_id TEXT NOT NULL,
            binding_key TEXT NOT NULL,
            binding TEXT NOT NULL,
            segment_index INTEGER NOT NULL CHECK(segment_index >= 0),
            start_byte INTEGER NOT NULL CHECK(start_byte >= 0),
            end_byte INTEGER NOT NULL CHECK(end_byte > start_byte),
            source_content_hash TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            dims INTEGER NOT NULL CHECK(dims > 0),
            blob_version TEXT NOT NULL,
            vector BLOB NOT NULL,
            PRIMARY KEY(owner_id, source_id, binding_key, segment_index)
        );",
    )?;
    Ok(())
}

fn body(source: &SourceRecord) -> Option<&str> {
    (source.enrichment != "deleted")
        .then(|| crate::lexical::body(source))
        .flatten()
}

/// Run for every business transaction, even when lexical indexing is disabled.
pub(crate) fn synchronize(
    db: &Connection,
    owner: &UserId,
    before: &BTreeMap<String, SourceRecord>,
    after: &BTreeMap<String, SourceRecord>,
) -> PersistenceResult<()> {
    let current: BTreeMap<&str, Vec<&SourceRecord>> =
        after.values().fold(BTreeMap::new(), |mut map, source| {
            map.entry(source.id.as_str()).or_default().push(source);
            map
        });
    for old in before.values() {
        let unchanged = current.get(old.id.as_str()).is_some_and(|sources| {
            sources.len() == 1 && body(old).is_some() && body(old) == body(sources[0])
        });
        if !unchanged {
            db.execute(
                "DELETE FROM source_vectors WHERE owner_id=?1 AND source_id=?2",
                params![owner.0, old.id],
            )?;
        }
    }
    Ok(())
}

fn content_hash(text: &str) -> String {
    let hash = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    hash.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn check_token(db: &Connection, owner: &UserId, token: &BindingToken) -> PersistenceResult<()> {
    token.binding.check()?;
    if owner != &token.owner {
        return Err("embedding_index_owner_mismatch".into());
    }
    if let Some(lease) = &token.lease {
        check_lease(db, owner, lease)?;
    }
    let stored: Option<(String, i64)> = db
        .query_row(
            "SELECT binding, generation FROM source_vector_bindings WHERE owner_id=?1",
            [&owner.0],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if stored != Some((serde_json::to_string(&token.binding)?, token.generation)) {
        return Err("embedding_index_binding_changed".into());
    }
    Ok(())
}

fn source_input(
    db: &Connection,
    owner: &UserId,
    token: &BindingToken,
    source_id: &str,
) -> PersistenceResult<SourceInput> {
    check_token(db, owner, token)?;
    let state = SqliteMemoryStore::load(db, owner)?;
    let mut sources = state.sources.values().filter(|s| s.id == source_id);
    let source = sources.next().ok_or("embedding_index_source_unavailable")?;
    if sources.next().is_some() {
        return Err("embedding_index_source_ambiguous".into());
    }
    source_of(token, source)
}

fn source_of(token: &BindingToken, source: &SourceRecord) -> PersistenceResult<SourceInput> {
    let text = body(source)
        .filter(|text| !text.trim().is_empty())
        .ok_or("embedding_index_source_unavailable")?;
    Ok(SourceInput {
        token: token.clone(),
        source_id: source.id.clone(),
        content_hash: content_hash(text),
        segments: embedding::segments(text)
            .into_iter()
            .enumerate()
            .map(|(index, (start, end))| Segment {
                index,
                start,
                end,
                content_hash: content_hash(&text[start..end]),
                text: text[start..end].into(),
            })
            .collect(),
    })
}

fn load_binding(db: &Connection, owner: &UserId) -> PersistenceResult<Option<BindingToken>> {
    let stored: Option<(String, i64)> = db
        .query_row(
            "SELECT binding, generation FROM source_vector_bindings WHERE owner_id=?1",
            [&owner.0],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((json, generation)) = stored else {
        return Ok(None);
    };
    let binding: Binding = serde_json::from_str(&json)?;
    binding.check()?;
    if generation <= 0 {
        return Err("embedding_index_generation_invalid".into());
    }
    Ok(Some(BindingToken {
        owner: owner.clone(),
        binding,
        generation,
        lease: None,
    }))
}

impl SqliteMemoryStore {
    /// Read the persisted binding after reopen without activating or rewriting anything.
    pub fn embedding_binding(&self) -> PersistenceResult<Option<BindingToken>> {
        load_binding(&self.connection, &self.owner)
    }

    /// Select the current cache namespace without changing revisions, receipts or W4 tasks.
    pub fn set_embedding_binding(&mut self, binding: Binding) -> PersistenceResult<BindingToken> {
        binding.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let token = set_binding(&tx, &self.owner, binding, None)?;
        tx.commit()?;
        Ok(token)
    }

    pub fn embedding_source_input(
        &self,
        token: &BindingToken,
        source_id: &str,
    ) -> PersistenceResult<SourceInput> {
        let tx = self.connection.unchecked_transaction()?;
        let input = source_input(&tx, &self.owner, token, source_id)?;
        tx.commit()?;
        Ok(input)
    }

    /// Atomic source write: no HTTP or business-snapshot save occurs under this lock.
    pub fn write_embedding_vectors(
        &mut self,
        input: &SourceInput,
        vectors: &[Vec<f32>],
    ) -> PersistenceResult<()> {
        let indices: Vec<_> = (0..input.segments.len()).collect();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        write_vectors(&tx, &self.owner, input, &indices, vectors, false)?;
        tx.commit()?;
        Ok(())
    }

    /// Partial coverage is allowed; a malformed row is an error, never a usable vector.
    pub fn read_embedding_vectors(
        &self,
        token: &BindingToken,
        source_id: &str,
    ) -> PersistenceResult<Vec<CachedSegment>> {
        let tx = self.connection.unchecked_transaction()?;
        let input = source_input(&tx, &self.owner, token, source_id)?;
        let result = read_vectors(&tx, &self.owner, token, &input)?;
        tx.commit()?;
        Ok(result)
    }
}

// At most twelve batch attempts per minute per owner, across workers/processes.
pub const MIN_BATCH_INTERVAL_MS: i64 = 5_000;
pub const FAILURE_BACKOFF_MS: i64 = 30_000;
pub const MAX_FAILURE_BACKOFF_MS: i64 = 300_000;
const IDLE_INTERVAL_MS: i64 = 60_000;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Stats {
    /// Invocations reserved before the provider runs; an interrupted invocation stays unfinished.
    pub metadata_calls: u64,
    pub embedding_calls: u64,
    pub completed_calls: u64,
    pub failed_calls: u64,
    pub discarded_results: u64,
    pub elapsed_ms: u64,
    pub input_tokens: u64,
    pub usage_reported_calls: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Control {
    epoch: u64,
    holder: Option<String>,
    lease_until_ms: i64,
    next_attempt_ms: i64,
    cursor: String,
    metadata_failures: u32,
    verify_after_ms: i64,
    failures: BTreeMap<String, Failure>,
    stats: Stats,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Failure {
    content_hash: String,
    binding_key: String,
    attempts: u32,
    retry_at_ms: i64,
}

type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;
#[derive(Clone)]
struct Lease {
    owner: UserId,
    holder: String,
    epoch: u64,
    clock: Clock,
}
impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("owner", &self.owner)
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn load_control(db: &Connection, owner: &UserId) -> PersistenceResult<Control> {
    let json: Option<String> = db
        .query_row(
            "SELECT state_json FROM source_embedding_control WHERE owner_id=?1",
            [&owner.0],
            |r| r.get(0),
        )
        .optional()?;
    json.map(|json| serde_json::from_str(&json).map_err(Into::into))
        .unwrap_or_else(|| Ok(Control::default()))
}
fn save_control(db: &Connection, owner: &UserId, control: &Control) -> PersistenceResult<()> {
    db.execute("INSERT INTO source_embedding_control VALUES (?1, ?2) ON CONFLICT(owner_id) DO UPDATE SET state_json=excluded.state_json", params![owner.0, serde_json::to_string(control)?])?;
    Ok(())
}
fn check_lease(db: &Connection, owner: &UserId, lease: &Lease) -> PersistenceResult<Control> {
    let control = load_control(db, owner)?;
    if owner != &lease.owner
        || control.holder.as_deref() != Some(&lease.holder)
        || control.epoch != lease.epoch
        || control.lease_until_ms <= (lease.clock)()
    {
        return Err("embedding_lease_lost".into());
    }
    Ok(control)
}
fn backoff(attempts: u32) -> i64 {
    FAILURE_BACKOFF_MS
        .saturating_mul(1_i64 << attempts.saturating_sub(1).min(4))
        .min(MAX_FAILURE_BACKOFF_MS)
}

impl SqliteMemoryStore {
    /// Optional derived control/metrics, initialized without changing the DB version or profile.
    fn ensure_embedding_control(&self) -> PersistenceResult<()> {
        self.connection.execute_batch("CREATE TABLE IF NOT EXISTS source_embedding_control (owner_id TEXT PRIMARY KEY, state_json TEXT NOT NULL);")?;
        Ok(())
    }
    pub fn embedding_stats(&self) -> PersistenceResult<Stats> {
        let exists: bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='source_embedding_control')", [], |r| r.get(0))?;
        Ok(if exists {
            load_control(&self.connection, &self.owner)?.stats
        } else {
            Stats::default()
        })
    }
    fn claim_embedding(
        &mut self,
        timeout: u64,
        clock: Clock,
    ) -> PersistenceResult<Result<Lease, Step>> {
        self.ensure_embedding_control()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut control = load_control(&tx, &self.owner)?;
        let now = clock();
        let rejected = if control.holder.is_some() && control.lease_until_ms > now {
            Some(Step::at("busy", control.lease_until_ms))
        } else if control.next_attempt_ms > now {
            Some(Step::at("backoff", control.next_attempt_ms))
        } else {
            None
        };
        if let Some(step) = rejected {
            return Ok(Err(step));
        }
        control.epoch = control
            .epoch
            .checked_add(1)
            .ok_or("embedding_epoch_overflow")?;
        let lease = Lease {
            owner: self.owner.clone(),
            holder: crate::diagnostics::new_id(),
            epoch: control.epoch,
            clock,
        };
        control.holder = Some(lease.holder.clone());
        control.lease_until_ms = now
            .checked_add(i64::try_from(timeout)?)
            .and_then(|n| n.checked_add(MIN_BATCH_INTERVAL_MS))
            .ok_or("embedding_deadline_overflow")?;
        control.next_attempt_ms = now
            .checked_add(MIN_BATCH_INTERVAL_MS)
            .ok_or("embedding_deadline_overflow")?;
        save_control(&tx, &self.owner, &control)?;
        tx.commit()?;
        Ok(Ok(lease))
    }
    fn begin_embedding_call(&mut self, lease: &Lease, metadata: bool) -> PersistenceResult<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut control = check_lease(&tx, &self.owner, lease)?;
        let calls = if metadata {
            &mut control.stats.metadata_calls
        } else {
            &mut control.stats.embedding_calls
        };
        *calls = calls.checked_add(1).ok_or("embedding_counter_overflow")?;
        save_control(&tx, &self.owner, &control)?;
        tx.commit()?;
        Ok(())
    }
    fn end_embedding_call(
        &mut self,
        failed: bool,
        elapsed_ms: u64,
        tokens: Option<u64>,
    ) -> PersistenceResult<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Completion metrics are additive, even after lease loss; never restore an old holder/cursor.
        let mut control = load_control(&tx, &self.owner)?;
        control.stats.completed_calls = control.stats.completed_calls.saturating_add(1);
        control.stats.failed_calls = control.stats.failed_calls.saturating_add(u64::from(failed));
        control.stats.elapsed_ms = control.stats.elapsed_ms.saturating_add(elapsed_ms);
        if let Some(tokens) = tokens {
            control.stats.input_tokens = control.stats.input_tokens.saturating_add(tokens);
            control.stats.usage_reported_calls =
                control.stats.usage_reported_calls.saturating_add(1);
        }
        save_control(&tx, &self.owner, &control)?;
        tx.commit()?;
        Ok(())
    }
    fn leased_binding(
        &mut self,
        binding: Binding,
        lease: &Lease,
    ) -> PersistenceResult<BindingToken> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let token = set_binding(&tx, &self.owner, binding, Some(lease.clone()))?;
        tx.commit()?;
        Ok(token)
    }
}

struct Job {
    input: SourceInput,
    indices: Vec<usize>,
    reset: bool,
}

fn next_job(
    db: &Connection,
    owner: &UserId,
    token: &BindingToken,
    batch: usize,
) -> PersistenceResult<Option<Job>> {
    check_token(db, owner, token)?;
    let control = load_control(db, owner)?;
    let state = SqliteMemoryStore::load(db, owner)?;
    let now = token
        .lease
        .as_ref()
        .map(|l| (l.clock)())
        .unwrap_or_else(now_ms);
    let key = token.binding.fingerprint()?;
    let mut sources: Vec<_> = state
        .sources
        .values()
        .filter(|s| body(s).is_some_and(|s| !s.trim().is_empty()))
        .collect();
    sources.sort_by(|a, b| a.id.cmp(&b.id));
    if sources.windows(2).any(|s| s[0].id == s[1].id) {
        return Err("embedding_index_source_ambiguous".into());
    }
    let offset = sources.partition_point(|s| s.id <= control.cursor);
    for source in sources[offset..].iter().chain(&sources[..offset]) {
        let input = source_of(token, source)?;
        if control.failures.get(&source.id).is_some_and(|f| {
            f.content_hash == input.content_hash && f.binding_key == key && f.retry_at_ms > now
        }) {
            continue;
        }
        let (cached, reset) = match read_vectors(db, owner, token, &input) {
            Ok(cached) => (cached, false),
            Err(e)
                if matches!(
                    e.to_string().as_str(),
                    "embedding_index_segment_invalid"
                        | "embedding_index_row_invalid"
                        | "embedding_cached_vector_invalid"
                ) =>
            {
                (Vec::new(), true)
            }
            Err(e) => return Err(e),
        };
        let indices: Vec<_> = input
            .segments
            .iter()
            .filter(|s| !cached.iter().any(|c| c.index == s.index))
            .map(|s| s.index)
            .take(batch)
            .collect();
        if !indices.is_empty() {
            return Ok(Some(Job {
                input,
                indices,
                reset,
            }));
        }
    }
    Ok(None)
}

#[derive(Debug, Serialize)]
pub(crate) struct Step {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    pub segments: usize,
    pub attempted: bool,
    pub retry_at_ms: i64,
}
impl Step {
    fn at(status: &'static str, retry_at_ms: i64) -> Self {
        Self {
            status,
            source_id: None,
            segments: 0,
            attempted: false,
            retry_at_ms,
        }
    }
}

pub(crate) struct Runner {
    config: embedding::Config,
    provider: Arc<dyn embedding::Provider>,
    clock: Clock,
    allowed: Arc<dyn Fn() -> bool + Send + Sync>,
    stopped: Arc<AtomicBool>,
}

impl Runner {
    #[cfg(test)]
    pub(crate) fn injected(
        config: embedding::Config,
        provider: Arc<dyn embedding::Provider>,
        clock: Clock,
        allowed: Arc<dyn Fn() -> bool + Send + Sync>,
        stopped: Arc<AtomicBool>,
    ) -> Self {
        Self {
            config,
            provider,
            clock,
            allowed,
            stopped,
        }
    }
    pub(crate) fn runtime(
        config: embedding::Config,
        stopped: Arc<AtomicBool>,
    ) -> PersistenceResult<Self> {
        config.validate()?;
        Ok(Self {
            provider: Arc::new(embedding::Ollama::runtime(&config.base_url)?),
            config,
            clock: Arc::new(now_ms),
            allowed: Arc::new(embedding::runtime_network_allowed),
            stopped,
        })
    }
    pub(crate) fn once(&self, store: &mut SqliteMemoryStore) -> PersistenceResult<Step> {
        if !self.config.enabled || !(self.allowed)() {
            return Ok(Step::at("disabled", 0));
        }
        if self.stopped.load(Ordering::SeqCst) {
            return Ok(Step::at("cancelled", 0));
        }
        let tx = store.connection.unchecked_transaction()?;
        let state = SqliteMemoryStore::load(&tx, &store.owner)?;
        let eligible: Vec<_> = state
            .sources
            .values()
            .filter(|s| body(s).is_some_and(|b| !b.trim().is_empty()))
            .collect();
        if eligible.is_empty() {
            return Ok(Step::at("idle", 0));
        }
        if let Some(token) = load_binding(&tx, &store.owner)? {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='source_embedding_control')",
                [],
                |r| r.get(0),
            )?;
            if exists
                && token.binding.embedding.model == self.config.model
                && load_control(&tx, &store.owner)?.verify_after_ms > (self.clock)()
                && eligible.iter().all(|s| {
                    source_of(&token, s)
                        .and_then(|input| {
                            read_vectors(&tx, &store.owner, &token, &input)
                                .map(|v| v.len() == input.segments.len())
                        })
                        .unwrap_or(false)
                })
            {
                return Ok(Step::at("idle", 0));
            }
        }
        tx.commit()?;
        let lease =
            match store.claim_embedding(self.config.query_timeout_ms, Arc::clone(&self.clock))? {
                Ok(lease) => lease,
                Err(step) => return Ok(step),
            };
        let deadline =
            embedding::RequestControl::new(self.config.query_timeout_ms, Arc::clone(&self.stopped));
        // All paths release only their own lease; an error leaves a bounded expiry for recovery.
        let result = self.execute(store, &lease, &deadline);
        if result.is_err() {
            let _ = self.finish(store, &lease, None, "failed", true);
        }
        result
    }
    fn execute(
        &self,
        store: &mut SqliteMemoryStore,
        lease: &Lease,
        deadline: &embedding::RequestControl,
    ) -> PersistenceResult<Step> {
        deadline.check()?;
        store.begin_embedding_call(lease, true)?;
        let started = Instant::now();
        let binding = self.provider.resolve(self.config.model, deadline);
        store.end_embedding_call(binding.is_err(), elapsed(started), None)?;
        let binding = match binding {
            Ok(binding) if binding.model == self.config.model => binding,
            _ => return self.finish(store, lease, None, "metadata_failed", true),
        };
        if deadline.check().is_err() || !(self.allowed)() {
            return self.finish(store, lease, None, "cancelled", false);
        }
        let token = match store.leased_binding(Binding::new(binding)?, lease) {
            Ok(token) => token,
            Err(e) if e.to_string() == "embedding_lease_lost" => {
                return Ok(Step::at("lease_lost", 0));
            }
            Err(e) => return Err(e),
        };
        let job = {
            let tx = store.connection.unchecked_transaction()?;
            let job = next_job(&tx, &store.owner, &token, self.config.batch)?;
            tx.commit()?;
            job
        };
        let Some(job) = job else {
            return self.finish(store, lease, None, "idle", false);
        };
        deadline.check()?;
        if !(self.allowed)() {
            return self.finish(store, lease, None, "cancelled", false);
        }
        store.begin_embedding_call(lease, false)?;
        let texts: Vec<_> = job
            .indices
            .iter()
            .map(|i| job.input.segments[*i].text.clone())
            .collect();
        let started = Instant::now();
        let result = self
            .provider
            .embed(&token.binding.embedding, &texts, deadline);
        store.end_embedding_call(
            result.is_err(),
            elapsed(started),
            result.as_ref().ok().and_then(|b| b.input_tokens),
        )?;
        if deadline.check().is_err() || !(self.allowed)() {
            return self.finish(store, lease, Some(&job), "cancelled", false);
        }
        let success = match result {
            Ok(batch) => {
                let tx = store
                    .connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                let result = write_vectors(
                    &tx,
                    &store.owner,
                    &job.input,
                    &job.indices,
                    &batch.vectors,
                    job.reset,
                );
                match result {
                    Ok(()) => {
                        if deadline.check().is_err() || !(self.allowed)() {
                            drop(tx);
                            return self.finish(store, lease, Some(&job), "cancelled", false);
                        }
                        tx.commit()?;
                        true
                    }
                    Err(e)
                        if matches!(
                            e.to_string().as_str(),
                            "embedding_lease_lost"
                                | "embedding_index_source_unavailable"
                                | "embedding_index_source_changed"
                                | "embedding_index_binding_changed"
                        ) =>
                    {
                        drop(tx);
                        return self.finish(store, lease, Some(&job), "discarded", false);
                    }
                    Err(_) => false,
                }
            }
            Err(_) => false,
        };
        self.finish(
            store,
            lease,
            Some(&job),
            if success { "completed" } else { "failed" },
            false,
        )
    }
    fn finish(
        &self,
        store: &mut SqliteMemoryStore,
        lease: &Lease,
        job: Option<&Job>,
        status: &'static str,
        metadata_failed: bool,
    ) -> PersistenceResult<Step> {
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut control = match check_lease(&tx, &store.owner, lease) {
            Ok(c) => c,
            Err(e) if e.to_string() == "embedding_lease_lost" => {
                return Ok(Step::at("lease_lost", 0));
            }
            Err(e) => return Err(e),
        };
        let now = (self.clock)();
        let state = SqliteMemoryStore::load(&tx, &store.owner)?;
        control.failures.retain(|id, _| {
            state
                .sources
                .values()
                .any(|s| &s.id == id && body(s).is_some())
        });
        if metadata_failed {
            control.metadata_failures = control.metadata_failures.saturating_add(1);
            control.next_attempt_ms = now.saturating_add(backoff(control.metadata_failures));
        } else {
            control.metadata_failures = 0;
            if let Some(job) = job {
                control.cursor = job.input.source_id.clone();
                if status == "failed" {
                    let key = job.input.token.binding.fingerprint()?;
                    let attempts = control
                        .failures
                        .get(&job.input.source_id)
                        .filter(|f| {
                            f.content_hash == job.input.content_hash && f.binding_key == key
                        })
                        .map_or(1, |f| f.attempts.saturating_add(1));
                    control.failures.insert(
                        job.input.source_id.clone(),
                        Failure {
                            content_hash: job.input.content_hash.clone(),
                            binding_key: key,
                            attempts,
                            retry_at_ms: now.saturating_add(backoff(attempts)),
                        },
                    );
                } else {
                    control.failures.remove(&job.input.source_id);
                }
                if matches!(status, "discarded" | "cancelled") {
                    control.stats.discarded_results =
                        control.stats.discarded_results.saturating_add(1);
                }
            } else if status == "idle" {
                control.next_attempt_ms = now.saturating_add(MIN_BATCH_INTERVAL_MS);
                // Remove old namespaces only after validating complete current coverage.
                if let Some(token) = load_binding(&tx, &store.owner)? {
                    let all_ready = state
                        .sources
                        .values()
                        .filter(|s| body(s).is_some_and(|s| !s.trim().is_empty()))
                        .all(|s| {
                            source_of(&token, s)
                                .and_then(|input| {
                                    read_vectors(&tx, &store.owner, &token, &input)
                                        .map(|v| v.len() == input.segments.len())
                                })
                                .unwrap_or(false)
                        });
                    if all_ready {
                        control.verify_after_ms = now.saturating_add(IDLE_INTERVAL_MS);
                        tx.execute(
                            "DELETE FROM source_vectors WHERE owner_id=?1 AND binding_key<>?2",
                            params![store.owner.0, token.binding.fingerprint()?],
                        )?;
                    }
                }
            }
        }
        control.holder = None;
        control.lease_until_ms = 0;
        let retry_at_ms = control.next_attempt_ms;
        save_control(&tx, &store.owner, &control)?;
        tx.commit()?;
        Ok(Step {
            status,
            source_id: job.map(|j| j.input.source_id.clone()),
            segments: if status == "completed" {
                job.map_or(0, |j| j.indices.len())
            } else {
                0
            },
            attempted: true,
            retry_at_ms,
        })
    }
    pub(crate) fn drain(
        &self,
        store: &mut SqliteMemoryStore,
        max_batches: usize,
    ) -> PersistenceResult<serde_json::Value> {
        if !(1..=100).contains(&max_batches) {
            return Err("max_batches must be 1..100".into());
        }
        let mut steps = Vec::new();
        for _ in 0..max_batches {
            let step = self.once(store)?;
            let again = matches!(step.status, "completed" | "failed" | "discarded");
            let wait = step.retry_at_ms.saturating_sub((self.clock)());
            steps.push(step);
            if !again {
                break;
            }
            if steps.len() < max_batches && wait > 0 {
                // Maintenance waits only the success cadence, never a failed source's backoff.
                std::thread::sleep(std::time::Duration::from_millis(
                    wait.min(MIN_BATCH_INTERVAL_MS) as u64,
                ));
            }
        }
        Ok(
            serde_json::json!({"max_batches":max_batches,"attempted_batches":steps.iter().filter(|s|s.attempted).count(),"steps":steps,"stats":store.embedding_stats()?}),
        )
    }
}

fn elapsed(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

fn set_binding(
    db: &Connection,
    owner: &UserId,
    binding: Binding,
    lease: Option<Lease>,
) -> PersistenceResult<BindingToken> {
    binding.check()?;
    if let Some(lease) = &lease {
        check_lease(db, owner, lease)?;
    }
    let json = serde_json::to_string(&binding)?;
    let previous: Option<(String, i64)> = db
        .query_row(
            "SELECT binding, generation FROM source_vector_bindings WHERE owner_id=?1",
            [&owner.0],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let generation = match previous {
        Some((old, generation)) if old == json && generation > 0 => generation,
        Some((_, generation)) => generation
            .checked_add(1)
            .filter(|g| *g > 0)
            .ok_or("embedding_index_generation_invalid")?,
        None => 1,
    };
    db.execute(
            "INSERT INTO source_vector_bindings VALUES (?1, ?2, ?3)
             ON CONFLICT(owner_id) DO UPDATE SET binding=excluded.binding, generation=excluded.generation",
            params![owner.0, json, generation],
        )?;
    Ok(BindingToken {
        owner: owner.clone(),
        binding,
        generation,
        lease,
    })
}

fn write_vectors(
    db: &Connection,
    owner: &UserId,
    input: &SourceInput,
    indices: &[usize],
    vectors: &[Vec<f32>],
    reset: bool,
) -> PersistenceResult<()> {
    let current = source_input(db, owner, &input.token, &input.source_id)?;
    if current.content_hash != input.content_hash || current.segments != input.segments {
        return Err("embedding_index_source_changed".into());
    }
    if indices.is_empty()
        || vectors.len() != indices.len()
        || indices.iter().any(|i| *i >= current.segments.len())
        || indices
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != indices.len()
    {
        return Err("embedding_index_batch_mismatch".into());
    }
    let binding = &input.token.binding;
    let key = binding.fingerprint()?;
    let json = serde_json::to_string(binding)?;
    if reset {
        db.execute(
            "DELETE FROM source_vectors WHERE owner_id=?1 AND source_id=?2 AND binding_key=?3",
            params![owner.0, current.source_id, key],
        )?;
    }
    for (index, vector) in indices.iter().zip(vectors) {
        let segment = &current.segments[*index];
        embedding::check_normalized(vector, binding.embedding.dimensions)?;
        let blob: Vec<u8> = vector.iter().flat_map(|n| n.to_le_bytes()).collect();
        db.execute(
                "INSERT INTO source_vectors VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(owner_id, source_id, binding_key, segment_index) DO UPDATE SET
                 binding=excluded.binding, start_byte=excluded.start_byte, end_byte=excluded.end_byte,
                 source_content_hash=excluded.source_content_hash, content_hash=excluded.content_hash,
                 dims=excluded.dims, blob_version=excluded.blob_version, vector=excluded.vector",
                params![
                    owner.0, current.source_id, key, json, i64::try_from(segment.index)?,
                    i64::try_from(segment.start)?, i64::try_from(segment.end)?, current.content_hash,
                    segment.content_hash, i64::try_from(binding.embedding.dimensions)?,
                    binding.blob_version, blob
                ],
            )?;
    }
    Ok(())
}

fn read_vectors(
    db: &Connection,
    owner: &UserId,
    token: &BindingToken,
    input: &SourceInput,
) -> PersistenceResult<Vec<CachedSegment>> {
    let binding = &token.binding;
    let json = serde_json::to_string(binding)?;
    let mut result = Vec::new();
    {
        let mut statement = db.prepare(
            "SELECT segment_index, start_byte, end_byte, source_content_hash, content_hash,
                 binding, dims, blob_version, length(vector), vector FROM source_vectors
                 WHERE owner_id=?1 AND source_id=?2 AND binding_key=?3 ORDER BY segment_index",
        )?;
        let mut rows =
            statement.query(params![owner.0, input.source_id, binding.fingerprint()?])?;
        while let Some(row) = rows.next()? {
            // Row conversion errors are corrupt cache data; SQL/query errors still propagate.
            let cached = (|| -> PersistenceResult<CachedSegment> {
                let index = usize::try_from(row.get::<_, i64>(0)?)?;
                let segment = input
                    .segments
                    .get(index)
                    .ok_or("embedding_index_segment_invalid")?;
                if row.get::<_, i64>(1)? != i64::try_from(segment.start)?
                    || row.get::<_, i64>(2)? != i64::try_from(segment.end)?
                    || row.get::<_, String>(3)? != input.content_hash
                    || row.get::<_, String>(4)? != segment.content_hash
                    || row.get::<_, String>(5)? != json
                    || row.get::<_, i64>(6)? != i64::try_from(binding.embedding.dimensions)?
                    || row.get::<_, String>(7)? != BLOB_VERSION
                    || row.get::<_, i64>(8)? != i64::try_from(binding.embedding.dimensions * 4)?
                {
                    return Err("embedding_index_row_invalid".into());
                }
                let vector: Vec<f32> = row
                    .get_ref(9)?
                    .as_blob()?
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes(b.try_into().expect("four-byte chunk")))
                    .collect();
                embedding::check_normalized(&vector, binding.embedding.dimensions)?;
                Ok(CachedSegment {
                    index,
                    start: segment.start,
                    end: segment.end,
                    vector,
                })
            })()
            .map_err(|_| "embedding_index_row_invalid")?;
            result.push(cached);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removed_source_invalidates_all_bindings_without_touching_other_sources_or_owners() {
        let db = Connection::open_in_memory().unwrap();
        create_schema(&db).unwrap();
        for (owner, id, key) in [
            ("alice", "removed", "a"),
            ("alice", "removed", "b"),
            ("alice", "retained", "a"),
            ("bob", "removed", "a"),
        ] {
            db.execute("INSERT INTO source_vectors VALUES (?1, ?2, ?3, '{}', 0, 0, 1, 'body', 'segment', 1, 'f32-le-v1', ?4)",
                params![owner, id, key, 1.0f32.to_le_bytes().as_slice()]).unwrap();
        }
        let source: SourceRecord = serde_json::from_value(serde_json::json!({
            "id":"removed", "session_id":"s", "turn_id":"t", "observed_at":0,
            "request":{"text":"x", "capture_source":true}, "enrichment":"pending",
            "error":null, "result":null, "base_revision":0
        }))
        .unwrap();
        let before = BTreeMap::from([("internal-key".into(), source)]);
        let tx = db.unchecked_transaction().unwrap();
        synchronize(&tx, &UserId::from("alice"), &before, &BTreeMap::new()).unwrap();
        tx.commit().unwrap();
        let remaining: Vec<(String, String)> = db
            .prepare("SELECT owner_id, source_id FROM source_vectors ORDER BY owner_id, source_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            remaining,
            [
                ("alice".into(), "retained".into()),
                ("bob".into(), "removed".into())
            ]
        );
    }
}

#[cfg(test)]
#[path = "embedding_background_tests.rs"]
mod background_tests;

/// Cache data captured in R3's business read transaction. No metadata requests or writes.
pub(crate) struct VectorSnapshot {
    pub binding: Option<embedding::Binding>,
    pub vectors: BTreeMap<String, Vec<CachedSegment>>,
    pub totals: BTreeMap<String, usize>,
}
pub(crate) fn read_snapshot(
    db: &Connection,
    owner: &UserId,
    sources: &[&SourceRecord],
    model: embedding::Model,
) -> PersistenceResult<VectorSnapshot> {
    let token = load_binding(db, owner)?;
    let mut snapshot = VectorSnapshot {
        binding: token.as_ref().map(|t| t.binding.embedding.clone()),
        vectors: BTreeMap::new(),
        totals: BTreeMap::new(),
    };
    for source in sources {
        let text = body(source).ok_or("embedding_index_source_unavailable")?;
        if snapshot
            .totals
            .insert(source.id.clone(), embedding::segments(text).len())
            .is_some()
        {
            return Err("embedding_index_source_ambiguous".into());
        }
        if let Some(token) = &token
            && token.binding.embedding.model == model
        {
            let input = source_of(token, source)?;
            match read_vectors(db, owner, token, &input) {
                Ok(vectors) => {
                    snapshot.vectors.insert(source.id.clone(), vectors);
                }
                Err(e) if e.to_string() == "embedding_index_row_invalid" => (),
                Err(e) => return Err(e),
            }
        }
    }
    Ok(snapshot)
}
