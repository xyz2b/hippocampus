#!/usr/bin/env python3
"""Offline JSON Schema checks against actual extraction requests, with old T02 controls.

Run in a Python environment with jsonschema installed; the checks use no network:
  python3 tests/subject_time_schema.py \
    --request REQUEST.json [--empty-request EMPTY_REQUEST.json] --output NEW_DIRECTORY
The checks establish structural constraints, not calendar accuracy or USER support.
"""
import argparse
import copy
import hashlib
import importlib.metadata
import json
from pathlib import Path

try:
    import jsonschema
except ImportError as error:
    raise SystemExit("Install jsonschema in your Python environment: python3 -m pip install jsonschema") from error


ROOT = Path(__file__).resolve().parents[1]
OLD_TRACE = ROOT / "docs/refactor/s5/source-repair-calendar-live-20260928/specialist/iso_date/trace"
LEGACY_FIELDS = (
    "subject_id", "unresolved_subject", "temporal_expression", "temporal_date",
    "temporal_end_date", "expiry_expression", "expiry_date", "explicit_memory_request",
    "valid_from", "valid_to", "expires_at",
)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def request_from(path):
    value = json.loads(path.read_text())
    if "request" in value:
        value = value["request"]
    assert value["text"]["format"]["name"] == "personal_memory_candidates_v1"
    assert value["text"]["format"]["type"] == "json_schema"
    assert value["text"]["format"]["strict"] is True
    return value


def validator_for(schema):
    # Only local references are allowed, so validation cannot retrieve a remote schema.
    def local_refs(value):
        if isinstance(value, dict):
            for key, child in value.items():
                if key in ("$ref", "$dynamicRef", "$recursiveRef"):
                    assert isinstance(child, str) and child.startswith("#"), "external schema reference forbidden"
                local_refs(child)
        elif isinstance(value, list):
            for child in value:
                local_refs(child)
    local_refs(schema)
    cls = jsonschema.validators.validator_for(schema)
    cls.check_schema(schema)
    return cls(schema)


def inspect(validator, name, document, expected):
    errors = list(validator.iter_errors(document))
    allowed = not errors
    return dict(case=name, expected_allowed=expected, actual_allowed=allowed,
        passed=allowed == expected, errors=[dict(path=list(e.absolute_path),
        schema_path=list(e.absolute_schema_path), keyword=e.validator) for e in errors[:4]])


def old_control():
    paths = sorted(OLD_TRACE.glob("*.jsonl"))
    assert paths, "archived T02 trace is required"
    events = [json.loads(line) for path in paths for line in path.read_text().splitlines()]
    starts = [e["data"]["request"] for e in events if e["kind"] == "model_start"]
    ends = [e["data"]["response"] for e in events if e["kind"] == "model_end"]
    assert len(starts) == len(ends) == 1
    request = starts[0]
    texts = [c["text"] for o in ends[0]["output"] for c in o.get("content", []) if c.get("type") == "output_text"]
    assert len(texts) == 1
    raw = json.loads(texts[0])
    candidate, = raw["candidates"]
    assert candidate["subject_id"] is None and candidate["unresolved_subject"] == ""
    assert candidate["expiry_expression"] is None and candidate["expiry_date"] == "2026-10-02"
    payload = json.loads(request["input"])
    known_id = next(e["id"] for e in payload["known_entities"] if e["canonical_name"] == "银杏工作坊")
    validator = validator_for(request["text"]["format"]["schema"])
    rows = [inspect(validator, "old_actual_empty_subject_already_invalid", raw, False)]
    for name, changes, expected in (
        ("old_whitespace_subject_allowed", dict(unresolved_subject=" \t\n"), True),
        ("old_known_subject_orphan_expiry_allowed", dict(subject_id=known_id, unresolved_subject=None), True),
        ("old_correct_fact_date_allowed", dict(subject_id=known_id, unresolved_subject=None,
            expiry_date=None, temporal_expression="2026-10-02", temporal_date="2026-10-02"), True),
    ):
        document = copy.deepcopy(raw)
        document["candidates"][0].update(changes)
        rows.append(inspect(validator, name, document, expected))
    return raw, rows, {str(p.relative_to(ROOT)): digest(p) for p in paths}


