#!/usr/bin/env python3
"""S2 acceptance through real MCP/SQLite; synthetic loopback Responses, no provider calls."""
import argparse
import contextlib
import http.server
import json
import os
import sqlite3
import subprocess
import tempfile
import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from inspector_support import export_runtime, inspector_binary, schema_check
from mcp_smoke import Client, Responses


def state(db, owner="smoke-user"):
    with sqlite3.connect(db) as connection:
        assert connection.execute("PRAGMA user_version").fetchone()[0] == 8
        row = connection.execute(
            "SELECT state_json FROM memory_profiles WHERE owner_id=?", (owner,)
        ).fetchone()
    return json.loads(row[0]) if row else {"sources": {}, "receipts": {}}


def source(db, turn_id):
    return next(record for record in state(db)["sources"].values() if record["turn_id"] == turn_id)


class Fixture(Responses):
    mode = "success"
    requests = []
    barriers = {}

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        assert self.path == "/v1/responses"
        assert self.headers["Authorization"] == "Bearer local-fixture-key"
        type(self).requests.append(request)
        payload = json.loads(request["input"]) if isinstance(request["input"], str) else request["input"]
        schema = request["text"]["format"]["name"]
        is_query = "runtime_context" in schema
        barrier = self.barriers.pop(payload.get("turn_id"), None) if not is_query else None
        if barrier:
            entered, release = barrier
            entered.set()
            assert release.wait(30), "fixture barrier was not released"
        output = self.output(request)
        if schema == "personal_memory_candidates_v1" and self.mode in ("empty", "incomplete"):
            output = {"candidates": [], "omission_reason": "no_personal_assertion"}
        if schema == "personal_memory_candidates_v1" and self.mode == "rejected":
            output["candidates"][0]["source"] = "assistant_inference"
        incomplete = (self.mode == "incomplete" and schema == "personal_memory_source_support_v1"
                      or self.mode == "initial_incomplete" and schema == "personal_memory_candidates_v1")
        response = {
            "status": "incomplete" if incomplete else "completed",
            "output": [{"type": "message", "content": [
                {"type": "output_text", "text": json.dumps(output, ensure_ascii=False)}
            ]}],
        }
        if incomplete:
            response["incomplete_details"] = {"reason": "max_output_tokens"}
        if self.mode == "http_timeout":
            response = {"error": {"message": "synthetic upstream timeout"}}
        data = b"not-json" if self.mode == "bad_json" else json.dumps(response).encode()
        self.send_response(504 if self.mode == "http_timeout" else 200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        try:
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError):
            pass  # The crash-after-T1 case deliberately disconnects this fixture request.


@contextlib.contextmanager
def client(binary, db, env, owner="smoke-user"):
    current = Client(binary, db, env, owner, budget_calls=200)
    try:
        yield current
    finally:
        if current.proc.poll() is None:
            current.close()
        else:
            current.reader.join(timeout=2)
            current.proc.stdin.close()
            current.proc.stdout.close()
            current.errors.close()


def turn(identifier):
    return dict(text="以后所有回答都必须使用中文。", session_id="s2-synthetic",
                turn_id=identifier, capture_source=True)


def statuses(result, enrichment):
    assert result["source"]["status"] == "stored", result
    assert result["source"]["source_id"], result
    assert result["index"]["status"] == "ready", result
    assert result["enrichment"]["status"] == enrichment, result


