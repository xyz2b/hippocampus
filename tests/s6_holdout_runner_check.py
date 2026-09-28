#!/usr/bin/env python3
"""Offline runner checks using synthetic micro-data and old S5 development cases only."""
import contextlib
import copy
import http.server
import io
import json
import os
import sqlite3
import tempfile
import threading
from pathlib import Path
from unittest.mock import Mock, patch

import s6_holdout_live as runner
from s5_general_fact_process import Fixture, INPUTS, LABELS
from s6_holdout_scoring import self_check


ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "docs/refactor/s6/runner-v1-20260927/offline-check.json"
DEPENDENCIES = (
    "tests/s6_holdout_live.py", "tests/s6_holdout_scoring.py",
    "tests/s6_holdout_runner_check.py", "tests/s5_original_n05_fullchain.py",
    "tests/mcp_smoke.py", "tests/s5_normal_intake.py",
)


def rejects(action, error=AssertionError):
    try:
        action()
    except error:
        return
    raise AssertionError("unsafe operation was accepted")


def request_checks(directory):
    scenario = dict(id="mini", sources=[dict(id="S1", text="甲灯是蓝色。"),
                                        dict(id="S2", text="甲灯是绿色。")],
                    queries=[dict(id="Q2", after_source_count=2), dict(id="Q1", after_source_count=1)],
                    validations=[dict(id="V2", query_id="Q2"), dict(id="V1", query_id="Q1")])
    operations = runner.plan({"scenarios": [scenario]})
    assert [(op["phase"], op["id"], op["after_source_count"]) for op in operations] == [
        ("intake", "S1", 1), ("recall", "Q1", 1), ("validate", "V1", 1),
        ("intake", "S2", 2), ("recall", "Q2", 2), ("validate", "V2", 2)]
    scenario["sources"] = [dict(id="A", text="原话甲。", task_id="task-a"),
                           dict(id="B", text="隔离原话。", task_id="task-b"),
                           dict(id="C", text="原话乙。", task_id="task-a"),
                           dict(id="D", text="无任务原话。"), dict(id="E", text="无任务补充。")]
    requests = [runner.source_request(scenario, source)["user_turn"] for source in scenario["sources"]]
    assert [row["history"] for row in requests] == [[], [], ["原话甲。"], [], ["无任务原话。"]]
    assert len({row["session_id"] for row in requests}) == 1
    assert [row["turn_id"] for row in requests] == ["A", "B", "C", "D", "E"]
    assert all(row["capture_source"] is True for row in requests)
    assert [row["text"] for row in requests] == [row["text"] for row in scenario["sources"]]
    assert [row.get("task_id") for row in requests] == ["task-a", "task-b", "task-a", None, None]
    other = runner.source_request({"id": "other", "sources": [scenario["sources"][0]]}, scenario["sources"][0])
    assert other["user_turn"]["session_id"] != requests[0]["session_id"]
    query = dict(id="Q", query="  它的颜色呢？\n", context=["原话甲。"], task_id="task-a", after_source_count=1)
    assert runner.query_request(query) == {key: query[key] for key in ("query", "context", "task_id")}
    arguments = {"user_turn": requests[2]}
    original = copy.deepcopy(arguments)
    client = Mock()
    client.rpc.side_effect = [{"structuredContent": {"status": "in_progress"}},
                              {"content": [{"text": '{"status":"completed"}'}]}]
    with patch.object(runner.time, "sleep"):
        assert runner.call(client, "memory_prepare", arguments, directory / "poll.json") == {"status": "completed"}
    expected = (("tools/call", {"name": "memory_prepare", "arguments": original}),)
    assert [item.args for item in client.rpc.call_args_list] == [expected[0], expected[0]]
    assert arguments == original and len(list(directory.glob("poll-poll-*.json"))) == 2
    client.reset_mock(side_effect=True)
    client.rpc.return_value = {"isError": True, "content": [{"text": "fixture error"}]}
    rejects(lambda: runner.call(client, "memory_prepare", arguments, directory / "error.json"))
    assert client.rpc.call_count == 1 and (directory / "error-poll-01.json").is_file()
    client.reset_mock()
    rejects(lambda: runner.call(client, "memory_prepare", arguments, directory / "poll.json"))
    client.rpc.assert_not_called()
    runner.runtime_checks({"recall": {"result": {"status": "completed", "text": "failed"}}})
    rejects(lambda: runner.runtime_checks({"recall": {"result": {"assistance": {"reason": "model_request_failed"}}}}))
    rejects(lambda: runner.runtime_checks({"diagnostics": {"status": "evidence_incomplete"}}))
    sources = {source["id"]: source for source in scenario["sources"]}
    assert runner.check_task_visibility({"nested": [{"source_id": "saved-b"}]}, query, sources,
                                        {"A": "saved-a", "B": "saved-b"}) == ["saved-b"]
    assert runner.check_task_visibility({"safe_source_ids": ["saved-a"]}, query, sources,
                                        {"A": "saved-a", "B": "saved-b"}) == []
    return ["source_query_validate_order", "same_task_history_and_isolation", "original_query_preserved",
            "prepare_poll_same_arguments_and_ids", "tool_error_preserved_without_retry",
            "existing_poll_receipt_blocks_rpc", "nested_runtime_failure_and_task_visibility"]


