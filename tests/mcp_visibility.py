#!/usr/bin/env python3
"""target-design 2.6: new inline write + recall.v8 over MCP and synthetic loopback HTTP."""
import argparse
import contextlib
import datetime
import http.server
import io
import json
import os
import sys
from pathlib import Path
import threading
import tempfile

from mcp_recall_concurrency import ConcurrentClient
from mcp_background import wait_status
from write_pipeline_process import Fixture, OWNER, batch, source, state, turn

OLD = "我住上海。"
NEW = "我已搬到南京。"
HYPOTHESIS = "假如我搬到南京，通勤会怎样？"
RELATIVE = "我昨天搬到南京。"


def fact(statement, evidence, ids=None, supersedes=None, times=None):
    return {"statement": statement, "evidence": evidence, "record_ids": ids or [],
            "polarity": "affirm", "strength": None, "uncertainty": None,
            "conditions": [], "times": times or [], "current": "yes", "applies": "yes",
            "supersedes_record_ids": supersedes or []}


class VisibilityFixture(Fixture):
    gates = []
    requests = []

    def do_POST(self):
        raw = self.rfile.read(int(self.headers["Content-Length"]))
        body = json.loads(raw)
        assert "model" not in json.loads(body["input"])
        for stage, text, entered, release in type(self).gates:
            payload = json.loads(body["input"])
            if body["text"]["format"]["name"] == stage and (text is None or payload.get("current_user_message") == text):
                entered.set()
                assert release.wait(20), "test did not release model response"
        stream = self.rfile
        try:
            self.rfile = io.BytesIO(raw)
            super().do_POST()
        finally:
            self.rfile = stream

    def output(self, body):
        stage = body["text"]["format"]["name"]
        payload = json.loads(body["input"])
        if stage == "w1_v4":
            text = payload["current_user_message"]
            if text == HYPOTHESIS:
                return {"candidates": [], "memory_requests": []}
            if "失败" in text:
                return {"invalid": True}
            result = super().output(body)
            result["candidates"][0]["relation"] = "lives_in"
            if text == RELATIVE:
                result["candidates"][0]["times"] = [{"role": "event", "quote": {"text": "昨天"},
                    "interp": {"kind": "relative", "unit": "day", "offset": -1}, "anchor": "said_on"}]
            return result
        if stage == "w4_v5":
            w4 = payload["w4_input"]
            old = next((r for r in w4["records"] if r["format"] == "current" and r["stored_status"] == "active"), None)
            return {"decisions": [{"candidate": c["index"], "status": "decided",
                    "action": "state_change" if old else "new", "targets": [old["id"]] if old else [],
                    "resolves_relations": [], "basis": [{"source": c["source"], "quote": c["candidate"]["sentence"]},
                        *(old["evidence"] if old else [])],
                    "effective": None, "relation_scope": None, "pending_reason": None} for c in w4["candidates"]]}
        if stage == "r1_v2":
            return {"need": "judge", "current_evidence": "query_only", "type": "single",
                    "subject": {"entity_id": "self"}, "relation": "居住地", "conditions": [], "time": None,
                    "plan": {"op": "lookup", "filter": {"relation": "lives_in", "time": None}, "unit": None}}
        if stage == "r4_v7":
            evidence = payload["evidence"]
            records = [r for r in evidence["records"] if r["format"] == "current" and r["stored_status"] == "active"]
            raw = next((r for r in evidence["raw"] if "搬到" in r["quote"]["text"] and r["quote"]["text"] != HYPOTHESIS and r["enrichment"] != "needs_rejudge"), None)
            if raw:
                basis = [{"source": {"kind": "stored", "source_id": raw["source_id"]}, "quote": raw["quote"]}]
                basis += [e for r in records for e in r["evidence"]]
                view = fact(raw["quote"]["text"], basis, supersedes=[r["id"] for r in records])
            elif records:
                r = records[0]
                view = fact(r["candidate"]["sentence"]["text"], r["evidence"], [r["id"]], times=r["candidate"]["times"])
            else:
                return {"verdict": "insufficient", "facts": [], "compute": None}
            return {"verdict": "supported", "facts": [view], "compute": None}
        return super().output(body)


