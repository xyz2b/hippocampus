#!/usr/bin/env python3
"""Current USER review quotes across four MCP paths; synthetic loopback only."""
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
import traceback
from pathlib import Path

import source_review_cardinality_process as process


grounding = process.grounding
SUBJECTS, TEXTS = process.SUBJECTS, process.TEXTS
HISTORY = "上次我提到了雾岚打印机的型号。"
STAGES = ["extraction", "extraction_review", "extraction_repair", "extraction_repair_review"]
# Expected physical calls, committed facts, enrichment status.
CASES = {
    "current_quote": (2, 1, "committed"),
    "current_plus_history": (2, 1, "committed"),
    "legacy_current": (2, 0, "failed"),
    "missing_quote": (2, 0, "failed"),
    "legacy_history_only": (2, 0, "failed"),
    "empty_quote_no_fallback": (2, 0, "failed"),
    "blank_quote_no_fallback": (2, 0, "failed"),
    "null_quote_no_fallback": (2, 0, "failed"),
    "forged_quote_no_fallback": (2, 0, "failed"),
    "history_quote_no_fallback": (2, 0, "failed"),
    "single_character_no_fallback": (2, 0, "failed"),
    "truncated_quote_no_fallback": (2, 0, "failed"),
    "missing_punctuation_no_fallback": (2, 0, "failed"),
    "too_many_supplements": (2, 0, "failed"),
    "negative_counterevidence": (2, 0, "completed_rejected"),
    "empty_candidates": (2, 0, "completed_no_facts"),
    "repair_success": (4, 2, "committed"),
}


def grouped_candidate(subject, text):
    candidate = grounding.candidate(subject, text, predicate="fact")
    for field in ("subject_id", "unresolved_subject", "temporal_expression", "temporal_date",
            "temporal_end_date", "expiry_expression", "expiry_date", "valid_from", "valid_to",
            "expires_at", "explicit_memory_request"):
        candidate.pop(field, None)
    candidate.update(subject={"entity_id": subject}, fact_time=None,
        memory_request={"kind": "not_requested"})
    return candidate


class Fixture(grounding.Fixture):
    bodies, outputs, errors = [], [], []
    selected, text, db, start = None, None, None, 0

    def do_POST(self):
        try:
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert self.path == "/v1/responses" and self.headers["Authorization"] == "Bearer local-fixture-key"
            type(self).bodies.append(request)
            number = len(self.bodies) - self.start
            assert number <= CASES[self.selected][0], "unexpected extra model request"
            payload = json.loads(request["input"])
            assert payload["current_user_turn"] == self.text and payload["conversation_history"] == [HISTORY]
            assert not grounding.active(self.db), "facts existed before completed review"
            if request["text"]["format"]["name"] == grounding.EXTRACTION:
                count = 0 if self.selected == "empty_candidates" else 2 if number == 3 else 1
                candidates = [grouped_candidate(SUBJECTS[i][0], TEXTS[i]) for i in range(count)]
                if self.selected == "negative_counterevidence":
                    candidates[0].update(predicate="material",
                        value=dict(kind="text", text="塑料", integer=None, boolean=None, entity_id=None))
                output = dict(omission_reason=None if count else "no_personal_assertion", candidates=candidates)
            else:
                assert request["text"]["format"]["name"] == grounding.REVIEW
                output = grounding.source_support_review(request)
                for entry in output["reviews"]:
                    entry.update(current_user_quote=self.text, evidence=[])
                if output["reviews"]:
                    entry = output["reviews"][0]
                    if self.selected in ("legacy_current", "missing_quote", "legacy_history_only"):
                        entry.pop("current_user_quote")
                    if self.selected == "legacy_current":
                        entry["evidence"] = [dict(turn_index=0, quote=self.text)]
                    if self.selected in ("current_plus_history", "legacy_history_only"):
                        entry["evidence"] = [dict(turn_index=1, quote=HISTORY)]
                    if self.selected.endswith("_no_fallback"):
                        entry["current_user_quote"] = {
                            "empty_quote_no_fallback": "", "blank_quote_no_fallback": " \t\n",
                            "null_quote_no_fallback": None, "forged_quote_no_fallback": "用户没有说过的引文。",
                            "history_quote_no_fallback": HISTORY,
                            "single_character_no_fallback": self.text[:1],
                            "truncated_quote_no_fallback": self.text[:-3],
                            "missing_punctuation_no_fallback": self.text[:-1]}[self.selected]
                        entry["evidence"] = [dict(turn_index=0, quote=self.text)]
                    if self.selected == "too_many_supplements":
                        entry["evidence"] = [dict(turn_index=0, quote=self.text)] * 16
                    if self.selected == "negative_counterevidence":
                        entry["checks"]["value"] = "contradicted"
                        entry["issues"] = [dict(check="value", field="value.text",
                            reason="原文明说橡木，候选错误地给出塑料。",
                            evidence=[dict(turn_index=0, quote=self.text)])]
                if self.selected == "repair_success" and number == 2:
                    output["coverage"] = dict(status="incomplete", missing_user_spans=[TEXTS[1]])
            type(self).outputs.append(output)
            self.respond(200, process.Fixture.completed(output))
        except Exception as error:
            type(self).errors.append(str(error))
            self.respond(500, {"error": {"message": "fixture assertion failed"}})


