#!/usr/bin/env python3
"""S5 batch A contracts: disposable databases, denied network or synthetic loopback only."""
import argparse
import concurrent.futures
import contextlib
import http.server
import json
import os
import sqlite3
import subprocess
import tempfile
import threading
from pathlib import Path

from mcp_smoke import Client, Responses


class Fixture(Responses):
    mode = "success"
    assistance_mode = "success"
    requests = []
    assistance_queries = []
    clarification_proposal = {}
    pause_assistance = False
    planner_entered = threading.Event()
    planner_release = threading.Event()

    def output(self, request):
        if request["text"]["format"]["name"] == "personal_memory_assistance_queries_v1":
            return {"queries": type(self).assistance_queries}
        if request["text"]["format"]["name"] == "personal_memory_clarification_v1":
            return type(self).clarification_proposal
        output = super().output(request)
        if self.mode == "project_budget" and "candidates" in output:
            output["candidates"][0].update(
                subject_id="松石47", memory_type="fact", predicate="fact",
                value=dict(kind="text", text=json.loads(request["input"])["current_user_turn"], integer=None, boolean=None, entity_id=None),
                explicit_memory_request=False)
        if self.mode == "phone_cause" and "candidates" in output:
            output["candidates"][0].update(
                subject_id="王阿姨", memory_type="fact", predicate="fact",
                value=dict(kind="text", text=json.loads(request["input"])["current_user_turn"], integer=None, boolean=None, entity_id=None),
                explicit_memory_request=False)
        return output

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        assert self.path == "/v1/responses"
        assert self.headers["Authorization"] == "Bearer local-fixture-key"
        type(self).requests.append(request)
        assistance = request["text"]["format"]["name"] in (
            "personal_memory_assistance_queries_v1", "personal_memory_clarification_v1")
        if assistance and self.pause_assistance:
            type(self).planner_entered.set()
            assert type(self).planner_release.wait(15), "timed out waiting for the concurrent deletion"
        if assistance and self.assistance_mode == "timeout":
            threading.Event().wait(16)  # The production assistance deadline is 15 seconds.
            return
        if assistance and self.assistance_mode == "http":
            data = json.dumps({"error": {"message": "fixture unavailable"}}).encode()
            self.send_response(503)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        review = request["text"]["format"]["name"] == "personal_memory_source_support_v1"
        incomplete = (assistance and self.assistance_mode == "incomplete"
                      or self.mode == "initial_incomplete" or self.mode == "review_incomplete" and review)
        response = {
            "status": "incomplete" if incomplete else "completed",
            "usage": {"input_tokens": 10, "output_tokens": 10},
            "output": [{"type": "message", "content": [{
                "type": "output_text", "text": "not-json" if assistance and self.assistance_mode == "bad_json"
                else json.dumps(self.output(request), ensure_ascii=False)
            }]}],
        }
        if incomplete:
            response["incomplete_details"] = {"reason": "max_output_tokens"}
        data = json.dumps(response).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def state(db):
    with sqlite3.connect(db) as connection:
        row = connection.execute(
            "SELECT state_json FROM memory_profiles WHERE owner_id='s5-synthetic'"
        ).fetchone()
    return json.loads(row[0]) if row else {"sources": {}, "receipts": {}}


def business_rows(db):
    with sqlite3.connect(db) as connection:
        return {table: connection.execute(f"SELECT * FROM {table} ORDER BY rowid").fetchall()
                for table in ("memory_profiles", "source_indexes", "source_segments", "fact_indexes")}


def config(root, name, **values):
    path = root / (name + ".json")
    path.write_text(json.dumps(values))
    return path


@contextlib.contextmanager
def client(binary, db, env, settings):
    current = Client(binary, db, env, "s5-synthetic", config=settings)
    try:
        yield current
    finally:
        current.close()


def turn(identifier, text="合成验收雪松65使用 SQLite。"):
    return dict(text=text, session_id="s5-synthetic", turn_id=identifier, capture_source=True)


def source_status(result, enrichment):
    assert result["source"]["status"] == "stored", result
    assert result["index"]["status"] == "ready", result
    assert result["enrichment"]["status"] == enrichment, result


def local_result(result, phrase):
    assert result["retrieval_mode"] == "local", result
    assert result["backend_generation_calls_in_recall"] == 0, result
    assert result["model_calls"] == [], result
    assert any(phrase in item["text"] for item in result["source_evidence"]), result
    assert "structured_enrichment_incomplete" in result["degraded_reasons"], result


