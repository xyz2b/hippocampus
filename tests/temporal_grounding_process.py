#!/usr/bin/env python3
"""Model time expressions and numeric rejection through the four-entry MCP harness."""
import argparse
import contextlib
import copy
from datetime import date, datetime, time, timedelta, timezone
import hashlib
import http.server
import io
import json
import os
import sqlite3
import tempfile
import threading
from pathlib import Path

import source_review_cardinality_process as process


TRACE = process.grounding.ROOT / "docs/refactor/s5/review-cardinality-live-20260927/evidence/event_registration_fee/trace"
CASE = "archived_unsupported_valid_from"
T02 = "archived_wrong_iso_timestamp"
T02_TRACE = process.grounding.ROOT / "docs/refactor/s5/temporal-grounding-live-20260927/specialist/iso_date/trace"
FACT = "海盐折纸工作坊的报名费为36元。"
BASE_CANDIDATE = process.grounding.candidate
CASES = {
    CASE: (FACT, False),
    T02: ("2026-10-02银杏工作坊的报名费为58元。", False),
    "recorded_iso_expression": ("2026-10-02银杏工作坊的报名费为58元。", True),
    "undated_fact": (FACT, True),
    "iso_day_fact": ("2026-10-02" + FACT, True),
    "current_fact": ("现在" + FACT, True),
    "tomorrow_fact": ("明天" + FACT, True),
    "explicit_retention": ("海盐折纸工作坊的报名费为36元，保留到明天。", True),
    "dated_short_constraint": ("明天我不要咖啡。", True),
    "invented_valid_to": (FACT, False),
    "invented_expiry": (FACT, False),
    "correct_numeric_date": ("2026-10-02" + FACT, False),
    "single_start_expression": ("从明天起" + FACT, False),
    "single_end_expression": ("截至2026-10-02，" + FACT, False),
    "range_expression": ("从2026-10-02到2026-10-05，" + FACT, False),
    "other_subject_date": (FACT + "明天银杏折纸工作坊的报名费为58元。", False),
    "quoted_date": (FACT + "“明天海盐折纸工作坊的报名费为36元。”", False),
    "negated_date": ("海盐折纸工作坊的报名费为36元，不是明天生效。", False),
    "other_property_date": ("海盐折纸工作坊的报名费为36元，明天公布举办地点。", False),
}
NUMERIC_CASES = {CASE, T02, "invented_valid_to", "invented_expiry", "correct_numeric_date"}


def day_start(iso):
    return int(datetime.combine(date.fromisoformat(iso), time.min, timezone(timedelta(hours=8))).timestamp())


def candidate(*_args, **_kwargs):
    name, payload = Fixture.selected, Fixture.payload
    raw = BASE_CANDIDATE("event_haiyan_origami", CASES[name][0], predicate="fact")
    if name == "recorded_iso_expression":
        raw = json.loads(process.output_block(Fixture.t02_envelopes[0])["text"])["candidates"][0]
        raw.update(valid_from=None, valid_to=None, temporal_expression="2026-10-02")
    raw["expiry_expression"] = None
    if name in ("iso_day_fact", "current_fact", "tomorrow_fact"):
        raw["temporal_expression"] = {"iso_day_fact": "2026-10-02", "current_fact": "现在", "tomorrow_fact": "明天"}[name]
    if name == "explicit_retention":
        raw["expiry_expression"] = "明天"
    if name == "dated_short_constraint":
        raw.update(subject_id="self", memory_type="constraint", polarity="negative", retention_tier="short_term")
        raw["scope"]["conditions"]["date"] = (date.fromisoformat(payload["current_date"]) + timedelta(days=1)).isoformat()
    if name == "invented_valid_to":
        raw["valid_to"] = {"at": payload["now"] + 86400, "source": "inferred"}
    if name == "invented_expiry":
        raw["expires_at"] = payload["now"] + 86400
    if name == "correct_numeric_date":
        start = day_start("2026-10-02")
        raw.update(valid_from={"at": start, "source": "explicit"}, valid_to={"at": start + 86400, "source": "explicit"})
    if name in ("single_start_expression", "single_end_expression", "range_expression"):
        raw["temporal_expression"] = {"single_start_expression": "从明天起", "single_end_expression": "截至2026-10-02",
                                      "range_expression": "从2026-10-02到2026-10-05"}[name]
    if name in ("other_subject_date", "quoted_date", "negated_date", "other_property_date"):
        raw["temporal_expression"] = "明天"
        if name in ("other_subject_date", "quoted_date"):
            raw["value"]["text"] = FACT
    # Synthetic replies follow the model-date contract; exact archived envelopes
    # are returned separately and stay byte-for-byte unchanged.
    tomorrow = (date.fromisoformat(payload["current_date"]) + timedelta(days=1)).isoformat()
    raw.update(temporal_date=None, temporal_end_date=None, expiry_date=None)
    if raw.get("temporal_expression") in ("明天", "2026-10-02"):
        raw["temporal_date"] = tomorrow if raw["temporal_expression"] == "明天" else "2026-10-02"
    if raw.get("expiry_expression") == "明天":
        raw.update(expiry_date=tomorrow, explicit_memory_request=True)
    return raw