def audit_checks(directory):
    scenario = directory / "mini"
    trace = scenario / "trace"
    trace.mkdir(parents=True)
    db = scenario / "business.sqlite3"
    row = dict(call_id="call-1", owner_id=runner.OWNER, group_id="mini-intake", operation_id="operation-1",
               stage="extraction", output_token_limit=16384, input_bytes=64, outcome="completed",
               input_tokens=10, output_tokens=10, usage_status="known")
    budgets = dict(groups={"mini": {"intake": dict(group_id="mini-intake", max_calls=2, max_operation_calls=2)}},
                   total_max_physical_requests=2, model="fixture-model", reasoning="medium")
    start = dict(kind="model_start", data=dict(call_id="call-1", attempt=1, timeout_ms=180000,
                 provider="ark.cn-beijing.volces.com", budget_group="mini-intake",
                 request=dict(model="fixture-model", max_output_tokens=16384, reasoning={"effort": "medium"})))
    end = dict(kind="model_end", data=dict(call_id="call-1", http_status=200, status="completed",
               response={"status": "completed"}, parsing_status="valid_json", error=None))
    with sqlite3.connect(db) as connection:
        connection.execute("CREATE TABLE model_calls (" + ",".join(
            key + (" INTEGER" if isinstance(value, int) else " TEXT") for key, value in row.items()) + ")")
        connection.execute("INSERT INTO model_calls VALUES (" + ",".join("?" for _ in row) + ")", tuple(row.values()))
    tracefile = trace / "fixture.jsonl"
    tracefile.write_text("\n".join(json.dumps(event) for event in (start, end)) + "\n")
    receipt = scenario / "fixture-poll-01.json"
    runner.save(receipt, {"structuredContent": {"remember": {"result": {"model_calls": [{"call_id": "call-1"}]}}}})
    assert runner.audit(directory, budgets)["physical_requests"] == 1
    with sqlite3.connect(db) as connection:
        connection.execute("UPDATE model_calls SET group_id='unknown'")
    rejects(lambda: runner.audit(directory, budgets))
    assert runner.read(directory / "accounting.json")["physical_requests"] == 1
    with sqlite3.connect(db) as connection:
        connection.execute("UPDATE model_calls SET group_id='mini-intake'")
    for key in ("max_calls", "max_operation_calls", "total_max_physical_requests"):
        limited = copy.deepcopy(budgets)
        target = limited if key == "total_max_physical_requests" else limited["groups"]["mini"]["intake"]
        target[key] = 0
        rejects(lambda: runner.audit(directory, limited))
    tracefile.write_text(json.dumps(start) + "\n")
    rejects(lambda: runner.audit(directory, budgets))
    wrong = copy.deepcopy(end)
    wrong["data"]["call_id"] = "unmatched-call"
    tracefile.write_text("\n".join(json.dumps(event) for event in (start, wrong)) + "\n")
    rejects(lambda: runner.audit(directory, budgets))
    tracefile.write_text("\n".join(json.dumps(event) for event in (start, end)) + "\n")
    runner.save(receipt, {"structuredContent": {"model_calls": []}})
    rejects(lambda: runner.audit(directory, budgets))
    return ["audit_valid_ledger", "audit_unknown_group_rejected", "audit_phase_operation_total_caps",
            "audit_trace_count_and_identity_mismatch", "audit_receipt_ledger_mismatch",
            "failed_audit_retains_request_accounting"]


