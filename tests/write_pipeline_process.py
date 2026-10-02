#!/usr/bin/env python3
"""④-5 CLI dispatch against synthetic Responses HTTP and disposable SQLite only."""
import argparse
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

OWNER = "write-pipeline-fixture"
TEXT = "我正在研究数据库索引。"
LEGACY_TEXT = "以后所有回答都必须使用中文。"


class Fixture(Responses):
    requests = []

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        assert self.path == "/v1/responses"
        assert self.headers["Authorization"] == "Bearer local-fixture-key"
        type(self).requests.append(body)
        output = self.output(body)
        data = json.dumps({"status": "completed", "usage": {"input_tokens": 10, "output_tokens": 10},
                           "output": [{"type": "message", "content": [{"type": "output_text",
                           "text": json.dumps(output, ensure_ascii=False)}]}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def output(self, body):
        name = body["text"]["format"]["name"]
        payload = json.loads(body["input"])
        if name == "w1_v1":
            text = payload["current_user_message"]
            return {"candidates": [{"kind": "assertion", "speech": "user_statement",
                    "subject": {"entity_id": "self"}, "relation": "fact", "memory_type": "fact",
                    "value": {"kind": "text", "text": text, "quote": {"text": text}},
                    "sentence": {"text": text}, "polarity": "affirm", "strength": None,
                    "uncertainty": None, "conditions": [], "times": []}], "memory_requests": []}
        if name == "w3_v1":
            return {"reviews": [{"candidate": c["index"], "verdict": "supported", "problems": [],
                                 "episode_value": "not_applicable"} for c in payload["candidates"]],
                    "coverage": {"status": "complete", "missing": []}, "request_reviews": []}
        if name == "w4_v2":
            decisions = []
            for candidate in payload["w4_input"]["candidates"]:
                matching = next((r for r in payload["w4_input"]["records"]
                                 if r["format"] == "current" and r["candidate"] == candidate["candidate"]), None)
                basis = [{"source": candidate["source"], "quote": candidate["candidate"]["sentence"]}]
                if matching:
                    basis.append(matching["evidence"][0])
                decisions.append({"candidate": candidate["index"], "status": "decided",
                                  "action": "reinforce" if matching else "new",
                                  "targets": [matching["id"]] if matching else [],
                                  "resolves_relations": [], "basis": basis, "effective": None,
                                  "relation_scope": None, "pending_reason": None})
            return {"decisions": decisions}
        return super().output(body)


def state(db):
    with sqlite3.connect(db) as connection:
        return json.loads(connection.execute("SELECT state_json FROM memory_profiles WHERE owner_id=?", (OWNER,)).fetchone()[0])


def source(db, turn):
    return next(s for s in state(db)["sources"].values() if s["turn_id"] == turn)


def calls(db):
    with sqlite3.connect(db) as connection:
        return connection.execute("SELECT count(*) FROM model_calls").fetchone()[0]


def settings(root, pipeline, mode):
    path = root / f"{pipeline}-{mode}.json"
    path.write_text(json.dumps({"write_pipeline": pipeline, "enrichment_mode": mode,
                               "retrieval_mode": "local", "model_budget": {
                                   "group_id": "write-pipeline-http", "max_calls": 30}}))
    return path


def turn(name, text=TEXT):
    return {"session_id": "write-pipeline", "turn_id": name, "text": text,
            "capture_source": True, "observed_at": 1790899200}


def save(binary, db, env, config, request):
    client = Client(binary, db, env, OWNER, config=config)
    try:
        return client.call("memory_remember", request)
    finally:
        client.close()


def batch(binary, db, env, config, *args):
    process = subprocess.run([str(binary), "--db", str(db), "--owner", OWNER,
                              "--config", str(config), *args], env=env, text=True,
                             capture_output=True, timeout=35)
    assert process.returncode == 0, (process.stdout, process.stderr)
    return json.loads(process.stdout)


def frozen(result, status, mcp=False):
    assert set(result) == {"source", "enrichment"} | ({"global_context"} if mcp else set()), result
    assert set(result["source"]) == {"status", "source_id"}, result
    assert result["source"]["status"] == "stored", result
    assert set(result["enrichment"]) == {"status", "candidates", "memory_requests", "retention_pending"}, result
    assert result["enrichment"]["status"] == status, result
    assert result["enrichment"]["memory_requests"] == [], result
    assert result["enrichment"]["retention_pending"] is False, result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--mode", choices=["explicit", "drain"], required=True)
    args = parser.parse_args()
    with contextlib.ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="write-pipeline-http-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("PMR_", "OPENAI_", "DEEPSEEK_"))}
        env.update(PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
                   DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1")
        db = root / "memory.sqlite3"
        legacy = settings(root, "legacy", "deferred")
        new = settings(root, "w4_runtime_v1", "off" if args.mode == "explicit" else "deferred")
        if args.mode == "explicit":
            first = save(args.binary, db, env, new, turn("first"))
            frozen(first, "not_requested", mcp=True)
            assert Fixture.requests == []
            first_id = first["source"]["source_id"]
            processed = batch(args.binary, db, env, legacy, "--enrich-source", first_id)
            frozen(processed, "committed")
            assert [r["text"]["format"]["name"] for r in Fixture.requests] == ["w1_v1", "w3_v1"]
            second = save(args.binary, db, env, new, turn("second"))
            processed = batch(args.binary, db, env, legacy, "--enrich-source", second["source"]["source_id"])
            frozen(processed, "committed")
            assert [r["text"]["format"]["name"] for r in Fixture.requests][-3:] == ["w1_v1", "w3_v1", "w4_v2"]
            assert processed["enrichment"]["candidates"][0]["memory_id"] is not None
            assert len(state(db)["repository"]["memories"]) == 1
            before, sends, paid = state(db), len(Fixture.requests), calls(db)
            replay = save(args.binary, db, env, legacy, turn("second"))
            frozen(replay, "committed", mcp=True)
            assert {k: replay[k] for k in ["source", "enrichment"]} == processed
            assert batch(args.binary, db, env, legacy, "--enrich-source", second["source"]["source_id"]) == processed
            assert state(db) == before and len(Fixture.requests) == sends and calls(db) == paid == 5
            assert source(db, "second")["task"]["round"] == 0
        else:
            expected = []
            for name, config, text in [("old1", legacy, LEGACY_TEXT), ("new1", new, TEXT),
                                       ("old2", legacy, LEGACY_TEXT), ("new2", new, TEXT)]:
                result = save(args.binary, db, env, config, turn(name, text))
                expected.append(result["source"]["source_id"])
            assert Fixture.requests == [] and calls(db) == 0
            before = state(db)
            assert save(args.binary, db, env, new, turn("old1", LEGACY_TEXT))["replayed"] is True
            frozen(save(args.binary, db, env, legacy, turn("new1")), "queued", mcp=True)
            assert state(db) == before and Fixture.requests == []
            drained = batch(args.binary, db, env, legacy, "--drain-enrichment", "--max-jobs", "4")
            assert set(drained) == {"max_jobs", "attempted", "results"}, drained
            assert drained["attempted"] == 4 and drained["max_jobs"] == 4, drained
            assert [r["source_id"] for r in drained["results"]] == expected, drained
            for index, result in enumerate(drained["results"]):
                assert set(result) == {"source_id", "status", "result"} and result["status"] == "completed", result
                if index % 2:
                    frozen(result["result"], "committed")
                else:
                    old = result["result"]
                    assert old["status"] == "completed" and old["enrichment_mode"] == "deferred", old
                    assert old["index"]["status"] == "ready" and "outcomes" in old, old
                    assert old["model_calls"] and "w4_checkpoint" not in json.dumps(old), old
            names = [r["text"]["format"]["name"] for r in Fixture.requests]
            assert len(names) == calls(db) == 9, names
            assert names[2:4] == ["w1_v1", "w3_v1"] and names[-3:] == ["w1_v1", "w3_v1", "w4_v2"], names
            before, sends = state(db), len(Fixture.requests)
            assert batch(args.binary, db, env, new, "--drain-enrichment", "--max-jobs", "4")["attempted"] == 0
            assert state(db) == before and len(Fixture.requests) == sends
            for name in ["new1", "new2"]:
                assert source(db, name)["task"]["processing_version"] == "w4_runtime_v1"
                assert source(db, name)["task"]["round"] == 0
            for name in ["old1", "old2"]:
                assert source(db, name)["task"]["processing_version"] == "s5_enrichment_v1"
        print(f"write_pipeline {args.mode}: HTTP dispatch, SQLite reopen, frozen receipts and zero-send replay passed")


if __name__ == "__main__":
    main()
