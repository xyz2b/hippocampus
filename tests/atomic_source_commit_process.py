#!/usr/bin/env python3
"""Four actual MCP intake paths; synthetic source-supported candidates, loopback only."""
import argparse
import contextlib
import hashlib
import http.server
import json
import os
import sqlite3
import subprocess
import tempfile
import threading
from pathlib import Path

import intake_grounding_process as base


CASES = ("rejected", "pending", "conflict", "ignored", "accepted", "single_pending")
FIRST = "霜叶31的材质是橡木。"
SECOND = "青石82的材质是钢。"
UNRESOLVED = "那个设备的材质是钢。"


class Fixture(base.Fixture):
    candidates = []
    text = ""

    def do_POST(self):
        try:
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert self.path == "/v1/responses" and self.headers["Authorization"] == "Bearer local-fixture-key"
            payload = json.loads(request["input"])
            assert payload["current_user_turn"] == self.text
            name = request["text"]["format"]["name"]
            type(self).requests.append(name)
            if name == base.EXTRACTION:
                output = dict(omission_reason=None, candidates=self.candidates)
            else:
                assert name == base.REVIEW and not base.active(self.db)
                output = base.source_support_review(request)
                if "issues" not in request["text"]["format"]["schema"]["properties"]["reviews"]["items"]["properties"]:
                    # The frozen pre-fix binary predates field-specific review diagnostics.
                    for review in output["reviews"]:
                        review.pop("issues", None)
            self.respond(200, {"status": "completed", "usage": {"input_tokens": 10, "output_tokens": 10},
                "output": [{"type": "message", "content": [{"type": "output_text", "text": json.dumps(output, ensure_ascii=False)}]}]})
        except Exception as error:
            type(self).errors.append(str(error))
            self.respond(500, {"error": {"message": "fixture assertion failed"}})


