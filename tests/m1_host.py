#!/usr/bin/env python3
"""Default-path stdio guards. Disposable legacy data; denied network; no HTTP listener."""
import argparse
import copy
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import time

from mcp_smoke import Client


def profile(db):
    with sqlite3.connect(db) as c:
        return json.loads(c.execute("SELECT state_json FROM memory_profiles WHERE owner_id='alice'").fetchone()[0])


def seed(db):
    old = json.loads(Path("tests/fixtures/m1_legacy/source-less-profile.json").read_text())
    with sqlite3.connect(db) as c:
        c.execute("CREATE TABLE memory_profiles(owner_id TEXT PRIMARY KEY, state_json TEXT NOT NULL)")
        c.execute("INSERT INTO memory_profiles VALUES ('alice',?)", (json.dumps(old),))
        c.execute("PRAGMA user_version=1")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--check", choices=["smoke", "global", "prepare", "trace", "source", "local", "modes"], required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="m1-host-") as directory:
        root = Path(directory)
        db = root / "memory.sqlite3"
        seed(db)
        config = root / "config.json"
        # Only disable automatic enrichment. Both pipeline selectors and models are omitted.
        config.write_text(json.dumps({"enrichment_mode": "off"}))
        env = {k: v for k, v in os.environ.items() if not k.startswith(("PMR_", "DEEPSEEK_", "OPENAI_"))}
        env["PMR_MODEL_NETWORK"] = "deny"
        if args.check == "trace":
            trace = root / "trace"
            trace.mkdir(mode=0o700)
            env["PMR_TRACE_DIR"] = str(trace)
        client = Client(args.binary.resolve(), db, env, "alice", config=config)
        try:
            tools = {t["name"]: t for t in client.rpc("tools/list", {})["tools"]}
            assert len(tools) == 11
            assert "memory_source_import_history" in tools
            assert tools["memory_source_import_history"]["annotations"]["readOnlyHint"] is False
            assert "memory_source_reprocess" in tools
            assert tools["memory_source_reprocess"]["annotations"]["readOnlyHint"] is False
            assert "memory_source_status" in tools
            assert tools["memory_source_status"]["annotations"]["readOnlyHint"] is True
            assert "request_id" in tools["memory_recall"]["inputSchema"]["required"]
            turn = {"text": "我有一把蓝伞。", "session_id": "m1", "turn_id": "one", "capture_source": True}
            if args.check in ("smoke", "source", "trace", "modes"):
                for capture in (None, False):
                    invalid = copy.deepcopy(turn)
                    if capture is None:
                        invalid.pop("capture_source")
                    else:
                        invalid["capture_source"] = capture
                    before = profile(db)
                    client.call("memory_remember", invalid, error=True)
                    assert profile(db) == before
                saved = client.call("memory_remember", turn)
                assert saved["source"]["status"] == "stored"
                assert saved["enrichment"]["status"] == "not_requested"
                assert client.call("memory_remember", turn) == saved
                before = profile(db)
                client.call("memory_remember", {**turn, "text": "同键换原话。"}, error=True)
                assert profile(db) == before
                offline = client.call("memory_recall", {"query": "我的伞", "request_id": "raw", "offline": True})
                assert offline["recall"]["version"] == "recall.v6"
                assert offline["recall"]["semantic_judgment"] == "skipped"
                assert any(r["source_id"] == saved["source"]["source_id"] for r in offline["recall"]["raw"])
                client.call("memory_forget", {"source_id": saved["source"]["source_id"]})
                client.call("memory_remember", turn, error=True)
                client.call("memory_recall", {"query": "我的伞", "request_id": "raw", "offline": True}, error=True)
                fresh = client.call("memory_recall", {"query": "我的伞", "request_id": "deleted", "offline": True})
                assert "我有一把蓝伞" not in json.dumps(fresh, ensure_ascii=False)
            if args.check == "prepare":
                prepared = client.call("memory_prepare", {"user_turn": turn, "offline": True})
                assert set(prepared) == {"source", "enrichment", "recall", "global_context"}
                assert prepared["source"]["status"] == "stored"
                assert prepared["enrichment"]["status"] == "not_requested"
                assert any(r["source_id"] == prepared["source"]["source_id"] for r in prepared["recall"]["raw"])
                assert client.call("memory_prepare", {"user_turn": turn, "offline": True}) == prepared
                rejected = client.call("memory_prepare", {"user_turn": {**turn, "text": "原话已变"}, "offline": True})
                assert rejected["source"]["status"] == "rejected"
                assert rejected["recall"]["status"] == "not_requested"
            if args.check in ("global", "local"):
                before = profile(db)
                boot = client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True, "request_id": "bootstrap"})
                assert boot["global_context"]["status"] == "available"
                assert boot["recall"]["semantic_judgment"] == "skipped"
                ids = {m["id"] for lane in boot["global_context"]["result"].values() if isinstance(lane, list)
                       for m in lane if isinstance(m, dict) and "id" in m}
                assert ids == {2}, ids
                after = profile(db)
                # Request replay receipts are retained; business data and counters are read-only.
                assert {k: v for k, v in after.items() if k != "receipts"} == {
                    k: v for k, v in before.items() if k != "receipts"
                }
                old = client.call("memory_recall", {"query": "旧记录", "offline": True, "request_id": "legacy-data"})
                record = next(r for r in old["recall"]["records"] if r["id"] == 1)
                assert record["format"] == "legacy" and record["grade"] == "committed"
                assert record["evidence"] == [] and record["update"] is None
                assert "source_missing" in record["gaps"]
                assert record["data"]["retrieval_count"] == 7
                before = profile(db)
                client.call("memory_clarify", {}, error=True)
                assert profile(db) == before
                client.call("memory_forget", {"memory_id": 1})
                client.call("memory_recall", {"query": "旧记录", "offline": True, "request_id": "legacy-data"}, error=True)
                fresh = client.call("memory_recall", {"query": "旧记录", "offline": True, "request_id": "no-legacy"})
                assert "广州" not in json.dumps(fresh, ensure_ascii=False)
                if args.check == "global":
                    # A new-default T1 commit remains successful even if the following snapshot fails.
                    with sqlite3.connect(db) as c:
                        c.execute("CREATE TRIGGER break_context AFTER UPDATE ON memory_profiles BEGIN UPDATE memory_profiles SET state_json='invalid-json' WHERE owner_id=NEW.owner_id; END")
                    saved = client.call("memory_remember", turn)
                    assert saved["source"]["status"] == "stored"
                    assert saved["global_context"]["status"] == "unavailable"
            if args.check in ("smoke", "modes"):
                for tool in ("memory_recall", "memory_prepare", "memory_validate_answer"):
                    unsupported = client.call(tool, {"recall_version": "answer_contract.v1"}, error=True)
                    assert "contract_version_unsupported" in json.dumps(unsupported)
                client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True}, error=True)
            with sqlite3.connect(db) as c:
                assert c.execute("SELECT count(*) FROM model_calls").fetchone()[0] == 0
        finally:
            client.close()
        if args.check == "smoke":
            # Restart keeps old data and deletion; owner isolation requires no credentials.
            other = Client(args.binary.resolve(), db, env, "bob", config=config)
            try:
                assert other.call("memory_list", {})["total"] == 0
                other.call("memory_forget", {"memory_id": 2}, error=True)
            finally:
                other.close()
            restarted = Client(args.binary.resolve(), db, env, "alice", config=config)
            try:
                assert restarted.call("memory_list", {})["total"] >= 1
                recalled = restarted.call("memory_recall", {"query": "我的伞", "request_id": "restart", "offline": True})
                assert "我有一把蓝伞" not in json.dumps(recalled, ensure_ascii=False)
            finally:
                restarted.close()
        if args.check == "modes":
            # Missing default endpoint is explicit for an authorized background worker, with no send.
            config.write_text(json.dumps({"model_budget": {"group_id": "missing-default", "max_calls": 3}, "background_operation_limit": 3}))
            env.update(PMR_MODEL_NETWORK="offline", DEEPSEEK_BASE_URL="http://127.0.0.1:1/v1", DEEPSEEK_API_KEY="synthetic-unused")
            worker = Client(args.binary.resolve(), db, env, "alice", config=config)
            try:
                stored = worker.call("memory_remember", {**turn, "turn_id": "missing-endpoint"})
                assert stored["source"]["status"] == "stored"
                deadline = time.monotonic() + 8
                while time.monotonic() < deadline:
                    source = next(s for s in profile(db)["sources"].values() if s["turn_id"] == "missing-endpoint")
                    if source["enrichment"] == "failed":
                        break
                    time.sleep(0.02)
                assert source["enrichment"] == "failed"
                worker.errors.seek(0)
                assert "DEEPSEEK_PRO_MODEL" in worker.errors.read()
                with sqlite3.connect(db) as c:
                    assert c.execute("SELECT count(*) FROM model_calls").fetchone()[0] == 0
            finally:
                worker.close()
        if args.check == "trace":
            events = [json.loads(line) for f in trace.glob("*.jsonl") for line in f.read_text().splitlines()]
            assert events and not any(e["kind"] == "model_start" for e in events)
            assert not any("Bearer" in json.dumps(e) for e in events)
        print(f"M1 default {args.check}: passed, physical model calls=0")


if __name__ == "__main__":
    main()
