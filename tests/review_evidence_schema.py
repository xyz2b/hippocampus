#!/usr/bin/env python3
"""Offline constraints for source-review evidence, with an immutable real T01 control.

Uses the installed jsonschema interpreter; never calls a provider or changes archives.
Without --request this intentionally tests the old request against the new requirements
and fails, recording that an all-supported review with empty evidence was schema-valid.
"""
import argparse
import copy
import hashlib
import importlib.metadata
import json
from pathlib import Path

from subject_time_schema import digest, inspect, validator_for


ROOT = Path(__file__).resolve().parents[1]
TRACE = ROOT / "docs/refactor/s5/subject-time-contract-live-20260928/specialist/undated/trace/trace-36124-1790565144793020000-1.jsonl"
TRACE_SHA = "a5d17fc9dc6f8a2b18666400deb755c5d523dc16b8354ddd8c5445872a5f8b85"
RAW_SHA = "99c0535fcefc14d90d17d71cc697a673fcdb70bc2e766107c7d7a9758a558aa4"
SCHEMA_NAME = "personal_memory_source_support_v1"
SHORT_TRACE = ROOT / "docs/refactor/s5/review-evidence-live-20260928/specialist/undated/trace/trace-40046-1790566619144481000-1.jsonl"
SHORT_TRACE_SHA = "13f8f68551e678904b53c98421a723275cc65bfdb058d692b1d904777387d8f7"
SHORT_RAW_SHA = "bb7c363291f14fee513d34c0f00998167ccbb5a8c242ca88f2f7630b5f749c71"


def archived_review():
    assert digest(TRACE) == TRACE_SHA, "archived trace changed"
    events = [json.loads(line) for line in TRACE.read_text().splitlines()]
    requests = [e["data"]["request"] for e in events if e["kind"] == "model_start"]
    responses = [e["data"]["response"] for e in events if e["kind"] == "model_end"]
    assert len(requests) == len(responses) == 2
    request, response = requests[1], responses[1]
    assert request["text"]["format"]["name"] == SCHEMA_NAME
    assert response["status"] == "completed"
    raw, = [item["text"] for output in response["output"] for item in output.get("content", [])
        if item.get("type") == "output_text"]
    assert hashlib.sha256(raw.encode()).hexdigest() == RAW_SHA
    document = json.loads(raw)
    review, = document["reviews"]
    assert set(review["checks"].values()) == {"supported"}
    assert review["evidence"] == [] and review["issues"] == []
    return request, raw, document


def old_requirements(request, document):
    validator = validator_for(request["text"]["format"]["schema"])
    text = json.loads(request["input"])["current_user_turn"]
    rows = [inspect(validator, "actual_T01_approval_without_current_evidence", document, False)]
    for name, evidence, expected in (
        ("approval_blank_quote", [{"turn_index": 0, "quote": " \t\n"}], False),
        ("approval_history_only", [{"turn_index": 1, "quote": "合成历史原话。"}], False),
        ("approval_current_quote", [{"turn_index": 0, "quote": text}], True),
        ("approval_current_plus_history", [{"turn_index": 0, "quote": text},
            {"turn_index": 1, "quote": "合成历史原话。"}], True),
    ):
        changed = copy.deepcopy(document)
        changed["reviews"][0]["evidence"] = evidence
        rows.append(inspect(validator, name, changed, expected))
    return rows


def short_review():
    assert digest(SHORT_TRACE) == SHORT_TRACE_SHA
    events = [json.loads(line) for line in SHORT_TRACE.read_text().splitlines()]
    request = [e["data"]["request"] for e in events if e["kind"] == "model_start"][-1]
    response = [e["data"]["response"] for e in events if e["kind"] == "model_end"][-1]
    assert request["text"]["format"]["name"] == SCHEMA_NAME and response["status"] == "completed"
    raw, = [item["text"] for output in response["output"] for item in output.get("content", [])
        if item.get("type") == "output_text"]
    assert hashlib.sha256(raw.encode()).hexdigest() == SHORT_RAW_SHA
    document = json.loads(raw)
    assert document["reviews"][0]["current_user_quote"] == "海"
    return request, raw, document