def started_marker_check(directory):
    directory.mkdir()
    pack = directory / "synthetic-pack"
    pack.mkdir()
    scenario = dict(id="mini", sources=[dict(id="S1", text="甲灯是蓝色。")], queries=[], validations=[])
    runner.save(pack / "inputs.json", {"scenarios": [scenario]})
    runner.save(pack / "labels.json", {})
    budgets = dict(groups={"mini": {}}, total_max_physical_requests=84)
    manifest = dict(operations=runner.plan({"scenarios": [scenario]}), budgets=budgets,
                    runtime={"formal_baseline": {"fixture": True}})
    runner.save(directory / "manifest.json", manifest)
    runner.save(directory / "report.json", dict(status="ready", rows=[], source_ids={}, blocked_scenarios=[]))
    (directory / "mini").mkdir()
    marker = directory / "mini/00-S1.started"
    marker.write_text("an earlier attempt already started\n")
    original = marker.read_bytes(), marker.stat().st_mtime_ns
    with patch.object(runner, "PACK", pack), patch.object(runner, "verify"), \
            patch.object(runner, "clean_env", return_value={"PMR_MODEL_NETWORK": "deny"}), \
            patch.object(runner.PREP, "formal_state", return_value={"fixture": True}), \
            patch.object(runner, "LongClient") as client, contextlib.redirect_stdout(io.StringIO()):
        runner.step(directory)
        client.assert_not_called()
    report = runner.read(directory / "report.json")
    assert report["status"] == "stopped" and report["rows"][0]["status"] == "runtime_error"
    assert report["rows"][0]["error"].startswith("FileExistsError:")
    assert (marker.read_bytes(), marker.stat().st_mtime_ns) == original
    return ["started_marker_blocks_replay_and_overwrite"]