def dump(db, log, owner="smoke-user"):
    result = subprocess.run(
        [str(inspector_binary()), "--dump", "--db", str(db), "--owner", owner, "--log", str(log)],
        text=True, capture_output=True, timeout=20, check=True,
    )
    bundle = json.loads(result.stdout)
    check = schema_check(bundle)
    assert check.returncode == 0 and check.stdout.strip() == "[]", check.stderr + check.stdout
    return bundle


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", nargs="?", type=Path)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/pmr-mcp"))
    arguments = parser.parse_args()
    binary = arguments.binary.resolve()
    with contextlib.ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-s2-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        env = dict(os.environ, PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
                   DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1")
        for name in ("PMR_TRACE_DIR", "PMR_OPENAI_TRACE", "PMR_OPENAI_TRACE_CONTENT",
                     "OPENAI_API_KEY", "OPENAI_BASE_URL"):
            env.pop(name, None)
        offline = {key: value for key, value in env.items() if key != "DEEPSEEK_API_KEY"}
        log = root / "empty.log"
        log.write_text("")
        db = root / "memory.sqlite3"
        checks = []

        with client(binary, db, env) as c:
            defaults = json.loads(subprocess.check_output([str(binary), "--print-default-config"], env=env))
            assert defaults["source_capture_enabled"] is True
            tools = c.rpc("tools/list", {})["tools"]
            assert next(tool for tool in tools if tool["name"] == "memory_remember")["inputSchema"]["properties"]["capture_source"]
            before = len(Fixture.requests)
            for invalid in ({**turn("invalid-owner"), "owner_id": "someone-else"},
                            {**turn("invalid-length"), "text": "x" * 24_001},
                            {**turn("invalid-history"), "history": ["x" * 24_001]},
                            {**turn("invalid-id"), "session_id": " "}):
                c.call("memory_remember", invalid, error=True)
            assert not state(db)["sources"] and len(Fixture.requests) == before
            checks.append("owner/size/ID rejection before capture or model")

            for mode, expected in (("initial_incomplete", "extraction_incomplete"),
                                   ("incomplete", "review_incomplete"), ("bad_json", "failed"),
                                   ("http_timeout", "failed"),
                                   ("empty", "completed_no_facts"), ("rejected", "completed_rejected")):
                Fixture.mode = mode
                request = turn(mode)
                failed = mode in ("initial_incomplete", "incomplete", "bad_json", "http_timeout")
                result = c.call("memory_remember", request, error=failed)
                statuses(result, expected)
                persisted = source(db, mode)
                assert persisted["request"]["text"] == request["text"]
                assert persisted["enrichment"] == expected
                before = len(Fixture.requests)
                replay = c.call("memory_remember", request, error=failed)
                statuses(replay, expected)
                assert replay["source"]["source_id"] == result["source"]["source_id"]
                assert source(db, mode)["received_sequence"] == persisted["received_sequence"]
                assert len(Fixture.requests) == before
                c.call("memory_remember", {**request, "text": "同一个消息键改成英语。"}, error=True)
                assert len(Fixture.requests) == before
            assert c.call("memory_list", {})["total"] == 0
            checks.append("initial versus review incomplete/malformed/HTTP504/empty/rejected durable source and replay")

            Fixture.mode = "success"
            saved = c.call("memory_remember", turn("success"))
            statuses(saved, "committed")
            original = c.stored_memories()[0]
            before = len(Fixture.requests)
            statuses(c.call("memory_remember", turn("success")), "committed")
            assert len(Fixture.requests) == before
            assert c.stored_memories()[0]["reinforcement_count"] == original["reinforcement_count"]
            second = c.call("memory_remember", turn("same-text-new-id"))
            assert second["source"]["source_id"] != saved["source"]["source_id"]
            assert source(db, "same-text-new-id")["received_sequence"] > source(db, "success")["received_sequence"]
            sequences = [record["received_sequence"] for record in state(db)["sources"].values()]
            assert sorted(sequences) == list(range(1, len(sequences) + 1))
            c.call("memory_remember", {**turn("success"), "capture_source": False}, error=True)
            checks.append("same-message replay versus identical-content independent event")

            count = len(state(db)["sources"])
            legacy = turn("legacy")
            del legacy["capture_source"]
            c.call("memory_remember", legacy)
            c.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
            c.call("memory_recall", {"query": "使用什么语言回答？"})
            Fixture.mode = "empty"
            c.call("memory_prepare", {"user_turn": {**legacy, "turn_id": "pure-question", "text": "1+1是多少？"}})
            assert len(state(db)["sources"]) == count
            checks.append("legacy calls and read-only queries do not capture")

        with client(binary, db, offline) as c:
            statuses(c.call("memory_remember", turn("success")), "committed")
            missing = c.call("memory_remember", turn("missing-key"), error=True)
            statuses(missing, "failed")
            assert source(db, "missing-key")["request"]["text"] == turn("missing-key")["text"]
            prepared = c.call("memory_prepare", {"user_turn": turn("prepare-no-key")})
            assert prepared["remember"]["status"] == prepared["recall"]["status"] == "failed"
            statuses(prepared["remember"], "failed")
            checks.append("restart replay and missing credentials preserve source including prepare")

            before = len(Fixture.requests)
            bundle = dump(db, log)
            raw = next(item for item in bundle["sources"] if item["id"] == missing["source"]["source_id"])
            assert raw["kind"] == "persisted_source" and raw["text"] == turn("missing-key")["text"]
            receipt = next(run for run in bundle["runs"] if run["id"] == "source-receipt:" + raw["id"])
            statuses(receipt, "failed")
            assert not dump(db, log, "someone-else")["sources"]
            assert len(Fixture.requests) == before
            checks.append("Inspector reads independent source without diagnostics or model")

            c.call("memory_forget", {"source_id": missing["source"]["source_id"]})
            tombstone = source(db, "missing-key")
            assert tombstone["request"] is None and tombstone["result"] is None
            for capture in (True, False):
                c.call("memory_remember", {**turn("missing-key"), "capture_source": capture}, error=True)
            c.call("memory_forget", {"memory_id": original["id"]})
            assert source(db, "success")["request"] is None
            assert source(db, "same-text-new-id")["request"] is None
            c.call("memory_remember", turn("success"), error=True)
            assert c.call("memory_list", {"include_history": True})["total"] == 0
            deleted_bundle = dump(db, log)
            assert not any(item["id"] == missing["source"]["source_id"] for item in deleted_bundle["sources"])
            checks.append("source deletion and memory deletion tombstones prevent replay")

        with client(binary, db, offline, "someone-else") as other:
            other.call("memory_forget", {"source_id": saved["source"]["source_id"]}, error=True)
            assert not state(db, "someone-else")["sources"]

        trace = root / "trace"
        trace.mkdir(mode=0o700)
        with client(binary, root / "trace.sqlite3", dict(env, PMR_TRACE_DIR=str(trace))) as c:
            Fixture.mode = "success"
            statuses(c.call("memory_remember", turn("trace-success")), "committed")
            Fixture.mode = "incomplete"
            statuses(c.call("memory_remember", turn("trace-review-failure"), error=True), "review_incomplete")
        files = list(trace.glob("trace-*.jsonl"))
        assert len(files) == 1
        trace_bundle = export_runtime(files[0])
        check = schema_check(trace_bundle)
        assert check.returncode == 0 and check.stdout.strip() == "[]", check.stderr + check.stdout
        assert trace_bundle["meta"]["channels"]["source_capture"] == "enabled"
        expected = {"trace-success": "committed", "trace-review-failure": "review_incomplete"}
        assert len(trace_bundle["runs"]) == len(expected)
        for run in trace_bundle["runs"]:
            statuses(run, expected[run["input"]["arguments"]["turn_id"]])
            receipts = [span for span in run["spans"] if span["name"] == "source_receipt"]
            assert receipts and receipts[-1]["output"]["enrichment"] == run["enrichment"]
            assert run["source"]["source_id"] in run["source_ids"]
            assert any(item["id"] == run["source"]["source_id"] and item["kind"] == "persisted_source"
                       for item in trace_bundle["sources"])
        checks.append("source-enabled real execution trace: receipts, status, source links and schema")

        Fixture.mode = "success"
        # Event barriers let the actual second process write while the first waits outside SQLite.
        for scenario in ("late-after-delete", "concurrent", "crash"):
            case_db = root / (scenario + ".sqlite3")
            entered, release = threading.Event(), threading.Event()
            Fixture.barriers[scenario] = (entered, release)
            with client(binary, case_db, env) as first, client(binary, case_db, env) as second:
                with ThreadPoolExecutor(max_workers=1) as worker:
                    future = worker.submit(first.call, "memory_remember", turn(scenario), True)
                    try:
                        assert entered.wait(10), "model fixture not reached"
                        pending = source(case_db, scenario)
                        assert pending["request"]["text"] == turn(scenario)["text"]
                        assert pending["enrichment"] == "running"
                        before = len(Fixture.requests)
                        retry = second.call("memory_remember", turn(scenario))
                        statuses(retry, "running")
                        assert retry["status"] == "in_progress" and len(Fixture.requests) == before
                        if scenario == "late-after-delete":
                            second.call("memory_forget", {"source_id": pending["id"]})
                        elif scenario == "concurrent":
                            statuses(second.call("memory_remember", turn("concurrent-newer")), "committed")
                        else:
                            first.proc.kill()
                            first.proc.wait(timeout=10)
                    finally:
                        release.set()
                    if scenario == "crash":
                        try:
                            future.result(timeout=10)
                        except AssertionError:
                            pass
                        else:
                            raise AssertionError("crashed MCP unexpectedly completed")
                    else:
                        failed = future.result(timeout=10)
                        assert "error" in failed
                        if scenario == "late-after-delete":
                            assert source(case_db, scenario)["request"] is None
                            assert second.call("memory_list", {})["total"] == 0
                        else:
                            statuses(failed, "stale_requires_review")
                            assert failed["error"] == "newer_source_applied"
                            assert second.call("memory_list", {})["total"] == 1
                            assert len(state(case_db)["sources"]) == 2
            if scenario == "crash":
                with client(binary, case_db, offline) as restarted:
                    before = len(Fixture.requests)
                    retry = restarted.call("memory_remember", turn(scenario))
                    statuses(retry, "running")
                    assert retry["status"] == "in_progress" and len(Fixture.requests) == before
                    assert restarted.call("memory_list", {})["total"] == 0
            checks.append(scenario + ": T1 precedes model; no duplicate work or stale commit")

        legacy_db = root / "legacy-capture-race.sqlite3"
        entered, release = threading.Event(), threading.Event()
        Fixture.barriers["legacy-race"] = (entered, release)
        legacy = turn("legacy-race")
        del legacy["capture_source"]
        captured = {**turn("legacy-race"), "text": "我仍要求以后所有回答使用中文。"}
        with client(binary, legacy_db, env) as first, client(binary, legacy_db, env) as second:
            with ThreadPoolExecutor(max_workers=1) as worker:
                future = worker.submit(first.call, "memory_remember", legacy, True)
                try:
                    assert entered.wait(10)
                    assert not state(legacy_db)["sources"]
                    statuses(second.call("memory_remember", captured), "committed")
                    committed = state(legacy_db)
                    assert source(legacy_db, "legacy-race")["request"]["text"] == captured["text"]
                finally:
                    release.set()
                rejected = future.result(timeout=10)
                assert "already used with different arguments" in rejected["error"], rejected
                assert state(legacy_db) == committed, "late legacy result changed another source or memory reinforcement"
        checks.append("late legacy payload cannot attach to or reinforce a different captured request")

        summary = dict(status="passed", checks=checks, provider_calls=0,
                       fixture_calls=len(Fixture.requests), fixtures="synthetic loopback only")
        if arguments.output:
            arguments.output.mkdir(parents=True, exist_ok=True)
            (arguments.output / "acceptance.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
            (arguments.output / "inspector-source-fixture.json").write_text(json.dumps(bundle, ensure_ascii=False, indent=2) + "\n")
            trace_bundle["meta"]["notes"] = "实际 MCP/SQLite 原文留存执行与合成 loopback 模型响应；不证明真实模型语义质量。"
            (arguments.output / "runtime-trace-fixture.json").write_text(json.dumps(trace_bundle, ensure_ascii=False, indent=2) + "\n")
            (arguments.output / "runtime-events.jsonl").write_text(files[0].read_text())
        print(json.dumps(summary, ensure_ascii=False))


if __name__ == "__main__":
    main()
