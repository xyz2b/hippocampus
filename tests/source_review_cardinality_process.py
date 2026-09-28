#!/usr/bin/env python3
"""Source-review cardinality through real MCP intake paths; loopback responses only."""
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

import intake_grounding_process as grounding


TRACE = grounding.ROOT / "docs/refactor/s5/relation-quantity-live-20260927/evidence/device_default_paper/trace"
SUBJECTS = [("device_wulan_printer", "雾岚打印机"), ("device_qingtai_printer", "青苔打印机")]
TEXTS = ["雾岚打印机的默认纸张规格是A4。", "青苔打印机的默认纸张规格是B5。"]
COUNTS = dict(empty=0, one=1, two=2, archived_duplicate=1, repeated_index=2, empty_denied=0, archived_exact=1)


def output_block(envelope):
    blocks = [part for output in envelope["output"] for part in output.get("content", [])
              if part.get("type") == "output_text"]
    assert len(blocks) == 1
    return blocks[0]


def archive():
    paths = sorted(TRACE.glob("*.jsonl"))
    events = [json.loads(line) for path in paths for line in path.read_text().splitlines()]
    envelopes = [event["data"]["response"] for event in events if event["kind"] == "model_end"]
    assert len(envelopes) == 2 and all(item["status"] == "completed" for item in envelopes)
    extract, review = [json.loads(output_block(item)["text"]) for item in envelopes]
    assert len(extract["candidates"]) == 1 and len(review["reviews"]) == 2
    assert review["reviews"][0] == review["reviews"][1] and review["reviews"][0]["candidate_index"] == 0
    return envelopes, {str(path.relative_to(grounding.ROOT)): hashlib.sha256(path.read_bytes()).hexdigest() for path in paths}


class Fixture(grounding.Fixture):
    bodies = []
    sent = []
    case = None
    count = 0
    text = None
    envelopes = None

    def do_POST(self):
        try:
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert self.path == "/v1/responses"
            assert self.headers["Authorization"] == "Bearer local-fixture-key"
            type(self).bodies.append(request)
            name = request["text"]["format"]["name"]
            type(self).requests.append(name)
            payload = json.loads(request["input"])
            assert payload["current_user_turn"] == self.text and payload["conversation_history"] == []
            recorded = self.case.startswith("archived_")
            if name == grounding.EXTRACTION:
                if recorded:
                    envelope = copy.deepcopy(self.envelopes[0])
                else:
                    candidates = [grounding.candidate(SUBJECTS[index][0], TEXTS[index], predicate="fact")
                                  for index in range(self.count)]
                    result = {"omission_reason": None if candidates else "no_personal_assertion", "candidates": candidates}
                    envelope = self.completed(result)
            else:
                assert name == grounding.REVIEW
                assert not grounding.active(self.db), "facts became Active before source review"
                assert len(payload["frozen_candidates"]) == self.count
                if recorded:
                    envelope = copy.deepcopy(self.envelopes[1])
                    if self.case != "archived_exact":
                        block = output_block(envelope)
                        result = json.loads(block["text"])
                        result["review_binding"] = payload["review_binding"]
                        block["text"] = json.dumps(result, ensure_ascii=False)
                else:
                    result = grounding.source_support_review(request)
                    if self.case == "repeated_index":
                        result["reviews"][1] = copy.deepcopy(result["reviews"][0])
                    if self.case == "empty_denied":
                        result["omission_supported"] = False
                    envelope = self.completed(result)
            type(self).sent.append(envelope)
            self.respond(200, envelope)
        except Exception as error:
            type(self).errors.append(str(error))
            self.respond(500, {"error": {"message": "fixture assertion failed"}})

    @staticmethod
    def completed(result):
        return {"status": "completed", "usage": {"input_tokens": 10, "output_tokens": 10},
                "output": [{"type": "message", "content": [{"type": "output_text", "text": json.dumps(result, ensure_ascii=False)}]}]}


