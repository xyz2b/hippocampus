//! Atomic SQLite snapshots reuse the tested in-memory engine without splitting a reconciliation.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::*;

pub type PersistenceResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PersistedMemoryState {
    pub repository: InMemoryMemoryRepository,
    pub entities: InMemoryEntityRepository,
    pub staging: CandidateStaging,
    pub(crate) receipts: BTreeMap<String, TurnReceipt>,
    #[serde(default)]
    pub sources: BTreeMap<String, SourceRecord>,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub source_revision: u64,
    #[serde(default)]
    pub last_applied_source_sequence: u64,
    #[serde(default)]
    pub source_less_barrier_sequence: u64,
    /// W5 提交回执，按依赖组 ID 保存；重放同一组不产生变更。
    #[serde(default)]
    pub w5_receipts: BTreeMap<String, W5Receipt>,
    /// 冲突关系，按关系 ID（`rel-<n>`，owner 内唯一）保存；与 Memory 生命周期分开。
    #[serde(default)]
    pub relations: BTreeMap<String, StoredRelation>,
    /// 下一个关系编号；单调递增，不复用。分配与关系写入在同一事务。
    #[serde(default)]
    pub next_relation: u64,
    /// W5 产生或重判过的记录的更新状态；没有条目表示未知（召回为 null），不是 judged。
    /// 不放进 Memory：Memory 的序列化形状是冻结的 LegacyMemory。
    #[serde(default)]
    pub record_updates: BTreeMap<MemoryId, UpdateState>,
    /// Set by an operation whose business change belongs to the source that is already the
    /// last applied one (a later W5 group of the same source). Not persisted; without it the
    /// change would count as source-less and invalidate that source's own running task.
    #[serde(skip)]
    pub(crate) source_attributed_change: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EnrichmentTask {
    #[serde(skip_serializing_if = "is_zero_generation")]
    pub generation: u64,
    pub processing_version: Option<String>,
    pub attempt_count: u32,
    pub attempt_id: Option<String>,
    pub lease_until: Option<i64>,
    pub claim_revision: Option<u64>,
    pub accepted_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejudge: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_sequence: Option<u64>,
}

fn is_zero_generation(value: &u64) -> bool {
    *value == 0
}

/// Source controls are separate from model decisions and never rewrite USER text.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SourceManagement {
    pub provenance: Option<Value>,
    pub time_anchor: Option<Value>,
    pub confirmations: Vec<SubjectConfirmation>,
    pub audits: Vec<Value>,
    pub selection: Option<(u32, usize)>,
}
impl SourceManagement {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubjectConfirmation {
    pub generation: u64,
    pub candidate_index: usize,
    pub candidate_identity: String,
    pub original_subject: crate::contract::Subject,
    pub entity_id: EntityId,
    pub evidence: crate::contract::EvidenceRef,
    pub evidence_binding: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRecord {
    #[serde(default, skip_serializing_if = "SourceManagement::is_empty")]
    pub management: SourceManagement,
    pub id: String,
    pub session_id: String,
    pub turn_id: String,
    pub observed_at: i64,
    #[serde(default)]
    pub received_sequence: u64,
    pub request: Option<Value>,
    pub enrichment: String,
    pub error: Option<String>,
    pub result: Option<Value>,
    pub base_revision: u64,
    #[serde(default)]
    pub index: Value,
    #[serde(default)]
    pub task: EnrichmentTask,
}

impl SourceRecord {
    pub(crate) fn is_history_import(&self) -> bool {
        self.management
            .provenance
            .as_ref()
            .is_some_and(|p| p["kind"] == "history_import")
    }

    /// Explicit null remains unknown; reception is only a fallback for ordinary legacy sources.
    pub(crate) fn said_on(&self, offset: i32) -> PersistenceResult<Option<String>> {
        if let Some(anchor) = &self.management.time_anchor {
            return match &anchor["said_on"] {
                Value::Null => Ok(None),
                Value::String(date) if crate::temporal::parse_iso_date(date).is_some() => {
                    Ok(Some(date.clone()))
                }
                _ => Err("invalid source time anchor".into()),
            };
        }
        if self.is_history_import() {
            return Ok(None);
        }
        Ok(Some(
            time::OffsetDateTime::from_unix_timestamp(self.observed_at)?
                .to_offset(time::UtcOffset::from_whole_seconds(offset)?)
                .date()
                .to_string(),
        ))
    }

    /// 重判取得新的写入排序序号，原文接收顺序与时间保持不变。
    pub fn order_sequence(&self) -> u64 {
        self.task.order_sequence.unwrap_or(self.received_sequence)
    }

