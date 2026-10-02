-- v8-9933ece: released 9933ece writer upgraded the genuine v7-ca16b64 w4_01 database and added a failed legacy task.
-- Regenerate only from the commit itself; do not edit by hand.
BEGIN;
CREATE TABLE memory_profiles (owner_id TEXT PRIMARY KEY, state_json TEXT NOT NULL);
CREATE TABLE source_indexes (
            owner_id TEXT PRIMARY KEY, generation INTEGER NOT NULL,
            processing_version TEXT NOT NULL, source_revision INTEGER NOT NULL);
CREATE TABLE source_segments (
            id INTEGER PRIMARY KEY, owner_id TEXT NOT NULL, source_id TEXT NOT NULL,
            source_version INTEGER NOT NULL, generation INTEGER NOT NULL, task_id TEXT,
            received_sequence INTEGER NOT NULL, byte_start INTEGER NOT NULL, byte_end INTEGER NOT NULL,
            text TEXT NOT NULL, index_text TEXT NOT NULL,
            UNIQUE(owner_id, generation, source_id, byte_start));
CREATE VIRTUAL TABLE source_fts USING fts5(index_text,
            content='source_segments', content_rowid='id', tokenize='unicode61 remove_diacritics 0');
CREATE TABLE model_budget_groups (
        group_id TEXT PRIMARY KEY, policy_json TEXT NOT NULL);
CREATE TABLE model_calls (
        call_id TEXT PRIMARY KEY, group_id TEXT NOT NULL, owner_id TEXT NOT NULL,
        operation_id TEXT NOT NULL, stage TEXT NOT NULL, request_fingerprint TEXT NOT NULL,
        input_bytes INTEGER NOT NULL, output_token_limit INTEGER NOT NULL,
        reserved_output_tokens INTEGER NOT NULL, outcome TEXT NOT NULL,
        usage_status TEXT NOT NULL, input_tokens INTEGER, output_tokens INTEGER,
        created_at INTEGER NOT NULL);
CREATE TABLE fact_indexes (
        owner_id TEXT PRIMARY KEY, manifest_json TEXT NOT NULL, hints_json TEXT NOT NULL);