class Fixture(process.Fixture):
    selected = None
    payload = None
    archived_envelopes = None
    t02_envelopes = None
    source_subjects = None
    t02_subjects = None

    def do_POST(self):
        raw = self.rfile.read(int(self.headers["Content-Length"]))
        request = json.loads(raw)
        type(self).payload = json.loads(request["input"])
        if self.selected in (CASE, T02):
            type(self).envelopes = copy.deepcopy(self.archived_envelopes if self.selected == CASE else self.t02_envelopes)
        elif request["text"]["format"]["name"] == process.grounding.EXTRACTION:
            type(self).envelopes = [self.completed({"omission_reason": None, "candidates": [candidate()]}), {}]
        else:
            type(self).envelopes[1] = self.completed(process.grounding.source_support_review(request))
        self.rfile = io.BytesIO(raw)
        super().do_POST()


def recorded(trace=TRACE):
    paths = sorted(trace.glob("*.jsonl"))
    events = [json.loads(line) for path in paths for line in path.read_text().splitlines()]
    envelopes = [event["data"]["response"] for event in events if event["kind"] == "model_end"]
    starts = [event["data"]["request"] for event in events if event["kind"] == "model_start"]
    assert len(envelopes) == len(starts) == 2
    extracted = json.loads(process.output_block(envelopes[0])["text"])
    candidate, = extracted["candidates"]
    assert candidate["valid_from"] == ({"at": 1790514273, "source": "inferred"} if trace == TRACE else {"at": 1780272000, "source": "explicit"})
    assert candidate["temporal_expression"] is None
    payload = json.loads(starts[0]["input"])
    assert payload["current_user_turn"] == CASES[CASE if trace == TRACE else T02][0]
    review = json.loads(process.output_block(envelopes[1])["text"])
    assert len(review["reviews"]) == 1 and review["reviews"][0]["checks"]["temporal"] == ("supported" if trace == TRACE else "contradicted")
    return envelopes, payload, {str(path.relative_to(process.grounding.ROOT)): hashlib.sha256(path.read_bytes()).hexdigest() for path in paths}


