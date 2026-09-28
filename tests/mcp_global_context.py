#!/usr/bin/env python3
"""Real stdio regression, isolated SQLite and loopback model fixture; never a web test."""
import argparse
import contextlib
import copy
import http.server
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import threading

from mcp_smoke import Client, Responses


def check(name, condition, observed):
    print(json.dumps({"acf_assertion": {"id": name, "outcome": "pass" if condition else "fail",
                                       "observed": json.dumps(observed, ensure_ascii=False)}}, ensure_ascii=False), flush=True)
    if not condition:
        raise SystemExit(1)


def snapshot(db):
    with sqlite3.connect(db) as conn:
        return json.loads(conn.execute("SELECT state_json FROM memory_profiles WHERE owner_id='smoke-user'").fetchone()[0])


def save_snapshot(db, state):
    with sqlite3.connect(db) as conn:
        conn.execute("UPDATE memory_profiles SET state_json=? WHERE owner_id='smoke-user'", (json.dumps(state),))


def requirements(value):
    ctx = value.get("global_context", {})
    assert ctx.get("status") == "available", value
    return [m for lane in ("mandatory", "constraints", "preferences", "facts", "episodes")
            for m in ctx["result"][lane]]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build", action="store_true")
    parser.add_argument("--binary", type=Path, default=Path("target/debug/pmr-mcp"))
    args = parser.parse_args()
    if args.build:
        subprocess.run(["cargo", "build", "--offline", "--bin", "pmr-mcp"], check=True)
    with contextlib.ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-global-context-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Responses)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        env = dict(os.environ, PMR_MODEL_NETWORK="offline", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1", DEEPSEEK_API_KEY="local-fixture-key")
        db = root / "memory.sqlite3"
        client = Client(args.binary.resolve(), db, env, budget_calls=100)
        stack.callback(client.close)
        request = dict(text="以后无论聊什么，回答都必须使用中文。", session_id="direct-save", turn_id="one")
        saved = client.call("memory_remember", request)
        # The first request is remember, with no bootstrap or list beforehand.
        check("direct-save", saved.get("global_context", {}).get("status") == "available"
              and saved.get("replayed") is False
              and any(m["value"] == {"Text": "中文"} for m in requirements(saved)),
              {"first_tool": "memory_remember", "reply": saved})
        original = snapshot(db)
        template = copy.deepcopy(next(iter(original["repository"]["memories"].values())))
        seeded = copy.deepcopy(original)
        def seed(mid, **fields):
            m = copy.deepcopy(template)
            m.update(id=mid, **fields)
            seeded["repository"]["memories"][str(mid)] = m
        seed(2, subject_id="colleague", value={"Text": "OTHER_SUBJECT"})
        seed(3, owner_id="different-owner", value={"Text": "OTHER_OWNER"})
        seed(4, scope={"domain": None, "conditions": [{"Task": "project-a"}]}, value={"Text": "PROJECT_ONLY"})
        seed(5, scope={"domain": "breakfast", "conditions": []}, value={"Text": "DOMAIN_ONLY"})
        seed(6, status="Deleted", value={"Text": "DELETED"})
        seed(7, status="Superseded", value={"Text": "OLD"})
        seed(8, retention_tier="ShortTerm", temporal={**template["temporal"], "expires_at": 1}, value={"Text": "EXPIRED"})
        seed(9, temporal={**template["temporal"], "valid_from": {"at": 4102444800, "source": "Explicit"}}, value={"Text": "FUTURE"})
        seed(10, predicate="likes", memory_type="Fact", value={"Text": "UNRELATED"})
        seed(11, predicate="response_style", memory_type="Preference", value={"Text": "先给结论"})
        seed(12, temporal={**template["temporal"], "valid_to": {"at": 1, "source": "Explicit"}}, value={"Text": "NO_LONGER_VALID"})
        seeded["repository"]["next_id"] = 12
        save_snapshot(db, seeded)
        replayed = client.call("memory_remember", request)
        after = snapshot(db)
        actual = requirements(replayed)
        # Replay performs no write. The extra snapshot must not update expiration or counters either.
        guard_ok = after == seeded and {m["id"] for m in actual} == {1, 11}
        guard_ok &= all(m["retrieval_count"] == 0 and m["last_retrieved_at"] is None for m in actual)
        guard_ok &= replayed["original_result"] == {k: v for k, v in saved.items()
                                                    if k not in ("global_context", "enrichment_mode", "model_calls")}
        check("snapshot-guards", guard_ok, {"returned_ids": [m["id"] for m in actual], "persistent_state_unchanged": after == seeded})
        # Successful, failed and replayed calls all pass through the same response envelope.
        observed = []
        for name, arguments, error in [
            ("memory_list", {}, False),
            ("memory_entity_upsert", dict(id="person", canonical_name="测试人物", entity_type="Person"), False),
            ("memory_clarify", dict(staged_id=999, entity_reference="测试人物", text="指测试人物", session_id="clarify", turn_id="one"), True),
            ("memory_recall", dict(query="全局回答要求", bootstrap=True), False),
            ("memory_remember", {**request, "text": "different request, same IDs"}, True),
        ]:
            r = client.call(name, arguments, error=error)
            observed.append((name, [m["id"] for m in requirements(r)]))
            assert {m["id"] for m in requirements(r)} == {1, 11}, r
        # Read the latest version on every call, including replay (no cached original context).
        latest = snapshot(db)
        latest["repository"]["memories"]["1"]["value"] = {"Text": "英语"}
        save_snapshot(db, latest)
        assert next(m for m in requirements(client.call("memory_remember", request)) if m["id"] == 1)["value"] == {"Text": "英语"}
        deleted = client.call("memory_forget", {"memory_id": 1})
        assert deleted["forgotten_ids"] == [1] and {m["id"] for m in requirements(deleted)} == {11}, deleted
        other = Client(args.binary.resolve(), db, env, owner="another-owner")
        try:
            assert requirements(other.call("memory_list", {})) == []
        finally:
            other.close()
        check("all-tools", True, {"calls": observed, "forget_excludes_deleted": True, "owner_isolated": True})
        # A real post-commit unreadable snapshot: SQLite trigger corrupts JSON only after
        # a successful remember commit. No production-only test hooks are added.
        save_snapshot(db, original)
        with sqlite3.connect(db) as conn:
            conn.execute("CREATE TRIGGER break_context AFTER UPDATE ON memory_profiles BEGIN UPDATE memory_profiles SET state_json='invalid-json' WHERE owner_id=NEW.owner_id; END")
        r = client.call("memory_remember", {**request, "session_id": "context-failure"})
        check("context-failure", r.get("replayed") is False
              and r.get("outcomes", [{}])[0].get("Ingested", {}).get("Applied", {}).get("action") == "Reinforce"
              and r.get("global_context", {}).get("status") == "unavailable", r)
        assert Responses.calls == 4, f"two extractions + two reviews; snapshot/replay must not send: {Responses.calls}"


if __name__ == "__main__":
    main()
