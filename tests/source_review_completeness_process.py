#!/usr/bin/env python3
"""Source completeness through four real MCP paths; synthetic loopback replies only."""
import argparse
import contextlib
import copy
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
from datetime import date, datetime, timedelta, timezone


grounding = process.grounding
ARCHIVE = grounding.ROOT / "docs/refactor/s5/temporal-expression-live-20260928/specialist"
# scenario, expected enrichment, expected public error; complete candidates stay immutable.
CASES = {
    "iso_supported": ("iso_date", "committed", None),
    "iso_insufficient": ("iso_date", "completed_rejected", None),
    "multi_complete": ("other_subject", "committed", None),
    "multi_incomplete": ("other_subject", "failed", "source_coverage_incomplete"),
    "missing_coverage": ("other_subject", "failed", "invalid_candidates"),
    "uncertain_coverage": ("other_subject", "failed", "source_coverage_incomplete"),
    "fabricated_span": ("other_subject", "failed", "invalid_candidates"),
    "complete_with_missing": ("other_subject", "failed", "invalid_candidates"),
    "duplicate_missing_span": ("other_subject", "failed", "invalid_candidates"),
    "ttl_fragment": ("retention", "failed", "invalid_candidates"),
    "ttl_complete": ("retention", "committed", None),
}
MISSING = "云杉工作坊的报名费为54元。"


def recorded():
    records, hashes = {}, {}
    for scenario in {case[0] for case in CASES.values()}:
        paths = sorted((ARCHIVE / scenario / "trace").glob("*.jsonl"))
        assert paths
        events = [json.loads(line) for path in paths for line in path.read_text().splitlines()]
        starts = [e["data"]["request"] for e in events if e["kind"] == "model_start"]
        ends = [e["data"]["response"] for e in events if e["kind"] == "model_end"]
        assert len(starts) == len(ends) == (1 if scenario == "retention" else 2)
        records[scenario] = dict(payload=json.loads(starts[0]["input"]),
            extraction=json.loads(process.output_block(ends[0])["text"]),
            review=json.loads(process.output_block(ends[1])["text"]) if len(ends) == 2 else None)
        hashes.update({str(path.relative_to(grounding.ROOT)): hashlib.sha256(path.read_bytes()).hexdigest() for path in paths})
    assert records["iso_date"]["review"]["reviews"][0]["checks"]["temporal"] == "insufficient"
    assert len(records["other_subject"]["extraction"]["candidates"]) == 1
    assert records["retention"]["extraction"]["candidates"][0]["value"]["text"] == "赤砂工作坊的报名费为68元"
    return records, hashes


class Fixture(grounding.Fixture):
    bodies = []
    sent = []
    selected = None
    record = None
    candidates = None

    def do_POST(self):
        try:
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert self.path == "/v1/responses" and self.headers["Authorization"] == "Bearer local-fixture-key"
            type(self).bodies.append(request)
            name = request["text"]["format"]["name"]
            payload = json.loads(request["input"])
            assert payload["current_user_turn"] == self.record["payload"]["current_user_turn"]
            assert payload["conversation_history"] == []
            if name == grounding.EXTRACTION:
                tomorrow = (date.fromisoformat(payload["current_date"]) + timedelta(days=1)).isoformat()
                for candidate in self.candidates:
                    candidate.update(temporal_date=None, temporal_end_date=None, expiry_date=None)
                    if candidate.get("temporal_expression") in ("明天", "2026-10-02"):
                        candidate["temporal_date"] = tomorrow if candidate["temporal_expression"] == "明天" else "2026-10-02"
                    if candidate.get("expiry_expression") == "明天":
                        candidate.update(expiry_date=tomorrow, explicit_memory_request=True)
                output = dict(omission_reason=None, candidates=self.candidates)
            else:
                assert name == grounding.REVIEW and not grounding.active(self.db)
                assert len(payload["frozen_candidates"]) == len(self.candidates)
                output = grounding.source_support_review(request)
                output["coverage"] = dict(status="complete", missing_user_spans=[])
                if self.selected == "iso_insufficient":
                    output["reviews"][0]["checks"] = copy.deepcopy(self.record["review"]["reviews"][0]["checks"])
                    assert {check for check, verdict in output["reviews"][0]["checks"].items() if verdict != "supported"} == {"temporal"}
                    output["reviews"][0]["issues"] = [dict(check="temporal", field="temporal",
                        reason="Synthetic diagnostic: the reviewer cannot establish the candidate's asserted validity from this USER statement.",
                        evidence=[dict(turn_index=0, quote=payload["current_user_turn"])])]
                if self.selected == "missing_coverage":
                    output.pop("coverage")
                elif self.selected == "uncertain_coverage":
                    output["coverage"] = dict(status="uncertain", missing_user_spans=[])
                elif self.selected in ("multi_incomplete", "fabricated_span", "duplicate_missing_span"):
                    spans = ["云杉工作坊的报名费为99元。"] if self.selected == "fabricated_span" else [MISSING]
                    if self.selected == "duplicate_missing_span":
                        spans *= 2
                    output["coverage"] = dict(status="incomplete", missing_user_spans=spans)
                elif self.selected == "complete_with_missing":
                    output["coverage"]["missing_user_spans"] = [MISSING]
            type(self).sent.append(output)
            self.respond(200, process.Fixture.completed(output))
        except Exception as error:
            type(self).errors.append(str(error))
            self.respond(500, {"error": {"message": "fixture assertion failed"}})


