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
from write_pipeline_process import frozen


ROOT = Path(__file__).resolve().parents[1]
OWNER = "intake-grounding-synthetic"
SUBJECT = "霜叶31"
OTHER = "青石82"
REVIEW = "w3_v6"
EXTRACTION = "w1_v5"
PAPER = "霜叶31默认用 A4 纸打印，纸张放在右侧纸盒。"
OTHER_SENTENCE = "青石82收藏木牌。"
# text, predicate, value kind, value, review verdict; all statements are synthetic.
CASES = {
    "a4_false": (PAPER, "fact", "boolean", False, "format_reject"),
    "fact_fragment": (PAPER, "fact", "text", "A4", "insufficient"),
    "fact_fragment_outside_sentence": (PAPER + OTHER_SENTENCE, "fact", "text", OTHER_SENTENCE, "format_reject"),
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
    quote = str(value) if kind in ("text", "integer", "entity") else text
    # The negative control cites real source text outside this candidate's sentence.
    sentence = PAPER if case == CASES["fact_fragment_outside_sentence"] else text
    return {"kind": "assertion", "speech": "user_statement",
            "subject": {"entity_id": SUBJECT}, "relation": predicate, "memory_type": "fact",
            "value": {"kind": kind, "entity_id" if kind == "entity" else kind: value,
                      "quote": {"text": quote}},
            "sentence": {"text": sentence}, "polarity": "affirm", "strength": None,
            "uncertainty": None, "conditions": [], "times": []}


def model_output(request, case, db):
    name = request["text"]["format"]["name"]
    prompt = json.loads(request["input"])
    text, predicate, kind, value, verdict = case
    assert prompt["current_user_message"] == text
    assert prompt["user_history"] == []
    assert "model" not in prompt
    assert request["model"] == "pro-fixture"
    assert request["reasoning"]["effort"] == "low"
    if name == EXTRACTION:
        return {"candidates": [raw_candidate(case)], "memory_requests": []}
    assert name == REVIEW, name
    assert not active(db), "facts became Active before independent review"
    assert {e["id"] for e in prompt["entities"]} >= {SUBJECT, OTHER}
    assert "previous_output" not in prompt and "validation_feedback" not in prompt
    candidates = prompt["candidates"]
    assert len(candidates) == 1 and candidates[0]["index"] == 0, candidates
    assert candidates[0]["status"] == "ok", candidates
    assert candidates[0]["candidate"] == raw_candidate(case), candidates
    assert prompt["memory_requests"] == []
    return review_output(case)


def review_output(case):
    text, _, _, _, verdict = case
    if verdict == "bad_format":
        return {"reviews": "invalid review array"}
    return {"reviews": [{"candidate": 0,
            "verdict": verdict if verdict in ("contradicted", "insufficient") else "supported",
            "problems": [{"aspect": "value", "quote": {"text": text}}]
                        if verdict in ("contradicted", "insufficient") else [], "episode_value": "not_applicable"}],
            "coverage": {"status": "complete", "missing": []}, "request_reviews": []}


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
            output = model_output(request, self.case, self.db)
            if name == REVIEW and self.case[-1] == "http_error":
                self.respond(503, {"error": {"message": "synthetic review unavailable"}})
                return
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


def cli(binary, db, config, env, *args, arguments=None, success=True):
    if arguments is not None:
        path = db.with_suffix(".arguments.json")
        path.write_text(json.dumps(arguments, ensure_ascii=False))
        args = (*args, "--arguments", str(path))
    result = subprocess.run([str(binary), "--db", str(db), "--owner", OWNER,
                             "--config", str(config), *args], env=env, text=True,
                            capture_output=True, timeout=35)
    assert (result.returncode == 0) == success, (result.stdout, result.stderr)
    return json.loads(result.stdout) if success else result.stderr


def run_case(binary, directory, env, mode, name):
    case = CASES[name]
    text, predicate, kind, value, verdict = case
    capture = mode != "unauthorized"
    expected_calls = (1 if verdict == "format_reject" else 2) if capture else 0
    expected_active = capture and verdict == "supported"
    db = directory / f"{mode}-{name}.sqlite3"
    Fixture.case, Fixture.db = case, db
    before, errors_before = len(Fixture.requests), len(Fixture.errors)
    mode_value = {"explicit": "off", "unauthorized": "inline"}.get(mode, mode)
    policy_calls = max(1, expected_calls)
    config_data = dict(enrichment_mode=mode_value, fact_index_enabled=True,
                       memory_types=[],
                       predicates=[{"predicate": item, "cardinality": "Multiple" if item == "fact" else "Single"}
                                   for item in ("fact", "material", "maintenance_budget", "owned_by", "duplex_enabled")],
                       value_kinds=[["material", ["Text"]], ["maintenance_budget", ["Integer"]],
                                    ["owned_by", ["Entity"]], ["duplex_enabled", ["Boolean"]]],
                       model_budget=dict(group_id=f"{mode}-{name}", max_calls=policy_calls,
                                         max_operation_calls=2))
    config = directory / f"{mode}-{name}.json"
    config.write_text(json.dumps(config_data))
    # The read/setup host has no worker; CLI capture tests each actual accepted mode without races.
    reader = directory / f"{mode}-{name}-reader.json"
    reader.write_text(json.dumps(dict(config_data, enrichment_mode="off")))
    failures = []

    def check(condition, message):
        if not condition:
            failures.append(message)

    with contextlib.closing(Client(binary, db, env, OWNER, config=reader)) as client:
        boot = client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True,
                                           "request_id": f"{mode}-{name}-bootstrap"})
        check(boot["recall"]["semantic_judgment"] == "skipped", "bootstrap called a model")
        for entity in (SUBJECT, OTHER):
            client.call("memory_entity_upsert", {"id": entity, "canonical_name": entity,
                                                "aliases": [], "entity_type": "Other"})

    turn = dict(text=text, history=[], session_id="synthetic-grounding", turn_id=name,
                capture_source=capture)
    tool = "memory_prepare" if mode == "inline" else "memory_remember"
    arguments = {"user_turn": turn, "offline": True} if mode == "inline" else turn
    initial = snapshot(db)
    receipt = cli(binary, db, config, env, "--call", tool, arguments=arguments, success=capture)
    if not capture:
        check("capture_source_required" in receipt, "unauthorized write was not rejected")
        check(snapshot(db) == initial and not active(db), "unauthorized write changed business state")
    else:
        queued = "not_requested" if mode == "explicit" else "queued"
        if mode == "inline":
            check(receipt["recall"]["semantic_judgment"] == "skipped", "offline prepare ran a model")
            check(any(r["source_id"] == receipt["source"]["source_id"] for r in receipt["recall"]["raw"]),
                  "prepare cannot see the captured source")
            receipt = {k: receipt[k] for k in ("source", "enrichment")}
        frozen(receipt, queued)
        check(not active(db) and len(Fixture.requests) == before, "capture performed model work")
        source_id = receipt["source"]["source_id"]
        args = ("--drain-enrichment", "--max-jobs", "1") if mode == "deferred" else ("--enrich-source", source_id)
        processed = cli(binary, db, config, env, *args)
        if mode == "deferred":
            check(processed["attempted"] == 1, "deferred batch did not process exactly one source")
            processed = processed["results"][0]["result"]
        status = "committed" if expected_active else "failed" if verdict in ("bad_format", "http_error") else "pending"
        frozen(processed, status)
        if verdict not in ("bad_format", "http_error"):
            candidate_result = processed["enrichment"]["candidates"][0]
            # A valid continuous fragment passes W2; this fixture's W3 still finds it insufficient.
            expected_candidate_status = ("committed" if expected_active else
                                         "pending" if verdict == "insufficient" else "rejected")
            check(candidate_result["status"] == expected_candidate_status,
                  "candidate receipt disagrees with grounding result")
            if verdict == "format_reject":
                check("candidate_rejected:fact_sentence_mismatch" in candidate_result["reasons"],
                      "fact format rejection lost its reason")
            if verdict == "contradicted":
                check("review_contradicted" in candidate_result["reasons"], "independent review verdict lost")
            if verdict == "insufficient":
                check("review_insufficient" in candidate_result["reasons"], "independent review insufficiency lost")
        after_batch = len(Fixture.requests)
        replay = cli(binary, db, config, env, "--call", "memory_remember", arguments=turn)
        check(replay == processed and len(Fixture.requests) == after_batch, "receipt replay changed or sent another request")

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
        query = {"fact_sentence": "霜叶31 A4 纸", "text": "霜叶31的材质是什么？",
                 "integer": "霜叶31的维护预算是多少？", "entity": "霜叶31 所有者 青石82",
                 "boolean": "霜叶31 自动双面打印"}.get(name, "霜叶31的维护预算是多少？")
        with contextlib.closing(Client(binary, db, env, OWNER, config=reader)) as client:
            recalled = client.call("memory_recall", {"query": query, "offline": True,
                                                    "request_id": f"{mode}-{name}-recall"})["recall"]
        check(recalled["semantic_judgment"] == "skipped" and recalled["facts"] == [],
              "offline recall made a semantic answer")
        if expected_active:
            check(any(r["grade"] == "committed" and r["id"] == memories[0]["id"]
                      for r in recalled["records"]), "committed fact missing from offline recall")
        else:
            check(not any(r["grade"] == "committed" for r in recalled["records"]),
                  "unreviewed fact exposed as committed evidence")
            check(any(r["source_id"] == source_id and r["quote"]["text"] == text for r in recalled["raw"]),
                  "unreviewed raw source is no longer visible")
    else:
        check(not state["sources"], "unauthorized capture saved a raw source")

    requests = Fixture.requests[before:]
    expected_names = ([EXTRACTION] + ([REVIEW] if expected_calls == 2 else [])) if capture else []
    check(requests == expected_names, f"requests {requests}, expected {expected_names}")
    with sqlite3.connect(db) as connection:
        ledger = connection.execute("SELECT operation_id, stage FROM model_calls ORDER BY created_at, rowid").fetchall()
        policy_row = connection.execute("SELECT policy_json FROM model_budget_groups").fetchone()
        policy = json.loads(policy_row[0]) if policy_row else None
        if capture:
            indexed = connection.execute("SELECT text FROM source_segments WHERE owner_id=?", (OWNER,)).fetchall()
            check(any(row[0] == text for row in indexed), "original text missing from source index")
    check(len(ledger) == expected_calls, f"ledger count {len(ledger)}, expected {expected_calls}")
    check(len({operation for operation, _ in ledger}) == int(capture), "intake split across model operations")
    check(len(ledger) <= 2 and len(set(ledger)) == len(ledger), "operation or per-stage limit exceeded")
    expected_stages = (["w1"] + (["w3"] if expected_calls == 2 else [])) if capture else []
    check([stage for _, stage in ledger] == expected_stages,
          "ledger stage sequence differs from W1 plus W3")
    if capture:
        check(policy is not None and policy["per_stage_calls"] == 1
              and policy["limits"]["max_calls"] == policy_calls
              and policy["limits"]["max_operation_calls"] == 2, "configured budget changed")
    else:
        check(policy is None, "unauthorized capture registered a model operation budget")
    check(not Fixture.errors[errors_before:], "fixture errors: " + str(Fixture.errors[errors_before:]))
    return dict(mode=mode, case=name, expected_calls=expected_calls, actual_calls=len(requests),
                active=len(memories), passed=not failures, failures=failures)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/hippo-mcp")
    parser.add_argument("--modes", nargs="+", choices=["inline", "deferred", "explicit", "unauthorized"],
                        default=["inline", "deferred", "explicit", "unauthorized"])
    parser.add_argument("--cases", nargs="+", choices=list(CASES), default=list(CASES))
    parser.add_argument("--max-calls", type=int, default=72, help="Fixed total loopback allowance; never auto-expanded")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--fixtures-json", action="store_true", help="Print synthetic stage outputs; no process or port")
    args = parser.parse_args()
    if args.fixtures_json:
        print(json.dumps([{"name": name, "text": case[0], "verdict": case[-1],
                           "w1": {"candidates": [raw_candidate(case)], "memory_requests": []},
                           "w3": review_output(case)} for name, case in CASES.items()], ensure_ascii=False))
        return
    predicted = sum(mode != "unauthorized" for mode in args.modes) * sum(1 if CASES[name][-1] == "format_reject" else 2 for name in args.cases)
    assert predicted <= args.max_calls, f"predicted {predicted} calls exceed explicit allowance {args.max_calls}"
    binary = args.binary.resolve()
    assert binary.is_file(), f"build the MCP binary first: {binary}"
    Fixture.requests, Fixture.errors = [], []
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-grounding-")))
        endpoint = "http://127.0.0.1:1/v1"
        if predicted:
            server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
            threading.Thread(target=server.serve_forever, daemon=True).start()
            stack.callback(server.server_close)
            stack.callback(server.shutdown)
            endpoint = f"http://127.0.0.1:{server.server_port}/v1"
        clean = {key: value for key, value in os.environ.items()
                 if not key.startswith(("DEEPSEEK_", "OPENAI_", "PMR_", "HIPPOCAMPUS_"))}
        env = dict(clean, HIPPOCAMPUS_MODEL_NETWORK="offline", DEEPSEEK_PRO_MODEL="pro-fixture", DEEPSEEK_API_KEY="local-fixture-key",
                   DEEPSEEK_BASE_URL=endpoint,
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
