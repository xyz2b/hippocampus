//! Durable model-call allowances, independent of memory snapshots and optional trace files.
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::OpenAiAdapterError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelBudgetConfig {
    /// Explicit authorization identity. Reusing an ID never resets its allowance.
    pub group_id: String,
    pub max_calls: u64,
    #[serde(default = "operation_limit")]
    pub max_operation_calls: u64,
    /// Authorizes one coverage correction and its independent review in the same operation.
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_coverage_repair: bool,
    /// Authorizes one correction of a localized candidate error and its independent review.
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_extraction_repair: bool,
    /// Full serialized provider request bytes; this is not a token count.
    #[serde(default = "input_limit")]
    pub max_input_bytes: usize,
    #[serde(default = "output_limit")]
    pub extraction_output_tokens: u32,
    #[serde(default = "output_limit")]
    pub review_output_tokens: u32,
    #[serde(default = "output_limit")]
    pub query_output_tokens: u32,
}

fn operation_limit() -> u64 {
    4
}
fn input_limit() -> usize {
    256_000
}
fn output_limit() -> u32 {
    4096
}
fn is_false(value: &bool) -> bool {
    !value
}

impl Default for ModelBudgetConfig {
    fn default() -> Self {
        Self {
            group_id: String::new(),
            max_calls: 0,
            max_operation_calls: operation_limit(),
            allow_coverage_repair: false,
            allow_extraction_repair: false,
            max_input_bytes: input_limit(),
            extraction_output_tokens: output_limit(),
            review_output_tokens: output_limit(),
            query_output_tokens: output_limit(),
        }
    }
}