def run_case(binary, directory, env, mode, name, records):
    scenario, expected_status, expected_error = CASES[name]
    record = records[scenario]
    text = record["payload"]["current_user_turn"]
    candidates = copy.deepcopy(record["extraction"]["candidates"])
    if name == "multi_complete":
        candidates.append(grounding.candidate("event_yunshan", MISSING, predicate="fact"))
    if name == "ttl_complete":
        candidates[0]["value"]["text"] = text
    expected_calls = 1 if name == "ttl_fragment" else 2
    capture = mode != "legacy"
    work = directory / (mode + "-" + name)
    work.mkdir()
    (work / "trace").mkdir(mode=0o700)
    db = work / "state.sqlite3"
    env = dict(env, PMR_TRACE_DIR=str(work / "trace"))
    Fixture.selected, Fixture.record, Fixture.candidates, Fixture.db = name, record, candidates, db
    before, errors_before = len(Fixture.bodies), len(Fixture.errors)
    config = grounding.settings(work, "config", retrieval_mode="local", fact_index_enabled=True,
        enrichment_mode={"explicit": "off", "legacy": "inline"}.get(mode, mode),
        source_capture_enabled=capture, utc_offset_seconds=28800, memory_types=[], value_kinds=[],
        predicates=[{"predicate": "fact", "cardinality": "Multiple"}],
        model_budget=dict(group_id=mode + "-" + name, max_calls=2, max_operation_calls=2))
    failures = []
    def check(condition, message):
        if not condition:
            failures.append(message)
    with contextlib.closing(grounding.Client(binary, db, env, grounding.OWNER, config=config)) as client:
        client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
        for entity in record["payload"]["known_entities"]:
            if entity["id"] != "self":
                client.call("memory_entity_upsert", dict(id=entity["id"], canonical_name=entity["canonical_name"],
                    aliases=entity["aliases"], entity_type="Other"))
        turn = dict(text=text, history=[], session_id="source-completeness", turn_id=name, capture_source=capture)
        if mode == "inline":
            prepared = grounding.tool(client, "memory_prepare", {"user_turn": turn})
            receipt = prepared["remember"].get("result", prepared["remember"])
            check(prepared["recall"]["status"] == "completed", "local recall failed")
        else:
            receipt = grounding.tool(client, "memory_remember", turn)
        if mode in ("deferred", "explicit"):
            check(not grounding.active(db) and len(Fixture.bodies) == before, "capture performed model work")
            batch = ["--drain-enrichment", "--max-jobs", "1"] if mode == "deferred" else ["--enrich-source", receipt["source"]["source_id"]]
            completed = subprocess.run([str(binary), "--db", str(db), "--owner", grounding.OWNER,
                "--config", str(config), *batch], env=env, text=True, capture_output=True, timeout=30)
            check((completed.returncode == 0) == (mode == "deferred" or expected_status != "failed"), "unexpected batch exit")
            if mode == "deferred":
                check(json.loads(completed.stdout)["attempted"] == 1, "deferred batch must attempt one source")
            receipt = grounding.tool(client, "memory_remember", turn)
            check(receipt["replayed"], "batch replay flag missing")
        state = grounding.snapshot(db)
        memories = grounding.active(db)
        accepted = expected_status == "committed"
        check(len(memories) == (len(candidates) if accepted else 0), "wrong Active count")
        check(not state["staging"]["pending"], "unexpected pending candidates")
        if not accepted:
            check(not state["repository"]["memories"], "rejected source created facts")
        else:
            check({(m["subject_id"], m["value"]["Text"]) for m in memories}
                == {(c["subject_id"], c["value"]["text"]) for c in candidates}, "committed assertion set differs")
            check(all(m["evidence"][0]["quote"] == text for m in memories), "original evidence changed")
        if capture:
            source, = state["sources"].values()
            check(source["request"]["text"] == text, "original source changed")
            check(receipt["source"]["status"] == "stored" and receipt["index"]["status"] == "ready", "source/index not preserved")
            check(receipt["enrichment"] == dict(status=expected_status, error=expected_error), "wrong enrichment classification: " + str(receipt["enrichment"]))
            after = len(Fixture.bodies)
            replay = grounding.tool(client, "memory_remember", turn)
            check(replay["replayed"] and len(Fixture.bodies) == after, "receipt replay sent model requests")
        else:
            check(not state["sources"], "legacy unexpectedly captured source")
        requests = Fixture.bodies[before:]
        check(not Fixture.errors[errors_before:], "fixture error: " + str(Fixture.errors[errors_before:]))
        check([r["text"]["format"]["name"] for r in requests]
            == [grounding.EXTRACTION] + ([grounding.REVIEW] if expected_calls == 2 else []), "unexpected model request sequence")
        with sqlite3.connect(db) as connection:
            ledger = connection.execute("SELECT operation_id, stage, outcome FROM model_calls ORDER BY rowid").fetchall()
            check(len(ledger) == expected_calls and len({r[0] for r in ledger}) == 1, "wrong physical ledger count or operation")
            check([r[1] for r in ledger] == ["extraction"] + (["extraction_review"] if expected_calls == 2 else []), "wrong ledger stages")
            check(all(r[2] == "completed" for r in ledger), "fixture provider failure")
            if capture:
                check((text,) in connection.execute("SELECT text FROM source_segments").fetchall(), "source index text missing")
        payload = json.loads(requests[0]["input"])
        if len(requests) == 2:
            reviewed = json.loads(requests[1]["input"])
            check(reviewed["now"] == payload["now"], "review changed source clock")
            schema = requests[1]["text"]["format"]["schema"]
            check("coverage" in schema.get("required", []), "review schema lacks required coverage")
            check(reviewed.get("utc_offset_seconds") == 28800, "review context lacks exact UTC offset")
            temporal_context = reviewed.get("temporal_context", {})
            for key, value in dict(utc_offset_seconds=28800,
                    validity_interval="start_inclusive_end_exclusive", retention_deadline="end_exclusive",
                    expression_authority="untrusted_extractor_proposal_check_against_USER",
                    boundary_origin="model_calendar_dates_verified_and_converted_by_backend").items():
                check(temporal_context.get(key) == value, "wrong temporal context: " + key)
            views = temporal_context.get("candidates", [])
            check(len(views) == len(candidates), "temporal views do not cover frozen candidates")
            for index, (raw, frozen, view) in enumerate(zip(candidates, reviewed["frozen_candidates"], views)):
                check(set(view) == {"candidate_index", "proposed_temporal_expression", "proposed_expiry_expression",
                    "proposed_temporal_date", "proposed_temporal_end_date", "proposed_expiry_date",
                    "observed_at", "valid_from", "valid_to", "expires_at", "reference"}, "temporal view fields differ")
                check(view.get("candidate_index") == index, "temporal view candidate index mismatch")
                check(view.get("proposed_temporal_expression") == raw.get("temporal_expression")
                    and view.get("proposed_expiry_expression") == raw.get("expiry_expression"), "extractor expressions changed in review")
                for field in ("temporal_date", "temporal_end_date", "expiry_date"):
                    check(view.get("proposed_" + field) == raw.get(field), "extractor model date changed in review: " + field)
                if raw.get("expiry_expression") is not None:
                    check(frozen["explicit_memory_request"] is True, "explicit retention flag lost before source review")
                check(view.get("reference") == frozen["temporal"]["reference"], "temporal reference lost in review")
                for field in ("observed_at", "valid_from", "valid_to", "expires_at"):
                    stamp = frozen["temporal"][field]
                    if isinstance(stamp, dict):
                        stamp = stamp["at"]
                    expected_view = None if stamp is None else dict(unix_seconds=stamp, kind="calendar",
                        local=datetime.fromtimestamp(stamp, timezone(timedelta(hours=8))).isoformat(timespec="seconds"))
                    check(view.get(field) == expected_view, "incorrect local time view: " + field)
        if capture:
            check(source["observed_at"] == payload["now"], "source/model observation clock differs")
        tomorrow = (date.fromisoformat(payload["current_date"]) + timedelta(days=1)).isoformat()
        for memory in memories if accepted else []:
            expected = dict(observed_at=payload["now"], valid_from=None, valid_to=None, expires_at=None, reference=None)
            if name == "iso_supported" or name == "multi_complete" and memory["subject_id"] == "event_shuangye":
                start = day_start("2026-10-02" if name == "iso_supported" else tomorrow)
                origin = "Explicit" if name == "iso_supported" else "Inferred"
                expected.update(valid_from=dict(at=start, source=origin), valid_to=dict(at=start+86400, source=origin))
            if name == "ttl_complete":
                expected["expires_at"] = day_start(tomorrow) + 86400
            check(memory["temporal"] == expected, "incorrect persisted temporal or retention fields")
    return dict(mode=mode, case=name, passed=not failures, failures=failures,
        facts=len(memories), expected_enrichment=expected_status, expected_calls=expected_calls,
        loopback_requests=len(requests), persisted_temporal=[m["temporal"] for m in memories])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=grounding.ROOT / "target/debug/pmr-mcp")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--modes", nargs="+", choices=("inline", "deferred", "explicit", "legacy"), default=["inline", "deferred", "explicit", "legacy"])
    parser.add_argument("--cases", nargs="+", choices=list(CASES), default=list(CASES))
    args = parser.parse_args()
    binary = args.binary.resolve()
    records, hashes = recorded()
    Fixture.bodies, Fixture.sent, Fixture.errors = [], [], []
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-source-completeness-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        clean = {key: value for key, value in os.environ.items() if not key.startswith(("DEEPSEEK_", "OPENAI_", "ARK_", "PMR_"))}
        env = dict(clean, PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
            DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1", NO_PROXY="127.0.0.1,localhost")
        rows = [run_case(binary, directory, env, mode, name, records) for mode in args.modes for name in args.cases]
    expected_calls = len(args.modes) * sum(1 if name == "ttl_fragment" else 2 for name in args.cases)
    assert len(Fixture.bodies) == expected_calls, "loopback request budget changed"
    report = dict(passed=all(r["passed"] for r in rows), cases=len(rows), real_provider_requests=0,
        loopback_requests=len(Fixture.bodies), binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
        archive_trace_sha256=hashes, results=rows,
        replay_note="Archived T02/T04/T06 supply extraction and source examples, copied without editing historical evidence. Synthetic extractions add current-clock model calendar dates and explicit retention flags; reviews add coverage/issues and bind to the current candidate snapshot. T02 insufficient preserves its recorded semantic checks with an explicitly synthetic diagnostic, not an inferred provider explanation. T04 complete adds the omitted fact; T06 complete restores the original complete value and keeps expiry_expression. This is protocol/process regression, not real-model semantic acceptance.")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({k: report[k] for k in ("passed", "cases", "real_provider_requests", "loopback_requests")}))
    for row in rows:
        if not row["passed"]:
            print(row["mode"], row["case"], row["failures"])
    assert report["passed"], "source completeness process regression failed"


if __name__ == "__main__":
    main()
