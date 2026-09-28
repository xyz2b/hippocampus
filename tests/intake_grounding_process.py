#!/usr/bin/env python3
"""Source grounding through real stdio MCP and loopback HTTP; never calls a provider."""
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

from mcp_smoke import Client
from s5_normal_intake import candidate, settings
from source_support_fixture import source_support_review


ROOT = Path(__file__).resolve().parents[1]
OWNER = "intake-grounding-synthetic"
SUBJECT = "霜叶31"
OTHER = "青石82"
REVIEW = "personal_memory_source_support_v1"
EXTRACTION = "personal_memory_candidates_v1"
CHECKS = "subject relation value polarity scope temporal intent source metadata".split()
PAPER = "霜叶31默认用 A4 纸打印，纸张放在右侧纸盒。"
# text, predicate, value kind, value, review verdict; all statements are synthetic.
CASES = {
    "a4_false": (PAPER, "fact", "boolean", False, "format_reject"),
    "fact_fragment": (PAPER, "fact", "text", "A4", "format_reject"),
    "fact_sentence": (PAPER, "fact", "text", PAPER, "supported"),
    "text": ("霜叶31的材质是橡木。", "material", "text", "橡木", "supported"),
    "integer": ("霜叶31的维护预算是 500 元。", "maintenance_budget", "integer", 500, "supported"),
    "entity": ("霜叶31的所有者是青石82。", "owned_by", "entity", OTHER, "supported"),
    "boolean": ("霜叶31的自动双面打印功能未启用。", "duplex_enabled", "boolean", False, "supported"),
    "wrong_value": ("霜叶31的维护预算是 500 元，青石82的维护预算是 700 元。",
                    "maintenance_budget", "integer", 700, "contradicted"),
    "review_format": ("霜叶31的材质是橡木。", "material", "text", "橡木", "bad_format"),
    "review_network": ("霜叶31的材质是橡木。", "material", "text", "橡木", "http_error"),
}


def snapshot(db):
    with sqlite3.connect(db) as connection:
        row = connection.execute("SELECT state_json FROM memory_profiles WHERE owner_id=?", (OWNER,)).fetchone()
    return json.loads(row[0]) if row else {"sources": {}, "repository": {"memories": {}}}


def active(db):
    memories = snapshot(db)["repository"]["memories"]
    return [memory for memory in (memories.values() if isinstance(memories, dict) else memories)
            if memory["status"] == "Active"]


def raw_candidate(case):
    text, predicate, kind, value, _ = case
    result = candidate(SUBJECT, text, predicate=predicate)
    result["value"] = dict(kind=kind, text=None, integer=None, boolean=None, entity_id=None)
    result["value"]["entity_id" if kind == "entity" else kind] = value
    return result


