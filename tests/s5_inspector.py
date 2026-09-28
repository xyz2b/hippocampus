#!/usr/bin/env python3
"""S5 deferred trace/Inspector contract with a local Responses fixture."""
import argparse
import contextlib
import http.server
import json
import os
import subprocess
import tempfile
import threading
from pathlib import Path

from inspector_support import schema_check
from s5_deferred_process import OWNER, Fixture, batch, client, config, queued, turn


def inspect(binary, *args):
    output = subprocess.run([str(binary), *map(str, args)], text=True,
                            capture_output=True, check=True, timeout=30)
    result = json.loads(output.stdout)
    check = schema_check(result)
    assert check.returncode == 0 and check.stdout.strip() == "[]", (check.stdout, check.stderr)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/pmr-mcp"))
    parser.add_argument("--inspector", type=Path, default=Path("target/debug/pmr-inspector"))
    args = parser.parse_args()
    binary, inspector = args.binary.resolve(), args.inspector.resolve()
    clean = {key: value for key, value in os.environ.items()
             if not key.startswith(("DEEPSEEK_", "OPENAI_", "PMR_"))}
    with contextlib.ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-s5-inspector-")))
        trace = root / "traces"
        trace.mkdir(mode=0o700)
        db = root / "memory.sqlite3"
        settings = config(root, "deferred", "deferred")
        denied = dict(clean, PMR_MODEL_NETWORK="deny", PMR_TRACE_DIR=str(trace))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        Fixture.requests = []
        loopback = dict(clean, PMR_MODEL_NETWORK="offline", PMR_TRACE_DIR=str(trace),
                        DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
                        DEEPSEEK_API_KEY="local-fixture-key")
        with client(binary, db, denied, settings) as connection:
            source_id = queued(connection.call("memory_remember", turn("inspector")))
        completed = batch(binary, db, loopback, settings, "--drain-enrichment", "--max-jobs", "1")
        assert completed.returncode == 0, completed.stderr
        assert len(Fixture.requests) == 1
        with client(binary, db, denied, settings) as connection:
            failed_source_id = queued(connection.call("memory_remember", turn("inspector-failed")))
        failed = batch(binary, db, denied, settings, "--drain-enrichment", "--max-jobs", "1")
        assert failed.returncode == 0 and len(Fixture.requests) == 1, failed.stderr
        paths = list(trace.glob("trace-*.jsonl"))
        assert len(paths) == 4, paths
        exports = [inspect(inspector, "--export-trace", path) for path in paths]
        runs = [run for bundle in exports for run in bundle["runs"]]
        capture = next(run for run in runs if run["tool"] == "memory_remember" and run["source"]["source_id"] == source_id)
        process = next(run for run in runs if run["tool"] == "source_enrichment" and run["source"]["source_id"] == source_id)
        failed_process = next(run for run in runs if run["tool"] == "source_enrichment" and run["source"]["source_id"] == failed_source_id)
        assert capture["save_status"] == "queued" and capture["enrichment_task"]["attempt_count"] == 0
        assert process["save_status"] == "committed" and process["enrichment_task"]["attempt_count"] == 1
        assert capture["source"]["source_id"] == process["source"]["source_id"] == source_id
        assert source_id in capture["source_ids"] and source_id in process["source_ids"]
        assert process["entrypoint"] == "internal_cli" and process["mcp_response"]["recorded"] is False
        assert process["spans"][-1]["name"] == "internal_enrichment_run_end"
        assert "内部索引操作" not in process["notes"]
        assert failed_process["status"] == "failed" and failed_process["save_status"] == "failed"
        assert failed_process["enrichment"]["error"] == "model_endpoint_missing"
        calls = [call for bundle in exports for call in bundle["llm_calls"]]
        assert len(calls) == 1 and calls[0]["run_id"] == process["id"]
        log = root / "empty.log"
        log.write_text("")
        live = inspect(inspector, "--dump", "--db", db, "--owner", OWNER,
                       "--trace-dir", trace, "--log", log)
        linked = [run for run in live["runs"] if run.get("source", {}).get("source_id") == source_id]
        assert live["live"]["trace_status"] == "available"
        assert {run["tool"] for run in linked} >= {"memory_remember", "source_enrichment"}
        assert len([call for call in live["llm_calls"] if call["run_id"] == process["id"]]) == 1
    print(json.dumps({"status": "passed", "checks": ["queued capture", "CLI completion", "CLI failure",
                      "same-source Inspector links", "single loopback model call", "TraceBundle schema"]}))


if __name__ == "__main__":
    main()
