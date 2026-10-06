#!/usr/bin/env python3
"""S3 internal CLI acceptance against a synthetic, temporary v2 database."""
import argparse
import hashlib
import json
import os
import sqlite3
import subprocess
import tempfile
from pathlib import Path



def source(identifier, text, task="project-a"):
    return {
        "id": identifier, "session_id": "synthetic-s3", "turn_id": identifier,
        "observed_at": 1_750_000_000, "received_sequence": 1,
        "request": {
            "text": text, "history": [], "session_id": "synthetic-s3",
            "turn_id": identifier, "task_id": task, "capture_source": True,
        } if text is not None else None,
        "enrichment": "failed", "error": "synthetic_model_failure",
        "result": None, "base_revision": 0,
    }


def seed(db):
    with sqlite3.connect(db) as connection:
        connection.execute("CREATE TABLE memory_profiles (owner_id TEXT PRIMARY KEY, state_json TEXT NOT NULL)")
        connection.execute("PRAGMA user_version=2")
        for owner, sources in (
            ("owner-a", {
                "a": source("source-a", "松石47使用 SQLite，Redis 暂不引入。"),
                "deleted": source("deleted-source", None),
            }),
            ("owner-b", {"b": source("source-b", "松石47 SQLite 另一用户机密。")}),
        ):
            state = {
                "repository": {"next_id": 0, "memories": {}},
                "entities": {"entities": {}}, "staging": {"next_id": 0, "pending": {}},
                "receipts": {}, "revision": 0, "sources": sources,
            }
            connection.execute("INSERT INTO memory_profiles VALUES (?, ?)", (owner, json.dumps(state)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", nargs="?", type=Path)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/examples/source_search"))
    options = parser.parse_args()
    binary = options.binary.resolve()
    env = dict(os.environ, PMR_MODEL_NETWORK="deny")
    for key in ("DEEPSEEK_API_KEY", "DEEPSEEK_BASE_URL", "OPENAI_API_KEY", "OPENAI_BASE_URL",
                "PMR_TRACE_DIR", "PMR_OPENAI_TRACE", "PMR_OPENAI_TRACE_CONTENT"):
        env.pop(key, None)
    checks = []
    with tempfile.TemporaryDirectory(prefix="pmr-s3-process-") as directory:
        root = Path(directory)
        db = root / "synthetic.sqlite3"
        seed(db)

        def run(*args, owner="owner-a", success=True, trace=None):
            result = subprocess.run(
                [str(binary), "--db", str(db), "--owner", owner, *args],
                env=env | ({"PMR_TRACE_DIR": str(trace)} if trace else {}),
                text=True, capture_output=True, timeout=20,
            )
            assert (result.returncode == 0) == success, result.stdout + result.stderr
            return json.loads(result.stdout) if success else result

        before = hashlib.sha256(db.read_bytes()).hexdigest()
        run("--query", "SQLite", success=False)
        run("--query", "SQLite", "--rebuild", success=False)
        run("--rebuild", "--disabled", success=False)
        run("--query", "SQLite", "--limit", "0", success=False)
        missing_owner = subprocess.run(
            [str(binary), "--db", str(db), "--query", "SQLite"],
            env=env, text=True, capture_output=True, timeout=20,
        )
        assert missing_owner.returncode != 0
        assert hashlib.sha256(db.read_bytes()).hexdigest() == before
        checks.append("readonly refuses v2; invalid or implicit owner/mode never changes the database")

        run("--rebuild")
        with sqlite3.connect(db) as connection:
            assert connection.execute("PRAGMA user_version").fetchone()[0] == 9
        trace = root / "trace"
        trace.mkdir(mode=0o700)
        found = run("--query", "SQLite", trace=trace)
        assert found["status"] == "completed" and found["coverage_complete"] is True, found
        assert found["candidate_count"] == 1 and found["candidates"][0]["source_id"] == "source-a", found
        assert found["candidates"][0]["enrichment"] == "failed", found
        assert found["backend_generation_calls"] == 0, found
        serialized = json.dumps(found, ensure_ascii=False)
        assert "source-a" in serialized and "松石47使用 SQLite" in serialized, found
        assert "source-b" not in serialized and "另一用户机密" not in serialized, found
        assert "deleted-source" not in serialized, found
        excluded = run("--query", "SQLite", "--task", "other-project")
        assert excluded["status"] == "completed" and excluded["candidates"] == [], excluded
        disabled = run("--query", "SQLite", "--disabled")
        assert disabled["status"] == "disabled" and disabled["candidates"] == [], disabled
        checks.append("explicit migration/rebuild; fresh-process query finds failed-enrichment source with owner/task/deletion isolation")

        with sqlite3.connect(db) as connection:
            before_read = list(connection.iterdump())
        run("--query", "SQLite")
        with sqlite3.connect(db) as connection:
            assert list(connection.iterdump()) == before_read
        checks.append("readonly query neither repairs nor modifies source/index state")

        events = [json.loads(line) for path in trace.glob("*.jsonl") for line in path.read_text().splitlines()]
        assert {event["kind"] for event in events} >= {"run_start", "lexical_raw", "run_end"}, events
        assert len({event["run_id"] for event in events}) == 1
        assert not any(event["kind"] in ("rpc_response", "model_start") for event in events)
        end = next(event["data"] for event in events if event["kind"] == "run_end")
        assert end["entrypoint"] == "internal_cli" and end["status"] == "completed"
        assert "response" not in end and "另一用户机密" not in json.dumps(events, ensure_ascii=False)
        checks.append("correlated internal lexical trace contains no fake MCP response, model call, or other-owner text")

        run("--rebuild", owner="owner-b")
        other = run("--query", "SQLite", owner="owner-b")
        assert [item["source_id"] for item in other["candidates"]] == ["source-b"], other
        assert [item["source_id"] for item in run("--query", "SQLite")["candidates"]] == ["source-a"]
        checks.append("rebuilding another owner preserves first-owner results across process restarts")
    print(json.dumps({"checks": checks, "passed": len(checks)}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
