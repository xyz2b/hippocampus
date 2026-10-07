-- Historical table definitions from w5_storage/v5.sql (no profile/data rewrite).
CREATE TABLE model_budget_groups (
        group_id TEXT PRIMARY KEY, policy_json TEXT NOT NULL);
CREATE TABLE model_calls (
        call_id TEXT PRIMARY KEY, group_id TEXT NOT NULL, owner_id TEXT NOT NULL,
        operation_id TEXT NOT NULL, stage TEXT NOT NULL, request_fingerprint TEXT NOT NULL,
        input_bytes INTEGER NOT NULL, output_token_limit INTEGER NOT NULL,
        reserved_output_tokens INTEGER NOT NULL, outcome TEXT NOT NULL,
        usage_status TEXT NOT NULL, input_tokens INTEGER, output_tokens INTEGER,
        created_at INTEGER NOT NULL);
CREATE INDEX model_calls_group ON model_calls(group_id);
CREATE UNIQUE INDEX model_calls_operation_stage
        ON model_calls(group_id,owner_id,operation_id,stage);