INSERT INTO "memory_profiles" (owner_id, state_json) VALUES ('alice', '{"repository":{"next_id":2,"memories":{"1":{"id":1,"owner_id":"alice","evidence":[{"id":"s-101","session_id":"chat","turn_id":"s-101","quote":"我住在北京。"}],"subject_id":"self","memory_type":"Fact","predicate":"lives_in","value":{"Text":"北京"},"polarity":null,"strength":null,"scope":{"domain":null,"conditions":[]},"retention_tier":"LongTerm","status":"Superseded","temporal":{"observed_at":100,"valid_from":null,"valid_to":{"at":1785513600,"source":"Inferred"},"expires_at":null,"reference":null},"extraction_confidence":"High","certainty":"High","supersedes":null,"superseded_by":2,"supersede_reason":"StateChanged","explicit_memory_request":false,"reinforcement_count":0,"retrieval_count":0,"last_reinforced_at":null,"last_retrieved_at":null,"tags":[]},"2":{"id":2,"owner_id":"alice","evidence":[{"id":"s-205","session_id":"chat","turn_id":"s-205","quote":"我上个月从北京搬到上海了。"}],"subject_id":"self","memory_type":"Fact","predicate":"lives_in","value":{"Text":"上海"},"polarity":null,"strength":null,"scope":{"domain":null,"conditions":[]},"retention_tier":"LongTerm","status":"Active","temporal":{"observed_at":200,"valid_from":{"at":1785513600,"source":"Inferred"},"valid_to":null,"expires_at":null,"reference":null},"extraction_confidence":"High","certainty":"High","supersedes":1,"superseded_by":null,"supersede_reason":null,"explicit_memory_request":false,"reinforcement_count":0,"retrieval_count":0,"last_reinforced_at":null,"last_retrieved_at":null,"tags":[]}}},"entities":{"entities":{}},"staging":{"next_id":0,"pending":{}},"receipts":{},"sources":{"legacy":{"id":"legacy","session_id":"chat","turn_id":"legacy","observed_at":1700000000,"received_sequence":1,"request":{"capture_source":true,"text":"旧任务原文"},"enrichment":"failed","error":"interrupted","result":null,"base_revision":0,"index":{"generation":1,"processing_version":"cjk-position-v1","source_revision":2,"source_version":1,"status":"ready"},"task":{"processing_version":null,"attempt_count":0,"attempt_id":null,"lease_until":null,"claim_revision":null,"accepted_mode":null}},"s-205":{"id":"s-205","session_id":"chat","turn_id":"s-205","observed_at":200,"received_sequence":1,"request":{"text":"s-205"},"enrichment":"running","error":null,"result":null,"base_revision":1,"index":{"generation":1,"processing_version":"cjk-position-v1","source_revision":2,"source_version":1,"status":"not_requested"},"task":{"processing_version":"w5-acceptance","attempt_count":1,"attempt_id":"attempt-1","lease_until":1790835099,"claim_revision":1,"accepted_mode":"inline"}}},"revision":2,"source_revision":2,"last_applied_source_sequence":1,"source_less_barrier_sequence":0,"w5_receipts":{"s-205#0":{"group_id":"s-205#0","fingerprint":"{\"id\":\"s-205#0\",\"source_key\":\"s-205\",\"candidates\":[{\"index\":0,\"candidate\":{\"owner_id\":\"alice\",\"evidence\":[{\"id\":\"s-205\",\"session_id\":\"chat\",\"turn_id\":\"s-205\",\"quote\":\"我上个月从北京搬到上海了。\"}],\"subject\":{\"Resolved\":\"self\"},\"memory_type\":\"Fact\",\"predicate\":\"lives_in\",\"value\":{\"Text\":\"上海\"},\"polarity\":null,\"strength\":null,\"scope\":{\"domain\":null,\"conditions\":[]},\"retention_tier\":\"LongTerm\",\"temporal\":{\"observed_at\":200,\"valid_from\":null,\"valid_to\":null,\"expires_at\":null,\"reference\":null},\"extraction_confidence\":\"High\",\"certainty\":\"High\",\"source\":\"UserStatement\",\"intent\":\"Assert\",\"explicit_memory_request\":false,\"cross_turn_useful\":true,\"tags\":[]}}],\"decisions\":[{\"candidate\":0,\"action\":\"StateChange\",\"targets\":[1],\"basis_sources\":[\"s-101\",\"s-205\"],\"reason\":null,\"effective\":{\"at\":1785513600,\"source\":\"Inferred\"}}],\"seen\":[{\"id\":1,\"fingerprint\":\"{\\\"id\\\":1,\\\"owner_id\\\":\\\"alice\\\",\\\"evidence\\\":[{\\\"id\\\":\\\"s-101\\\",\\\"session_id\\\":\\\"chat\\\",\\\"turn_id\\\":\\\"s-101\\\",\\\"quote\\\":\\\"我住在北京。\\\"}],\\\"subject_id\\\":\\\"self\\\",\\\"memory_type\\\":\\\"Fact\\\",\\\"predicate\\\":\\\"lives_in\\\",\\\"value\\\":{\\\"Text\\\":\\\"北京\\\"},\\\"polarity\\\":null,\\\"strength\\\":null,\\\"scope\\\":{\\\"domain\\\":null,\\\"conditions\\\":[]},\\\"retention_tier\\\":\\\"LongTerm\\\",\\\"status\\\":\\\"Active\\\",\\\"temporal\\\":{\\\"observed_at\\\":100,\\\"valid_from\\\":null,\\\"valid_to\\\":null,\\\"expires_at\\\":null,\\\"reference\\\":null},\\\"extraction_confidence\\\":\\\"High\\\",\\\"certainty\\\":\\\"High\\\",\\\"supersedes\\\":null,\\\"superseded_by\\\":null,\\\"supersede_reason\\\":null,\\\"explicit_memory_request\\\":false,\\\"reinforcement_count\\\":0,\\\"retrieval_count\\\":0,\\\"last_reinforced_at\\\":null,\\\"last_retrieved_at\\\":null,\\\"tags\\\":[]}\"}]}","status":"Committed","legacy_fingerprint":false,"products":{"0":2},"decisions":[{"candidate":0,"action":"StateChange","targets":[1],"basis_sources":["s-101","s-205"],"reason":null,"effective":{"at":1785513600,"source":"Inferred"}}],"updates":{},"merged":{},"form":"pre8"}},"relations":{},"next_relation":0,"record_updates":{}}');
INSERT INTO "source_indexes" (owner_id, generation, processing_version, source_revision) VALUES ('alice', 1, 'cjk-position-v1', 2);
INSERT INTO "source_segments" (id, owner_id, source_id, source_version, generation, task_id, received_sequence, byte_start, byte_end, text, index_text) VALUES (1, 'alice', 'legacy', 1, 1, NULL, 1, 0, 15, '旧任务原文', 'c65e7 c4efb c52a1 c539f c6587');
CREATE INDEX source_segments_scope ON source_segments(owner_id, generation, task_id);
CREATE TRIGGER source_segments_insert AFTER INSERT ON source_segments BEGIN
            INSERT INTO source_fts(rowid, index_text) VALUES (new.id, new.index_text);
         END;
CREATE TRIGGER source_segments_delete AFTER DELETE ON source_segments BEGIN
            INSERT INTO source_fts(source_fts, rowid, index_text) VALUES ('delete', old.id, old.index_text);
         END;
CREATE TRIGGER source_segments_update AFTER UPDATE ON source_segments BEGIN
            INSERT INTO source_fts(source_fts, rowid, index_text) VALUES ('delete', old.id, old.index_text);
            INSERT INTO source_fts(rowid, index_text) VALUES (new.id, new.index_text);
         END;
CREATE INDEX model_calls_group ON model_calls(group_id);
CREATE UNIQUE INDEX model_calls_operation_stage
        ON model_calls(group_id,owner_id,operation_id,stage);
COMMIT;
PRAGMA user_version = 8;
