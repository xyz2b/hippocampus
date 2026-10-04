#!/usr/bin/env python3
"""Single MCP background worker, startup scan, T1 notification and foreground event barrier."""
import argparse
import contextlib
import http.server
import io
import json
import os
from pathlib import Path
import tempfile
import threading
import time

from mcp_recall_concurrency import ConcurrentClient
from write_pipeline_process import Fixture, batch, calls, save_cli, source, turn


class BackgroundFixture(Fixture):
    requests = []
    gate = None
    invalid = False

    def do_POST(self):
        raw = self.rfile.read(int(self.headers["Content-Length"]))
        body = json.loads(raw)
        gate = type(self).gate
        if gate and body["text"]["format"]["name"] == gate[0]:
            gate[1].set()
            assert gate[2].wait(20), "background model was not released"
        stream = self.rfile
        try:
            self.rfile = io.BytesIO(raw)
            super().do_POST()
        finally:
            self.rfile = stream

    def output(self, body):
        stage = body["text"]["format"]["name"]
        if stage == "r1_v1":
            return {"need": "judge", "current_evidence": "query_only", "type": "single",
                    "subject": {"entity_id": "self"}, "relation": "居住地", "conditions": [],
                    "time": None, "plan": {"op": "lookup", "filter": {"relation": "lives_in", "time": None}, "unit": None}}
        if stage == "r4_v3":
            return {"verdict": "insufficient", "facts": [], "compute": None}
        if stage == "w1_v1" and type(self).invalid:
            return {"synthetic_invalid_output": True}
        if stage == "w1_v1":
            result = super().output(body)
            candidate = result["candidates"][0]
            text = candidate["sentence"]["text"]
            city = next(city for city in ("海棠市", "青岚市") if city in text)
            candidate["relation"] = "lives_in"
            candidate["value"] = {"kind": "text", "text": city, "quote": {"text": city}}
            return result
        if stage == "w4_v2":
            w4 = json.loads(body["input"])["w4_input"]
            old = next(record for record in w4["records"] if record["format"] == "current")
            return {"decisions": [{"candidate": candidate["index"], "status": "decided",
                "action": "state_change", "targets": [old["id"]], "resolves_relations": [],
                "basis": [{"source": candidate["source"], "quote": candidate["candidate"]["sentence"]}, *old["evidence"]],
                "effective": None, "relation_scope": None, "pending_reason": None}
                for candidate in w4["candidates"]]}
        return super().output(body)


def wait_status(db, name, status):
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        if source(db, name)["enrichment"] == status:
            return
        threading.Event().wait(0.01)
    raise AssertionError((name, source(db, name)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    binary = parser.parse_args().binary
    with contextlib.ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="mcp-background-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), BackgroundFixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("PMR_", "OPENAI_", "DEEPSEEK_"))}
        env.update(PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
                   DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1")
        db = root / "memory.sqlite3"
        config = root / "joint.json"
        config.write_text(json.dumps({"write_pipeline": "w4_runtime_v1", "recall_pipeline": "recall_v3",
            "enrichment_mode": "deferred", "background_operation_limit": 3,
            "model_budget": {"group_id": "mcp-background-fixture", "max_calls": 10, "max_operation_calls": 2}}))
        old = save_cli(binary, db, env, config, turn("old", "我住在海棠市。"))
        batch(binary, db, env, config, "--enrich-source", old["source"]["source_id"])
        new_text = "我已搬到青岚市。"
        save_cli(binary, db, env, config, turn("startup", new_text))
        assert calls(db) == 2
        entered, release = threading.Event(), threading.Event()
        BackgroundFixture.gate = "w1_v1", entered, release
        client = ConcurrentClient(binary, db, env, "write-pipeline-fixture", config=config)
        try:
            assert entered.wait(8), "startup scan did not recover the queued source"
            result = client.finish_call(client.start_call("memory_recall", {
                "query": "我住在哪？", "request_id": "while-background-held", "at": 1790899200}), timeout=5)
            recall = result["recall"]
            assert not release.is_set()
            assert recall["status"] == "insufficient" and recall["facts"] == [], recall
            assert any(raw["quote"]["text"] == new_text and raw["enrichment"] == "running"
                       for raw in recall["raw"]), recall
            assert recall["records"], recall
            assert calls(db) == 5  # old2 + reserved W1 + completed R1/R4
            release.set()
            wait_status(db, "startup", "committed")
            assert calls(db) == 7
            before = len(BackgroundFixture.requests)
            BackgroundFixture.gate = None
            BackgroundFixture.invalid = True
            result = client.call("memory_remember", turn("notification", "我在整理书架。"))
            assert result["source"]["status"] == "stored", result
            wait_status(db, "notification", "failed")
            assert calls(db) == 8 and len(BackgroundFixture.requests) == before + 1
            # The failure is visible and never becomes an automatic retry on later foreground calls.
            for _ in range(3):
                client.call("memory_remember", turn("notification", "我在整理书架。"))
            assert calls(db) == 8 and source(db, "notification")["enrichment"] == "failed"
        finally:
            release.set()
            client.close()
        print("MCP deferred startup/notification, foreground-before-background-release, raw visibility and no failure retry passed")


if __name__ == "__main__":
    main()
