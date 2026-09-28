#!/usr/bin/env python3
"""Deferred T1/T2 checks through disposable MCP and explicit batch processes."""
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


OWNER = "s5-deferred-fixture"
TEXT = "以后所有回答都必须使用中文。"


class Fixture(Responses):
    requests = []
    blocked_turn = None
    entered = threading.Event()
    release = threading.Event()

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        assert self.path == "/v1/responses"
        assert self.headers["Authorization"] == "Bearer local-fixture-key"
        type(self).requests.append(body)
        payload = json.loads(body["input"])
        if payload.get("turn_id") == self.blocked_turn:
            type(self).entered.set()
            assert type(self).release.wait(20), "blocked worker was not released"
        output = self.output(body)
        data = json.dumps({"status": "completed", "usage": {"input_tokens": 10, "output_tokens": 10},
                           "output": [{"type": "message", "content": [{"type": "output_text",
                           "text": json.dumps(output, ensure_ascii=False)}]}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        try:
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError):
            pass  # A worker can be killed while its synthetic response is held.


def state(db):
    with sqlite3.connect(db) as connection:
        row = connection.execute("SELECT state_json FROM memory_profiles WHERE owner_id=?", (OWNER,)).fetchone()
    return json.loads(row[0])


def source(db, turn):
    return next(value for value in state(db)["sources"].values() if value["turn_id"] == turn)


def calls(db):
    with sqlite3.connect(db) as connection:
        if not connection.execute("SELECT 1 FROM sqlite_master WHERE name='model_calls'").fetchone():
            return 0
        return connection.execute("SELECT count(*) FROM model_calls").fetchone()[0]


def downgrade_v4(db, mutate=None):
    with sqlite3.connect(db) as connection:
        old = json.loads(connection.execute("SELECT state_json FROM memory_profiles WHERE owner_id=?",
                                            (OWNER,)).fetchone()[0])
        if mutate:
            mutate(old)
        old.pop("last_applied_source_sequence", None)
        old.pop("source_less_barrier_sequence", None)
        for record in old["sources"].values():
            record.pop("task", None)
        connection.execute("UPDATE memory_profiles SET state_json=? WHERE owner_id=?",
                           (json.dumps(old, ensure_ascii=False), OWNER))
        connection.execute("PRAGMA user_version=4")


def config(directory, name, mode):
    path = directory / f"{name}.json"
    path.write_text(json.dumps({"retrieval_mode": "local", "enrichment_mode": mode,
                                "model_budget": {"group_id": f"s5-deferred-{name}", "max_calls": 20}}))
    return path


def turn(name, history=None):
    return {"text": TEXT, "session_id": "deferred-fixture", "turn_id": name,
            "history": history or [], "capture_source": True}


@contextlib.contextmanager
def client(binary, db, env, settings):
    current = Client(binary, db, env, OWNER, config=settings)
    try:
        yield current
    finally:
        current.close()


def batch(binary, db, env, settings, *args):
    return subprocess.run([str(binary), "--db", str(db), "--owner", OWNER,
                           "--config", str(settings), *args], env=env,
                          text=True, capture_output=True, timeout=35)


def blocked_drain(binary, db, env, settings, turn_id):
    Fixture.blocked_turn = turn_id
    Fixture.entered.clear()
    Fixture.release.clear()
    worker = subprocess.Popen([str(binary), "--db", str(db), "--owner", OWNER,
                               "--config", str(settings), "--drain-enrichment", "--max-jobs", "1"],
                              env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    assert Fixture.entered.wait(10), "deferred worker never reached the model fixture"
    return worker


def finish_drain(worker):
    Fixture.release.set()
    output, errors = worker.communicate(timeout=35)
    assert worker.returncode == 0, (output, errors)
    Fixture.blocked_turn = None
    return output


def expire_lease(db, turn_id):
    with sqlite3.connect(db) as connection:
        row = connection.execute("SELECT state_json FROM memory_profiles WHERE owner_id=?", (OWNER,)).fetchone()
        snapshot = json.loads(row[0])
        next(value for value in snapshot["sources"].values() if value["turn_id"] == turn_id)["task"]["lease_until"] = 0
        connection.execute("UPDATE memory_profiles SET state_json=? WHERE owner_id=?",
                           (json.dumps(snapshot, ensure_ascii=False), OWNER))


def queued(result):
    assert result["source"]["status"] == "stored", result
    assert result["index"]["status"] == "ready", result
    assert result["enrichment"]["status"] == "queued", result
    assert result["status"] == "queued" and result["enrichment_mode"] == "deferred", result
    assert result["model_calls"] == [], result
    return result["source"]["source_id"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/pmr-mcp"))
    args = parser.parse_args()
    binary = args.binary.resolve()
    clean = {key: value for key, value in os.environ.items()
             if not key.startswith(("DEEPSEEK_", "OPENAI_", "PMR_"))}
    denied = dict(clean, PMR_MODEL_NETWORK="deny")
    checks = []
    with contextlib.ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-s5-deferred-")))
        deferred = config(root, "deferred", "deferred")
        off = config(root, "off", "off")
        inline = config(root, "inline", "inline")
        db = root / "deny.sqlite3"
        with client(binary, db, denied, deferred) as c:
            first = queued(c.call("memory_remember", turn("first")))
            second = queued(c.call("memory_remember", turn("second")))
            assert c.call("memory_list", {})["total"] == 0
            assert calls(db) == 0
            assert source(db, "first")["task"]["accepted_mode"] == "deferred"
            assert source(db, "second")["task"]["accepted_mode"] == "deferred"
        with client(binary, db, denied, deferred) as c:
            replay = c.call("memory_remember", turn("first"))
            assert replay["replayed"] is True and queued(replay) == first
            before_read = state(db)
            c.call("memory_recall", {"query": "以后回答语言"})
            assert state(db) == before_read, "read-only recall changed a queued task"
            assert calls(db) == 0
        checks.append("T1 queued and indexed; restart and replay do not send or duplicate")

        drained = batch(binary, db, denied, deferred, "--drain-enrichment", "--max-jobs", "2")
        assert drained.returncode == 0, drained.stderr
        for name, expected in (("first", first), ("second", second)):
            record = source(db, name)
            assert record["id"] == expected and record["request"]["text"] == TEXT, record
            assert record["enrichment"] == "failed", record
        assert calls(db) == 0 and not state(db)["repository"]["memories"]
        checks.append("denied first task fails without a model send and does not block the next")

        with client(binary, db, denied, off) as c:
            explicit = c.call("memory_remember", turn("off-source"))
            assert explicit["enrichment"]["status"] == "not_requested", explicit
            off_id = explicit["source"]["source_id"]
            deleted = c.call("memory_remember", turn("deleted-source"))
            deleted_id = deleted["source"]["source_id"]
            c.call("memory_forget", {"source_id": deleted_id})
        processed = batch(binary, db, denied, deferred, "--enrich-source", off_id)
        assert "model_endpoint_missing" in processed.stderr + processed.stdout, processed
        assert source(db, "off-source")["enrichment"] == "failed"
        assert source(db, "off-source")["request"]["text"] == TEXT
        rejected = batch(binary, db, denied, deferred, "--enrich-source", deleted_id)
        assert rejected.returncode != 0 or "source_deleted" in rejected.stdout, rejected
        assert source(db, "deleted-source")["request"] is None
        assert calls(db) == 0
        checks.append("explicit off source remains recoverable; deleted source cannot be revived")

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        loopback = dict(clean, PMR_MODEL_NETWORK="offline",
                        DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
                        DEEPSEEK_API_KEY="local-fixture-key")
        no_budget = root / "no-budget.json"
        no_budget.write_text(json.dumps({"retrieval_mode": "local", "enrichment_mode": "deferred"}))
        command = ["--db", str(db), "--owner", OWNER, "--config", str(deferred),
                   "--drain-enrichment", "--max-jobs", "1"]
        before_state = state(db)
        before_requests = len(Fixture.requests)
        for label, arguments in [
            ("db", command[2:]),
            ("owner", command[:2] + command[4:]),
            ("config", command[:4] + command[6:]),
            ("model_budget", command[:4] + ["--config", str(no_budget)] + command[6:]),
            ("max_jobs_zero", command[:-1] + ["0"]),
            ("max_jobs_over_limit", command[:-1] + ["101"]),
        ]:
            rejected = subprocess.run([str(binary), *arguments], env=loopback,
                                      text=True, capture_output=True, timeout=10)
            assert rejected.returncode != 0, (label, rejected)
            assert state(db) == before_state and calls(db) == 0, (label, rejected)
            assert len(Fixture.requests) == before_requests, (label, Fixture.requests)
        checks.append("batch CLI rejects missing db/owner/config/budget and invalid limits before any send")

        rollback_db = root / "rollback.sqlite3"
        with client(binary, rollback_db, denied, deferred) as c:
            queued(c.call("memory_remember", turn("rollback")))
        pending = state(rollback_db)
        before_requests = len(Fixture.requests)
        with client(binary, rollback_db, denied, off) as c:
            c.call("memory_recall", {"query": "回答语言"})
        paused = batch(binary, rollback_db, loopback, off,
                       "--drain-enrichment", "--max-jobs", "1")
        assert paused.returncode != 0 and "deferred mode is disabled" in paused.stderr, paused
        assert state(rollback_db) == pending and calls(rollback_db) == 0
        assert len(Fixture.requests) == before_requests
        resumed = batch(binary, rollback_db, loopback, deferred,
                        "--drain-enrichment", "--max-jobs", "1")
        assert resumed.returncode == 0 and json.loads(resumed.stdout)["attempted"] == 1, resumed
        assert source(rollback_db, "rollback")["enrichment"] == "committed"
        assert calls(rollback_db) == 2 and len(Fixture.requests) == before_requests + 2
        checks.append("turning deferred off preserves queued work; re-enabling drains it once")

        ordered_db = root / "ordered.sqlite3"
        with client(binary, ordered_db, denied, deferred) as c:
            older_id = queued(c.call("memory_remember", turn("older")))
            queued(c.call("memory_remember", turn("newer")))
        ordered = batch(binary, ordered_db, loopback, deferred,
                        "--drain-enrichment", "--max-jobs", "2")
        assert ordered.returncode == 0, ordered.stderr
        assert source(ordered_db, "older")["enrichment"] == "committed"
        assert source(ordered_db, "newer")["enrichment"] == "committed"
        assert calls(ordered_db) == 4
        committed = state(ordered_db)["repository"]
        with client(binary, ordered_db, denied, deferred) as c:
            replay = c.call("memory_remember", turn("older"))
            assert replay["replayed"] is True and replay["source"]["source_id"] == older_id
            assert replay["enrichment"]["status"] == "committed" and replay["model_calls"] == []
        assert calls(ordered_db) == 4 and state(ordered_db)["repository"] == committed
        checks.append("serial queue refreshes the claim revision; lost post-commit reply replays without impact")

        race_db = root / "race.sqlite3"
        with client(binary, race_db, denied, deferred) as c:
            queued(c.call("memory_remember", turn("late")))
        worker = blocked_drain(binary, race_db, loopback, deferred, "late")
        try:
            with client(binary, race_db, loopback, inline) as c:
                newer = c.call("memory_remember", turn("newer-inline"))
                assert newer["enrichment"]["status"] == "committed", newer
        finally:
            Fixture.release.set()
        output = finish_drain(worker)
        assert source(race_db, "late")["enrichment"] == "stale_requires_review", (source(race_db, "late"), output)
        assert source(race_db, "newer-inline")["enrichment"] == "committed"
        assert calls(race_db) == 4
        checks.append("newer inline commit defeats an older in-flight deferred result")

        deleted_db = root / "deleted-in-flight.sqlite3"
        with client(binary, deleted_db, denied, deferred) as c:
            deleted_id = queued(c.call("memory_remember", turn("delete-in-flight")))
        worker = blocked_drain(binary, deleted_db, loopback, deferred, "delete-in-flight")
        before = len(Fixture.requests)
        duplicate = batch(binary, deleted_db, loopback, deferred,
                          "--drain-enrichment", "--max-jobs", "1")
        assert duplicate.returncode == 0 and json.loads(duplicate.stdout)["attempted"] == 0, duplicate
        assert len(Fixture.requests) == before, "a running task was sent twice"
        with client(binary, deleted_db, denied, deferred) as c:
            c.call("memory_forget", {"source_id": deleted_id})
        finish_drain(worker)
        assert source(deleted_db, "delete-in-flight")["request"] is None
        assert not state(deleted_db)["repository"]["memories"]
        assert calls(deleted_db) == 2
        checks.append("duplicate drain does not claim running work; deletion defeats its late result")

        history_db = root / "history.sqlite3"
        with client(binary, history_db, denied, off) as c:
            history_id = c.call("memory_remember", turn("history-parent"))["source"]["source_id"]
        with client(binary, history_db, denied, deferred) as c:
            queued(c.call("memory_remember", turn("history-child", [TEXT])))
        worker = blocked_drain(binary, history_db, loopback, deferred, "history-child")
        with client(binary, history_db, denied, deferred) as c:
            c.call("memory_forget", {"source_id": history_id})
        assert source(history_db, "history-child")["request"]["history"] == []
        finish_drain(worker)
        assert source(history_db, "history-child")["enrichment"] == "stale_requires_review"
        assert not state(history_db)["repository"]["memories"]
        checks.append("deleting another source changes request history and rejects the old T2")

        lease_db = root / "lease.sqlite3"
        with client(binary, lease_db, denied, deferred) as c:
            lease_id = queued(c.call("memory_remember", turn("expired")))
        worker = blocked_drain(binary, lease_db, loopback, deferred, "expired")
        observed_requests = len(Fixture.requests)
        worker.kill()
        worker.communicate(timeout=10)
        Fixture.release.set()
        Fixture.blocked_turn = None
        assert source(lease_db, "expired")["enrichment"] == "running"
        assert calls(lease_db) == 1
        automatic = batch(binary, lease_db, loopback, deferred,
                          "--drain-enrichment", "--max-jobs", "1")
        assert automatic.returncode == 0 and json.loads(automatic.stdout)["attempted"] == 0
        premature = batch(binary, lease_db, loopback, deferred, "--enrich-source", lease_id)
        assert premature.returncode != 0 and len(Fixture.requests) == observed_requests
        expire_lease(lease_db, "expired")
        recovered = batch(binary, lease_db, loopback, deferred, "--enrich-source", lease_id)
        assert recovered.returncode == 0, recovered.stderr
        assert source(lease_db, "expired")["enrichment"] == "committed"
        assert source(lease_db, "expired")["task"]["attempt_count"] == 2
        assert calls(lease_db) == 3 and len(Fixture.requests) == observed_requests + 2
        checks.append("interrupted send is not auto-reissued; expired lease needs explicit new attempt")

        legacy_db = root / "legacy-upgrade.sqlite3"
        with client(binary, legacy_db, denied, inline) as c:
            old = c.call("memory_remember", turn("legacy-failed"), error=True)
            old_id = old["source"]["source_id"]
        with client(binary, legacy_db, loopback, inline) as c:
            assert c.call("memory_remember", turn("legacy-new"))["enrichment"]["status"] == "committed"
        downgrade_v4(legacy_db)
        sent = len(Fixture.requests)
        prior_calls = calls(legacy_db)
        blocked = batch(binary, legacy_db, loopback, deferred, "--enrich-source", old_id)
        assert blocked.returncode != 0 and "stale_requires_review" in blocked.stderr, blocked
        assert source(legacy_db, "legacy-failed")["enrichment"] == "stale_requires_review"
        assert state(legacy_db)["last_applied_source_sequence"] == 2
        assert calls(legacy_db) == prior_calls and len(Fixture.requests) == sent
        checks.append("v4 failed source cannot overtake a newer committed source after v5 migration")

        legacy_off_db = root / "legacy-off.sqlite3"
        with client(binary, legacy_off_db, denied, off) as c:
            off_id = c.call("memory_remember", turn("legacy-off"))["source"]["source_id"]
        downgrade_v4(legacy_off_db)
        sent = len(Fixture.requests)
        accepted = batch(binary, legacy_off_db, loopback, deferred, "--enrich-source", off_id)
        assert accepted.returncode == 0 and source(legacy_off_db, "legacy-off")["enrichment"] == "committed", accepted
        assert len(Fixture.requests) == sent + 2
        checks.append("v4 off source with unchanged business revision remains explicitly processable")

        legacy_resume_db = root / "legacy-resume.sqlite3"
        with client(binary, legacy_resume_db, denied, off) as c:
            resumed_id = c.call("memory_remember", turn("legacy-resumed"))["source"]["source_id"]
        def old_resume(snapshot):
            record = next(record for record in snapshot["sources"].values()
                          if record["id"] == resumed_id)
            record["enrichment"] = "failed"
            record["result"] = {"resume_attempts": [{"attempt": "historical"}]}
            record["base_revision"] = snapshot["revision"]
        downgrade_v4(legacy_resume_db, old_resume)
        sent = len(Fixture.requests)
        blocked = batch(binary, legacy_resume_db, loopback, deferred,
                        "--enrich-source", resumed_id)
        assert blocked.returncode != 0 and "stale_requires_review" in blocked.stderr, blocked
        assert source(legacy_resume_db, "legacy-resumed")["error"] == "legacy_resume_revision_untrusted"
        assert calls(legacy_resume_db) == 0 and len(Fixture.requests) == sent
        checks.append("v4 historical resume cannot launder its refreshed base revision into a new paid attempt")

    print(json.dumps({"status": "passed", "checks": checks,
                      "provider_calls": 0, "loopback_requests": len(Fixture.requests)}, ensure_ascii=False))


if __name__ == "__main__":
    main()