def grouped_checks(request, legacy, *, empty_catalog=False):
    payload = json.loads(request["input"])
    ids = [entity["id"] for entity in payload["known_entities"]]
    assert (not ids) == empty_catalog, "request does not have the expected entity catalog"
    validator = validator_for(request["text"]["format"]["schema"])
    candidate = copy.deepcopy(legacy["candidates"][0])
    for field in LEGACY_FIELDS:
        candidate.pop(field, None)
    candidate.update(subject={"unresolved_reference": "那位同事"} if empty_catalog else {"entity_id": ids[0]},
        fact_time=None, memory_request={"kind": "not_requested"})
    candidate["value"]["text"] = payload["current_user_turn"]
    base = dict(omission_reason=None, candidates=[candidate])
    rows = [inspect(validator, "grouped_without_time", base, True)]

    def check(name, changes, expected):
        document = copy.deepcopy(base)
        document["candidates"][0].update(changes)
        rows.append(inspect(validator, name, document, expected))

    check("unresolved_nonblank", {"subject": {"unresolved_reference": "那位同事"}}, True)
    check("subject_null", {"subject": None}, False)
    check("subject_empty_object", {"subject": {}}, False)
    check("unresolved_empty", {"subject": {"unresolved_reference": ""}}, False)
    check("unresolved_whitespace", {"subject": {"unresolved_reference": " \t\n"}}, False)
    check("unknown_entity_id", {"subject": {"entity_id": "not-in-the-entity-catalog"}}, False)
    if empty_catalog:
        check("empty_catalog_arbitrary_entity_id", {"subject": {"entity_id": "entity-any"}}, False)
    else:
        for entity_id in ids:
            check("catalog_id_" + entity_id, {"subject": {"entity_id": entity_id}}, True)
        check("both_subject_branches", {"subject": {"entity_id": ids[0], "unresolved_reference": "那位同事"}}, False)
    check("fact_calendar_day", {"fact_time": {"expression": "2026-10-02", "date": "2026-10-02", "end_date": None}}, True)
    check("fact_calendar_range", {"fact_time": {"expression": "下个月", "date": "2026-10-01", "end_date": "2026-11-01"}}, True)
    check("unresolved_fact_time", {"fact_time": {"expression": "旅行结束后", "date": None, "end_date": None}}, True)
    check("orphan_grouped_fact_date", {"fact_time": {"date": "2026-10-02", "end_date": None}}, False)
    check("numeric_fact_date", {"fact_time": {"expression": "明天", "date": 1790870400, "end_date": None}}, False)
    check("remember_without_deadline", {"memory_request": {"kind": "remember"}}, True)
    check("retain_until", {"memory_request": {"kind": "retain_until", "expression": "明天", "date": "2026-10-02"}}, True)
    check("both_time_roles", {"fact_time": {"expression": "2026-10-02", "date": "2026-10-02", "end_date": None},
        "memory_request": {"kind": "retain_until", "expression": "2026-10-05", "date": "2026-10-05"}}, True)
    check("remember_with_date", {"memory_request": {"kind": "remember", "date": "2026-10-02"}}, False)
    check("not_requested_with_date", {"memory_request": {"kind": "not_requested", "date": "2026-10-02"}}, False)
    check("retention_missing_kind", {"memory_request": {"expression": "明天", "date": "2026-10-02"}}, False)
    check("retain_until_missing_date", {"memory_request": {"kind": "retain_until", "expression": "明天"}}, False)
    check("retain_until_null_date", {"memory_request": {"kind": "retain_until", "expression": "明天", "date": None}}, False)
    check("retain_until_missing_expression", {"memory_request": {"kind": "retain_until", "date": "2026-10-02"}}, False)
    check("unknown_memory_kind", {"memory_request": {"kind": "implicit"}}, False)
    check("orphan_flat_date", {"expiry_date": "2026-10-02"}, False)
    check("new_old_subject_mixed", {"subject_id": ids[0] if ids else "entity-any", "unresolved_subject": None}, False)
    check("new_old_time_mixed", {"temporal_expression": "明天", "temporal_date": "2026-10-02"}, False)
    check("new_old_memory_flag_mixed", {"explicit_memory_request": True}, False)
    check("numeric_epoch_in_new_wire", {"valid_from": {"at": 1790870400, "source": "explicit"}}, False)
    old = copy.deepcopy(legacy)
    old["candidates"][0].update(subject_id=ids[0] if ids else "entity-any", unresolved_subject=None)
    rows.append(inspect(validator, "legacy_flat_wire", old, False))
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--request", type=Path, action="append", required=True)
    parser.add_argument("--empty-request", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    legacy, old_rows, old_hashes = old_control()
    requests, rows = {}, [{"schema": "old_actual_T02", **row} for row in old_rows]
    sources = [(p, False) for p in args.request]
    if args.empty_request is not None:
        sources.append((args.empty_request, True))
    for index, (path, empty) in enumerate(sources):
        request = request_from(path)
        key = "empty_catalog" if empty else f"current_{index}"
        requests[key] = dict(path=str(path.resolve()), sha256=digest(path), known_entities=len(json.loads(request["input"])["known_entities"]))
        rows.extend({"schema": key, **row} for row in grouped_checks(request, legacy, empty_catalog=empty))
    assert all(digest(ROOT / path) == fingerprint for path, fingerprint in old_hashes.items())
    report = dict(status="passed" if all(row["passed"] for row in rows) else "failed",
        cases=len(rows), passed=sum(row["passed"] for row in rows), real_provider_requests=0,
        jsonschema_version=importlib.metadata.version("jsonschema"), requests=requests,
        empty_catalog_exercised=args.empty_request is not None,
        archived_trace_sha256=old_hashes, results=rows,
        limits="Structural JSON Schema checks only. Relative-date accuracy, complete USER support, fact-versus-retention meaning, and admission remain backend and independent-review checks. Old T02's empty string already violated its transmitted schema; whitespace and orphan expiry were separately schema-valid.")
    args.output.mkdir(parents=True, exist_ok=True)
    with (args.output / "schema-check.json").open("x") as output:
        json.dump(report, output, ensure_ascii=False, indent=2)
        output.write("\n")
    print(json.dumps({key: report[key] for key in ("status", "cases", "passed", "real_provider_requests")}, ensure_ascii=False))
    for row in rows:
        if not row["passed"]:
            print(row["schema"], row["case"], "expected", row["expected_allowed"], "actual", row["actual_allowed"])
    assert report["status"] == "passed", "actual request schema did not enforce the intended grouped protocol"


if __name__ == "__main__":
    main()