    pub fn status(&self) -> Value {
        serde_json::json!({
            "source": {
                "status": if self.request.is_some() { "stored" } else { "deleted" },
                "source_id": self.id,
                "received_sequence": self.received_sequence,
                "provenance": if self.is_history_import() { "history_import" } else { "mcp_received" },
                "message_id_trust": if self.is_history_import() { "server_generated" } else { "client_supplied" }
            },
            "index": if self.request.is_none() { serde_json::json!({"status":"deleted"}) }
                else if self.index.is_object() { self.index.clone() }
                else { serde_json::json!({"status":"not_requested"}) },
            "enrichment": { "status": self.enrichment, "error": self.error }
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TurnReceipt {
    pub request: Value,
    pub result: Value,
}

impl PersistedMemoryState {
    /// 原文接收与显式重判共用一个 owner 内的序号空间。
    pub fn next_source_sequence(&self) -> PersistenceResult<u64> {
        self.sources
            .values()
            .map(|s| s.received_sequence.max(s.order_sequence()))
            .max()
            .unwrap_or(0)
            .max(self.last_applied_source_sequence)
            .max(self.source_less_barrier_sequence)
            .checked_add(1)
            .ok_or_else(|| "source sequence overflow".into())
    }

    /// Restore both stores even if the operation returns an error; the SQLite transaction rolls back.
    pub fn with_engine<T>(
        &mut self,
        predicates: InMemoryPredicateRegistry,
        operation: impl FnOnce(
            &mut MemoryEngine<InMemoryMemoryRepository, InMemoryPredicateRegistry>,
            &InMemoryEntityRepository,
        ) -> PersistenceResult<T>,
    ) -> PersistenceResult<T> {
        let mut engine = MemoryEngine::new(std::mem::take(&mut self.repository), predicates);
        engine.staging = std::mem::take(&mut self.staging);
        let result = operation(&mut engine, &self.entities);
        self.repository = engine.repository;
        self.staging = engine.staging;
        result
    }
}

/// One connection can be shared by sequential operations; independent processes may use the same DB.
pub struct SqliteMemoryStore {
    pub(crate) connection: Connection,
    pub(crate) owner: UserId,
    pub(crate) source_search_enabled: bool,
    pub(crate) fact_index_enabled: bool,
    path: PathBuf,
}

impl SqliteMemoryStore {
    pub fn open(path: impl AsRef<Path>, owner: UserId) -> PersistenceResult<Self> {
        if owner.0.trim().is_empty() {
            return Err("owner must not be empty".into());
        }
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(path) {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => tx.execute_batch(
                "CREATE TABLE memory_profiles (owner_id TEXT PRIMARY KEY, state_json TEXT NOT NULL);",
            )?,
            1..=11 => (),
            _ => return Err(format!("unsupported memory database version {version}").into()),
        }
        // Older binaries must reject snapshots whose source records they would silently discard.
        if version < 3 {
            crate::lexical::create_schema(&tx)?;
        }
        if version < 4 {
            crate::model_budget::create_schema(&tx)?;
        }
        if version < 5 {
            // user_version is database-wide: backfill every owner before raising it.
            let profiles = {
                let mut statement =
                    tx.prepare("SELECT owner_id, state_json FROM memory_profiles")?;
                statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?
            };
            for (owner_id, json) in profiles {
                let mut profile = Self::decode_trusted_state(&json)?;
                let applied = profile
                    .sources
                    .values()
                    .filter(|source| {
                        matches!(
                            source.enrichment.as_str(),
                            "committed" | "completed_pending" | "stale_requires_review"
                        )
                    })
                    .map(|source| source.received_sequence)
                    .max()
                    .unwrap_or(0);
                if applied > profile.last_applied_source_sequence {
                    profile.last_applied_source_sequence = applied;
                    Self::save(&tx, &UserId(owner_id), &profile)?;
                }
            }
            tx.pragma_update(None, "user_version", 5)?;
        }
        if version < 6 {
            // W5 receipts live in the owner snapshot. Older binaries would drop them when
            // rewriting it and then re-apply committed groups, so they must reject version 6.
            tx.pragma_update(None, "user_version", 6)?;
        }
        if version < 7 {
            // v6 receipts (W5 A1–A3) carry no status and were all committed; decoding fills in
            // Committed. A v6 binary would drop the status (and turn a needs_rejudge group into
            // a committed one) when rewriting the snapshot, so it must reject version 7. The
            // rewrite that stores the status explicitly is the version 8 step below, in the same
            // transaction: one decode/save writes both the status and the receipt form.
            tx.pragma_update(None, "user_version", 7)?;
        }
        if version < 8 {
            // Relations, update states and receipt fields live in the owner snapshot; a v7
            // binary would drop them when rewriting it (conflicts vanish, pending turns into
            // unknown), so it must reject version 8. Rewrite every profile with receipts so
            // each receipt's `form` is stored explicitly as pre8: after this rewrite the
            // receipt also carries `updates`, so the absence of a field can no longer tell
            // which group form its fingerprint was taken from. The same rewrite stores the
            // status of v6 receipts explicitly (see the version 7 step).
            let profiles = {
                let mut statement =
                    tx.prepare("SELECT owner_id, state_json FROM memory_profiles")?;
                statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?
            };
            for (owner_id, json) in profiles {
                let profile = Self::decode_trusted_state(&json)?;
                if !profile.w5_receipts.is_empty() {
                    Self::save(&tx, &UserId(owner_id), &profile)?;
                }
            }
            tx.pragma_update(None, "user_version", 8)?;
        }
        if version < 9 {
            // v8 writers would drop round/operation/order fields when saving the snapshot.
            // Missing optional fields remain absent; do not enroll legacy tasks or rewrite receipts.
            tx.pragma_update(None, "user_version", 9)?;
        }
        if version < 10 {
            // All owners decode missing source controls as ordinary provenance/generation 0.
            // No enrollment or receipt rewrite: v9 writers must reject this version.
            tx.pragma_update(None, "user_version", 10)?;
        }
        if version < 11 {
            // Derived vectors stay outside owner snapshots; v10 writers lack atomic invalidation.
            crate::embedding_index::create_schema(&tx)?;
            tx.pragma_update(None, "user_version", 11)?;
        }
        crate::fact_index::create_schema(&tx)?;
        let mut state = Self::load(&tx, &owner)?;
        if crate::lexical::initialize(&tx, &owner, &mut state)? && !state.sources.is_empty() {
            Self::save(&tx, &owner, &state)?;
        }
        tx.commit()?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let store = Self {
            connection,
            owner,
            source_search_enabled: true,
            fact_index_enabled: false,
            path: std::fs::canonicalize(path)?,
        };
        // Fail at startup on invalid JSON instead of replacing an unreadable profile with an empty one.
        store.read()?;
        Ok(store)
    }

    pub fn owner(&self) -> &UserId {
        &self.owner
    }

    /// The same database holds the business snapshot and the independent model-call ledger.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A foreground operation connects only to an initialized database; no migration or index repair.
    pub fn open_operation(path: impl AsRef<Path>, owner: UserId) -> PersistenceResult<Self> {
        if owner.0.trim().is_empty() {
            return Err("owner must not be empty".into());
        }
        let path = path.as_ref();
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version != 11 {
            return Err(format!(
                "operation connection requires initialized database version 11; found {version}"
            )
            .into());
        }
        let journal: String =
            connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if journal != "wal" {
            return Err("operation connection requires initialized WAL database".into());
        }
        connection.pragma_update(None, "synchronous", "FULL")?;
        let store = Self {
            connection,
            owner,
            source_search_enabled: true,
            fact_index_enabled: false,
            path: std::fs::canonicalize(path)?,
        };
        store.read()?;
        Ok(store)
    }

    /// Internal search/inspection never migrates, backfills or repairs a database.
    pub fn open_read_only(path: impl AsRef<Path>, owner: UserId) -> PersistenceResult<Self> {
        if owner.0.trim().is_empty() {
            return Err("owner must not be empty".into());
        }
        let path = path.as_ref();
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if !(3..=11).contains(&version) {
            return Err(format!(
                "source search requires database version 3 to 11; found {version}; migrate explicitly"
            )
            .into());
        }
        connection.pragma_update(None, "query_only", true)?;
        let store = Self {
            connection,
            owner,
            source_search_enabled: true,
            fact_index_enabled: false,
            path: std::fs::canonicalize(path)?,
        };
        store.read()?;
        Ok(store)
    }

    pub fn read(&self) -> PersistenceResult<PersistedMemoryState> {
        Self::load(&self.connection, &self.owner)
    }

    /// 短发布边界：加载与发布期间阻止其他连接提交删除；不保存或推进任何版本。
    /// 回调内不得调用模型、等待工作者或开启另一个写事务。
    pub(crate) fn read_for_publication<T>(
        &self,
        publish: impl FnOnce(&PersistedMemoryState) -> PersistenceResult<T>,
    ) -> PersistenceResult<T> {
        let tx =
            rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let state = Self::load(&tx, &self.owner)?;
        let result = publish(&state)?;
        tx.commit()?;
        Ok(result)
    }

    // Only snapshots read from the local database may restore staged source approval.
    fn decode_trusted_state(json: &str) -> PersistenceResult<PersistedMemoryState> {
        let mut state: PersistedMemoryState = serde_json::from_str(json)?;
        let stored: Value = serde_json::from_str(json)?;
        state
            .staging
            .restore_trusted_source_support(&stored["staging"]);
        Ok(state)
    }

    pub(crate) fn load(
        connection: &Connection,
        owner: &UserId,
    ) -> PersistenceResult<PersistedMemoryState> {
        let json: Option<String> = connection
            .query_row(
                "SELECT state_json FROM memory_profiles WHERE owner_id = ?1",
                [&owner.0],
                |row| row.get(0),
            )
            .optional()?;
        match json {
            Some(json) => Self::decode_trusted_state(&json),
            None => Ok(PersistedMemoryState::default()),
        }
    }

    pub(crate) fn save(
        connection: &Connection,
        owner: &UserId,
        state: &PersistedMemoryState,
    ) -> PersistenceResult<()> {
        connection.execute(
            "INSERT INTO memory_profiles(owner_id, state_json) VALUES (?1, ?2)
             ON CONFLICT(owner_id) DO UPDATE SET state_json = excluded.state_json",
            params![owner.0, serde_json::to_string(state)?],
        )?;
        Ok(())
    }

    /// Load after taking the write lock so two processes cannot overwrite each other's updates.
    pub fn transaction<T>(
        &mut self,
        operation: impl FnOnce(&mut PersistedMemoryState) -> PersistenceResult<T>,
    ) -> PersistenceResult<T> {
        let started = std::time::Instant::now();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut state = Self::load(&tx, &self.owner)?;
        let sources_before = state.sources.clone();
        let old_w5: std::collections::BTreeSet<_> = if crate::diagnostics::enabled() {
            state.w5_receipts.keys().cloned().collect()
        } else {
            Default::default()
        };
        let source_revision = state.source_revision;
        let read_revision = state.revision;
        let applied_before = state.last_applied_source_sequence;
        // ponytail: compare one owner's full business snapshot; use indexed revisions if profiles grow large.
        // Relations and update states are business data too: their change advances the
        // revision and triggers the ordering barrier like a record change.
        let business_before = serde_json::to_value((
            &state.repository,
            &state.entities,
            &state.staging,
            &state.relations,
            &state.record_updates,
        ))?;
        let before = crate::diagnostics::full_snapshots()
            .then(|| serde_json::to_value(&state).ok())
            .flatten();
        let result = match operation(&mut state) {
            Ok(result) => result,
            Err(error) => {
                crate::diagnostics::record(
                    "transaction_rollback",
                    || serde_json::json!({"error":error.to_string(),"before":before,"duration_ms":started.elapsed().as_millis()}),
                );
                return Err(error);
            }
        };
        let business_after = serde_json::to_value((
            &state.repository,
            &state.entities,
            &state.staging,
            &state.relations,
            &state.record_updates,
        ))?;
        let business_changed = business_before != business_after;
        state.revision = if business_changed {
            read_revision
                .checked_add(1)
                .ok_or("memory revision overflow")?
        } else {
            read_revision
        };
        if business_changed {
            if state.last_applied_source_sequence == applied_before
                && !state.source_attributed_change
                && !pure_entity_addition(
                    &business_before,
                    &business_after,
                    &state.entities,
                    &self.owner,
                )
            {
                // ponytail: owner-wide barrier; split by semantic slot only if unrelated jobs are measurably blocked.
                state.source_less_barrier_sequence = state.source_less_barrier_sequence.max(
                    state
                        .sources
                        .values()
                        .map(SourceRecord::order_sequence)
                        .max()
                        .unwrap_or(0),
                );
            }
            let barrier = state.source_less_barrier_sequence;
            let applied = state.last_applied_source_sequence;
            for source in state.sources.values_mut() {
                if source.task.processing_version.is_none()
                    || !matches!(source.enrichment.as_str(), "queued" | "running")
                {
                    continue;
                }
                let reason = if source.order_sequence() <= barrier {
                    Some("source_less_authority_changed")
                } else if source.order_sequence() < applied {
                    Some("newer_source_applied")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    source.enrichment = "stale_requires_review".into();
                    source.error = Some(reason.into());
                }
            }
        }
        // ponytail: one JSON snapshot per owner; use indexed rows if profile size makes writes slow.
        crate::lexical::synchronize(
            &tx,
            &self.owner,
            &sources_before,
            source_revision,
            &mut state,
        )?;
        crate::embedding_index::synchronize(&tx, &self.owner, &sources_before, &state.sources)?;
        Self::save(&tx, &self.owner, &state)?;
        tx.commit()?;
        let sources_deleted = sources_before.iter().any(|(key, old)| {
            old.request.is_some()
                && state
                    .sources
                    .get(key)
                    .is_none_or(|new| new.request.is_none() || new.enrichment == "deleted")
        });
        if sources_deleted {
            crate::diagnostics::sources_deleted(
                state.revision,
                &self.owner.0,
                state
                    .sources
                    .values()
                    .filter(|s| s.request.is_none() || s.enrichment == "deleted")
                    .map(|s| s.id.clone())
                    .collect(),
            );
        }
        crate::diagnostics::record("transaction_committed", || {
            serde_json::json!({
            "before":if sources_deleted {None} else {before},"after":if crate::diagnostics::full_snapshots() && !sources_deleted {Some(&state)} else {None},"counts":{"sources":state.sources.len(),"relations":state.relations.len(),"updates":state.record_updates.len()},"business_changed":business_changed,"sources_changed":source_revision != state.source_revision,"read_revision":read_revision,"committed_revision":state.revision,
            "revision_observability":"persisted","duration_ms":started.elapsed().as_millis()})
        });
        if crate::diagnostics::enabled() {
            for (id, receipt) in state
                .w5_receipts
                .iter()
                .filter(|(id, _)| !old_w5.contains(*id))
            {
                if !matches!(receipt.status, crate::W5Status::Committed) {
                    continue;
                }
                let group = serde_json::from_str::<crate::W5Group>(&receipt.fingerprint).ok();
                let source = group
                    .as_ref()
                    .and_then(|g| state.sources.get(&g.source_key));
                let mut deps = crate::diagnostics::ContentDeps::complete();
                if let Some(source) = source {
                    deps.source(&self.owner, source);
                } else {
                    deps.block("source_unavailable");
                }
                for product in receipt.products.values() {
                    deps.memory(&self.owner, &state, *product);
                }
                for decision in &receipt.decisions {
                    for target in &decision.targets {
                        deps.memory(&self.owner, &state, *target);
                    }
                    for id in &decision.basis_sources {
                        if let Some(source) = state.sources.values().find(|s| &s.id == id) {
                            deps.source(&self.owner, source);
                        } else {
                            deps.block("source_unavailable");
                        }
                    }
                }
                deps.summary_ids(receipt.products.len().max(receipt.decisions.len()));
                let trace = source
                    .map(|s| {
                        crate::diagnostics::TraceContext::source(
                            &s.id,
                            s.task.generation,
                            s.task.round.unwrap_or(0),
                            s.task.attempt_id.as_deref().unwrap_or("unknown"),
                        )
                    })
                    .unwrap_or_default()
                    .with_deps(deps);
                trace.record("w5_commit", || serde_json::json!({"group_fingerprint":crate::openai::recall_models::fingerprint(&serde_json::json!(id)),
                    "decision_types":receipt.decisions.iter().take(128).map(|d|&d.action).collect::<Vec<_>>(),
                    "product_ids":receipt.products.values().take(128).collect::<Vec<_>>(),
                    "ids_truncated":receipt.products.len()>128 || receipt.decisions.len()>128,"revision":state.revision}));
            }
        }
        self.refresh_fact_index_after_commit();
        Ok(result)
    }
}

/// AF78: compare identities and directory resolution, never infer a person's meaning.
fn pure_entity_addition(
    before: &Value,
    after: &Value,
    entities: &InMemoryEntityRepository,
    owner: &UserId,
) -> bool {
    if [0, 2, 3, 4].into_iter().any(|i| before[i] != after[i]) {
        return false;
    }
    let (Some(old), Some(new)) = (
        before[1]["entities"].as_object(),
        after[1]["entities"].as_object(),
    ) else {
        return false;
    };
    let Ok(previous_entities) =
        serde_json::from_value::<InMemoryEntityRepository>(before[1].clone())
    else {
        return false;
    };
    new.len() > old.len()
        && old.iter().all(|(id, entity)| new.get(id) == Some(entity))
        && new.keys().filter(|id| !old.contains_key(*id)).all(|id| {
            let Some(entity) = entities.get(&EntityId::from(id.as_str())) else {
                return false;
            };
            &entity.owner_id == owner
                && std::iter::once(&entity.canonical_name)
                    .chain(&entity.aliases)
                    .all(|name| {
                        previous_entities.resolve(owner, name) == EntityResolution::NotFound
                    })
        })
}