def gate(stage, text=None):
    entered, release = threading.Event(), threading.Event()
    VisibilityFixture.gates.append((stage, text, entered, release))
    return entered, release


def recall(client, request_id):
    return client.call("memory_recall", {"query": "我住在哪？", "at": 1790899200, "request_id": request_id})["recall"]


def active_rows(profile):
    return [r for r in profile["repository"]["memories"].values() if r["status"] == "Active"]


def current(db):
    return active_rows(state(db))


def run(case, binary):
    with contextlib.ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix=f"mcp-visibility-{case}-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), VisibilityFixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        stack.callback(lambda: [g[3].set() for g in VisibilityFixture.gates])
        env = {k: v for k, v in os.environ.items() if not k.startswith(("PMR_", "OPENAI_", "DEEPSEEK_"))}
        env.update(PMR_MODEL_NETWORK="offline", DEEPSEEK_PRO_MODEL="pro-fixture", DEEPSEEK_API_KEY="local-fixture-key",
                   DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1")
        config = root / "inline.json"
        config.write_text(json.dumps({"write_pipeline": "w4_runtime_v1", "recall_pipeline": "recall_v3",
            "enrichment_mode": "inline", "background_operation_limit": 3,
            "model_budget": {"group_id": "visibility-fixture", "max_calls": 150, "max_operation_calls": 2}}))
        db = root / "memory.sqlite3"
        client = ConcurrentClient(binary, db, env, OWNER, config=config)
        stack.callback(client.close)
        if case != "f":
            client.call("memory_remember", turn("old", OLD))
            wait_status(db, "old", "committed")
        if case == "a":
            entered, release = gate("w1_v4", NEW)
            saved = client.finish_call(client.start_call("memory_remember", turn("new", NEW)), timeout=5)
            # Startup/periodic scans can claim between T1 and receipt reading, before the explicit wake.
            assert saved["enrichment"]["status"] in ("queued", "running"), saved
            assert entered.wait(8)
            before = recall(client, "before")
            assert not release.is_set()
            assert before["status"] == "answered" and before["facts"][0]["statement"] == NEW, before
            assert before["facts"][0]["grade"] == "uncommitted"
            assert any(r["quote"]["text"] == NEW for r in before["raw"])
            release.set()
            wait_status(db, "new", "committed")
            after = recall(client, "after")
            assert after["facts"][0]["statement"] == NEW and after["facts"][0]["grade"] == "committed", after
            assert after["snapshot"] != before["snapshot"]
        elif case == "b":
            client.call("memory_remember", turn("hypothesis", HYPOTHESIS))
            wait_status(db, "hypothesis", "no_candidates_unreviewed")
            result = recall(client, "hypothesis")
            assert result["facts"][0]["statement"] == OLD, result
            assert len(current(db)) == 1
        elif case == "c":
            text = "我已搬到南京。整理失败测试。"
            client.call("memory_remember", turn("failure", text))
            wait_status(db, "failure", "failed")
            saved = client.call("memory_remember", turn("failure", text))
            assert saved["enrichment"]["status"] == "failed", saved
            result = recall(client, "failure")
            assert result["facts"][0]["statement"] == text, result
            assert any(r["enrichment"] == "failed" and r["quote"]["text"] == text for r in result["raw"])
        elif case == "d":
            entered, release = gate("w1_v4", NEW)
            client.call("memory_remember", turn("omitted", NEW))
            assert entered.wait(8)
            for n in range(32):
                client.call("memory_remember", turn(f"noise-{n}", "我有一本画册。"))
            result = recall(client, "truncated")
            assert result["status"] == "insufficient", result
            assert result["completeness"]["retrieval_truncated"] and not result["completeness"]["plan_complete"]
            assert result["facts"][0]["current"] != "yes" and "budget_exhausted" not in result["degraded_reasons"]
            release.set()
        elif case == "e":
            entered, release = gate("w1_v4", NEW)
            client.call("memory_remember", turn("older", NEW))
            assert entered.wait(8)
            latest = "我已搬到杭州。"
            saved = client.call("memory_remember", turn("newer", latest))
            # The single worker is held; a separately authorized CLI operation finishes the newer update first.
            batch(binary, db, env, config, "--enrich-source", saved["source"]["source_id"])
            wait_status(db, "newer", "committed")
            release.set()
            wait_status(db, "older", "needs_rejudge")
            rows = current(db)
            assert any(e["quote"] == latest for r in rows for e in r["evidence"]), rows
            assert not any(e["quote"] == NEW for r in rows for e in r["evidence"]), rows
            result = recall(client, "ordered")
            assert result["facts"][0]["statement"] == latest, result
            assert any(r["enrichment"] == "needs_rejudge" for r in result["raw"]), result
        elif case == "f":
            client.call("memory_remember", turn("relative", RELATIVE))
            wait_status(db, "relative", "committed")
            s = source(db, "relative")
            assert s["observed_at"] == 1790899200
            rows = current(db)
            assert rows
            # Frozen W2 resolution is in the saved upstream batch, not the execution clock.
            result_json = json.dumps(s["result"], ensure_ascii=False)
            assert "time_resolutions" in result_json, s
            def resolutions(obj):
                if isinstance(obj, dict):
                    if "time_resolutions" in obj:
                        yield from obj["time_resolutions"]
                    for value in obj.values():
                        yield from resolutions(value)
                elif isinstance(obj, list):
                    for value in obj:
                        yield from resolutions(value)
            dates = [datetime.datetime.fromtimestamp(r["start"] + 28800, datetime.timezone.utc).date().isoformat()
                     for r in resolutions(s["result"]) if r.get("start") is not None]
            assert dates and set(dates) == {"2026-10-01"}, dates
            w1 = next(r for r in VisibilityFixture.requests if r["text"]["format"]["name"] == "w1_v4")
            assert json.loads(w1["input"])["said_on"] == "2026-10-02"
            result = recall(client, "relative")
            assert result["facts"][0]["grade"] == "committed", result
        elif case == "g":
            entered, release = gate("w1_v4", NEW)
            r4_entered, r4_release = gate("r4_v7")
            saved = client.call("memory_remember", turn("deleted", NEW))
            assert entered.wait(8)
            foreground = client.start_call("memory_recall", {"query": "我住在哪？", "at": 1790899200, "request_id": "deleted"})
            assert r4_entered.wait(8)
            client.finish_call(client.start_call("memory_forget", {"source_id": saved["source"]["source_id"]}), timeout=5)
            release.set()
            r4_release.set()
            failed = client.finish_call(foreground, error=True)
            assert "recall_claim_lost" in failed["error"] or "recall_snapshot_changed" in failed["error"], failed
            assert NEW not in json.dumps(failed, ensure_ascii=False)
            wait_status(db, "deleted", "deleted")
            # Join the executor by letting a following task finish, proving the late response has arrived.
            VisibilityFixture.gates = []
            client.call("memory_remember", turn("after-delete", HYPOTHESIS))
            wait_status(db, "after-delete", "no_candidates_unreviewed")
            assert NEW not in json.dumps(state(db), ensure_ascii=False)
            result = recall(client, "after-delete")
            assert NEW not in json.dumps(result, ensure_ascii=False)
        print(f"MCP visibility {case} passed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--case", choices=list("abcdefg"))
    parser.add_argument("--check-fixture", action="store_true")
    args = parser.parse_args()
    if args.check_fixture:
        fixture = VisibilityFixture.__new__(VisibilityFixture)
        print(json.dumps([fixture.output(body) for body in json.load(sys.stdin)], ensure_ascii=False))
    else:
        if not (args.binary and args.case):
            parser.error("--binary and --case are required")
        run(args.case, args.binary)


if __name__ == "__main__":
    main()
