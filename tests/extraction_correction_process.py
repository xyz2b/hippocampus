#!/usr/bin/env python3
"""One complete extraction correction through real MCP; synthetic loopback only."""
import argparse
import contextlib
import copy
import hashlib
import http.server
import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import threading
import traceback
from pathlib import Path

import source_review_cardinality_process as process


grounding = process.grounding
TRACE = grounding.ROOT / "docs/refactor/s5/review-source-live-20260928/specialist/undated/trace/trace-43558-1790567394823246000-1.jsonl"
TRACE_SHA = "82393c175d24c9b10a82ba8247961eeb576c92d5df15bf67361f6331f626565c"
RAW_SHA = "a1b2bef3312b6d45bf542ab5cd1ae4535fab8198fc6f6169da9cfde14e7ce844"
TEXT = "海盐折纸工作坊的报名费为36元。"
STAGES = ["extraction", "extraction_repair", "extraction_repair_review"]
OTHER = "云杉工作坊的报名费为54元。"
# Physical requests, committed facts, enrichment status, public error.
CASES = {
    "correct": (3, 1, "committed", None),
    "correction_rejected": (2, 0, "failed", "invalid_candidates"),
    "negative_review": (3, 0, "completed_rejected", None),
    "budget_one": (1, 0, "failed", "model_budget_denied"),
    "budget_two": (2, 0, "failed", "model_budget_denied"),
    "coverage_only": (1, 0, "failed", "invalid_candidates"),
    "extraction_then_coverage": (3, 0, "failed", "source_coverage_incomplete"),
    "coverage_then_invalid": (3, 0, "failed", "invalid_candidates"),
    "malformed_extraction": (1, 0, "failed", "invalid_candidates"),
    "malformed_review": (3, 0, "failed", "invalid_candidates"),
    "flag_off": (1, 0, "failed", "invalid_candidates"),
    "review_negative_correct": (4, 2, "committed", None),
    "review_insufficient": (2, 0, "completed_rejected", None),
    "review_negative_again": (4, 0, "completed_rejected", None),
    "review_flag_off": (2, 0, "completed_rejected", None),
    "review_coverage_only": (2, 0, "completed_rejected", None),
    "normal": (2, 1, "committed", None),
}


def recorded():
    assert hashlib.sha256(TRACE.read_bytes()).hexdigest() == TRACE_SHA
    events = [json.loads(line) for line in TRACE.read_text().splitlines()]
    envelope, = [event["data"]["response"] for event in events if event["kind"] == "model_end"]
    assert envelope["status"] == "completed"
    raw = process.output_block(envelope)["text"]
    assert hashlib.sha256(raw.encode()).hexdigest() == RAW_SHA
    candidate, = json.loads(raw)["candidates"]
    assert candidate["memory_request"] == dict(kind="retain_until", expression="海", date="2026-09-28")
    assert candidate["value"]["text"] == TEXT
    return envelope