def contract_failures(request, count):
    schema = request["text"]["format"]["schema"]
    reviews = schema["properties"]["reviews"]
    index = reviews["items"]["properties"]["candidate_index"]
    payload = json.loads(request["input"])
    checks = {
        "exact_review_length": reviews.get("minItems") == reviews.get("maxItems") == count,
        "index_bounds": index.get("minimum") == 0 and index.get("maximum") == max(0, count - 1),
        "expected_indices": payload.get("expected_candidate_indices") == list(range(count)),
        "expected_count": payload.get("expected_review_count") == count,
        "explicit_coverage_instructions": all(word in request["instructions"] for word in
                                               ("expected_candidate_indices", "expected_review_count", "exactly once")),
        "empty_denial_allowed": schema["properties"]["omission_supported"] == {"type": "boolean"},
    }
    return [name for name, passed in checks.items() if not passed]


def run_case(binary, directory, env, mode, name, *, extraction_rejected=False):
    count = COUNTS[name]
    rejected = name not in ("empty", "one", "two")
    expected_active = 0 if rejected else count
    assert not extraction_rejected or rejected
    expected_stages = ["extraction"] if extraction_rejected else ["extraction", "extraction_review"]
    expected_requests = [grounding.EXTRACTION] if extraction_rejected else [grounding.EXTRACTION, grounding.REVIEW]
    capture = mode != "legacy"
    work = directory / f"{mode}-{name}"
    work.mkdir()
    trace = work / "trace"
    trace.mkdir(mode=0o700)
    env = dict(env, PMR_TRACE_DIR=str(trace))
    db = work / "state.sqlite3"
    Fixture.case, Fixture.count, Fixture.text, Fixture.db = name, count, "".join(TEXTS[:count]) if count else "你好。", db
    before, errors_before = len(Fixture.bodies), len(Fixture.errors)
    config = grounding.settings(work, "config", retrieval_mode="local", fact_index_enabled=True,
        enrichment_mode={"explicit": "off", "legacy": "inline"}.get(mode, mode),
        source_capture_enabled=capture, memory_types=[], value_kinds=[],
        predicates=[{"predicate": "fact", "cardinality": "Multiple"}],
        model_budget=dict(group_id=f"{mode}-{name}", max_calls=2, max_operation_calls=2))
    with contextlib.closing(grounding.Client(binary, db, env, grounding.OWNER, config=config)) as client:
        client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
        for subject, canonical in SUBJECTS:
            client.call("memory_entity_upsert", {"id": subject, "canonical_name": canonical,
                                                "aliases": [], "entity_type": "Device"})
        turn = dict(text=Fixture.text, history=[], session_id="review-cardinality", turn_id=name, capture_source=capture)
        if mode == "inline":
            prepared = grounding.tool(client, "memory_prepare", {"user_turn": turn})
            receipt = prepared["remember"].get("result", prepared["remember"])
        else:
            receipt = grounding.tool(client, "memory_remember", turn)
        if mode in ("deferred", "explicit"):
            assert len(Fixture.bodies) == before and not grounding.active(db), "capture performed model work"
            batch = ["--drain-enrichment", "--max-jobs", "1"] if mode == "deferred" else ["--enrich-source", receipt["source"]["source_id"]]
            result = subprocess.run([str(binary), "--db", str(db), "--owner", grounding.OWNER,
                "--config", str(config), *batch], env=env, text=True, capture_output=True, timeout=30)
            assert (result.returncode == 0) == (mode == "deferred" or not rejected), result.stderr[:300]
            if mode == "deferred":
                assert json.loads(result.stdout)["attempted"] == 1
            receipt = grounding.tool(client, "memory_remember", turn)
            assert receipt["replayed"]
        state = grounding.snapshot(db)
        memories = grounding.active(db)
        assert len(memories) == expected_active, (mode, name, len(memories), expected_active)
        if rejected:
            assert not state["repository"]["memories"], "rejected review created facts"
        else:
            assert {memory["value"]["Text"] for memory in memories} == set(TEXTS[:count])
        if capture:
            assert len(state["sources"]) == 1 and next(iter(state["sources"].values()))["request"]["text"] == Fixture.text
            assert receipt["source"]["status"] == "stored" and receipt["index"]["status"] == "ready"
            expected_status = "failed" if rejected else "committed" if count else "completed_no_facts"
            assert receipt["enrichment"]["status"] == expected_status, (mode, name, receipt["enrichment"])
            if rejected:
                assert receipt["enrichment"]["error"] == "invalid_candidates", receipt["enrichment"]
            after = len(Fixture.bodies)
            replay = grounding.tool(client, "memory_remember", turn)
            assert replay["replayed"] and len(Fixture.bodies) == after, "receipt replay repeated model work"
        else:
            assert not state["sources"]
        with sqlite3.connect(db) as connection:
            stages = connection.execute("SELECT operation_id, stage FROM model_calls ORDER BY rowid").fetchall()
            assert len({row[0] for row in stages}) == 1
            assert [row[1] for row in stages] == expected_stages
            if capture:
                assert (Fixture.text,) in connection.execute("SELECT text FROM source_segments").fetchall()
        requests = Fixture.bodies[before:]
        assert [request["text"]["format"]["name"] for request in requests] == expected_requests
        assert not Fixture.errors[errors_before:], Fixture.errors[errors_before:]
        if name.startswith("archived_"):
            sent = Fixture.sent[before:]
            assert sent[0] == Fixture.envelopes[0], "archived extraction changed"
            if not extraction_rejected:
                actual = json.loads(output_block(sent[1])["text"])
                original = json.loads(output_block(Fixture.envelopes[1])["text"])
                assert actual["reviews"] == original["reviews"], "duplicate entries changed or deduplicated"
                if name == "archived_exact":
                    assert sent[1] == Fixture.envelopes[1]
                else:
                    original["review_binding"] = actual["review_binding"]
                    assert actual == original, "replay changed more than binding"
        failures = [] if extraction_rejected else contract_failures(requests[1], count)
    return dict(mode=mode, case=name, frozen_candidates=count, active=len(memories),
                physical_loopback_requests=len(requests), behavior_passed=True, request_contract_passed=not failures,
                passed=not failures, contract_failures=failures,
                archived_replay_binding="unchanged" if name == "archived_exact" else "rebound_only" if name == "archived_duplicate" else None)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=grounding.ROOT / "target/debug/pmr-mcp")
    parser.add_argument("--modes", nargs="+", choices=["inline", "deferred", "explicit", "legacy"], default=["inline", "deferred", "explicit", "legacy"])
    parser.add_argument("--cases", nargs="+", choices=list(COUNTS), default=list(COUNTS))
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    Fixture.envelopes, hashes = archive()
    Fixture.bodies, Fixture.requests, Fixture.sent, Fixture.errors = [], [], [], []
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-review-cardinality-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        clean = {key: value for key, value in os.environ.items() if not key.startswith(("DEEPSEEK_", "OPENAI_", "ARK_", "PMR_"))}
        env = dict(clean, PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
                   DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1", NO_PROXY="127.0.0.1,localhost")
        rows = [run_case(binary, directory, env, mode, name) for mode in args.modes for name in args.cases]
    report = dict(passed=all(row["passed"] for row in rows), cases=len(rows),
                  behavior_passed=sum(row["behavior_passed"] for row in rows),
                  loopback_requests=len(Fixture.requests), real_provider_requests=0,
                  binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(), archive_trace_sha256=hashes,
                  replay_note="Original envelopes stay archived. archived_exact is unchanged; archived_duplicate rebinds only review_binding to the new frozen candidate snapshot, retaining both identical index-0 reviews.", results=rows)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    for row in rows:
        print(f"{'PASS' if row['passed'] else 'FAIL'} {row['mode']}/{row['case']}: Active={row['active']}, "
              f"behavior=pass, contract_failures={row['contract_failures']}")
    print(json.dumps({key: report[key] for key in ("passed", "cases", "behavior_passed", "loopback_requests", "real_provider_requests")}))
    assert report["passed"], "source-review request contract regression failed"


if __name__ == "__main__":
    main()