def run_case(binary, directory, env, mode, name):
    calls, expected_facts, status = CASES[name]
    text = "你好。" if name == "empty_candidates" else "".join(TEXTS) if name == "repair_success" else TEXTS[0]
    if name == "negative_counterevidence":
        text = "雾岚打印机的材质是橡木。"
    work = directory / f"{mode}-{name}"
    work.mkdir()
    (work / "trace").mkdir(mode=0o700)
    db, capture = work / "state.sqlite3", mode != "legacy"
    env = dict(env, PMR_TRACE_DIR=str(work / "trace"))
    Fixture.selected, Fixture.text, Fixture.db, Fixture.start = name, text, db, len(Fixture.bodies)
    start, errors_start = Fixture.start, len(Fixture.errors)
    config = grounding.settings(work, "config", retrieval_mode="local", fact_index_enabled=True,
        enrichment_mode={"explicit": "off", "legacy": "inline"}.get(mode, mode),
        source_capture_enabled=capture, memory_types=[], value_kinds=[],
        predicates=[dict(predicate="fact", cardinality="Multiple"), dict(predicate="material", cardinality="Single")],
        model_budget=dict(group_id=f"{mode}-{name}", max_calls=4, max_operation_calls=4,
            allow_coverage_repair=True, extraction_output_tokens=16384, review_output_tokens=16384))
    with contextlib.closing(grounding.Client(binary, db, env, grounding.OWNER, config=config)) as client:
        client.call("memory_recall", dict(query="全局回答要求", bootstrap=True))
        for entity, canonical in SUBJECTS:
            client.call("memory_entity_upsert", dict(id=entity, canonical_name=canonical, aliases=[], entity_type="Device"))
        turn = dict(text=text, history=[HISTORY], session_id="review-evidence", turn_id=name, capture_source=capture)
        if mode == "inline":
            prepared = grounding.tool(client, "memory_prepare", dict(user_turn=turn))
            receipt = prepared["remember"].get("result", prepared["remember"])
            assert prepared["recall"]["status"] == "completed"
        else:
            receipt = grounding.tool(client, "memory_remember", turn)
        if mode in ("deferred", "explicit"):
            assert len(Fixture.bodies) == start and not grounding.active(db)
            args = ["--drain-enrichment", "--max-jobs", "1"] if mode == "deferred" else ["--enrich-source", receipt["source"]["source_id"]]
            batch = subprocess.run([str(binary), "--db", str(db), "--owner", grounding.OWNER,
                "--config", str(config), *args], env=env, capture_output=True, text=True, timeout=30)
            assert (batch.returncode == 0) == (mode == "deferred" or status != "failed"), batch.stderr[:200]
            if mode == "deferred":
                assert json.loads(batch.stdout)["attempted"] == 1
            receipt = grounding.tool(client, "memory_remember", turn)
            assert receipt["replayed"]
        state, memories = grounding.snapshot(db), grounding.active(db)
        assert len(memories) == expected_facts and not state["staging"]["pending"]
        if not expected_facts:
            assert not state["repository"]["memories"], "rejected review persisted a candidate/proof"
        else:
            assert {(m["subject_id"], m["value"]["Text"]) for m in memories} == set(zip([s[0] for s in SUBJECTS], TEXTS[:expected_facts]))
            assert all(m["evidence"][0]["quote"] == text for m in memories)
        if capture:
            source, = state["sources"].values()
            assert source["request"]["text"] == text and source["request"]["history"] == [HISTORY]
            assert receipt["source"]["status"] == "stored" and receipt["index"]["status"] == "ready"
            assert receipt["enrichment"] == dict(status=status, error="invalid_candidates" if status == "failed" else None), receipt["enrichment"]
            before = len(Fixture.bodies)
            assert grounding.tool(client, "memory_remember", turn)["replayed"]
            assert len(Fixture.bodies) == before
        else:
            assert not state["sources"]
        requests = Fixture.bodies[start:]
        assert not Fixture.errors[errors_start:], Fixture.errors[errors_start:]
        assert len(requests) == calls
        with sqlite3.connect(db) as connection:
            ledger = connection.execute("SELECT operation_id, stage, outcome FROM model_calls ORDER BY rowid").fetchall()
            assert len({r[0] for r in ledger}) == 1 and [r[1] for r in ledger] == STAGES[:calls]
            assert all(r[2] == "completed" for r in ledger)
            if capture:
                assert (text,) in connection.execute("SELECT text FROM source_segments").fetchall()
        for request in requests[1::2]:
            schema = request["text"]["format"]["schema"]["properties"]["reviews"]["items"]
            assert "current_user_quote" in schema["required"]
            quote_schema = schema["properties"]["current_user_quote"]
            assert quote_schema["type"] == "string" and quote_schema["enum"] == [text]
            assert schema["properties"]["evidence"]["maxItems"] == 15
    return dict(mode=mode, case=name, passed=True, facts=len(memories), loopback_requests=calls,
        ledger_stages=[r[1] for r in ledger], enrichment=receipt.get("enrichment")), requests


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=grounding.ROOT / "target/debug/pmr-mcp")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    binary, rows, samples = args.binary.resolve(), [], {}
    fingerprint = hashlib.sha256(binary.read_bytes()).hexdigest()
    Fixture.bodies, Fixture.outputs, Fixture.errors = [], [], []
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-review-evidence-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        env = {k: os.environ[k] for k in ("PATH", "HOME", "TMPDIR", "LANG") if k in os.environ}
        env.update(PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
            DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1", NO_PROXY="127.0.0.1,localhost")
        for mode in ("inline", "deferred", "explicit", "legacy"):
            for name in CASES:
                before = len(Fixture.bodies)
                try:
                    row, requests = run_case(binary, directory, env, mode, name)
                    if mode == "inline" and name in ("current_quote", "empty_candidates", "repair_success"):
                        samples[name] = requests[-1]
                except (AssertionError, KeyError, TypeError, ValueError, sqlite3.Error) as error:
                    row = dict(mode=mode, case=name, passed=False, failure=str(error),
                        failure_location=traceback.format_exc(), loopback_requests=len(Fixture.bodies)-before)
                rows.append(row)
    unchanged = hashlib.sha256(binary.read_bytes()).hexdigest() == fingerprint
    report = dict(passed=unchanged and all(r["passed"] for r in rows), cases=len(rows), results=rows,
        binary_sha256=fingerprint, binary_unchanged=unchanged, loopback_requests=len(Fixture.bodies), real_provider_requests=0,
        limits="Synthetic protocol fixtures exercise real MCP/backend storage and accounting, not model semantic quality. No archived response is altered or replayed here.")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    for name, request in samples.items():
        args.output.with_name(args.output.stem + "-" + name + "-request.json").write_text(json.dumps(request, ensure_ascii=False, indent=2)+"\n")
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2)+"\n")
    print(json.dumps({k: report[k] for k in ("passed", "cases", "loopback_requests", "real_provider_requests", "binary_sha256")}))
    for row in rows:
        if not row["passed"]:
            print(row["mode"], row["case"], row["failure"])
    assert report["passed"], "source review evidence process failed"


if __name__ == "__main__":
    main()