class Fixture(http.server.BaseHTTPRequestHandler):
    requests = []
    errors = []
    case = None
    db = None

    def log_message(self, *_):
        pass

    def do_POST(self):
        try:
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert self.path == "/v1/responses"
            assert self.headers["Authorization"] == "Bearer local-fixture-key"
            name = request["text"]["format"]["name"]
            type(self).requests.append(name)
            prompt = json.loads(request["input"])
            text, predicate, kind, value, verdict = self.case
            assert prompt["current_user_turn"] == text
            assert prompt["conversation_history"] == []
            if name == REVIEW:
                assert not active(self.db), "facts became Active before independent review"
                assert "known_entities" in prompt
                assert "previous_output" not in prompt and "validation_feedback" not in prompt
                frozen = prompt["frozen_candidates"]
                assert len(frozen) == 1, frozen
                assert frozen[0]["subject"] == {"Resolved": SUBJECT}, frozen
                assert frozen[0]["predicate"] == predicate, frozen
                expected = {dict(text="Text", integer="Integer", boolean="Boolean", entity="Entity")[kind]: value}
                assert frozen[0]["value"] == expected, frozen
                assert frozen[0]["evidence"][0]["quote"] == text, frozen
                if verdict == "http_error":
                    self.respond(503, {"error": {"message": "synthetic review unavailable"}})
                    return
                checks = dict.fromkeys(CHECKS, "supported")
                if verdict == "contradicted":
                    checks["value"] = "contradicted"
                output = source_support_review(request)
                output["reviews"][0]["checks"] = checks
                if verdict == "contradicted":
                    output["reviews"][0]["issues"] = [{"check":"value", "field":"value",
                        "reason":"The proposed value differs from this USER statement.",
                        "evidence":[{"turn_index":0,"quote":text}]}]
                if verdict == "bad_format":
                    output["reviews"] = "invalid review array"
            else:
                # The old implementation's repair request is answered without repairing anything.
                assert name in (EXTRACTION, "personal_memory_candidates_verification_v1"), name
                output = {"omission_reason": None, "candidates": [raw_candidate(self.case)]}
            self.respond(200, {"status": "completed", "usage": {"input_tokens": 10, "output_tokens": 10},
                         "output": [{"type": "message", "content": [{"type": "output_text",
                         "text": json.dumps(output, ensure_ascii=False)}]}]})
        except Exception as error:
            type(self).errors.append(str(error))
            self.respond(500, {"error": {"message": "fixture assertion failed"}})

    def respond(self, status, body):
        data = json.dumps(body, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def tool(client, name, args):
    result = client.rpc("tools/call", {"name": name, "arguments": args})
    return result.get("structuredContent") or json.loads(result["content"][0]["text"])


def run_case(binary, directory, env, mode, name):
    case = CASES[name]
    text, predicate, kind, value, verdict = case
    expected_calls = 1 if verdict == "format_reject" else 2
    expected_active = verdict == "supported"
    capture = mode != "legacy"
    db = directory / f"{mode}-{name}.sqlite3"
    Fixture.case, Fixture.db = case, db
    before, errors_before = len(Fixture.requests), len(Fixture.errors)
    config = settings(directory, f"{mode}-{name}", retrieval_mode="local", fact_index_enabled=True,
                      enrichment_mode={"explicit": "off", "legacy": "inline"}.get(mode, mode),
                      source_capture_enabled=capture, memory_types=[],
                      predicates=[{"predicate": item, "cardinality": "Multiple" if item == "fact" else "Single"}
                                  for item in ("fact", "material", "maintenance_budget", "owned_by", "duplex_enabled")],
                      value_kinds=[["material", ["Text"]],
                                   ["maintenance_budget", ["Integer"]], ["owned_by", ["Entity"]],
                                   ["duplex_enabled", ["Boolean"]]],
                      model_budget=dict(group_id=f"{mode}-{name}", max_calls=expected_calls,
                                        max_operation_calls=2))
    failures = []

    def check(condition, message):
        if not condition:
            failures.append(message)

    with contextlib.closing(Client(binary, db, env, OWNER, config=config)) as client:
        client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
        for entity in (SUBJECT, OTHER):
            client.call("memory_entity_upsert", {"id": entity, "canonical_name": entity,
                                                "aliases": [], "entity_type": "Other"})
        turn = dict(text=text, history=[], session_id="synthetic-grounding", turn_id=name,
                    capture_source=capture)
        if mode == "inline":
            prepared = tool(client, "memory_prepare", {"user_turn": turn})
            receipt = prepared["remember"].get("result", prepared["remember"])
            check(prepared["recall"]["status"] == "completed", "local recall failed")
        else:
            receipt = tool(client, "memory_remember", turn)
        if mode in ("deferred", "explicit"):
            check(not active(db) and len(Fixture.requests) == before, "capture performed model work")
            source_id = receipt["source"]["source_id"]
            batch_args = ["--drain-enrichment", "--max-jobs", "1"] if mode == "deferred" else ["--enrich-source", source_id]
            completed = subprocess.run([str(binary), "--db", str(db), "--owner", OWNER,
                                        "--config", str(config), *batch_args], env=env,
                                       text=True, capture_output=True, timeout=30)
            batch_ok = mode == "deferred" or verdict not in ("format_reject", "bad_format", "http_error")
            check((completed.returncode == 0) == batch_ok, "unexpected batch exit: " + completed.stderr[:200])
            if mode == "deferred":
                check(json.loads(completed.stdout)["attempted"] == 1, "deferred batch did not process exactly one source")
            after_batch = len(Fixture.requests)
            receipt = tool(client, "memory_remember", turn)
            check(receipt["replayed"] and len(Fixture.requests) == after_batch, "batch receipt replay sent another request")

        memories = active(db)
        check(len(memories) == int(expected_active), f"Active count {len(memories)}, expected {int(expected_active)}")
        if memories and expected_active:
            expected_value = {dict(text="Text", integer="Integer", boolean="Boolean", entity="Entity")[kind]: value}
            check(memories[0]["value"] == expected_value, "committed value changed after extraction")
            check(memories[0]["evidence"][0]["quote"] == text, "committed original quote changed")
        state = snapshot(db)
        if capture:
            sources = list(state["sources"].values())
            check(len(sources) == 1 and sources[0]["request"]["text"] == text, "original source not preserved")
            check(receipt["source"]["status"] == "stored", "source receipt is not stored")
            check(receipt["index"]["status"] == "ready", "source index is not ready")
            check((receipt["enrichment"]["status"] == "committed") == expected_active,
                  "enrichment receipt disagrees with independent review")
            query = {"fact_sentence": "霜叶31 A4 纸", "text": "霜叶31的材质是什么？",
                     "integer": "霜叶31的维护预算是多少？", "entity": "霜叶31 所有者 青石82",
                     "boolean": "霜叶31 自动双面打印"}.get(name, "霜叶31的维护预算是多少？")
            recalled = client.call("memory_recall", {"query": query})
            check(recalled["model_calls"] == [], "local recall consumed model budget")
            if expected_active:
                check(any(item["text"] == text for item in recalled["source_evidence"]),
                      "correct committed fact is not available to recall")
            else:
                check(not recalled["source_evidence"], "unreviewed source exposed as answer evidence")
        else:
            check(not state["sources"], "legacy capture_source=false saved a raw source")

        requests = Fixture.requests[before:]
        expected_names = [EXTRACTION] + ([REVIEW] if expected_calls == 2 else [])
        check(requests == expected_names, f"requests {requests}, expected {expected_names}")
        with sqlite3.connect(db) as connection:
            ledger = connection.execute("SELECT operation_id, stage FROM model_calls ORDER BY created_at, rowid").fetchall()
            policy = json.loads(connection.execute("SELECT policy_json FROM model_budget_groups").fetchone()[0])
            if capture:
                indexed = connection.execute("SELECT text FROM source_segments WHERE owner_id=?", (OWNER,)).fetchall()
                check(any(row[0] == text for row in indexed), "original text missing from source index")
        check(len(ledger) == expected_calls, f"ledger count {len(ledger)}, expected {expected_calls}")
        check(len({operation for operation, _ in ledger}) == 1, "intake split across model operations")
        check(len(ledger) <= 2 and len(set(ledger)) == len(ledger), "operation or per-stage limit exceeded")
        check([stage for _, stage in ledger] == ["extraction"] + (["extraction_review"] if expected_calls == 2 else []),
              "ledger stage sequence differs from extraction plus review")
        check(policy["per_stage_calls"] == 1 and policy["limits"]["max_calls"] == expected_calls
              and policy["limits"]["max_operation_calls"] == 2, "configured budget changed")
        check(not Fixture.errors[errors_before:], "fixture errors: " + str(Fixture.errors[errors_before:]))
    return dict(mode=mode, case=name, expected_calls=expected_calls, actual_calls=len(requests),
                active=len(memories), passed=not failures, failures=failures)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/pmr-mcp")
    parser.add_argument("--modes", nargs="+", choices=["inline", "deferred", "explicit", "legacy"],
                        default=["inline", "deferred", "explicit", "legacy"])
    parser.add_argument("--cases", nargs="+", choices=list(CASES), default=list(CASES))
    parser.add_argument("--max-calls", type=int, default=72, help="Fixed total loopback allowance; never auto-expanded")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    predicted = len(args.modes) * sum(1 if CASES[name][-1] == "format_reject" else 2 for name in args.cases)
    assert predicted <= args.max_calls, f"predicted {predicted} calls exceed explicit allowance {args.max_calls}"
    binary = args.binary.resolve()
    assert binary.is_file(), f"build the MCP binary first: {binary}"
    Fixture.requests, Fixture.errors = [], []
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-grounding-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        clean = {key: value for key, value in os.environ.items()
                 if not key.startswith(("DEEPSEEK_", "OPENAI_", "PMR_"))}
        env = dict(clean, PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
                   DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
                   NO_PROXY="127.0.0.1,localhost")
        rows = [run_case(binary, directory, env, mode, name) for mode in args.modes for name in args.cases]
    report = dict(passed=all(row["passed"] for row in rows), cases=len(rows),
                  predicted_calls=predicted, actual_calls=len(Fixture.requests),
                  max_calls=args.max_calls, real_provider_requests=0, results=rows)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    for row in rows:
        print(f"{'PASS' if row['passed'] else 'FAIL'} {row['mode']}/{row['case']}: "
              f"Active={row['active']}, calls={row['actual_calls']}/{row['expected_calls']}"
              + ("; " + "; ".join(row["failures"]) if row["failures"] else ""))
    print(json.dumps({key: value for key, value in report.items() if key != "results"}, ensure_ascii=False))
    assert report["passed"], "source grounding process regression failed"


if __name__ == "__main__":
    main()