def current_requirements(request, legacy):
    payload = json.loads(request["input"])
    validator = validator_for(request["text"]["format"]["schema"])
    count = len(payload["frozen_candidates"])
    base = copy.deepcopy(legacy)
    base.update(review_binding=payload["review_binding"], omission_supported=count == 0)
    base["reviews"] = []
    for index in range(count):
        entry = copy.deepcopy(legacy["reviews"][0])
        entry.update(candidate_index=index, current_user_quote=payload["current_user_turn"])
        base["reviews"].append(entry)
    rows = [inspect(validator, "empty_candidates" if count == 0 else "current_quote_without_supplements", base, True)]
    quote_enum = request["text"]["format"]["schema"]["properties"]["reviews"]["items"]["properties"]["current_user_quote"].get("enum")
    rows.append(dict(case="enum_is_exact_entire_current_USER", expected_allowed=True,
        actual_allowed=quote_enum == [payload["current_user_turn"]],
        passed=quote_enum == [payload["current_user_turn"]], errors=[]))
    if not count:
        return rows
    for name, changes, expected in (
        ("empty_current_quote", {"current_user_quote": ""}, False),
        ("blank_current_quote", {"current_user_quote": " \t\n"}, False),
        ("null_current_quote", {"current_user_quote": None}, False),
        ("single_character_quote", {"current_user_quote": payload["current_user_turn"][:1]}, False),
        ("truncated_quote", {"current_user_quote": payload["current_user_turn"][:-3]}, False),
        ("missing_final_punctuation", {"current_user_quote": payload["current_user_turn"][:-1]}, False),
        ("current_plus_history", {"evidence": [{"turn_index": 1, "quote": "合成历史原话。"}]}, True),
        ("fifteen_supplements", {"evidence": [{"turn_index": 0, "quote": payload["current_user_turn"]}] * 15}, True),
        ("sixteen_supplements", {"evidence": [{"turn_index": 0, "quote": payload["current_user_turn"]}] * 16}, False),
    ):
        changed = copy.deepcopy(base)
        changed["reviews"][0].update(changes)
        rows.append(inspect(validator, name, changed, expected))
    for name, evidence in (("missing_current_quote_empty_evidence", []),
            ("missing_current_quote_history_only", [{"turn_index": 1, "quote": "合成历史原话。"}]),
            ("legacy_current_evidence_requires_new_field", [{"turn_index": 0, "quote": payload["current_user_turn"]}])):
        changed = copy.deepcopy(base)
        changed["reviews"][0].pop("current_user_quote")
        changed["reviews"][0]["evidence"] = evidence
        rows.append(inspect(validator, name, changed, False))
    rejected = copy.deepcopy(base)
    rejected["reviews"][0]["checks"]["value"] = "contradicted"
    rejected["reviews"][0]["issues"] = [dict(check="value", field="value", reason="该值与当前原话冲突。",
        evidence=[dict(turn_index=0, quote=payload["current_user_turn"])])]
    rows.append(inspect(validator, "negative_with_current_counterevidence", rejected, True))
    if count == 1:
        rows.append(inspect(validator, "actual_archived_T01_missing_current_quote", legacy, False))
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--request", type=Path, action="append")
    parser.add_argument("--fragment-red", action="store_true", help="Replay the real one-character review under its old actual schema; expected red.")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    assert not (args.fragment_red and args.request)
    request, raw, document = archived_review()
    rows, requests = [], []
    short_request, short_raw, short_document = short_review()
    for path in args.request or []:
        actual = json.loads(path.read_text())
        assert actual["text"]["format"]["name"] == SCHEMA_NAME
        assert actual["text"]["format"]["strict"] is True
        requests.append(dict(path=str(path.resolve()), sha256=digest(path)))
        rows.extend(dict(request=str(path), **row) for row in current_requirements(actual, document))
    if not args.request:
        rows = current_requirements(short_request, document) if args.fragment_red else old_requirements(request, document)
        if args.fragment_red:
            rows.append(inspect(validator_for(short_request["text"]["format"]["schema"]),
                "actual_single_character_review", short_document, False))
    assert digest(TRACE) == TRACE_SHA
    assert digest(SHORT_TRACE) == SHORT_TRACE_SHA
    report = dict(status="passed" if all(row["passed"] for row in rows) else "failed",
        mode="new_request" if args.request else "old_fragment_request_red" if args.fragment_red else "old_request_new_requirements_red",
        requests=requests, cases=len(rows), passed=sum(row["passed"] for row in rows),
        real_provider_requests=0, jsonschema_version=importlib.metadata.version("jsonschema"),
        archived_trace=str(TRACE.relative_to(ROOT)), archived_trace_sha256=TRACE_SHA,
        archived_review_output_text_sha256=RAW_SHA, results=rows,
        short_review_trace_sha256=SHORT_TRACE_SHA, short_review_output_text_sha256=SHORT_RAW_SHA,
        limits="Structural checks require the entire current USER string, including punctuation. Quoting the complete source does not establish semantic support: candidate checks/issues and history availability remain independently checked. The original empty-evidence T01 was rejected; the later one-character T01 committed a correct fact but was manually marked a quote-quality gap. Mutations are synthetic controls. New online replies require the new full-source field; public offline verifier legacy compatibility is separate.")
    args.output.mkdir(parents=True, exist_ok=True)
    for name, value in (("schema-check.json", report), ("actual-review-request.json", request),
            ("short-actual-review-request.json", short_request)):
        with (args.output / name).open("x") as output:
            json.dump(value, output, ensure_ascii=False, indent=2)
            output.write("\n")
    with (args.output / "actual-review-output-text.json").open("x") as output:
        output.write(raw)  # Deliberately preserve exact original bytes, including whitespace.
    assert digest(args.output / "actual-review-output-text.json") == RAW_SHA
    with (args.output / "short-actual-review-output-text.json").open("x") as output:
        output.write(short_raw)
    assert digest(args.output / "short-actual-review-output-text.json") == SHORT_RAW_SHA
    print(json.dumps({key: report[key] for key in ("status", "cases", "passed", "real_provider_requests")}))
    for row in rows:
        if not row["passed"]:
            print(row["case"], "expected", row["expected_allowed"], "actual", row["actual_allowed"])
    assert report["status"] == "passed", "actual source-review schema does not enforce current USER evidence"


if __name__ == "__main__":
    main()