def events(trace):
    return [json.loads(line) for path in trace.glob("*.jsonl") for line in path.read_text().splitlines()]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/pmr-mcp"))
    parser.add_argument("--output", type=Path, help="Write the JSON report and an adjacent runtime-events JSONL")
    args = parser.parse_args()
    binary = args.binary.resolve()
    checks = []
    with contextlib.ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-s5-")))
        trace = root / "deny-trace"
        trace.mkdir(mode=0o700)
        clean = {k: v for k, v in os.environ.items() if not k.startswith(("DEEPSEEK_", "OPENAI_", "PMR_"))}
        denied = dict(clean, PMR_MODEL_NETWORK="deny", PMR_TRACE_DIR=str(trace))
        off = config(root, "off", retrieval_mode="local", enrichment_mode="off")
        inline = config(root, "inline", retrieval_mode="local", enrichment_mode="inline")
        db = root / "off.sqlite3"
        request = turn("off")
        with client(binary, db, denied, off) as c:
            bootstrap = c.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
            assert bootstrap["retrieval_mode"] == "bootstrap", bootstrap
            saved = c.call("memory_remember", request)
            source_status(saved, "not_requested")
            assert saved["status"] == "completed" and saved["enrichment_mode"] == "off", saved
            assert saved["outcomes"] == [] and not saved["replayed"], saved
            assert saved["model_calls"] == [], saved
            assert c.call("memory_list", {})["total"] == 0
            prepared = c.call("memory_prepare", {"user_turn": turn("prepare", "合成项目琥珀93使用 Rust。")})
            assert prepared["remember"]["status"] == prepared["recall"]["status"] == "completed", prepared
            source_status(prepared["remember"]["result"], "not_requested")
            local_result(prepared["recall"]["result"], "琥珀93")
            before = state(db)
            refused = c.call("memory_remember", {**turn("not-authorized"), "capture_source": False}, error=True)
            assert "capture_source_required" in json.dumps(refused), refused
            assert state(db) == before, "off must reject unauthorized source before storage"
            c.call("memory_remember", {**request, "text": "相同消息键不能更换原话。"}, error=True)
            assert state(db) == before
            local_result(c.call("memory_recall", {"query": "雪松65 SQLite"}), "雪松65")
            assert state(db) == before, "ordinary local recall must not mutate business state"
        checks.append("no credentials: local/off stores and indexes, prepare recalls, unauthorized capture rejects")

        with client(binary, db, denied, inline) as c:
            before = state(db)
            replay = c.call("memory_remember", request)
            source_status(replay, "not_requested")
            assert replay["replayed"] and replay["enrichment_mode"] == "off", replay
            assert replay["model_calls"] == [], replay
            assert replay["status"] != "in_progress", replay
            assert replay["source"]["source_id"] == saved["source"]["source_id"], replay
            assert state(db) == before, "configuration changes cannot re-enrich an accepted off receipt"
            local_result(c.call("memory_recall", {"query": "雪松65 SQLite"}), "雪松65")
            partial = c.call("memory_prepare", {"user_turn": turn("no-credentials", "合成白桦8426采用 SQLite。")})
            assert partial["remember"]["status"] == "failed", partial
            source_status(partial["remember"], "failed")
            assert partial["recall"]["status"] == "completed", partial
            local_result(partial["recall"]["result"], "白桦8426")
            assert c.call("memory_list", {})["total"] == 0
        checks.append("restart preserves off receipt and raw recall; inline failure still recalls new source")

        old = config(root, "old-local", local_recall_enabled=True, enrichment_mode="off")
        with client(binary, db, denied, old) as c:
            local_result(c.call("memory_recall", {"query": "雪松65 SQLite"}), "雪松65")
        legacy = config(root, "legacy", retrieval_mode="legacy", enrichment_mode="off")
        with client(binary, db, denied, legacy) as c:
            assert c.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})["retrieval_mode"] == "bootstrap"
        with client(binary, db, denied, off) as c:
            c.call("memory_forget", {"source_id": saved["source"]["source_id"]})
            tombstone = next(s for s in state(db)["sources"].values() if s["turn_id"] == "off")
            assert tombstone["request"] is None and tombstone["result"] == {"enrichment_mode": "off"}, tombstone
            for capture in (True, False):
                deleted = c.call("memory_remember", {**request, "capture_source": capture}, error=True)
                assert deleted["source"]["status"] == "deleted" and deleted["enrichment_mode"] == "off", deleted
                assert "original_result" not in deleted and "outcomes" not in deleted, deleted
                assert request["text"] not in json.dumps(deleted, ensure_ascii=False), deleted
        checks.append("deleted off receipt retains only its mode and cannot replay deleted content")
        for name, values, expected in [
            ("conflict", dict(retrieval_mode="legacy", local_recall_enabled=True), "conflict"),
            ("false-conflict", dict(retrieval_mode="local", local_recall_enabled=False), "conflict"),
        ]:
            settings = config(root, name, **values)
            rejected = subprocess.run(
                [str(binary), "--db", str(root / (name + ".sqlite3")), "--owner", "s5-synthetic", "--config", str(settings)],
                input="", text=True, capture_output=True, env=denied, timeout=10,
            )
            assert rejected.returncode != 0 and expected in rejected.stderr.lower(), rejected.stderr
        deferred = config(root, "deferred", retrieval_mode="local", enrichment_mode="deferred")
        deferred_db = root / "deferred.sqlite3"
        with client(binary, deferred_db, denied, deferred) as c:
            queued = c.call("memory_remember", turn("deferred", "合成验收枫树73使用 SQLite。"))
            source_status(queued, "queued")
            assert queued["status"] == "queued" and queued["model_calls"] == [], queued
            replay = c.call("memory_remember", turn("deferred", "合成验收枫树73使用 SQLite。"))
            assert replay["replayed"] and replay["status"] == "queued", replay
            assert c.call("memory_list", {})["total"] == 0
        assert next(iter(state(deferred_db)["sources"].values()))["enrichment"] == "queued"
        deny_events = events(trace)
        assert deny_events, "the actual denied-network MCP runs must leave diagnostics"
        assert not any(e["kind"] == "model_start" for e in deny_events)
        checks.append("old mode compatibility, explicit conflict rejection, bootstrap offline, deferred queues without a model send")

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        loopback_trace = root / "loopback-trace"
        loopback_trace.mkdir(mode=0o700)
        loopback = dict(clean, PMR_MODEL_NETWORK="offline", PMR_TRACE_DIR=str(loopback_trace),
                        DEEPSEEK_API_KEY="local-fixture-key", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1")
        external = root / "external-ledger.sqlite3"
        full_env = dict(loopback, PMR_MODEL_BUDGET_DB=str(external), PMR_MODEL_BUDGET_OWNER="wrong-owner",
                        PMR_MODEL_BUDGET_CONFIG=json.dumps(dict(group_id="wrong-group", max_calls=20)),
                        PMR_MODEL_OPERATION_ID="wrong-operation")
        partial_env = dict(loopback, PMR_MODEL_BUDGET_CONFIG="invalid-inherited-json")
        for label, polluted in [("none", loopback), ("full", full_env), ("partial", partial_env)]:
            with client(binary, root / "no-budget.sqlite3", polluted, inline) as c:
                refused = c.call("memory_prepare", {"user_turn": turn("no-budget-" + label)})
                source_status(refused["remember"], "failed")
                assert "budget" in json.dumps(refused["remember"]).lower(), refused
                assert refused["recall"]["status"] == "completed", refused
                assert refused["remember"]["model_calls"] == [], refused
                local_result(refused["recall"]["result"], "雪松65")
                assert not Fixture.requests, "credentials or inherited budgets cannot authorize an unbudgeted MCP"
                assert not external.exists(), "MCP must not create an inherited external ledger"
                assert c.call("memory_list", {})["total"] == 0
        checks.append("missing JSON budget refuses sending despite full/partial inherited budgets; raw recall survives")
        preserved_db = root / "preserve-facts.sqlite3"
        success = config(root, "success", retrieval_mode="local", enrichment_mode="inline",
                         model_budget=dict(group_id="success", max_calls=4))
        old_turn = turn("committed", "以后所有回答都必须使用中文。")
        for label, polluted in [("full", full_env), ("partial", partial_env)]:
            with client(binary, preserved_db, polluted, success) as c:
                accepted = old_turn if label == "full" else {**old_turn, "turn_id": "committed-partial"}
                source_status(c.call("memory_remember", accepted), "committed")
                assert c.call("memory_list", {})["total"] == 1
                assert not external.exists()
        with sqlite3.connect(preserved_db) as connection:
            calls = connection.execute("SELECT owner_id, operation_id, group_id FROM model_calls").fetchall()
        assert len(calls) == 4 and all(owner == "s5-synthetic" and operation != "wrong-operation" and group == "success"
                                       for owner, operation, group in calls), calls
        checks.append("valid JSON budget ignores full/partial ambient budgets and binds the actual database/owner/operation")
        with client(binary, preserved_db, denied, off) as c:
            existing = state(preserved_db)["repository"]
            source_status(c.call("memory_remember", turn("new-off")), "not_requested")
            old_replay = c.call("memory_remember", old_turn)
            source_status(old_replay, "committed")
            assert old_replay["enrichment_mode"] == "inline", old_replay
            assert state(preserved_db)["repository"] == existing, "off cannot clear or downgrade committed facts"
        checks.append("off preserves committed facts and the original inline receipt mode")
        fact_on = config(root, "fact-on", retrieval_mode="local", enrichment_mode="off", fact_index_enabled=True)
        with client(binary, preserved_db, denied, off) as c:
            baseline = c.call("memory_recall", {"query": "response_language"})
            assert baseline["source_evidence"] == [], baseline
        with client(binary, preserved_db, denied, fact_on) as c:
            before = state(preserved_db)
            enhanced = c.call("memory_recall", {"query": "response_language"})
            assert enhanced["fact_index"]["status"] == "ready", enhanced
            assert enhanced["source_evidence"], enhanced
            assert all("committed_fact_hint" in item["channels"] for item in enhanced["source_evidence"]), enhanced
            assert all(item["text"] == old_turn["text"] for item in enhanced["source_evidence"]), enhanced
            assert enhanced["global_context"] == baseline["global_context"], "fact keys cannot change global requirements"
            assert enhanced["model_calls"] == [] and enhanced["backend_generation_calls_in_recall"] == 0
            assert state(preserved_db) == before, "fact-key recall must remain read-only"
        checks.append("fact-key opt-in recalls raw evidence from committed synthetic facts with unchanged global context and no model calls")
        review_turn = "请保持使用汉语回答。"  # Fixture's 中文 value requires the existing independent verification.
        for mode, expected, calls in [
            ("initial_incomplete", "extraction_incomplete", 1),
            ("review_incomplete", "review_incomplete", 2),
            ("budget", "failed", 1),
        ]:
            Fixture.mode = mode
            settings = config(root, mode, retrieval_mode="local", enrichment_mode="inline",
                              model_budget=dict(group_id=mode, max_calls=1 if mode == "budget" else 4))
            failure_db = root / (mode + ".sqlite3")
            failed_turn = turn(mode, review_turn)
            before_calls = len(Fixture.requests)
            with client(binary, failure_db, loopback, settings) as c:
                failed = c.call("memory_prepare", {"user_turn": failed_turn})
                assert failed["remember"]["status"] == "failed", failed
                source_status(failed["remember"], expected)
                assert len(failed["remember"]["model_calls"]) == calls, failed
                assert failed["recall"]["status"] == "completed", failed
                local_result(failed["recall"]["result"], "汉语")
                assert len(Fixture.requests) == before_calls + calls, Fixture.requests
                assert c.call("memory_list", {})["total"] == 0, "incomplete or unreviewed candidates must not commit"
                if mode == "budget":
                    assert "budget" in json.dumps(failed["remember"]).lower(), failed
            with client(binary, failure_db, loopback, settings) as c:
                before = state(failure_db)
                replay = c.call("memory_remember", failed_turn, error=True)
                source_status(replay, expected)
                assert replay["model_calls"] == [], replay
                assert state(failure_db) == before
                assert len(Fixture.requests) == before_calls + calls
                if mode == "budget":
                    refused = c.call("memory_remember", turn("after-restart", review_turn), error=True)
                    source_status(refused, "failed")
                    assert "budget" in json.dumps(refused).lower(), refused
                    assert len(Fixture.requests) == before_calls + calls, "restart cannot reset the exhausted group"
        checks.append("initial/review incomplete and exhausted review budget preserve raw source without partial facts")

        Fixture.mode = "project_budget"
        prepare_db = root / "prepare-e2e.sqlite3"
        statement = "松石47的维护预算是 500 元。"
        question = "松石47的维护预算是多少？"
        prepare_settings = config(root, "prepare-e2e", retrieval_mode="local", enrichment_mode="inline",
                                  fact_index_enabled=True,
                                  model_budget=dict(group_id="prepare-e2e", max_calls=3))
        with client(binary, prepare_db, loopback, prepare_settings) as c:
            c.call("memory_entity_upsert", dict(id="松石47", canonical_name="松石47",
                                                aliases=[], entity_type="Other"))
            prepared = c.call("memory_prepare", {"user_turn": turn("prepare-e2e", statement)})
            assert prepared["remember"]["status"] == prepared["recall"]["status"] == "completed", prepared
            source_status(prepared["remember"]["result"], "committed")
            assert prepared["recall"]["result"]["retrieval_mode"] == "local", prepared
            listing = c.call("memory_list", {})
            assert listing["total"] == 1, listing
            assert c.stored_memories()[0]["predicate"] == "fact", listing
            assert c.stored_memories()[0]["evidence"][0]["quote"] == statement, listing
            source_id = prepared["remember"]["result"]["source"]["source_id"]
            local = c.call("memory_recall", {"query": question})
            assert local["model_calls"] == [], local
            assert [(item["source_id"], item["text"]) for item in local["source_evidence"]] == [(source_id, statement)], local
        before_business = state(prepare_db)
        Fixture.assistance_queries = ["维护预算"]
        assisted_prepare = config(root, "prepare-e2e-assisted", retrieval_mode="assisted",
                                  enrichment_mode="inline", fact_index_enabled=True,
                                  model_budget=dict(group_id="prepare-e2e-assisted", max_calls=1,
                                                    max_operation_calls=1, query_output_tokens=512))
        before_calls = len(Fixture.requests)
        with client(binary, prepare_db, loopback, assisted_prepare) as c:
            assisted_result = c.call("memory_recall", {"query": question})
            assert assisted_result["recall_status"] == "completed" and assisted_result["assistance"]["status"] == "completed", assisted_result
            assert len(assisted_result["model_calls"]) == 1 and assisted_result["model_calls"][0]["stage"] == "assistance", assisted_result
            assert [(item["source_id"], item["text"]) for item in assisted_result["source_evidence"]] == [(source_id, statement)], assisted_result
        assert len(Fixture.requests) == before_calls + 1
        assert state(prepare_db) == before_business, "assisted recall must not change saved memory"
        checks.append("inline prepare commits a project fact; local and assisted MCP recall return its exact USER quote")
        Fixture.mode = "success"

        assisted_db = root / "assisted.sqlite3"
        recall_budget = dict(candidate_k=10, source_limit=2, content_bytes=8192)
        assisted_off = config(root, "assisted-off", retrieval_mode="local", enrichment_mode="off",
                              local_recall_budget=recall_budget)
        primary_text = "合成代号玉米712存放在北仓。"
        extra_text = "合成代号银河836存放在南仓。"
        question = "玉米712放在哪里？"
        soft_query = "银河836 南仓"
        with client(binary, assisted_db, denied, assisted_off) as c:
            primary = c.call("memory_remember", turn("assisted-primary", primary_text))
            extra = c.call("memory_remember", turn("assisted-extra", extra_text))
            source_status(primary, "not_requested")
            source_status(extra, "not_requested")
            baseline = c.call("memory_recall", {"query": question})
        primary_id = primary["source"]["source_id"]
        extra_id = extra["source"]["source_id"]
        baseline_ids = {item["source_id"] for item in baseline["source_evidence"]}
        assert baseline_ids == set(), baseline
        before_business = state(assisted_db)
        assisted = config(root, "assisted", retrieval_mode="assisted", enrichment_mode="off",
                          local_recall_budget=recall_budget,
                          model_budget=dict(group_id="assisted-success", max_calls=1,
                                            max_operation_calls=1, query_output_tokens=512))
        with client(binary, assisted_db, denied, assisted) as c:
            missing = c.call("memory_recall", {"query": question})
            assert missing["retrieval_mode"] == "assisted" and missing["assistance"] == {
                "status": "fallback_local", "reason": "model_client_unavailable"}, missing
            assert missing["model_calls"] == [] and {item["source_id"] for item in missing["source_evidence"]} == baseline_ids
            assert c.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})["retrieval_mode"] == "bootstrap"
        assert state(assisted_db) == before_business
        denied_with_credentials = dict(denied, DEEPSEEK_API_KEY="local-fixture-key",
                                       DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1")
        before_calls = len(Fixture.requests)
        with client(binary, assisted_db, denied_with_credentials, assisted) as c:
            blocked = c.call("memory_recall", {"query": question})
            assert blocked["assistance"] == {"status": "fallback_local", "reason": "model_network_disabled"}, blocked
            assert blocked["model_calls"] == [] and {item["source_id"] for item in blocked["source_evidence"]} == baseline_ids
        assert len(Fixture.requests) == before_calls and state(assisted_db) == before_business
        checks.append("assisted missing credentials and denied network keep unverified questions source-empty without a model send")

        Fixture.mode = "success"
        Fixture.assistance_queries = [soft_query]
        with client(binary, assisted_db, loopback, assisted) as c:
            before_calls = len(Fixture.requests)
            unbound = c.call("memory_recall", {"query": "那件东西存在哪里？"})
            assert unbound["reference_resolution"]["needs_clarification"] is True, unbound
            assert unbound["source_evidence"] == [] and unbound["model_calls"] == [], unbound
            assert unbound["assistance"] == {"status": "not_requested", "reason": "clarification_required"}, unbound
            assert len(Fixture.requests) == before_calls, "clarification must not spend a Planner call"
            enhanced = c.call("memory_recall", {"query": question})
            assert len(Fixture.requests) == before_calls + 1, Fixture.requests
            assert enhanced["retrieval_mode"] == "assisted" and enhanced["assistance"] == {
                "status": "completed", "queries": [soft_query], "source": "llm_inferred", "strength": "soft"}, enhanced
            assert len(enhanced["model_calls"]) == 1 and enhanced["model_calls"][0]["stage"] == "assistance", enhanced
            assert enhanced["model_calls"][0]["group_id"] == "assisted-success", enhanced
            assert enhanced["backend_generation_calls_in_recall"] == 0, enhanced
            evidence = {item["source_id"]: item for item in enhanced["source_evidence"]}
            assert evidence == {}, enhanced
            assert any(item["reason"] in ("no_verified_relation", "source_not_committed")
                       for item in enhanced["source_decisions"]), enhanced
            assert enhanced["assistance_search"]["candidate_count"] >= 1, enhanced
            assert baseline["budget"]["config"] == enhanced["budget"]["config"] == recall_budget
            assert enhanced["budget"]["content_bytes"] <= recall_budget["content_bytes"], enhanced
            exhausted = c.call("memory_recall", {"query": question})
            assert exhausted["assistance"] == {"status": "fallback_local", "reason": "model_budget_rejected"}, exhausted
            assert len(Fixture.requests) == before_calls + 1 and exhausted["model_calls"] == [], exhausted
        assert state(assisted_db) == before_business, "assisted recall must not mutate memory state"
        with sqlite3.connect(assisted_db) as connection:
            calls = connection.execute(
                "SELECT owner_id, group_id, stage, outcome FROM model_calls ORDER BY created_at"
            ).fetchall()
        assert calls == [("s5-synthetic", "assisted-success", "assistance", "completed")], calls
        checks.append("one loopback planner call cannot promote an unrelated soft-query source; independent ledger and exhaustion checked")

        before_business_rows = business_rows(assisted_db)
        for fault, reason, outcome in [
            ("timeout", "model_request_failed", "failed"),
            ("http", "model_request_failed", "failed"),
            ("bad_json", "invalid_plan", "completed"),
            ("incomplete", "planner_incomplete", "incomplete"),
        ]:
            Fixture.assistance_mode = fault
            group = "assisted-" + fault
            settings = config(root, group, retrieval_mode="assisted", enrichment_mode="off",
                              local_recall_budget=recall_budget,
                              model_budget=dict(group_id=group, max_calls=1,
                                                max_operation_calls=1, query_output_tokens=512))
            before_calls = len(Fixture.requests)
            with client(binary, assisted_db, loopback, settings) as c:
                fallback = c.call("memory_recall", {"query": question})
            assert len(Fixture.requests) == before_calls + 1, (fault, Fixture.requests)
            assert fallback["retrieval_mode"] == "assisted" and fallback["assistance"] == {
                "status": "fallback_local", "reason": reason}, fallback
            assert fallback["model_calls"] and len(fallback["model_calls"]) == 1, fallback
            assert fallback["model_calls"][0]["group_id"] == group
            assert fallback["model_calls"][0]["stage"] == "assistance"
            assert fallback["backend_generation_calls_in_recall"] == 0, fallback
            for field in ("source_evidence", "historical_or_conflicting"):
                assert fallback[field] == baseline[field], (fault, field, fallback)
            after_business_rows = business_rows(assisted_db)
            assert after_business_rows == before_business_rows, (fault, {
                table: (before_business_rows[table], after_business_rows[table])
                for table in before_business_rows if before_business_rows[table] != after_business_rows[table]
            })
            with sqlite3.connect(assisted_db) as connection:
                recorded = connection.execute(
                    "SELECT call_id, stage, outcome FROM model_calls WHERE group_id=?", (group,)
                ).fetchall()
            assert len(recorded) == 1 and recorded[0][1:] == ("assistance", outcome), (fault, recorded)
            trace_events = events(loopback_trace)
            starts = [event for event in trace_events if event["kind"] == "model_start"
                      and event["data"].get("budget_group") == group]
            ends = [event for event in trace_events if event["kind"] == "model_end"
                    and event["data"].get("call_id") == recorded[0][0]]
            assert len(starts) == len(ends) == 1, (fault, starts, ends)
            assert starts[0]["data"]["call_id"] == recorded[0][0]
            assert ends[0]["data"]["status"] == outcome, (fault, ends)
            if fault == "timeout":
                assert ends[0]["data"]["http_status"] is None, ends
                assert any(term in ends[0]["data"]["error"].lower()
                           for term in ("timed out", "connection closed")), ends
            elif fault == "http":
                assert ends[0]["data"]["http_status"] == 503, ends
            elif fault == "bad_json":
                assert ends[0]["data"]["parsing_status"] == "invalid_json", ends
            else:
                assert ends[0]["data"]["response"]["status"] == "incomplete", ends
            checks.append(f"assisted {fault} falls back to identical local evidence; one accounted loopback send and no business write")
        Fixture.assistance_mode = "success"

        race = config(root, "assisted-race", retrieval_mode="assisted", enrichment_mode="off",
                      local_recall_budget=recall_budget,
                      model_budget=dict(group_id="assisted-race", max_calls=1,
                                        max_operation_calls=1, query_output_tokens=512))
        Fixture.planner_entered = threading.Event()
        Fixture.planner_release = threading.Event()
        Fixture.pause_assistance = True
        try:
            with client(binary, assisted_db, loopback, race) as c:
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                    pending = pool.submit(c.call, "memory_recall", {"query": question})
                    assert Fixture.planner_entered.wait(10), "planner did not reach the loopback fixture"
                    with client(binary, assisted_db, denied, assisted_off) as deleter:
                        deleter.call("memory_forget", {"source_id": primary_id})
                    Fixture.planner_release.set()
                    changed = pending.result(timeout=20)
        finally:
            Fixture.planner_release.set()
            Fixture.pause_assistance = False
        assert changed["assistance"] == {"status": "state_changed_during_assistance",
                                           "planner_status": "completed", "discarded_queries": 1}, changed
        assert changed["read_revision"] == state(assisted_db)["revision"], changed
        assert all(item["source_id"] not in (primary_id, extra_id) for item in changed["source_evidence"]), changed
        assert len(changed["model_calls"]) == 1 and changed["model_calls"][0]["group_id"] == "assisted-race", changed
        with sqlite3.connect(assisted_db) as connection:
            calls = connection.execute("SELECT group_id, stage, outcome FROM model_calls ORDER BY created_at").fetchall()
        assert {row[0] for row in calls} == {"assisted-success", "assisted-timeout", "assisted-http",
                                              "assisted-bad_json", "assisted-incomplete", "assisted-race"}, calls
        assert len(calls) == 6, calls
        checks.append("deletion while planner is waiting invalidates soft queries and returns a new local snapshot")

        failed_read_db = root / "assisted-final-read-failure.sqlite3"
        with client(binary, failed_read_db, denied, assisted_off) as c:
            source_status(c.call("memory_remember", turn("assisted-final-read", primary_text)), "not_requested")
        failed_read = config(root, "assisted-final-read", retrieval_mode="assisted", enrichment_mode="off",
                             local_recall_budget=recall_budget,
                             model_budget=dict(group_id="assisted-final-read-failure", max_calls=1,
                                               max_operation_calls=1, query_output_tokens=512))
        Fixture.planner_entered = threading.Event()
        Fixture.planner_release = threading.Event()
        Fixture.pause_assistance = True
        before_calls = len(Fixture.requests)
        try:
            with client(binary, failed_read_db, loopback, failed_read) as c:
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                    pending = pool.submit(c.call, "memory_recall", {"query": question}, True)
                    assert Fixture.planner_entered.wait(10), "planner did not reach the loopback fixture"
                    with sqlite3.connect(failed_read_db) as connection:
                        connection.execute("DROP TABLE source_indexes")
                    Fixture.planner_release.set()
                    failed = pending.result(timeout=20)
        finally:
            Fixture.planner_release.set()
            Fixture.pause_assistance = False
        assert len(Fixture.requests) == before_calls + 1, Fixture.requests
        assert "error" in failed and len(failed["model_calls"]) == 1, failed
        assert failed["model_calls"][0]["group_id"] == "assisted-final-read-failure", failed
        assert failed["model_calls"][0]["stage"] == "assistance", failed
        with sqlite3.connect(failed_read_db) as connection:
            calls = connection.execute("SELECT group_id, stage, outcome FROM model_calls").fetchall()
        assert calls == [("assisted-final-read-failure", "assistance", "completed")], calls
        checks.append("a final local read error retains the already sent and settled planner call in the MCP error")

        assert len(Fixture.requests) == 18, Fixture.requests
        Fixture.mode = "phone_cause"
        phone_db = root / "clarification-phone.sqlite3"
        phone_quote = "王阿姨的手机坏了，因为进水。"
        phone_intake = config(root, "clarification-phone-intake", retrieval_mode="local",
                              enrichment_mode="inline", model_budget=dict(
                                  group_id="clarification-phone-intake", max_calls=3))
        intake_start = len(Fixture.requests)
        with client(binary, phone_db, loopback, phone_intake) as c:
            for name in ("王阿姨", "李阿姨", "松石47"):
                c.call("memory_entity_upsert", dict(id=name, canonical_name=name, aliases=[],
                                                    entity_type="Person" if name.endswith("阿姨") else "Other"))
            prepared = c.call("memory_prepare", {"user_turn": turn("clarification-phone", phone_quote)})
            assert prepared["remember"]["status"] == "completed", prepared
            source_status(prepared["remember"]["result"], "committed")
            assert c.call("memory_list", {})["total"] == 1, prepared
            phone_source_id = prepared["remember"]["result"]["source"]["source_id"]
        intake_sends = len(Fixture.requests) - intake_start
        assert intake_sends == len(prepared["remember"]["result"]["model_calls"]), prepared
        Fixture.mode = "success"
        before_phone = business_rows(phone_db)
        before_phone_state = state(phone_db)

        def proposal(question, clarification, subject, relation="为什么坏了", ambiguous=False):
            return dict(question_text=question, clarification_text=clarification,
                        subject_text=subject, relation_text=relation, ambiguous=ambiguous)

        phone_question = "手机为什么坏了？"
        phone_clarification = "我说的是王阿姨的手机。"
        cases = [
            ("missing-subject", phone_question,
             proposal(phone_question, "", ""), False, 0),
            ("bound", phone_question + phone_clarification,
             proposal(phone_question, phone_clarification, "王阿姨的手机"), True, 1),
            ("rewritten", phone_question + phone_clarification,
             proposal("手机怎么坏了？", phone_clarification, "王阿姨的手机"), False, 1),
            ("invented", phone_question + phone_clarification,
             proposal(phone_question, phone_clarification, "李阿姨的手机"), False, 1),
            ("ambiguous", phone_question + phone_clarification,
             proposal(phone_question, phone_clarification, "王阿姨的手机", ambiguous=True), False, 1),
            ("negated", phone_question + "我说的不是王阿姨的手机。",
             proposal(phone_question, "我说的不是王阿姨的手机。", "王阿姨的手机"), False, 1),
            ("unknown-alternative", phone_question + "我说的是王阿姨的手机或另一部手机。",
             proposal(phone_question, "我说的是王阿姨的手机或另一部手机。",
                      "王阿姨的手机或另一部手机"), False, 1),
            ("second-question", phone_question + "我说的是王阿姨的手机，怎么修？",
             proposal(phone_question, "我说的是王阿姨的手机，怎么修？", "王阿姨的手机"), False, 0),
            ("multiple-subjects", phone_question + "我说的是王阿姨的手机和李阿姨的手机。",
             proposal(phone_question, "我说的是王阿姨的手机和李阿姨的手机。", "王阿姨的手机"), False, 0),
            ("unknown-subject", phone_question + "我说的是陌生人的手机。",
             proposal(phone_question, "我说的是陌生人的手机。", "陌生人的手机"), False, 0),
            ("unsupported-relation", "负责人是谁？我说的是松石47。",
             proposal("负责人是谁？", "我说的是松石47。", "松石47", "负责人是谁"), False, 1),
        ]
        clarification_sends = 0
        for label, joined_query, suggested, should_answer, expected_sends in cases:
            Fixture.clarification_proposal = suggested
            settings = config(root, "clarification-" + label, retrieval_mode="assisted",
                              enrichment_mode="off", model_budget=dict(
                                  group_id="clarification-" + label, max_calls=1,
                                  max_operation_calls=1, query_output_tokens=512))
            before_calls = len(Fixture.requests)
            with client(binary, phone_db, loopback, settings) as c:
                answer = c.call("memory_recall", {"query": joined_query})
            sent = len(Fixture.requests) - before_calls
            assert sent == expected_sends, (label, answer)
            clarification_sends += sent
            if sent:
                assert Fixture.requests[-1]["text"]["format"]["name"] == "personal_memory_clarification_v1", (label, answer)
                assert len(answer["model_calls"]) == 1 and answer["model_calls"][0]["stage"] == "assistance", (label, answer)
            else:
                assert answer["model_calls"] == [], (label, answer)
            evidence = [(item["source_id"], item["text"]) for item in answer["source_evidence"]]
            assert evidence == ([(phone_source_id, phone_quote)] if should_answer else []), (label, answer)
            if not should_answer:
                assert answer["result"]["facts"] == [] and answer["historical_or_conflicting"] == [], (label, answer)
            if label not in ("bound", "unsupported-relation"):
                assert answer["reference_resolution"]["needs_clarification"] is True, (label, answer)
            after_phone = business_rows(phone_db)
            assert state(phone_db) == before_phone_state and all(
                after_phone[name] == before_phone[name]
                for name in ("source_indexes", "source_segments", "fact_indexes")
            ), label
        for label, user_context, should_answer in [
            ("repeated-context", "王阿姨的手机。", True),
            ("conflicting-context", "李阿姨的手机。", False),
            ("excluded-context", "王阿姨的手机以外。", False),
            ("different-object-context", "王阿姨的车。", False),
        ]:
            Fixture.clarification_proposal = proposal(
                phone_question, phone_clarification, "王阿姨的手机")
            settings = config(root, "clarification-" + label, retrieval_mode="assisted",
                              enrichment_mode="off", model_budget=dict(
                                  group_id="clarification-" + label, max_calls=1,
                                  max_operation_calls=1, query_output_tokens=512))
            before_calls = len(Fixture.requests)
            with client(binary, phone_db, loopback, settings) as c:
                answer = c.call("memory_recall", {
                    "query": phone_question + phone_clarification, "context": [user_context]})
            sent = len(Fixture.requests) - before_calls
            assert sent <= 1 and (not should_answer or sent == 1), (label, answer)
            clarification_sends += sent
            if sent:
                assert Fixture.requests[-1]["text"]["format"]["name"] == "personal_memory_clarification_v1", (label, answer)
                assert len(answer["model_calls"]) == 1, (label, answer)
            evidence = [(item["source_id"], item["text"]) for item in answer["source_evidence"]]
            assert evidence == ([(phone_source_id, phone_quote)] if should_answer else []), (label, answer)
            if not should_answer:
                assert answer["result"]["facts"] == [] and answer["historical_or_conflicting"] == [], (label, answer)
                assert answer["reference_resolution"]["needs_clarification"] is True, answer
            assert state(phone_db) == before_phone_state, label

        Fixture.clarification_proposal = proposal(
            phone_question, phone_clarification, "王阿姨的手机", ambiguous=True)
        race_settings = config(root, "clarification-race", retrieval_mode="assisted",
                               enrichment_mode="off", model_budget=dict(
                                   group_id="clarification-race", max_calls=1,
                                   max_operation_calls=1, query_output_tokens=512))
        phone_local = config(root, "clarification-delete", retrieval_mode="local", enrichment_mode="off")
        Fixture.planner_entered = threading.Event()
        Fixture.planner_release = threading.Event()
        Fixture.pause_assistance = True
        before_calls = len(Fixture.requests)
        try:
            with client(binary, phone_db, loopback, race_settings) as c:
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                    pending = pool.submit(c.call, "memory_recall", {
                        "query": phone_question + phone_clarification})
                    assert Fixture.planner_entered.wait(10), "clarification proposal did not reach loopback fixture"
                    with client(binary, phone_db, denied, phone_local) as deleter:
                        deleter.call("memory_forget", {"source_id": phone_source_id})
                    Fixture.planner_release.set()
                    changed = pending.result(timeout=20)
        finally:
            Fixture.planner_release.set()
            Fixture.pause_assistance = False
        assert len(Fixture.requests) == before_calls + 1, Fixture.requests
        assert Fixture.requests[-1]["text"]["format"]["name"] == "personal_memory_clarification_v1", changed
        clarification_sends += 1
        assert changed["assistance"]["status"] == "state_changed_during_assistance", changed
        assert changed["source_evidence"] == [] and changed["read_revision"] == state(phone_db)["revision"], changed
        assert len(changed["model_calls"]) == 1 and changed["model_calls"][0]["stage"] == "assistance", changed
        checks.append("one loopback clarification proposal binds the exact phone quote; missing, rewritten, invented, ambiguous, negated, alternative, second-question, multi-subject and unknown subjects require clarification; unsupported relation stays source-empty")
        checks.append("repeated USER context binds the same phone; conflicting, excluded and different-object context stays source-empty; deletion during an ambiguous proposal discards the stale snapshot")

        loopback_events = events(loopback_trace)
        physical = sum(e["kind"] == "model_start" for e in loopback_events)
        assert physical == len(Fixture.requests) == 18 + intake_sends + clarification_sends
        deny_events = events(trace)
        assert not any(e["kind"] == "model_start" for e in deny_events)
        report = dict(status="passed", scope="S5 batch A, E1 and assisted synthetic engineering contracts; not semantic acceptance",
                      checks=checks, deny_model_requests=0, loopback_model_requests=physical, provider_model_requests=0)
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
            args.output.with_suffix(".runtime-events.jsonl").write_text("".join(json.dumps(e, ensure_ascii=False) + "\n" for e in deny_events + loopback_events))
        print(json.dumps(report, ensure_ascii=False))


if __name__ == "__main__":
    main()