impl ModelBudgetConfig {
    pub fn validate(&self) -> Result<(), OpenAiAdapterError> {
        identity(&self.group_id, "group_id")?;
        if self.max_calls == 0
            || self.max_calls > i64::MAX as u64
            || !(1..=4).contains(&self.max_operation_calls)
            || ((self.allow_coverage_repair || self.allow_extraction_repair)
                && self.max_operation_calls != 4)
            || !(1..=1_000_000).contains(&self.max_input_bytes)
            || [
                self.extraction_output_tokens,
                self.review_output_tokens,
                self.query_output_tokens,
            ]
            .iter()
            .any(|v| !(1..=65536).contains(v))
        {
            return Err(budget_error("invalid model budget limits"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ModelStage {
    R1,
    R4,
    W1,
    W3,
    W4,
    Extraction,
    ExtractionReview,
    ExtractionRepair,
    ExtractionRepairReview,
    Query,
    QueryReview,
    Assistance,
    /// Recall's answer check over candidate USER quotes.
    Answer,
    Reader,
    Judge,
}

impl ModelStage {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::R1 => "r1",
            Self::R4 => "r4",
            Self::W1 => "w1",
            Self::W3 => "w3",
            Self::W4 => "w4",
            Self::Extraction => "extraction",
            Self::ExtractionReview => "extraction_review",
            Self::ExtractionRepair => "extraction_repair",
            Self::ExtractionRepairReview => "extraction_repair_review",
            Self::Query => "query",
            Self::QueryReview => "query_review",
            Self::Assistance => "assistance",
            Self::Answer => "answer",
            Self::Reader => "reader",
            Self::Judge => "judge",
        }
    }
    pub(crate) fn timeout(self, config: &ModelBudgetConfig) -> Duration {
        Duration::from_secs(if matches!(self, Self::Query) {
            15
        } else if matches!(self, Self::Assistance | Self::Answer) {
            180
        } else if self.output_tokens(config) > 16384 {
            360
        } else {
            180
        })
    }
    pub fn output_tokens(self, config: &ModelBudgetConfig) -> u32 {
        match self {
            Self::R1 => config.query_output_tokens,
            Self::R4 => config.review_output_tokens,
            Self::W1 => 32768,
            Self::W3 | Self::W4 => 16384,
            Self::Extraction | Self::ExtractionRepair | Self::Reader => {
                config.extraction_output_tokens
            }
            Self::ExtractionReview
            | Self::ExtractionRepairReview
            | Self::QueryReview
            | Self::Judge => config.review_output_tokens,
            Self::Query | Self::Assistance | Self::Answer => config.query_output_tokens,
        }
    }
}

fn budget_error(message: impl Into<String>) -> OpenAiAdapterError {
    OpenAiAdapterError::Budget(message.into())
}
fn database_error(error: impl std::fmt::Display) -> OpenAiAdapterError {
    budget_error(format!("ledger: {error}"))
}
fn identity(value: &str, field: &str) -> Result<(), OpenAiAdapterError> {
    if value.trim().is_empty() || value.len() > 512 {
        Err(budget_error(format!(
            "{field} must contain 1..512 nonempty bytes"
        )))
    } else {
        Ok(())
    }
}

pub(crate) fn create_schema(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS model_budget_groups (
        group_id TEXT PRIMARY KEY, policy_json TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS model_calls (
        call_id TEXT PRIMARY KEY, group_id TEXT NOT NULL, owner_id TEXT NOT NULL,
        operation_id TEXT NOT NULL, stage TEXT NOT NULL, request_fingerprint TEXT NOT NULL,
        input_bytes INTEGER NOT NULL, output_token_limit INTEGER NOT NULL,
        reserved_output_tokens INTEGER NOT NULL, outcome TEXT NOT NULL,
        usage_status TEXT NOT NULL, input_tokens INTEGER, output_tokens INTEGER,
        created_at INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS model_calls_group ON model_calls(group_id);
        CREATE UNIQUE INDEX IF NOT EXISTS model_calls_operation_stage
        ON model_calls(group_id,owner_id,operation_id,stage);",
    )
}

#[derive(Clone)]
pub(crate) struct ModelBudget {
    path: PathBuf,
    owner: String,
    pub(crate) config: ModelBudgetConfig,
    operation_id: String,
    max_background_operation_calls: Option<u64>,
}

impl ModelBudget {
    pub(crate) fn with_background_operation_limit(
        mut self,
        limit: u64,
    ) -> Result<Self, OpenAiAdapterError> {
        if !(1..=3).contains(&limit) {
            return Err(budget_error("background operation limit must be 1..3"));
        }
        self.max_background_operation_calls = Some(limit);
        Ok(self)
    }

    pub(crate) fn for_operation(
        &self,
        path: &Path,
        owner: &str,
        operation: &str,
    ) -> Result<Self, OpenAiAdapterError> {
        if self.owner != owner
            || self.path.canonicalize().map_err(database_error)?
                != path.canonicalize().map_err(database_error)?
        {
            return Err(budget_error(
                "write provider budget must belong to this source database and owner",
            ));
        }
        let mut budget = Self::new(path, owner.into(), self.config.clone(), operation.into())?;
        budget.max_background_operation_calls = self.max_background_operation_calls;
        Ok(budget)
    }

    /// A reservation survives a crash even when no stage output was saved.
    pub(crate) fn has_call(&self, stage: ModelStage) -> Result<bool, OpenAiAdapterError> {
        let db = self.connect(false)?;
        let present: i64 = db.query_row(
            "SELECT count(*) FROM model_calls WHERE owner_id=?1 AND operation_id=?2 AND stage=?3",
            params![self.owner, self.operation_id, stage.name()],
            |r| r.get(0),
        ).map_err(database_error)?;
        Ok(present != 0)
    }

    pub(crate) fn new(
        path: &Path,
        owner: String,
        config: ModelBudgetConfig,
        operation_id: String,
    ) -> Result<Self, OpenAiAdapterError> {
        config.validate()?;
        identity(&owner, "owner")?;
        identity(&operation_id, "operation_id")?;
        if path.as_os_str().is_empty()
            || path == Path::new(":memory:")
            || path.to_string_lossy().starts_with("file:")
        {
            return Err(budget_error(
                "budget database must be a persistent filesystem path",
            ));
        }
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir().map_err(database_error)?.join(path)
        };
        Ok(Self {
            path,
            owner,
            config,
            operation_id,
            max_background_operation_calls: None,
        })
    }

    fn connect(&self, create: bool) -> Result<Connection, OpenAiAdapterError> {
        if create {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&self.path) {
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(database_error(e)),
            }
        }
        // Settlement must never recreate a missing ledger and pretend the reservation survived.
        let db =
            Connection::open_with_flags(&self.path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
                .map_err(database_error)?;
        db.busy_timeout(Duration::from_secs(5))
            .map_err(database_error)?;
        db.pragma_update(None, "synchronous", "FULL")
            .map_err(database_error)?;
        Ok(db)
    }

    pub(crate) fn reserve(
        &self,
        stage: ModelStage,
        request: &[u8],
    ) -> Result<ModelReservation, OpenAiAdapterError> {
        if matches!(
            stage,
            ModelStage::ExtractionRepair | ModelStage::ExtractionRepairReview
        ) && !self.config.allow_coverage_repair
            && !self.config.allow_extraction_repair
        {
            return Err(budget_error("coverage repair is not authorized"));
        }
        if request.len() > self.config.max_input_bytes {
            return Err(budget_error(format!(
                "request exceeds {} input bytes",
                self.config.max_input_bytes
            )));
        }
        let mut db = self.connect(true)?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        create_schema(&tx).map_err(database_error)?;
        let mut policy = json!({"version":1,"per_stage_calls":1,"limits":self.config});
        if let Some(limit) = self.max_background_operation_calls {
            policy["max_background_operation_calls"] = json!(limit);
        }
        let write_limits = json!({"w1":ModelStage::W1.output_tokens(&self.config),
            "w3":ModelStage::W3.output_tokens(&self.config),
            "w4":ModelStage::W4.output_tokens(&self.config)});
        let existing: Option<String> = tx
            .query_row(
                "SELECT policy_json FROM model_budget_groups WHERE group_id=?1",
                [&self.config.group_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(database_error)?;
        // JSON object order can differ when another workspace target enables preserve_order.
        let persisted_policy = existing
            .as_deref()
            .map(serde_json::from_str::<Value>)
            .transpose()
            .map_err(database_error)?;
        // 新授权同时绑定后台输出额度。旧授权可继续旧阶段，不能静默提高后台额度。
        if existing.is_none()
            || matches!(stage, ModelStage::W1 | ModelStage::W3 | ModelStage::W4)
            || persisted_policy
                .as_ref()
                .is_some_and(|old| old.get("write_output_tokens").is_some())
        {
            policy["write_output_tokens"] = write_limits;
        }
        if persisted_policy.as_ref().is_some_and(|old| old != &policy) {
            return Err(budget_error(
                "budget group policy differs from its persisted authorization",
            ));
        }
        if existing.is_none() {
            tx.execute(
                "INSERT INTO model_budget_groups VALUES (?1,?2)",
                params![self.config.group_id, policy.to_string()],
            )
            .map_err(database_error)?;
        }
        let (total, mut operation, mut stage_calls): (i64, i64, i64) = tx
            .query_row(
                "SELECT count(*), coalesce(sum(owner_id=?2 AND operation_id=?3
                 AND (stage IN ('w1','w3','w4'))=?5),0),
             coalesce(sum(owner_id=?2 AND operation_id=?3 AND stage=?4),0)
             FROM model_calls WHERE group_id=?1",
                params![
                    self.config.group_id,
                    self.owner,
                    self.operation_id,
                    stage.name(),
                    matches!(stage, ModelStage::W1 | ModelStage::W3 | ModelStage::W4)
                ],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(database_error)?;
        if matches!(stage, ModelStage::W1 | ModelStage::W3 | ModelStage::W4) {
            (operation, stage_calls) = tx
                .query_row(
                    "SELECT count(*), coalesce(sum(stage=?3),0) FROM model_calls
                 WHERE owner_id=?1 AND operation_id=?2 AND stage IN ('w1','w3','w4')",
                    params![self.owner, self.operation_id, stage.name()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(database_error)?;
        }
        // 新召回按请求跨授权组计数，换 group_id 也不能重做已尝试的阶段。
        if matches!(stage, ModelStage::R1 | ModelStage::R4) {
            (operation, stage_calls) = tx
                .query_row(
                    "SELECT count(*), coalesce(sum(stage=?3),0) FROM model_calls
                 WHERE owner_id=?1 AND operation_id=?2 AND stage IN ('r1','r4')",
                    params![self.owner, self.operation_id, stage.name()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(database_error)?;
        }
        if total >= self.config.max_calls as i64 {
            return Err(budget_error("budget group call limit reached"));
        }
        let operation_limit = if matches!(stage, ModelStage::W1 | ModelStage::W3 | ModelStage::W4) {
            self.max_background_operation_calls
                .unwrap_or(self.config.max_operation_calls.min(3))
        } else if matches!(stage, ModelStage::R1 | ModelStage::R4) {
            self.config.max_operation_calls.min(2)
        } else {
            self.config.max_operation_calls
        };
        if operation >= operation_limit as i64 {
            return Err(budget_error("operation call limit reached"));
        }
        if stage_calls != 0 {
            return Err(budget_error("operation stage call limit reached"));
        }
        let id = crate::diagnostics::new_id();
        let output_tokens = stage.output_tokens(&self.config);
        // Stable, non-cryptographic diagnostic fingerprint; never used for authorization or deduplication.
        let fingerprint = request.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
        tx.execute("INSERT INTO model_calls VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8,'reserved','unknown',NULL,NULL,?9)",
            params![id,self.config.group_id,self.owner,self.operation_id,stage.name(),
                format!("fnv1a64:{fingerprint:016x}"),request.len() as i64,output_tokens,
                time::OffsetDateTime::now_utc().unix_timestamp()]).map_err(database_error)?;
        tx.commit().map_err(database_error)?;
        Ok(ModelReservation {
            budget: self.clone(),
            id,
            stage,
        })
    }
}

pub(crate) struct ModelReservation {
    budget: ModelBudget,
    pub(crate) id: String,
    stage: ModelStage,
}

impl ModelReservation {
    /// Call allowances are never refunded. Unknown usage retains the output-token reservation.
    pub(crate) fn settle(
        &self,
        outcome: &str,
        usage: Option<&Value>,
    ) -> Result<Value, OpenAiAdapterError> {
        let usage = usage
            .and_then(|v| Some((v["input_tokens"].as_u64()?, v["output_tokens"].as_u64()?)))
            .filter(|(input, output)| *input <= i64::MAX as u64 && *output <= i64::MAX as u64);
        let db = self.budget.connect(false)?;
        let (input, output) = usage
            .map(|(i, o)| (Some(i as i64), Some(o as i64)))
            .unwrap_or((None, None));
        let changed = db.execute("UPDATE model_calls SET outcome=?2,usage_status=?3,input_tokens=?4,output_tokens=?5,
            reserved_output_tokens=CASE WHEN ?3='known' THEN 0 ELSE reserved_output_tokens END
            WHERE call_id=?1 AND outcome='reserved'",
            params![self.id,outcome,if usage.is_some() { "known" } else { "unknown" },input,output])
            .map_err(database_error)?;
        if changed != 1 {
            return Err(budget_error("reservation missing or already settled"));
        }
        Ok(
            json!({"call_id":self.id,"group_id":self.budget.config.group_id,"operation_id":self.budget.operation_id,
            "stage":self.stage.name(),"usage_status":if usage.is_some() { "known" } else { "unknown" },
            "input_tokens":input,"output_tokens":output,"output_reservation_retained":usage.is_none(),
            "pricing_status":"unknown","outcome":outcome}),
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::OpenAiClient;

    pub(crate) struct TestLedger {
        pub(crate) path: PathBuf,
    }
    impl TestLedger {
        pub(crate) fn new() -> Self {
            Self {
                path: std::env::temp_dir().join(format!(
                    "pmr-budget-{}.sqlite3",
                    crate::diagnostics::new_id()
                )),
            }
        }
        pub(crate) fn client(&self, base_url: impl Into<String>) -> OpenAiClient {
            OpenAiClient::new(base_url, "local-test")
                .unwrap()
                .with_budget(
                    &self.path,
                    "fixture",
                    ModelBudgetConfig {
                        group_id: "fixture".into(),
                        max_calls: 100,
                        ..Default::default()
                    },
                    "operation",
                )
                .unwrap()
        }
        fn budget(&self, operation: &str, calls: u64) -> ModelBudget {
            ModelBudget::new(
                &self.path,
                "fixture".into(),
                ModelBudgetConfig {
                    group_id: "fixture".into(),
                    max_calls: calls,
                    ..Default::default()
                },
                operation.into(),
            )
            .unwrap()
        }
    }
    impl Drop for TestLedger {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[test]
    fn extraction_repair_defaults_preserve_policy_and_require_four_call_authorization() {
        let old: ModelBudgetConfig =
            serde_json::from_value(json!({"group_id":"repair", "max_calls":4})).unwrap();
        let old_policy = serde_json::to_value(&old).unwrap();
        assert!(old_policy.get("allow_extraction_repair").is_none());
        let explicit_false: ModelBudgetConfig = serde_json::from_value(json!({
            "group_id":"repair", "max_calls":4, "allow_extraction_repair":false
        }))
        .unwrap();
        assert_eq!(serde_json::to_value(explicit_false).unwrap(), old_policy);
        for coverage in [false, true] {
            for allowed in 1..=4 {
                let config: ModelBudgetConfig = serde_json::from_value(json!({
                    "group_id":"repair", "max_calls":4, "max_operation_calls":allowed,
                    "allow_extraction_repair":true, "allow_coverage_repair":coverage
                }))
                .unwrap();
                assert_eq!(config.validate().is_ok(), allowed == 4);
                assert_eq!(
                    serde_json::to_value(config).unwrap()["allow_extraction_repair"],
                    true
                );
            }
        }
    }

    #[test]
    fn extraction_repair_three_stages_share_one_durable_allowance() {
        for coverage in [false, true] {
            let ledger = TestLedger::new();
            let config: ModelBudgetConfig = serde_json::from_value(json!({
                "group_id":"repair", "max_calls":3, "max_operation_calls":4,
                "allow_extraction_repair":true, "allow_coverage_repair":coverage
            }))
            .unwrap();
            let reopen = || {
                ModelBudget::new(
                    &ledger.path,
                    "fixture".into(),
                    config.clone(),
                    "same-operation".into(),
                )
                .unwrap()
            };
            let stages = [
                ModelStage::Extraction,
                ModelStage::ExtractionRepair,
                ModelStage::ExtractionRepairReview,
            ];
            for (index, stage) in stages.into_iter().enumerate() {
                let receipt = reopen()
                    .reserve(stage, b"request")
                    .unwrap()
                    .settle("completed", None)
                    .unwrap();
                assert_eq!(receipt["stage"], stage.name());
                assert_eq!(receipt["operation_id"], "same-operation");
                assert!(reopen().reserve(stage, b"repeat").is_err());
                let db = Connection::open(&ledger.path).unwrap();
                assert_eq!(
                    db.query_row("SELECT count(*) FROM model_calls", [], |r| r
                        .get::<_, i64>(0))
                        .unwrap(),
                    index as i64 + 1
                );
            }
            assert!(matches!(
                reopen().reserve(ModelStage::ExtractionReview, b"extra request"),
                Err(OpenAiAdapterError::Budget(message)) if message == "budget group call limit reached"
            ));
        }
    }

    #[test]
    fn extraction_repair_cannot_upgrade_old_or_coverage_only_groups() {
        for coverage in [false, true] {
            let ledger = TestLedger::new();
            let config: ModelBudgetConfig = serde_json::from_value(json!({
                "group_id":"repair", "max_calls":4, "allow_coverage_repair":coverage
            }))
            .unwrap();
            let old = ModelBudget::new(
                &ledger.path,
                "fixture".into(),
                config.clone(),
                "original".into(),
            )
            .unwrap();
            old.reserve(ModelStage::Extraction, b"original").unwrap();
            let db = Connection::open(&ledger.path).unwrap();
            let policy = || {
                db.query_row("SELECT policy_json FROM model_budget_groups", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap()
            };
            let before = policy();
            let mut changed = serde_json::to_value(config).unwrap();
            changed["allow_extraction_repair"] = json!(true);
            let upgraded = ModelBudget::new(
                &ledger.path,
                "fixture".into(),
                serde_json::from_value(changed).unwrap(),
                "new-operation".into(),
            )
            .unwrap();
            for stage in [
                ModelStage::Extraction,
                ModelStage::ExtractionRepair,
                ModelStage::ExtractionRepairReview,
            ] {
                assert!(matches!(upgraded.reserve(stage, b"upgrade"),
                    Err(OpenAiAdapterError::Budget(message)) if message.contains("persisted authorization")));
            }
            old.reserve(ModelStage::ExtractionReview, b"old review")
                .unwrap();
            assert_eq!(policy(), before);
            assert_eq!(
                db.query_row("SELECT count(*) FROM model_calls", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                2
            );
        }
    }

    #[test]
    fn coverage_repair_requires_explicit_four_call_authorization() {
        let legacy = json!({"group_id":"legacy", "max_calls":4});
        let config: ModelBudgetConfig = serde_json::from_value(legacy).unwrap();
        assert!(
            serde_json::to_value(&config)
                .unwrap()
                .get("allow_coverage_repair")
                .is_none()
        );
        for allowed in 1..=4 {
            let config: ModelBudgetConfig = serde_json::from_value(json!({
                "group_id":"repair", "max_calls":4,
                "max_operation_calls":allowed, "allow_coverage_repair":true
            }))
            .unwrap();
            assert_eq!(config.validate().is_ok(), allowed == 4);
            assert_eq!(
                serde_json::to_value(config).unwrap()["allow_coverage_repair"],
                true
            );
        }
        let config: ModelBudgetConfig = serde_json::from_value(json!({
            "group_id":"legacy", "max_calls":4, "allow_coverage_repair":false
        }))
        .unwrap();
        assert!(config.validate().is_ok());
        assert!(
            serde_json::to_value(config)
                .unwrap()
                .get("allow_coverage_repair")
                .is_none()
        );
    }

    #[test]
    fn coverage_repair_is_denied_before_creating_a_ledger_without_opt_in() {
        for stage in [
            ModelStage::ExtractionRepair,
            ModelStage::ExtractionRepairReview,
        ] {
            let ledger = TestLedger::new();
            assert!(matches!(
                ledger.budget("legacy", 4).reserve(stage, b"unauthorized repair"),
                Err(OpenAiAdapterError::Budget(message)) if message.contains("coverage repair is not authorized")
            ));
            assert!(!ledger.path.exists());
        }
    }

    #[test]
    fn coverage_repair_stays_in_one_durable_four_call_operation() {
        let ledger = TestLedger::new();
        let config = ModelBudgetConfig {
            group_id: "repair".into(),
            max_calls: 5,
            allow_coverage_repair: true,
            extraction_output_tokens: 8192,
            review_output_tokens: 16384,
            ..Default::default()
        };
        let reopen = |operation: &str| {
            ModelBudget::new(
                &ledger.path,
                "fixture".into(),
                config.clone(),
                operation.into(),
            )
            .unwrap()
        };
        let stages = [
            ModelStage::Extraction,
            ModelStage::ExtractionReview,
            ModelStage::ExtractionRepair,
            ModelStage::ExtractionRepairReview,
        ];
        for (index, stage) in stages.into_iter().enumerate() {
            // Reopen the durable ledger between every stage, including the correction.
            let budget = reopen("same-operation");
            let receipt = budget
                .reserve(stage, b"request")
                .unwrap()
                .settle(
                    "completed",
                    Some(&json!({"input_tokens":10,"output_tokens":2})),
                )
                .unwrap();
            assert_eq!(receipt["group_id"], "repair");
            assert_eq!(receipt["operation_id"], "same-operation");
            assert_eq!(receipt["stage"], stage.name());
            assert!(budget.reserve(stage, b"repeat").is_err());
            let db = Connection::open(&ledger.path).unwrap();
            assert_eq!(
                db.query_row("SELECT count(*) FROM model_calls", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                index as i64 + 1
            );
        }
        assert!(
            matches!(reopen("same-operation").reserve(ModelStage::Query, b"fifth"),
            Err(OpenAiAdapterError::Budget(message)) if message == "operation call limit reached")
        );
        // A distinct operation can use only the allowance genuinely left in this group.
        reopen("next-operation")
            .reserve(ModelStage::Extraction, b"last group call")
            .unwrap();
        assert!(
            matches!(reopen("another-operation").reserve(ModelStage::Extraction, b"sixth"),
            Err(OpenAiAdapterError::Budget(message)) if message == "budget group call limit reached")
        );
        let db = Connection::open(&ledger.path).unwrap();
        let rows: Vec<(String, u32)> = db.prepare(
            "SELECT stage, output_token_limit FROM model_calls WHERE operation_id='same-operation' ORDER BY rowid"
        ).unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
        assert_eq!(
            rows,
            vec![
                ("extraction".into(), 8192),
                ("extraction_review".into(), 16384),
                ("extraction_repair".into(), 8192),
                ("extraction_repair_review".into(), 16384),
            ]
        );
    }

    #[test]
    fn coverage_repair_cannot_upgrade_an_existing_group_authorization() {
        let ledger = TestLedger::new();
        let legacy = ledger.budget("original", 4);
        legacy
            .reserve(ModelStage::Extraction, b"original request")
            .unwrap();
        let mut upgraded = legacy.config.clone();
        upgraded.allow_coverage_repair = true;
        let changed = ModelBudget::new(
            &ledger.path,
            "fixture".into(),
            upgraded,
            "new-operation".into(),
        )
        .unwrap();
        for stage in [
            ModelStage::Extraction,
            ModelStage::ExtractionRepair,
            ModelStage::ExtractionRepairReview,
        ] {
            assert!(matches!(changed.reserve(stage, b"changed authorization"),
                Err(OpenAiAdapterError::Budget(message)) if message.contains("persisted authorization")));
        }
        // Legacy extraction/review still work against the unchanged old policy.
        legacy
            .reserve(ModelStage::ExtractionReview, b"old review")
            .unwrap();
        assert!(
            legacy
                .reserve(ModelStage::ExtractionRepair, b"old flag")
                .is_err()
        );
        let db = Connection::open(&ledger.path).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM model_calls", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        let policy: String = db
            .query_row("SELECT policy_json FROM model_budget_groups", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(
            serde_json::from_str::<Value>(&policy).unwrap()["limits"]
                .get("allow_coverage_repair")
                .is_none()
        );
    }

    #[test]
    fn policy_comparison_ignores_json_order_but_preserves_authorized_limits() {
        let ledger = TestLedger::new();
        ledger
            .budget("first", 4)
            .reserve(ModelStage::Extraction, b"first")
            .unwrap();
        let db = Connection::open(&ledger.path).unwrap();
        let reordered = r#"{
            "limits": {"review_output_tokens":4096,"query_output_tokens":4096,
                "max_operation_calls":4,"max_input_bytes":256000,"max_calls":4,
                "group_id":"fixture","extraction_output_tokens":4096},
            "version":1,"per_stage_calls":1
        }"#;
        db.execute("UPDATE model_budget_groups SET policy_json=?1", [reordered])
            .unwrap();
        ledger
            .budget("reopened", 4)
            .reserve(ModelStage::Extraction, b"second")
            .unwrap();
        assert_eq!(
            db.query_row("SELECT policy_json FROM model_budget_groups", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            reordered,
            "opening the same authorization must not rewrite its stored contract"
        );

        let original: Value = serde_json::from_str(reordered).unwrap();
        let mut rejected = vec!["invalid JSON".to_owned(), "null".to_owned()];
        for pointer in [
            "/version",
            "/per_stage_calls",
            "/limits/max_calls",
            "/limits/max_operation_calls",
            "/limits/max_input_bytes",
            "/limits/extraction_output_tokens",
            "/limits/review_output_tokens",
            "/limits/query_output_tokens",
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() =
                json!(original.pointer(pointer).unwrap().as_u64().unwrap() + 1);
            rejected.push(changed.to_string());
        }
        for policy in rejected {
            db.execute("UPDATE model_budget_groups SET policy_json=?1", [&policy])
                .unwrap();
            assert!(
                ledger
                    .budget("rejected", 4)
                    .reserve(ModelStage::Extraction, b"must not reserve")
                    .is_err(),
                "changed or corrupt authorization was accepted: {policy}"
            );
        }
        assert_eq!(
            db.query_row("SELECT count(*) FROM model_calls", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2,
            "a rejected contract must not consume or reset allowance"
        );
    }

    #[test]
    fn write_output_caps_bind_new_authorizations_and_preserve_legacy_defaults() {
        let ledger = TestLedger::new();
        let budget = ledger.budget("write-operation", 10);
        for (stage, limit) in [
            (ModelStage::W1, 32768),
            (ModelStage::W3, 16384),
            (ModelStage::W4, 16384),
        ] {
            assert_eq!(stage.output_tokens(&budget.config), limit);
            let request = crate::structured_request(
                stage,
                "test",
                "prompt",
                json!({}),
                json!({}),
                &budget.config,
            );
            assert_eq!(request["max_output_tokens"], limit);
            budget
                .reserve(stage, b"write")
                .unwrap()
                .settle("completed", None)
                .unwrap();
        }
        for stage in [
            ModelStage::Extraction,
            ModelStage::ExtractionReview,
            ModelStage::ExtractionRepair,
            ModelStage::ExtractionRepairReview,
            ModelStage::Query,
            ModelStage::QueryReview,
            ModelStage::Assistance,
            ModelStage::Answer,
            ModelStage::Reader,
            ModelStage::Judge,
        ] {
            assert_eq!(stage.output_tokens(&ModelBudgetConfig::default()), 4096);
        }
        let db = Connection::open(&ledger.path).unwrap();
        let rows: Vec<(String, u32, u32)> = db.prepare(
            "SELECT stage,output_token_limit,reserved_output_tokens FROM model_calls ORDER BY rowid"
        ).unwrap().query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))
            .unwrap().map(Result::unwrap).collect();
        assert_eq!(
            rows,
            vec![
                ("w1".into(), 32768, 32768),
                ("w3".into(), 16384, 16384),
                ("w4".into(), 16384, 16384)
            ]
        );
        // 模拟升级前授权：移除新额度绑定，重开仍可旧阶段，不准提高后台额度。
        let policy: String = db
            .query_row("SELECT policy_json FROM model_budget_groups", [], |r| {
                r.get(0)
            })
            .unwrap();
        let mut old: Value = serde_json::from_str(&policy).unwrap();
        old.as_object_mut().unwrap().remove("write_output_tokens");
        db.execute(
            "UPDATE model_budget_groups SET policy_json=?1",
            [old.to_string()],
        )
        .unwrap();
        let reopened = ledger.budget("reopened", 10);
        reopened.reserve(ModelStage::Extraction, b"legacy").unwrap();
        assert!(
            matches!(reopened.reserve(ModelStage::W1, b"unauthorized increase"),
            Err(OpenAiAdapterError::Budget(message)) if message.contains("persisted authorization"))
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM model_calls", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            4
        );
        let stored: String = db
            .query_row("SELECT policy_json FROM model_budget_groups", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(stored, old.to_string());
    }

    #[test]
    fn larger_output_caps_require_new_authorization_and_keep_call_limits() {
        let ledger = TestLedger::new();
        let mut config: ModelBudgetConfig =
            serde_json::from_value(json!({"group_id":"fixture","max_calls":2})).unwrap();
        for defaults in [&config, &ModelBudgetConfig::default()] {
            assert_eq!(
                [
                    defaults.extraction_output_tokens,
                    defaults.review_output_tokens,
                    defaults.query_output_tokens
                ],
                [4096; 3]
            );
        }
        for valid in [4096, 8192, 16384, 32768, 65536] {
            config.extraction_output_tokens = valid;
            config.review_output_tokens = valid;
            config.query_output_tokens = valid;
            config.validate().unwrap();
            for stage in [
                ModelStage::Extraction,
                ModelStage::ExtractionReview,
                ModelStage::ExtractionRepair,
                ModelStage::ExtractionRepairReview,
                ModelStage::QueryReview,
            ] {
                assert_eq!(
                    stage.timeout(&config),
                    Duration::from_secs(if valid > 16384 { 360 } else { 180 })
                );
            }
            assert_eq!(ModelStage::Query.timeout(&config), Duration::from_secs(15));
            assert_eq!(
                ModelStage::Assistance.timeout(&config),
                Duration::from_secs(180)
            );
        }
        for field in [
            "extraction_output_tokens",
            "review_output_tokens",
            "query_output_tokens",
        ] {
            for invalid in [0, 65537] {
                let mut value = serde_json::to_value(&config).unwrap();
                value[field] = json!(invalid);
                assert!(
                    serde_json::from_value::<ModelBudgetConfig>(value)
                        .unwrap()
                        .validate()
                        .is_err()
                );
            }
        }

        ledger
            .budget("original", 2)
            .reserve(ModelStage::Extraction, b"old")
            .unwrap();
        let db = Connection::open(&ledger.path).unwrap();
        let original_policy: String = db
            .query_row(
                "SELECT policy_json FROM model_budget_groups WHERE group_id='fixture'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let changed = ModelBudget::new(
            &ledger.path,
            "fixture".into(),
            config.clone(),
            "changed".into(),
        )
        .unwrap();
        assert!(
            matches!(changed.reserve(ModelStage::Extraction, b"rejected"),
            Err(OpenAiAdapterError::Budget(message)) if message.contains("policy differs"))
        );
        assert_eq!(
            db.query_row(
                "SELECT policy_json FROM model_budget_groups WHERE group_id='fixture'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            original_policy
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM model_calls", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );

        config.group_id = "larger-output".into();
        config.max_calls = 1;
        let larger =
            ModelBudget::new(&ledger.path, "fixture".into(), config.clone(), "new".into()).unwrap();
        let reservation = larger.reserve(ModelStage::Extraction, b"new").unwrap();
        let report = reservation.settle("failed", None).unwrap();
        assert_eq!(report["usage_status"], "unknown");
        assert_eq!(report["output_reservation_retained"], true);
        assert_eq!(db.query_row(
            "SELECT output_token_limit,reserved_output_tokens FROM model_calls WHERE group_id='larger-output'",
            [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        ).unwrap(), (65536, 65536));
        let restarted = ModelBudget::new(
            &ledger.path,
            "fixture".into(),
            config,
            "another-operation".into(),
        )
        .unwrap();
        assert!(
            matches!(restarted.reserve(ModelStage::Extraction, b"over limit"),
            Err(OpenAiAdapterError::Budget(message)) if message.contains("group call limit"))
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM model_calls WHERE group_id='larger-output'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn durable_limits_usage_and_failed_settlement() {
        let ledger = TestLedger::new();
        let budget = ledger.budget("one", 4);
        let first = budget.reserve(ModelStage::Extraction, b"request").unwrap();
        assert!(budget.reserve(ModelStage::Extraction, b"replay").is_err());
        let known = first
            .settle(
                "completed",
                Some(&json!({"input_tokens":7,"output_tokens":3})),
            )
            .unwrap();
        assert_eq!(known["output_reservation_retained"], false);
        let second = budget
            .reserve(ModelStage::ExtractionReview, b"review")
            .unwrap();
        assert_eq!(
            second.settle("failed", None).unwrap()["usage_status"],
            "unknown"
        );
        // Reopening the same persistent allowance cannot refund successful or failed attempts.
        let restarted = ledger.budget("one", 4);
        assert!(
            restarted
                .reserve(ModelStage::ExtractionReview, b"retry")
                .is_err()
        );
        assert!(
            ledger
                .budget("two", 5)
                .reserve(ModelStage::Query, b"changed allowance")
                .is_err()
        );
        let lost = ledger
            .budget("two", 4)
            .reserve(ModelStage::Query, b"lost process")
            .unwrap();
        drop(lost);
        let fourth = ledger
            .budget("three", 4)
            .reserve(ModelStage::Query, b"next operation")
            .unwrap();
        fourth
            .settle("incomplete", Some(&json!({"input_tokens":5})))
            .unwrap();
        assert!(
            ledger
                .budget("four", 4)
                .reserve(ModelStage::Query, b"over budget")
                .is_err()
        );
        let db = Connection::open(&ledger.path).unwrap();
        let rows: (i64,i64,i64) = db.query_row("SELECT count(*),sum(reserved_output_tokens),sum(usage_status='unknown') FROM model_calls",[],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(rows, (4, 3 * 4096, 3));
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM model_calls WHERE outcome='reserved'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );

        let limited = TestLedger::new();
        let mut config = ModelBudgetConfig {
            group_id: "small".into(),
            max_calls: 10,
            max_operation_calls: 1,
            max_input_bytes: 7,
            ..Default::default()
        };
        let budget = ModelBudget::new(
            &limited.path,
            "fixture".into(),
            config.clone(),
            "one".into(),
        )
        .unwrap();
        assert!(budget.reserve(ModelStage::Extraction, b"too long").is_err());
        assert!(
            !limited.path.exists(),
            "oversized input must not write a ledger"
        );
        let reservation = budget.reserve(ModelStage::Extraction, b"small").unwrap();
        assert!(budget.reserve(ModelStage::Query, b"small").is_err());
        config.max_operation_calls = 2;
        assert!(
            ModelBudget::new(&limited.path, "fixture".into(), config, "two".into())
                .unwrap()
                .reserve(ModelStage::Query, b"small")
                .is_err()
        );
        // A failed settlement preserves its original durable reservation rather than reporting success.
        let db = Connection::open(&limited.path).unwrap();
        db.execute_batch("CREATE TRIGGER block_budget_settlement BEFORE UPDATE ON model_calls BEGIN SELECT RAISE(FAIL,'fixture'); END;").unwrap();
        assert!(
            reservation
                .settle(
                    "completed",
                    Some(&json!({"input_tokens":1,"output_tokens":1}))
                )
                .is_err()
        );
        assert_eq!(
            db.query_row("SELECT reserved_output_tokens FROM model_calls", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
            4096
        );
        assert!(
            ModelBudgetConfig {
                max_calls: 0,
                ..ModelBudgetConfig::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn budget_process_worker() {
        let Some(path) = std::env::var_os("PMR_BUDGET_TEST_DB") else {
            return;
        };
        let budget = ModelBudget::new(
            Path::new(&path),
            "worker".into(),
            ModelBudgetConfig {
                group_id: "process-race".into(),
                max_calls: 1,
                ..Default::default()
            },
            format!("process-{}", std::process::id()),
        )
        .unwrap();
        match budget.reserve(ModelStage::Extraction, b"race") {
            Ok(_) => (),
            Err(OpenAiAdapterError::Budget(message)) => {
                assert!(message.contains("call limit reached"), "{message}")
            }
            Err(error) => panic!("{error}"),
        }
    }

    #[test]
    fn separate_processes_compete_for_last_allowance() {
        let ledger = TestLedger::new();
        let mut workers: Vec<_> = (0..4)
            .map(|_| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "model_budget::tests::budget_process_worker"])
                    .env("PMR_BUDGET_TEST_DB", &ledger.path)
                    .env("PMR_MODEL_NETWORK", "deny")
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for worker in &mut workers {
            assert!(worker.wait().unwrap().success());
        }
        let db = Connection::open(&ledger.path).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM model_calls", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}
