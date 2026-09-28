#!/usr/bin/env python3
"""Replay the archived real N05 comparison proposal through a loopback fixture."""
import contextlib
import hashlib
import http.server
import json
import os
import sqlite3
import threading
from pathlib import Path

from mcp_smoke import Client, Responses
from s5_comparison_process import PREDICATES, QUESTION
from s5_normal_intake import settings


ROOT = Path(__file__).resolve().parents[1]
ARCHIVE = ROOT / "docs/refactor/s5/assisted/comparison-good-trace-live-20260926-v1"
OUTPUT = ROOT / "docs/refactor/s5/assisted/comparison-good-trace-postfix-20260926"


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    assert not OUTPUT.exists(), "replay output must be new"
    trace_path, = (ARCHIVE / "good-trace").glob("*.jsonl")
    archive_files = [ARCHIVE / "good.sqlite3", ARCHIVE / "good-result.json",
                     ARCHIVE / "report.json", trace_path]
    before = {str(path.relative_to(ARCHIVE)): sha256(path) for path in archive_files}
    events = [json.loads(line) for line in trace_path.read_text().splitlines()]
    starts = [event for event in events if event["kind"] == "model_start"
              and event["data"].get("stage") == "personal_memory_comparison_v1"]
    ends = [event for event in events if event["kind"] == "model_end"]
    validations = [event for event in events if event["kind"] == "validation"
                   and event["data"].get("stage") == "comparison"]
    assert len(starts) == len(ends) == len(validations) == 1
    proposal = ends[0]["data"]["parsed_result"]
    original_input = json.loads(starts[0]["data"]["request"]["input"])
    assert proposal == validations[0]["data"]["parsed_result"]
    assert proposal["ambiguous"] is False and len(proposal["roles"]) == 2
    assert original_input["question"] == QUESTION
    for role in proposal["roles"]:
        assert role["query_relation"] in QUESTION
        assert role["evidence_relation"] in original_input["candidates"][role["candidate_index"]]["quote"]

    with sqlite3.connect(ARCHIVE / "good.sqlite3") as source:
        state = json.loads(source.execute(
            "SELECT state_json FROM memory_profiles WHERE owner_id='s5-comparison'"
        ).fetchone()[0])
    expected = {item["id"]: item["request"]["text"] for item in state["sources"].values()}
    assert set(expected.values()) == {"项目松石47选用 SQLite。", "松石47只能在一台机器部署。"}

    os.umask(0o077)
    OUTPUT.mkdir(mode=0o700)
    trace_dir = OUTPUT / "trace"
    trace_dir.mkdir(mode=0o700)
    copy = OUTPUT / "good.sqlite3"
    with sqlite3.connect(ARCHIVE / "good.sqlite3") as source, sqlite3.connect(copy) as destination:
        source.backup(destination)
    with sqlite3.connect(copy) as database:
        assert database.execute("PRAGMA integrity_check").fetchone()[0] == "ok"

    class Recorded(Responses):
        calls = 0
        matches = []

        def output(self, request):
            prompt = json.loads(request["input"])
            matched = (request["text"]["format"]["name"] == "personal_memory_comparison_v1"
                       and prompt == original_input)
            type(self).matches.append(matched)
            assert matched, "replay candidate mapping differs from the real run"
            return proposal

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Recorded)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    clean = {key: value for key, value in os.environ.items()
             if not key.startswith(("DEEPSEEK_", "OPENAI_", "PMR_"))}
    env = dict(clean, PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
               DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
               NO_PROXY="127.0.0.1,localhost", PMR_TRACE_DIR=str(trace_dir))
    config = settings(
        OUTPUT, "assisted", retrieval_mode="assisted", enrichment_mode="off",
        fact_index_enabled=True, predicates=PREDICATES,
        local_recall_budget={"candidate_k": 30, "source_limit": 2, "content_bytes": 8192},
        model_budget={"group_id": "s5-n05-recorded-proposal-postfix-20260926", "max_calls": 1,
                      "max_operation_calls": 1, "query_output_tokens": 4096,
                      "max_input_bytes": 65536})
    try:
        with contextlib.closing(Client(ROOT / "target/debug/pmr-mcp", copy, env,
                                       "s5-comparison", config=config)) as client:
            result = client.call("memory_recall", {"query": QUESTION})
    finally:
        server.shutdown()
        thread.join()
        server.server_close()

    trace_files = list(trace_dir.glob("trace-*.jsonl"))
    replay_events = [json.loads(line) for path in trace_files for line in path.read_text().splitlines()]
    replay_validations = [event for event in replay_events if event["kind"] == "validation"
                          and event["data"].get("stage") == "comparison"]
    replay_ends = [event for event in replay_events if event["kind"] == "model_end"]
    selected = [decision for decision in result["source_decisions"]
                if decision["decision"] == "selected" and decision["lane"] == "source_evidence"]
    actual = {item["source_id"]: item["text"] for item in result["source_evidence"]}
    after = {str(path.relative_to(ARCHIVE)): sha256(path) for path in archive_files}
    report = {
        "status": "passed" if (before == after and Recorded.calls == 1 and Recorded.matches == [True]
                               and actual == expected and len(result["source_evidence"]) == 2
                               and len(selected) == 2 and len(trace_files) == 1
                               and len(replay_validations) == len(replay_ends) == 1
                               and replay_validations[0]["data"]["status"] == "completed"
                               and replay_validations[0]["data"]["parsed_result"] == proposal
                               and replay_ends[0]["data"]["parsed_result"] == proposal
                               and result["assistance"]["status"] == "completed"
                               and len(result["model_calls"]) == 1) else "failed",
        "scope": "offline replay of archived real comparison proposal against SQLite backup",
        "ark_requests": 0, "fixture_requests": Recorded.calls,
        "fixture_request_matches_original_input": Recorded.matches,
        "fixture_output_token_limit": 4096,
        "original_output_token_limit": starts[0]["data"]["request"]["max_output_tokens"],
        "original_proposal_sha256": hashlib.sha256(json.dumps(
            proposal, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()).hexdigest(),
        "original_proposal": proposal,
        "original_archive_sha256_unchanged": before == after,
        "archive_sha256": before,
        "binary_sha256": sha256(ROOT / "target/debug/pmr-mcp"),
        "source_ids": sorted(actual), "source_quotes": sorted(actual.values()),
        "selected": selected, "assistance": result["assistance"],
        "evidence_status": result.get("evidence_status"),
        "trace": {"files": [str(path.relative_to(OUTPUT)) for path in trace_files],
                  "private_files": all(path.stat().st_mode & 0o777 == 0o600 for path in trace_files),
                  "comparison_validation": replay_validations[0]["data"]["status"]
                  if len(replay_validations) == 1 else "missing",
                  "model_end_exact_proposal": len(replay_ends) == 1
                  and replay_ends[0]["data"]["parsed_result"] == proposal},
        "model_calls": result["model_calls"],
    }
    (OUTPUT / "result.json").write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
    (OUTPUT / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    assert report["status"] == "passed", report
    print(json.dumps({"status": report["status"], "ark_requests": 0,
                      "fixture_requests": Recorded.calls, "source_quotes": report["source_quotes"],
                      "selected": len(selected), "report": str(OUTPUT / "report.json")}, ensure_ascii=False))


if __name__ == "__main__":
    main()
