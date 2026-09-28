#!/usr/bin/env python3
"""Bounded source repair and model calendar dates via real MCP; synthetic loopback only."""
import argparse
import contextlib
from datetime import date, timedelta
import hashlib
import http.server
import json
import os
import sqlite3
import subprocess
import tempfile
import threading
from pathlib import Path

import source_review_cardinality_process as process
from temporal_grounding_process import day_start


grounding = process.grounding
A = "明天霜叶工作坊的报名费为42元。"
B = "云杉工作坊的报名费为54元。"
TTL = "赤砂工作坊的报名费为68元，保留到明天。"
CROSS = "霜叶工作坊的报名费为42元。明天云杉工作坊的报名费为54元。"
SUBJECTS = [("event_shuangye", "霜叶工作坊"), ("event_yunshan", "云杉工作坊"), ("event_chisha", "赤砂工作坊")]
# Expected physical calls, committed facts, enrichment status/error. None allows either
# failed or completed_rejected for a semantically rejected complete candidate set.
CASES = {
    "repair_success": (4, 2, "committed", None),
    "repair_incomplete": (4, 0, "failed", "source_coverage_incomplete"),
    "uncertain": (2, 0, "failed", "source_coverage_incomplete"),
    "fabricated_span": (2, 0, "failed", "invalid_candidates"),
    "first_negative": (2, 0, "failed", "source_coverage_incomplete"),
    "second_negative": (4, 0, None, None),
    "budget_three": (3, 0, "failed", None),
    "ttl_correct_date": (2, 1, "committed", None),
    "ttl_wrong_date": (1, 0, "failed", "invalid_candidates"),
    "ttl_missing_date": (1, 0, "failed", "invalid_candidates"),
    "ttl_missing_intent": (1, 0, "failed", "invalid_candidates"),
    "cross_subject_date": (2, 0, "failed", "invalid_candidates"),
    "cross_subject_fee": (2, 0, None, None),
}
STAGES = ["extraction", "extraction_review", "extraction_repair", "extraction_repair_review"]


def raw_candidate(subject, text, **fields):
    raw = grounding.candidate(subject, text, predicate="fact")
    raw.update(expiry_expression=None, temporal_date=None, temporal_end_date=None, expiry_date=None)
    raw.update(fields)
    return raw


class Fixture(grounding.Fixture):
    bodies = []
    outputs = []
    errors = []
    selected = None
    text = None
    start = 0

    def do_POST(self):
        try:
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert self.path == "/v1/responses" and self.headers["Authorization"] == "Bearer local-fixture-key"
            type(self).bodies.append(request)
            number = len(self.bodies) - self.start
            assert number <= CASES[self.selected][0], "unexpected retry or correction round"
            payload = json.loads(request["input"])
            assert payload["current_user_turn"] == self.text and payload["conversation_history"] == []
            assert not grounding.active(self.db), "facts became Active before the final review"
            tomorrow = (date.fromisoformat(payload["current_date"]) + timedelta(days=1)).isoformat()
            name = request["text"]["format"]["name"]
            if name == grounding.EXTRACTION:
                if self.selected.startswith("ttl_"):
                    expiry = None if self.selected == "ttl_missing_date" else tomorrow
                    if self.selected == "ttl_wrong_date":
                        expiry = (date.fromisoformat(tomorrow) + timedelta(days=1)).isoformat()
                    candidates = [raw_candidate("event_chisha", TTL, expiry_expression="明天",
                        expiry_date=expiry, explicit_memory_request=self.selected != "ttl_missing_intent")]
                elif self.selected.startswith("cross_subject_"):
                    text = CROSS.split("。")[0] + "。" if self.selected == "cross_subject_date" else CROSS.split("。")[1] + "。"
                    candidates = [raw_candidate("event_shuangye", text, temporal_expression="明天", temporal_date=tomorrow)]
                else:
                    candidates = [raw_candidate("event_shuangye", A, temporal_expression="明天", temporal_date=tomorrow)]
                    if number == 3 and self.selected != "repair_incomplete":
                        candidates.append(raw_candidate("event_yunshan", B))
                output = dict(omission_reason=None, candidates=candidates)
            else:
                assert name == grounding.REVIEW
                output = grounding.source_support_review(request)
                if not self.selected.startswith(("ttl_", "cross_subject_")):
                    incomplete = number == 2 or self.selected == "repair_incomplete"
                    if incomplete:
                        output["coverage"] = dict(status="incomplete", missing_user_spans=[B])
                    if self.selected == "uncertain":
                        output["coverage"] = dict(status="uncertain", missing_user_spans=[])
                    elif self.selected == "fabricated_span":
                        output["coverage"]["missing_user_spans"] = ["云杉工作坊的报名费为99元。"]
                negative = self.selected == "first_negative" or self.selected == "second_negative" and number == 4 or self.selected == "cross_subject_fee"
                if negative:
                    # Reject only one candidate: another supported candidate must not leak
                    # into storage from a failed replacement set.
                    output["reviews"][0]["checks"]["value"] = "contradicted"
                    output["reviews"][0]["issues"] = [dict(check="value", field="value.text",
                        reason="合成复核负例：该值未获得当前主体原文支持。",
                        evidence=[dict(turn_index=0, quote=self.text)])]
            type(self).outputs.append(output)
            self.respond(200, process.Fixture.completed(output))
        except Exception as error:
            type(self).errors.append(str(error))
            self.respond(500, {"error": {"message": "fixture assertion failed"}})