def frozen_mcp_check(directory):
    freeze = runner.read(runner.PACK / "runtime-freeze.json")
    binary = Path(freeze["binary"]["path"])
    assert runner.digest(binary) == freeze["binary"]["sha256"]
    scenario = dict(id="old-development", sources=[dict(source, task_id="old-development")
                    for source in INPUTS["sources"] if source["id"] in ("P-A", "P-B")])
    Fixture.requests, Fixture.errors = [], []
    with contextlib.ExitStack() as stack:
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(("DEEPSEEK_", "OPENAI_", "ARK_", "PMR_"))}
        env.update(PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
                   DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1", NO_PROXY="127.0.0.1,localhost")
        paths = {}
        for phase in ("intake", "recall", "validate"):
            paths[phase] = runner.settings(directory, phase, retrieval_mode="local" if phase == "intake" else "assisted",
                enrichment_mode="inline" if phase == "intake" else "off", fact_index_enabled=True,
                model_budget=dict(group_id="offline-" + phase, max_calls=4, max_operation_calls=2 if phase == "intake" else 1,
                                  max_input_bytes=65536, extraction_output_tokens=16384, review_output_tokens=16384, query_output_tokens=8192))
        db = directory / "business.sqlite3"
        source_ids = {}
        with contextlib.closing(runner.LongClient(binary, db, env, runner.OWNER, config=paths["intake"])) as client:
            assert runner.call(client, "memory_recall", {"query": "全局回答要求", "bootstrap": True},
                               directory / "bootstrap-intake.json")["model_calls"] == []
            for entity in INPUTS["entities"][:2]:
                runner.call(client, "memory_entity_upsert", entity, directory / ("entity-" + entity["id"] + ".json"))
            for source in scenario["sources"]:
                request = runner.source_request(scenario, source)
                result = runner.call(client, "memory_prepare", request, directory / (source["id"] + ".json"))
                saved = runner.receipt(result)
                assert saved["source"]["status"] == "stored" and saved["enrichment"]["status"] == "committed"
                assert saved["outcomes"] and result["recall"]["result"]["model_calls"] == []
                source_ids[source["id"]] = saved["source"]["source_id"]
                stored = next(row for row in runner.profile(db)["sources"].values() if row["id"] == saved["source"]["source_id"])
                assert stored["request"] == request["user_turn"]
            assert runner.source_request(scenario, scenario["sources"][1])["user_turn"]["history"] == [scenario["sources"][0]["text"]]
        query = dict(next(query for query in INPUTS["queries"] if query["id"] == "P03"), task_id="old-development")
        request = runner.query_request(query)
        before = runner.profile(db)
        with contextlib.closing(runner.LongClient(binary, db, env, runner.OWNER, config=paths["recall"])) as client:
            runner.call(client, "memory_recall", {"query": "全局回答要求", "bootstrap": True}, directory / "bootstrap-recall.json")
            recalled = runner.call(client, "memory_recall", request, directory / "recall.json")
        contract = recalled["answer_contract"]
        assert {key: contract[key] for key in request} == request
        assert contract["safe_answer"] == "无糖豆浆。" and contract["safe_source_ids"] == [source_ids["P-A"]]
        assert contract["sources"] == [{"source_id": source_ids["P-A"], "quote": LABELS["intake"]["P-A"]["supporting_quote"]}]
        with contextlib.closing(runner.LongClient(binary, db, env, runner.OWNER, config=paths["validate"])) as client:
            runner.call(client, "memory_recall", {"query": "全局回答要求", "bootstrap": True}, directory / "bootstrap-validate.json")
            for name, draft, expected in (("correct", "无糖豆浆。", True), ("wrong", "温柠檬水。", False)):
                result = runner.call(client, "memory_validate_answer", dict(request, draft_answer=draft,
                                     source_ids=[source_ids["P-A"]]), directory / ("validate-" + name + ".json"))
                assert result["approved"] is expected
                assert result["approved_source_ids"] == ([source_ids["P-A"]] if expected else [])
                assert {key: result[key] for key in request} == request
                for key in ("read_revision", "source_revision", "safe_answer", "safe_source_ids", "sources"):
                    assert result[key] == contract[key], (name, key, result)
        after = runner.profile(db)
        assert all(before[key] == after[key] for key in ("revision", "source_revision"))
        assert not Fixture.errors and Fixture.requests
    return ["frozen_binary_sha256", "actual_prepare_source_and_history", "actual_recall_original_binding",
            "actual_validate_same_revision_correct_and_wrong_draft_fallback", "queries_do_not_mutate_profile"], len(Fixture.requests)


def main():
    os.umask(0o077)
    report = dict(status="failed", real_model_requests=0, checks=[], checked_at_utc=runner.now())
    try:
        with tempfile.TemporaryDirectory(prefix="pmr-s6-runner-offline-") as temporary:
            directory = Path(temporary)
            for name, check in (("requests", request_checks), ("audit", audit_checks), ("marker", started_marker_check)):
                child = directory / name
                if name != "marker":
                    child.mkdir()
                report["checks"].extend(dict(name=name, status="passed") for name in check(child))
            with contextlib.redirect_stdout(io.StringIO()):
                self_check()
            report["checks"].append(dict(name="scorer_micro_data_self_check", status="passed"))
            child = directory / "mcp"
            child.mkdir()
            checks, requests = frozen_mcp_check(child)
            report["checks"].extend(dict(name=name, status="passed") for name in checks)
            report["loopback_model_requests"] = requests
        report["status"] = "passed"
    except Exception as error:
        report["error"] = type(error).__name__ + ": " + str(error)
        raise
    finally:
        report["fingerprints"] = {name: runner.digest(ROOT / name) for name in DEPENDENCIES}
        OUTPUT.parent.mkdir(parents=True, exist_ok=True)
        runner.save(OUTPUT, report)
        print(json.dumps(report, ensure_ascii=False))


if __name__ == "__main__":
    main()