def replay(binary, directory, env, mode, name):
    Fixture.selected = name
    process.SUBJECTS = Fixture.t02_subjects if name in (T02, "recorded_iso_expression") else Fixture.source_subjects
    text, accepted = CASES[name]
    process.TEXTS = [text]
    harness_case = "one" if accepted else name if name.startswith("archived_") else "archived_" + name
    process.COUNTS[harness_case] = 1
    work = directory / name
    work.mkdir(exist_ok=True)
    before = len(process.Fixture.bodies)
    failures = []
    try:
        result = process.run_case(binary, work, env, mode, harness_case, extraction_rejected=name in NUMERIC_CASES)
        failures.extend(result["contract_failures"])
    except AssertionError as error:
        failures.append(str(error) or "intake result or call contract differs from expectation")
    db = work / f"{mode}-{harness_case}" / "state.sqlite3"
    state = process.grounding.snapshot(db)
    memories = list(state["repository"]["memories"].values())
    if memories and not accepted:
        failures.append("candidate forbidden by the time-output contract was committed; expected zero facts")
    if accepted and len(memories) != 1:
        failures.append("supported temporal candidate was not committed")
    sources = list(state["sources"].values())
    capture = mode != "legacy"
    assert len(sources) == int(capture)
    if capture:
        assert sources[0]["request"]["text"] == process.TEXTS[0]
        assert sources[0]["index"]["status"] == "ready"
        with sqlite3.connect(db) as connection:
            assert (process.TEXTS[0],) in connection.execute("SELECT text FROM source_segments").fetchall()
    requests = process.Fixture.bodies[before:]
    sent = process.Fixture.sent[before:]
    expected_count = 1 if name in NUMERIC_CASES else 2
    if len(requests) != expected_count or len(sent) != expected_count:
        failures.append(f"expected {expected_count} request(s), observed {len(requests)}")
    assert not process.Fixture.errors, process.Fixture.errors
    extraction_now = json.loads(requests[0]["input"])["now"]
    review_now = json.loads(requests[1]["input"])["now"] if len(requests) > 1 else None
    if review_now is not None:
        assert extraction_now == review_now, "review changed the extraction clock"
    if capture:
        assert sources[0]["observed_at"] == extraction_now, "model used a clock different from source capture"
    if name in (CASE, T02):
        archive = Fixture.archived_envelopes if name == CASE else Fixture.t02_envelopes
        assert sent[0] == archive[0], "recorded extraction envelope changed"
        if len(sent) > 1:
            actual = json.loads(process.output_block(sent[1])["text"])
            original = json.loads(process.output_block(archive[1])["text"])
            original["review_binding"] = actual["review_binding"]
            assert actual == original, "review replay changed more than binding"
    if accepted and memories:
        payload = json.loads(requests[0]["input"])
        now = payload["now"]
        tomorrow = (date.fromisoformat(payload["current_date"]) + timedelta(days=1)).isoformat()
        expected = dict(observed_at=now, valid_from=None, valid_to=None, expires_at=None, reference=None)
        if name in ("iso_day_fact", "recorded_iso_expression", "tomorrow_fact", "current_fact"):
            start = now if name == "current_fact" else day_start("2026-10-02" if name in ("iso_day_fact", "recorded_iso_expression") else tomorrow)
            end = 2**63 - 1 if name == "current_fact" else start + 86400
            source = "Explicit" if name in ("iso_day_fact", "recorded_iso_expression") else "Inferred"
            expected.update(valid_from={"at": start, "source": source}, valid_to={"at": end, "source": source})
        if name in ("explicit_retention", "dated_short_constraint"):
            expected["expires_at"] = day_start(tomorrow) + 86400
        if memories[0]["temporal"] != expected:
            failures.append("persisted temporal fields differ from local calendar resolution")
        if name == "dated_short_constraint":
            assert memories[0]["memory_type"] == "Constraint" and memories[0]["retention_tier"] == "ShortTerm"
            assert memories[0]["scope"]["conditions"] == [{"Time": {"Date": tomorrow}}]
    return dict(mode=mode, case=name, passed=not failures, failures=failures,
                source_preserved=capture, index_preserved=capture, facts=len(memories),
                persisted_temporal=[memory["temporal"] for memory in memories],
                request_stages=[request["text"]["format"]["name"] for request in requests],
                physical_loopback_requests=len(requests),
                source_observed_at=sources[0]["observed_at"] if sources else None,
                extraction_now=extraction_now, review_now=review_now,
                source_enrichment=sources[0]["enrichment"] if sources else None)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=process.grounding.ROOT / "target/debug/pmr-mcp")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cases", nargs="+", choices=list(CASES), default=list(CASES))
    args = parser.parse_args()
    binary = args.binary.resolve()
    envelopes, payload, hashes = recorded()
    t02_envelopes, t02_payload, t02_hashes = recorded(T02_TRACE)
    hashes.update(t02_hashes)
    source_entities = {item["id"]: item for item in payload["known_entities"] if item["id"] != "self"}
    t02_entities = {item["id"]: item for item in t02_payload["known_entities"] if item["id"] != "self"}
    entities = {**source_entities, **t02_entities}
    base_client = process.grounding.Client

    class Client(base_client):
        def call(self, name, arguments, error=False):
            if name == "memory_entity_upsert":
                arguments = dict(arguments, entity_type="Other", aliases=entities[arguments["id"]]["aliases"])
            return super().call(name, arguments, error)

    process.grounding.Client = Client
    Fixture.source_subjects = [(key, entity["canonical_name"]) for key, entity in source_entities.items()]
    Fixture.t02_subjects = [(key, entity["canonical_name"]) for key, entity in t02_entities.items()]
    process.grounding.candidate = candidate
    process.Fixture = Fixture
    Fixture.archived_envelopes = envelopes
    Fixture.t02_envelopes = t02_envelopes
    process.Fixture.envelopes = envelopes
    process.Fixture.bodies, process.Fixture.requests, process.Fixture.sent, process.Fixture.errors = [], [], [], []
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-temporal-grounding-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), process.Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        clean = {key: value for key, value in os.environ.items() if not key.startswith(("DEEPSEEK_", "OPENAI_", "ARK_", "PMR_"))}
        env = dict(clean, PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
                   DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1", NO_PROXY="127.0.0.1,localhost")
        rows = [replay(binary, directory, env, mode, name) for mode in ("inline", "deferred", "explicit", "legacy") for name in args.cases]
    report = dict(passed=all(row["passed"] for row in rows), cases=len(rows),
                  binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(), archive_trace_sha256=hashes,
                  replay_note="Both archived extraction envelopes stay unchanged and must be rejected before review. Synthetic replies include model calendar dates and explicit memory-retention flags under the current contract. recorded_iso_expression is a separate synthetic positive based on T02: it replaces numeric boundaries with the exact USER date expression and normalized date, and uses an all-supported synthetic review; it is not a real provider rerun. Single-boundary/range expressions remain unsupported and must not become a single day.",
                  real_provider_requests=0, loopback_requests=len(process.Fixture.requests), results=rows)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({key: report[key] for key in ("passed", "cases", "real_provider_requests", "loopback_requests")}, ensure_ascii=False))
    for row in rows:
        if not row["passed"]:
            print(row["mode"], row["case"], row["failures"])
    assert report["passed"], "time-output or admission contract failed"


if __name__ == "__main__":
    main()