def run_case(binary, directory, env, mode, name):
    calls, expected_facts, status, error = CASES[name]
    text = TTL if name.startswith("ttl_") else CROSS if name.startswith("cross_subject_") else A + B
    work = directory / f"{mode}-{name}"
    work.mkdir()
    (work / "trace").mkdir(mode=0o700)
    env = dict(env, PMR_TRACE_DIR=str(work / "trace"))
    db = work / "state.sqlite3"
    capture = mode != "legacy"
    Fixture.selected, Fixture.text, Fixture.db, Fixture.start = name, text, db, len(Fixture.bodies)
    start, errors_start = Fixture.start, len(Fixture.errors)
    budget = dict(group_id=f"{mode}-{name}", max_calls=3 if name == "budget_three" else 4,
        max_operation_calls=4, allow_coverage_repair=True, extraction_output_tokens=8192, review_output_tokens=16384)
    config = grounding.settings(work, "config", retrieval_mode="local", fact_index_enabled=True,
        enrichment_mode={"explicit": "off", "legacy": "inline"}.get(mode, mode),
        source_capture_enabled=capture, utc_offset_seconds=28800, memory_types=[], value_kinds=[],
        predicates=[dict(predicate="fact", cardinality="Multiple")], model_budget=budget)
    failures = []
    def check(condition, message):
        if not condition:
            failures.append(message)
    with contextlib.closing(grounding.Client(binary, db, env, grounding.OWNER, config=config)) as client:
        client.call("memory_recall", dict(query="全局回答要求", bootstrap=True))
        for subject, canonical in SUBJECTS:
            client.call("memory_entity_upsert", dict(id=subject, canonical_name=canonical, aliases=[], entity_type="Other"))
        turn = dict(text=text, history=[], session_id="repair-calendar", turn_id=name, capture_source=capture)
        if mode == "inline":
            prepared = grounding.tool(client, "memory_prepare", dict(user_turn=turn))
            receipt = prepared["remember"].get("result", prepared["remember"])
            check(prepared["recall"]["status"] == "completed", "local recall failed")
        else:
            receipt = grounding.tool(client, "memory_remember", turn)
        if mode in ("deferred", "explicit"):
            check(not grounding.active(db) and len(Fixture.bodies) == start, "capture sent a model request")
            args = ["--drain-enrichment", "--max-jobs", "1"] if mode == "deferred" else ["--enrich-source", receipt["source"]["source_id"]]
            batch = subprocess.run([str(binary), "--db", str(db), "--owner", grounding.OWNER,
                "--config", str(config), *args], env=env, text=True, capture_output=True, timeout=30)
            if mode == "deferred":
                check(batch.returncode == 0 and json.loads(batch.stdout)["attempted"] == 1, "deferred batch failed")
            elif expected_facts:
                check(batch.returncode == 0, "successful explicit enrichment failed")
            receipt = grounding.tool(client, "memory_remember", turn)
            check(receipt["replayed"], "batch result was not replayed")
        state = grounding.snapshot(db)
        memories = grounding.active(db)
        check(len(memories) == expected_facts, f"Active={len(memories)}, expected {expected_facts}")
        check(not state["staging"]["pending"], "unexpected staged candidate")
        if not expected_facts:
            check(not state["repository"]["memories"], "rejected set left persisted facts")
        elif name == "repair_success":
            check({(m["subject_id"], m["value"]["Text"]) for m in memories}
                == {("event_shuangye", A), ("event_yunshan", B)}, "replacement set mixed subjects or values")
        else:
            check(memories[0]["value"] == {"Text": TTL}, "TTL changed the original complete assertion")
        if capture:
            source, = state["sources"].values()
            check(source["request"]["text"] == text, "source changed")
            check(receipt["source"]["status"] == "stored" and receipt["index"]["status"] == "ready", "source/index missing")
            enrichment = receipt["enrichment"]
            check(enrichment["status"] == status if status else enrichment["status"] in ("failed", "completed_rejected"), "wrong enrichment: " + str(enrichment))
            if error:
                check(enrichment["error"] == error, "wrong public error: " + str(enrichment))
            count = len(Fixture.bodies)
            replay = grounding.tool(client, "memory_remember", turn)
            check(replay["replayed"] and len(Fixture.bodies) == count, "receipt replay sent requests")
        else:
            enrichment = None
            check(not state["sources"], "legacy captured a source")
        requests = Fixture.bodies[start:]
        check(not Fixture.errors[errors_start:], "fixture errors: " + str(Fixture.errors[errors_start:]))
        check(len(requests) == calls, f"physical calls={len(requests)}, expected {calls}")
        check([r["text"]["format"]["name"] for r in requests] == [grounding.EXTRACTION, grounding.REVIEW, grounding.EXTRACTION, grounding.REVIEW][:calls], "request sequence differs")
        with sqlite3.connect(db) as connection:
            ledger = connection.execute("SELECT operation_id, stage, outcome, output_token_limit FROM model_calls ORDER BY rowid").fetchall()
            check([r[1] for r in ledger] == STAGES[:calls] and len({r[0] for r in ledger}) == 1, "ledger stages or operation differ")
            check(all(r[2] == "completed" for r in ledger), "unexpected physical request failure")
            check([r[3] for r in ledger] == [8192, 16384, 8192, 16384][:calls], "stage token caps changed")
            policy = json.loads(connection.execute("SELECT policy_json FROM model_budget_groups").fetchone()[0])
            check(policy["limits"].get("allow_coverage_repair") is True and policy["limits"]["max_operation_calls"] == 4, "repair authorization changed")
            if capture:
                check((text,) in connection.execute("SELECT text FROM source_segments").fetchall(), "source index lost original")
        first = json.loads(requests[0]["input"])
        tomorrow = (date.fromisoformat(first["current_date"]) + timedelta(days=1)).isoformat()
        for index, request in enumerate(requests):
            payload = json.loads(request["input"])
            check(all(payload[k] == first[k] for k in ("owner_id", "now", "current_date", "utc_offset_seconds", "session_id", "turn_id", "current_user_turn", "conversation_history")), "correction changed trusted input")
            if index == 2:
                check(payload.get("coverage_correction", {}).get("missing_user_spans") == [B], "correction lost exact missing USER span")
                check("previous_output" not in payload and "frozen_candidates" not in payload, "correction received candidate replacements as trusted input")
            if index % 2:
                check("coverage_correction" not in payload, "review inherited correction hints")
                views = payload["temporal_context"]["candidates"]
                check(all("proposed_temporal_date" in v and "proposed_temporal_end_date" in v and "proposed_expiry_date" in v for v in views), "review lacks normalized date context")
                check("issues" in request["text"]["format"]["schema"]["properties"]["reviews"]["items"]["required"], "review schema does not require issues")
        if len(requests) == 4:
            check(json.loads(requests[1]["input"])["review_binding"] != json.loads(requests[3]["input"])["review_binding"] if name != "repair_incomplete" else True, "replacement review reused the first snapshot binding")
        for memory in memories:
            expected = dict(observed_at=first["now"], valid_from=None, valid_to=None, expires_at=None, reference=None)
            if name == "repair_success" and memory["subject_id"] == "event_shuangye":
                expected.update(valid_from=dict(at=day_start(tomorrow), source="Inferred"), valid_to=dict(at=day_start(tomorrow)+86400, source="Inferred"))
            if name == "ttl_correct_date":
                expected["expires_at"] = day_start(tomorrow) + 86400
                check(memory["explicit_memory_request"] is True, "explicit retention intent lost")
            check(memory["temporal"] == expected and memory["evidence"][0]["quote"] == text, "persisted time/source differs")
    return dict(mode=mode, case=name, passed=not failures, failures=failures, facts=len(memories),
        loopback_requests=len(requests), ledger_stages=[r[1] for r in ledger], enrichment=enrichment)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=grounding.ROOT / "target/debug/pmr-mcp")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--modes", nargs="+", choices=("inline", "deferred", "explicit", "legacy"), default=["inline", "deferred", "explicit", "legacy"])
    parser.add_argument("--cases", nargs="+", choices=list(CASES), default=list(CASES))
    args = parser.parse_args()
    binary = args.binary.resolve()
    binary_sha = hashlib.sha256(binary.read_bytes()).hexdigest()
    Fixture.bodies, Fixture.outputs, Fixture.errors = [], [], []
    rows = []
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-repair-calendar-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        clean = {k: v for k, v in os.environ.items() if not k.startswith(("DEEPSEEK_", "OPENAI_", "ARK_", "PMR_"))}
        env = dict(clean, PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
            DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1", NO_PROXY="127.0.0.1,localhost")
        for mode in args.modes:
            for name in args.cases:
                before = len(Fixture.bodies)
                try:
                    rows.append(run_case(binary, directory, env, mode, name))
                except (AssertionError, KeyError, TypeError, ValueError, sqlite3.Error) as error:
                    rows.append(dict(mode=mode, case=name, passed=False, failures=[str(error)], loopback_requests=len(Fixture.bodies)-before))
    binary_unchanged = hashlib.sha256(binary.read_bytes()).hexdigest() == binary_sha
    report = dict(passed=binary_unchanged and all(r["passed"] for r in rows), cases=len(rows), results=rows,
        loopback_requests=len(Fixture.bodies), real_provider_requests=0, binary_sha256=binary_sha,
        binary_unchanged=binary_unchanged,
        protocol_fixture_note="Synthetic extraction/review replies exercise the real four-entry intake and durable accounting. They do not measure real-model semantic accuracy.")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({k: report[k] for k in ("passed", "cases", "loopback_requests", "real_provider_requests")}))
    for row in rows:
        if not row["passed"]:
            print(row["mode"], row["case"], row["failures"])
    assert report["passed"], "source repair/calendar process regression failed"


if __name__ == "__main__":
    main()