def run_case(binary, directory, env, mode, name):
    work = directory / f"{mode}-{name}"
    work.mkdir()
    trace = work / "trace"
    trace.mkdir(mode=0o700)
    db = work / "state.sqlite3"
    capture = mode != "legacy"
    first = base.candidate(base.SUBJECT, FIRST)
    second = base.candidate(base.OTHER, SECOND)
    for candidate in (first, second):
        candidate["expiry_expression"] = None
    text = FIRST + SECOND
    if name == "rejected":
        second["extraction_confidence"] = "low"
    elif name in ("pending", "single_pending"):
        second.update(subject_id=None, unresolved_subject="那个设备")
        second["value"]["text"] = UNRESOLVED
        text = ("" if name == "single_pending" else FIRST) + UNRESOLVED
    elif name in ("conflict", "ignored"):
        second["intent"]["kind"] = "conflict" if name == "conflict" else "ignore"
    Fixture.candidates = [second] if name == "single_pending" else [first, second]
    Fixture.db, Fixture.text = db, text
    before, errors_before = len(Fixture.requests), len(Fixture.errors)
    config = base.settings(work, "config", retrieval_mode="local", fact_index_enabled=True,
        enrichment_mode={"explicit": "off", "legacy": "inline"}.get(mode, mode), source_capture_enabled=capture,
        predicates=[{"predicate": "fact", "cardinality": "Multiple"}], memory_types=[], value_kinds=[],
        model_budget=dict(group_id=f"{mode}-{name}", max_calls=2, max_operation_calls=2))
    failures = []
    def check(condition, message):
        if not condition:
            failures.append(message)
    env = dict(env, PMR_TRACE_DIR=str(trace))
    with contextlib.closing(base.Client(binary, db, env, base.OWNER, config=config)) as client:
        client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
        for entity in (base.SUBJECT, base.OTHER):
            client.call("memory_entity_upsert", dict(id=entity, canonical_name=entity, aliases=[], entity_type="Other"))
        turn = dict(text=text, history=[], session_id="atomic-source", turn_id=name, capture_source=capture)
        if mode == "inline":
            prepared = base.tool(client, "memory_prepare", {"user_turn": turn})
            receipt = prepared["remember"].get("result", prepared["remember"])
            check(prepared["recall"]["status"] == "completed", "local recall failed")
        else:
            receipt = base.tool(client, "memory_remember", turn)
        if mode in ("deferred", "explicit"):
            check(not base.active(db) and len(Fixture.requests) == before, "capture made model calls")
            batch = ["--drain-enrichment", "--max-jobs", "1"] if mode == "deferred" else ["--enrich-source", receipt["source"]["source_id"]]
            completed = subprocess.run([str(binary), "--db", str(db), "--owner", base.OWNER, "--config", str(config), *batch],
                env=env, text=True, capture_output=True, timeout=30)
            check(completed.returncode == 0, "batch failed: " + completed.stderr[:200])
            receipt = base.tool(client, "memory_remember", turn)
            check(receipt["replayed"], "missing batch replay")
        state = base.snapshot(db)
        expected_facts, expected_pending = (2 if name == "accepted" else 0), (1 if name == "single_pending" else 0)
        check(len(state["repository"]["memories"]) == expected_facts, "partial facts committed")
        check(len(state["staging"]["pending"]) == expected_pending, "partial pending candidates committed")
        outcomes = receipt.get("outcomes", receipt.get("original_result", {}).get("outcomes", []))
        rejected = name not in ("accepted", "single_pending")
        if rejected:
            check(len(outcomes) == 2 and all(isinstance(o.get("Ingested", {}).get("Rejected"), str) for o in outcomes),
                  "rolled-back receipt claims successful/staged outcome")
        if capture:
            source, = state["sources"].values()
            check(source["request"]["text"] == text, "source changed")
            check(receipt["source"]["status"] == "stored" and receipt["index"]["status"] == "ready", "source/index missing")
            expected = "completed_rejected" if rejected else ("committed" if name == "accepted" else "completed_pending")
            check(receipt["enrichment"]["status"] == expected, "wrong enrichment status")
        else:
            check(not state["sources"], "legacy saved unauthorized source")
        sent = len(Fixture.requests)
        replay = base.tool(client, "memory_remember", turn)
        check(replay.get("replayed") and len(Fixture.requests) == sent, "receipt replay sent another model request")
        with sqlite3.connect(db) as connection:
            ledger = connection.execute("SELECT operation_id,stage FROM model_calls ORDER BY rowid").fetchall()
            check(len(ledger) == 2 and len({r[0] for r in ledger}) == 1, "wrong physical requests/operation")
            if capture:
                indexed = connection.execute("SELECT text FROM source_segments WHERE owner_id=?", (base.OWNER,)).fetchall()
                check((text,) in indexed, "source missing from index")
        events = [json.loads(line) for path in trace.glob("*.jsonl") for line in path.read_text().splitlines()]
        decisions = [event["data"] for event in events if event["kind"] == "candidate_decision"]
        if rejected:
            check(len(decisions) == 2 and all(d["commit_status"] == "rolled_back" for d in decisions), "trace does not mark rollback")
        check(Fixture.requests[before:] == [base.EXTRACTION, base.REVIEW], "unexpected model sequence")
        check(not Fixture.errors[errors_before:], "fixture errors: " + str(Fixture.errors[errors_before:]))
    return dict(mode=mode, case=name, passed=not failures, failures=failures,
                active=len(state["repository"]["memories"]), pending=len(state["staging"]["pending"]),
                calls=len(Fixture.requests)-before)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=base.ROOT / "target/debug/pmr-mcp")
    parser.add_argument("--modes", nargs="+", choices=["inline", "deferred", "explicit", "legacy"], default=["inline", "deferred", "explicit", "legacy"])
    parser.add_argument("--cases", nargs="+", choices=CASES, default=list(CASES))
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    binary_sha256 = hashlib.sha256(binary.read_bytes()).hexdigest()
    Fixture.requests, Fixture.errors = [], []
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-atomic-source-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        clean = {k: v for k, v in os.environ.items() if not k.startswith(("DEEPSEEK_", "OPENAI_", "ARK_", "PMR_"))}
        env = dict(clean, PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
            DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1", NO_PROXY="127.0.0.1,localhost")
        rows = [run_case(binary, directory, env, mode, name) for mode in args.modes for name in args.cases]
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == binary_sha256, "binary changed during process matrix"
    report = dict(passed=all(r["passed"] for r in rows), cases=len(rows), results=rows,
        loopback_requests=len(Fixture.requests), real_provider_requests=0, binary_sha256=binary_sha256)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2)+"\n")
    for row in rows:
        print(f"{'PASS' if row['passed'] else 'FAIL'} {row['mode']}/{row['case']}: {row['failures']}")
    print(json.dumps({k:v for k,v in report.items() if k != "results"}))
    assert report["passed"], "source batch atomicity regression failed"


if __name__ == "__main__":
    main()
