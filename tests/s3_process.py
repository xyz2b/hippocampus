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

from inspector_support import export_runtime, schema_check
from s2_capture import client


def mcp_inspector_acceptance(root, binary, mcp_binary, env, output):
    """Real source capture and internal query; every byte of source data is synthetic."""
    db = root / "mcp-synthetic.sqlite3"
    trace_dir = root / "mcp-trace"
    trace_dir.mkdir(mode=0o700)
    owner = "s3-inspector-synthetic-owner"
    text = "合成验收：松石47使用 SQLite，Redis 暂不引入。标记 <synthetic-only>。"
    trace_env = env | {"PMR_TRACE_DIR": str(trace_dir), "DEEPSEEK_API_KEY": "synthetic-unused-key",
                       "DEEPSEEK_BASE_URL": "http://127.0.0.1:1/v1"}
    with client(mcp_binary, db, trace_env, owner) as current:
        receipt = current.call("memory_remember", {
            "text": text, "session_id": "s3-inspector-synthetic", "turn_id": "model-denied",
            "task_id": "synthetic-project", "capture_source": True,
        }, error=True)
    assert receipt["source"]["status"] == "stored", receipt
    assert receipt["index"]["status"] == "ready", receipt
    assert receipt["enrichment"]["status"] == "failed", receipt
    assert receipt["enrichment"]["error"] == "model_network_disabled", receipt
    capture_files = sorted(trace_dir.glob("*.jsonl"))
    with sqlite3.connect(db) as connection:
        before = list(connection.iterdump())
    process = subprocess.run(
        [str(binary), "--db", str(db), "--owner", owner,
         "--query", "SQLite", "--task", "synthetic-project"],
        env=trace_env, text=True, capture_output=True, timeout=20, check=True,
    )
    result = json.loads(process.stdout)
    assert result["status"] == "completed" and result["coverage_complete"] is True, result
    assert result["backend_generation_calls"] == 0 and result["candidate_count"] == 1, result
    candidate = result["candidates"][0]
    assert candidate["source_id"] == receipt["source"]["source_id"] and candidate["text"] == text
    assert candidate["enrichment"] == "failed", candidate
    with sqlite3.connect(db) as connection:
        assert list(connection.iterdump()) == before, "internal query changed the database"
    query_files = sorted(set(trace_dir.glob("*.jsonl")) - set(capture_files))
    assert len(query_files) == 1, query_files
    raw = "".join(path.read_text() for path in [*capture_files, *query_files])
    events = [json.loads(line) for line in raw.splitlines()]
    assert not any(event["kind"] == "model_start" for event in events)
    assert len({event["run_id"] for event in events}) == 2, "save/query must have independent operations"
    raw_path = root / "lexical-runtime-events.jsonl"
    raw_path.write_text(raw)
    bundle = export_runtime(raw_path)
    check = schema_check(bundle)
    assert check.returncode == 0 and check.stdout.strip() == "[]", check.stderr + check.stdout
    query = next(run for run in bundle["runs"] if run["tool"] == "lexical_search")
    assert query["lexical_raw"] == result and query["mcp_response"]["recorded"] is False
    assert query["retrieval"]["candidates"] is None and bundle["llm_calls"] == []
    assert bundle["meta"]["channels"]["lexical_index"] == "enabled"
    render_script = r"""
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const nodes=new Map();let network=0;
const document={getElementById(id){if(!nodes.has(id))nodes.set(id,{style:{},addEventListener(){}});return nodes.get(id);},querySelectorAll(){return [];},addEventListener(){}};
const context=vm.createContext({document,window:{scrollTo(){},addEventListener(){}},location:{protocol:'file:'},clearTimeout(){},setTimeout(){},fetch(){network++;throw Error('network forbidden');},fixture:JSON.parse(fs.readFileSync(0,'utf8'))});
const html=fs.readFileSync('inspector/web/index.html','utf8');
vm.runInContext(html.split('<script>')[1].split('</script>')[0],context);
const rendered=vm.runInContext(`load(fixture);runDetail(bundle.runs.find(run=>run.tool==='lexical_search'))`,context);
for(const text of ['lexical_raw','原文关键词检索','覆盖状态','松石47使用 SQLite','已知事实状态与纠正关系','内部只读检索记录','&lt;synthetic-only&gt;'])assert.ok(rendered.includes(text),text);
assert.ok(!rendered.includes('<synthetic-only>'));assert.equal(network,0);
console.log(JSON.stringify({status:'passed',network_requests:network,scope:'actual exported bundle in shipped Node rendering harness; not browser automation'}));
"""
    render = subprocess.run(["node", "-e", render_script], input=json.dumps(bundle),
                            text=True, capture_output=True, timeout=20, check=True)
    report = {
        "data_kind": "synthetic acceptance with actual binaries and SQLite",
        "model_network": "deny", "recorded_model_calls": len(bundle["llm_calls"]),
        "source": receipt["source"], "index": receipt["index"], "enrichment": receipt["enrichment"],
        "query_status": result["status"], "candidate_count": result["candidate_count"],
        "duration_ms": result.get("duration_ms"), "schema_warnings": json.loads(check.stdout),
        "render": json.loads(render.stdout), "query_readonly": True,
        "boundary": "lexical_search is an internal source search; formal MCP recall/answer path is not connected",
    }
    if output:
        output.mkdir(parents=True, exist_ok=True)
        (output / "lexical-runtime-events.jsonl").write_text(raw)
        (output / "lexical-trace.json").write_text(json.dumps(bundle, ensure_ascii=False, indent=2) + "\n")
        (output / "inspector-check.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    return report


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
    parser.add_argument("--mcp-binary", type=Path, default=Path("target/debug/pmr-mcp"))
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
            assert connection.execute("PRAGMA user_version").fetchone()[0] == 7
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
        inspector = mcp_inspector_acceptance(root, binary, options.mcp_binary.resolve(), env, options.output)
        checks.append("real MCP source capture survives network-denied enrichment; independent readonly search/export/schema/render passes")
    print(json.dumps({"checks": checks, "passed": len(checks), "inspector": inspector}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