class Fixture(grounding.Fixture):
    bodies, sent, errors = [], [], []
    original, db = None, None
    selected, text, start = None, None, 0

    def do_POST(self):
        try:
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert self.path == "/v1/responses" and self.headers["Authorization"] == "Bearer local-fixture-key"
            type(self).bodies.append(request)
            number = len(self.bodies) - self.start
            assert number <= CASES[self.selected][0], "more than the one authorized correction/budget"
            payload = json.loads(request["input"])
            assert payload["current_user_turn"] == self.text and payload["conversation_history"] == []
            assert not grounding.active(self.db), "fact became active before independent review"
            name = request["text"]["format"]["name"]
            coverage_first = self.selected == "coverage_then_invalid"
            review_first = self.selected.startswith("review_")
            if review_first and name == grounding.EXTRACTION:
                output = json.loads(process.output_block(self.original)["text"])
                first = output["candidates"][0]
                first.update(memory_request=dict(kind="not_requested"), predicate="registration_fee",
                    value=dict(kind="integer", integer=63 if number==1 or self.selected=="review_negative_again" else 36,
                        text=None, boolean=None, entity_id=None))
                other = copy.deepcopy(first)
                other.update(subject=dict(entity_id="event_yunshan"), predicate="fact",
                    value=dict(kind="text", text=OTHER, integer=None, boolean=None, entity_id=None))
                output["candidates"].append(other)
                envelope = process.Fixture.completed(output)
            elif self.selected=="normal" and number==1:
                output=json.loads(process.output_block(self.original)["text"])
                output["candidates"][0]["memory_request"]=dict(kind="not_requested")
                envelope=process.Fixture.completed(output)
            elif number == 1 and self.selected == "malformed_extraction":
                envelope = process.Fixture.completed(dict(omission_reason=None, candidates="not-an-array"))
            elif number == 1 and not coverage_first or coverage_first and number == 3 or self.selected == "correction_rejected" and number == 2:
                assert name == grounding.EXTRACTION
                envelope = copy.deepcopy(self.original)  # Exact real failed extraction; no rebinding.
            elif number == 2 and not coverage_first and not review_first and self.selected!="normal" or number == 1 and coverage_first:
                assert name == grounding.EXTRACTION
                output = json.loads(process.output_block(self.original)["text"])
                output["candidates"][0]["memory_request"] = dict(kind="not_requested")
                if self.selected == "negative_review":
                    output["candidates"][0].update(predicate="registration_fee",
                        value=dict(kind="integer", integer=63, text=None, boolean=None, entity_id=None))
                envelope = process.Fixture.completed(output)
            else:
                assert name == grounding.REVIEW and len(payload["frozen_candidates"]) == (2 if review_first else 1)
                output = grounding.source_support_review(request)
                if self.selected in ("extraction_then_coverage", "coverage_then_invalid"):
                    output["coverage"] = dict(status="incomplete", missing_user_spans=[OTHER])
                elif self.selected == "negative_review" or review_first and (number==2 or self.selected=="review_negative_again"):
                    output["reviews"][0]["checks"]["value"] = "insufficient" if self.selected=="review_insufficient" else "contradicted"
                    output["reviews"][0]["issues"] = [dict(check="value", field="value.integer",
                        reason="原文36元，候选63元与原话冲突。", evidence=[dict(turn_index=0, quote=TEXT)])]
                elif self.selected == "malformed_review":
                    output["reviews"] = "not-an-array"
                envelope = process.Fixture.completed(output)
            type(self).sent.append(envelope)
            self.respond(200, envelope)
        except Exception as error:
            type(self).errors.append(traceback.format_exc())
            self.respond(500, {"error": {"message": "fixture assertion failed"}})


def run(binary, directory, env, mode, name, legacy_red_config, evidence_dir):
    calls, expected_facts, status, error = CASES[name]
    directory = directory / (mode+"-"+name)
    directory.mkdir()
    db = directory / "state.sqlite3"
    (directory / "trace").mkdir(mode=0o700)
    capture = mode != "legacy"
    text = TEXT+OTHER if name in ("extraction_then_coverage", "coverage_then_invalid") or name.startswith("review_") else TEXT
    Fixture.db, Fixture.selected, Fixture.text, Fixture.start = db, name, text, len(Fixture.bodies)
    start, errors_start, sent_start = Fixture.start, len(Fixture.errors), len(Fixture.sent)
    budget = dict(group_id=mode+"-"+name, max_calls=1 if name=="budget_one" else 2 if name=="budget_two" else 4,
        max_operation_calls=4, allow_coverage_repair=name in ("coverage_only", "extraction_then_coverage", "coverage_then_invalid", "review_coverage_only") or legacy_red_config,
        extraction_output_tokens=16384, review_output_tokens=16384)
    if not legacy_red_config:
        budget["allow_extraction_repair"] = name not in ("coverage_only", "flag_off", "review_flag_off", "review_coverage_only")
    config = grounding.settings(directory, "config", retrieval_mode="local", fact_index_enabled=True,
        enrichment_mode={"explicit":"off", "legacy":"inline"}.get(mode,mode),
        source_capture_enabled=capture, memory_types=[], value_kinds=[],
        predicates=[dict(predicate="fact", cardinality="Multiple"), dict(predicate="registration_fee", cardinality="Single")],
        model_budget=budget)
    failures = []
    def check(condition, message):
        if not condition:
            failures.append(message)
    env = dict(env, PMR_TRACE_DIR=str(directory/"trace"))
    with contextlib.closing(grounding.Client(binary, db, env, grounding.OWNER, config=config)) as client:
        client.call("memory_recall", dict(query="全局回答要求", bootstrap=True))
        client.call("memory_entity_upsert", dict(id="event_haiyan", canonical_name="海盐折纸工作坊", aliases=[], entity_type="Other"))
        client.call("memory_entity_upsert", dict(id="event_yunshan", canonical_name="云杉工作坊", aliases=[], entity_type="Other"))
        turn = dict(text=text, history=[], session_id="extraction-correction", turn_id=name, capture_source=capture)
        if mode == "inline":
            wire = client.rpc("tools/call",dict(name="memory_prepare",arguments=dict(user_turn=turn)))
            result = wire.get("structuredContent") or json.loads(wire["content"][0]["text"])
            receipt = result["remember"].get("result", result["remember"])
            check(result["recall"]["status"] == "completed", "local recall failed")
        else:
            receipt = grounding.tool(client, "memory_remember", turn)
        if mode in ("deferred", "explicit"):
            check(len(Fixture.bodies)==start and not grounding.active(db), "capture sent model work")
            args = ["--drain-enrichment", "--max-jobs", "1"] if mode=="deferred" else ["--enrich-source", receipt["source"]["source_id"]]
            batch = subprocess.run([str(binary), "--db", str(db), "--owner", grounding.OWNER, "--config", str(config), *args],
                env=env, capture_output=True, text=True, timeout=30)
            check((batch.returncode==0)==(mode=="deferred" or status!="failed"), "unexpected batch exit: "+batch.stderr[:100])
            if mode=="deferred":
                check(json.loads(batch.stdout)["attempted"]==1, "deferred source not attempted once")
            receipt = grounding.tool(client,"memory_remember",turn)
            check(receipt["replayed"], "batch replay missing")
        state, facts = grounding.snapshot(db), grounding.active(db)
        if capture:
            check(len(state["sources"]) == 1 and next(iter(state["sources"].values()))["request"]["text"] == text, "source must stay exact and singular")
            check(receipt["source"]["status"] == "stored" and receipt["index"]["status"] == "ready", "source/index must survive correction")
            check(receipt["enrichment"] == dict(status=status, error=error), "incorrect terminal enrichment: "+str(receipt["enrichment"]))
        else:
            check(not state["sources"], "legacy captured raw source")
        check(len(facts) == expected_facts and not state["staging"]["pending"], "wrong reviewed facts/pending count")
        if not expected_facts:
            check(not state["repository"]["memories"], "failed correction persisted a candidate")
        requests, sent = Fixture.bodies[start:], Fixture.sent[sent_start:]
        check(len(requests) == calls, f"requests={len(requests)}, expected {calls}")
        if name not in ("malformed_extraction", "coverage_then_invalid", "normal") and not name.startswith("review_"):
            check(sent[0] == Fixture.original, "recorded first response was modified")
        check(not Fixture.errors[errors_start:], "fixture errors: "+str(Fixture.errors[errors_start:]))
        first = json.loads(requests[0]["input"])
        for request in requests[1:]:
            payload = json.loads(request["input"])
            check(all(payload[key] == first[key] for key in ("owner_id", "now", "current_date", "utc_offset_seconds", "session_id", "turn_id", "current_user_turn", "conversation_history")), "correction changed original USER clock or input")
            if request["text"]["format"]["name"] == grounding.REVIEW:
                check("extraction_correction" not in payload and "coverage_correction" not in payload, "independent reviewer received correction hints")
            elif name=="coverage_then_invalid":
                check(payload.get("coverage_correction",{}).get("missing_user_spans")==[OTHER], "coverage correction lost source span")
            else:
                feedback=payload.get("extraction_correction",{})
                issues=feedback.get("issues",[])
                check(feedback.get("authority")=="untrusted_validation_feedback_recheck_against_entire_USER_turn", "correction feedback authority differs")
                check(len(issues)==1 and issues[0].get("candidate_index")==0 and bool(issues[0].get("field")) and bool(issues[0].get("reason")), "correction missing located candidate diagnostic")
        expected_values = {"event_haiyan":{"Integer":36},"event_yunshan":{"Text":OTHER}} if name=="review_negative_correct" else {"event_haiyan":{"Text":TEXT}}
        for fact in facts:
            check(fact["value"] == expected_values.get(fact["subject_id"]), "corrected fact differs")
            check(fact["temporal"] == dict(observed_at=first["now"], valid_from=None, valid_to=None, expires_at=None, reference=None), "unsupported deadline survived")
            check(not fact["explicit_memory_request"] and fact["evidence"][0]["quote"] == text, "corrected intent/evidence differs")
        if capture:
            before = len(Fixture.bodies)
            check(grounding.tool(client, "memory_remember", turn)["replayed"] and len(Fixture.bodies)==before, "receipt replay sent another request")
        with sqlite3.connect(db) as connection:
            ledger = connection.execute("SELECT operation_id,stage,outcome FROM model_calls ORDER BY rowid").fetchall()
            expected_stages = ["extraction","extraction_review","extraction_repair","extraction_repair_review"][:calls] if name in ("coverage_then_invalid","normal") or name.startswith("review_") else STAGES[:calls]
            check(len({r[0] for r in ledger}) == 1 and [r[1] for r in ledger] == expected_stages, "correction changed operation or stages")
            check(all(r[2] == "completed" for r in ledger), "physical model call failed")
            if capture:
                check((text,) in connection.execute("SELECT text FROM source_segments").fetchall(), "original index missing")
    events = [json.loads(line) for p in (directory/"trace").glob("*.jsonl") for line in p.read_text().splitlines()]
    saved = None
    if evidence_dir is not None and mode=="inline" and name in ("correct", "review_negative_correct", "correction_rejected"):
        # The MCP process has closed; preserve the final DB, original trace and exact
        # response envelope without reopening a copied database.
        with sqlite3.connect(db) as connection:
            connection.row_factory = sqlite3.Row
            accounting = {table:[dict(row) for row in connection.execute("SELECT * FROM "+table+" ORDER BY rowid")]
                for table in ("model_calls", "model_budget_groups")}
        for filename,value in (("mcp-response.json",wire),("result.json",result),("receipt.json",receipt),
                ("profile.json",state),("accounting.json",accounting)):
            (directory/filename).write_text(json.dumps(value,ensure_ascii=False,indent=2)+"\n")
        saved = evidence_dir/(mode+"-"+name)
        saved.parent.mkdir(parents=True,exist_ok=True)
        shutil.copytree(directory,saved)
        for path in [saved,*saved.rglob("*")]:
            path.chmod(0o700 if path.is_dir() else 0o600)
    return dict(mode=mode,case=name,passed=not failures, failures=failures, sources=len(state["sources"]), facts=len(facts),
        pending=len(state["staging"]["pending"]), enrichment=receipt.get("enrichment"),
        loopback_requests=len(requests), ledger_stages=[r[1] for r in ledger],
        exact_archived_response_positions=[index+1 for index,envelope in enumerate(sent) if envelope==Fixture.original],
        saved_evidence=str(saved) if saved else None,
        validation_failures=[e["data"] for e in events if e["kind"]=="validation" and e["data"]["status"]=="failed"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cases", nargs="+", choices=list(CASES), default=[name for name in CASES if name!="normal"])
    parser.add_argument("--modes", nargs="+", choices=("inline","deferred","explicit","legacy"), default=["inline","deferred","explicit","legacy"])
    parser.add_argument("--legacy-red-config", action="store_true", help="Only for old frozen red reproduction; omit the previously unsupported extraction-repair flag.")
    parser.add_argument("--evidence-dir",type=Path,help="Save three selected inline case directories after closing the MCP process.")
    args = parser.parse_args()
    binary = args.binary.resolve()
    before = hashlib.sha256(binary.read_bytes()).hexdigest()
    Fixture.original = recorded()
    Fixture.bodies, Fixture.sent, Fixture.errors = [], [], []
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-extraction-correction-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1",0),Fixture)
        threading.Thread(target=server.serve_forever,daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        env = {k:os.environ[k] for k in ("PATH","HOME","TMPDIR","LANG") if k in os.environ}
        env.update(PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
            DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1", NO_PROXY="127.0.0.1,localhost")
        results = []
        for mode in args.modes:
            for name in args.cases:
                start = len(Fixture.bodies)
                try:
                    results.append(run(binary,directory,env,mode,name,args.legacy_red_config,args.evidence_dir))
                except (AssertionError,KeyError,IndexError,TypeError,ValueError,sqlite3.Error) as error:
                    results.append(dict(mode=mode,case=name,passed=False,failures=[str(error)],
                        failure_location=traceback.format_exc(),loopback_requests=len(Fixture.bodies)-start))
    unchanged = hashlib.sha256(binary.read_bytes()).hexdigest() == before
    assert hashlib.sha256(TRACE.read_bytes()).hexdigest() == TRACE_SHA
    report = dict(passed=all(r["passed"] for r in results) and unchanged, cases=len(results), binary_sha256=before, binary_unchanged=unchanged,
        archived_trace_sha256=TRACE_SHA, archived_output_text_sha256=RAW_SHA, real_provider_requests=0,
        results=results, loopback_requests=len(Fixture.bodies), legacy_red_config=args.legacy_red_config,
        limits="The archived failed extraction envelope is replayed unchanged at each result's exact_archived_response_positions. Other candidate batches and reviews are synthetic protocol fixtures, including genuine 63-versus-36 counterevidence. No archived evidence is edited; this does not measure provider correction quality.")
    args.output.parent.mkdir(parents=True,exist_ok=True)
    args.output.write_text(json.dumps(report,ensure_ascii=False,indent=2)+"\n")
    print(json.dumps({k:report[k] for k in ("passed","cases","loopback_requests","real_provider_requests","binary_sha256")}))
    for row in results:
        if not row["passed"]:
            print(row["mode"],row["case"],row["failures"])
    assert report["passed"], "one complete extraction correction is required"


if __name__ == "__main__":
    main()
