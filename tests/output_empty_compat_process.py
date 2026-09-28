#!/usr/bin/env python3
"""Replay optional-scope compatibility through four MCP intake paths, loopback only."""
import argparse
import contextlib
import http.server
import json
import os
import tempfile
import threading
from pathlib import Path

import intake_grounding_process as grounding


# Reuse the existing full intake checks: review before Active, source/index receipts,
# replay without another request, configured budgets, ledger stages and source recall.
BASE_CANDIDATE = grounding.raw_candidate
BASE_REVIEW = grounding.source_support_review
grounding.CASES["review_denied"] = (
    grounding.PAPER, "fact", "text", grounding.PAPER, "contradicted")
grounding.CASES["domain_nonempty"] = (
    grounding.PAPER, "fact", "text", grounding.PAPER, "format_reject")
grounding.CASES["city_nonempty"] = (
    grounding.PAPER, "fact", "text", grounding.PAPER, "format_reject")
grounding.CASES["integer"] = (
    "霜叶31的维护预算是 0 元。", "maintenance_budget", "integer", 0, "supported")
CASES = {
    "a4_empty_domain": ("fact_sentence", []),
    "review_denied": ("review_denied", []),
    "domain_nonempty": ("domain_nonempty", ["printing"]),
    "boolean_false": ("boolean", []),
    "integer_zero": ("integer", []),
    "standard_null": ("fact_sentence", None),
    "city_empty": ("fact_sentence", None),
    "city_activity_review_denied": ("review_denied", None),
    "multiple_empty_axes": ("fact_sentence", []),
    "city_nonempty": ("city_nonempty", None),
}
CONDITION_OVERRIDES = {
    "city_empty": {"city": []},
    "city_activity_review_denied": {"city": [], "activities": ["打印"]},
    "multiple_empty_axes": dict.fromkeys(
        ("geography", "country", "city", "date", "time_of_day", "task_id"), []),
    "city_nonempty": {"city": ["杭州"]},
}


def raw_candidate(case):
    result = BASE_CANDIDATE(case)
    result["scope"]["domain"] = grounding.Fixture.domain
    result["scope"]["conditions"].update(grounding.Fixture.condition_overrides)
    return result


def source_support_review(request):
    frozen = json.loads(request["input"])["frozen_candidates"]
    expected_conditions = [{"Activity": activity} for activity in
                           grounding.Fixture.condition_overrides.get("activities", [])]
    assert frozen[0]["scope"] == {"domain": None, "conditions": expected_conditions}, frozen
    return BASE_REVIEW(request)


def run_case(binary, directory, env, mode, name):
    case_name, domain = CASES[name]
    case_directory = directory / f"{mode}-{name}"
    case_directory.mkdir()
    trace = case_directory / "trace"
    trace.mkdir(mode=0o700)
    grounding.Fixture.domain = domain
    grounding.Fixture.condition_overrides = CONDITION_OVERRIDES.get(name, {})
    row = grounding.run_case(binary, case_directory, dict(env, PMR_TRACE_DIR=str(trace)),
                             mode, case_name)
    row["case"] = name
    events = [json.loads(line) for path in trace.glob("*.jsonl")
              for line in path.read_text().splitlines()]
    try:
        outputs = [event["data"] for event in events if event["kind"] == "model_end"
                   and isinstance(event["data"].get("parsed_result"), dict)
                   and "candidates" in event["data"]["parsed_result"]]
        assert len(outputs) == 1, "missing or repeated original extraction trace"
        original = outputs[0]["parsed_result"]
        assert original["candidates"][0]["scope"]["domain"] == domain
        for field, expected in grounding.Fixture.condition_overrides.items():
            assert original["candidates"][0]["scope"]["conditions"][field] == expected
        expected_corrections = []
        if domain == []:
            expected_corrections.append({
                "path": "/candidates/0/scope/domain", "from": [], "to": None,
                "reason": "empty_array_for_optional_domain"})
        for field, value in grounding.Fixture.condition_overrides.items():
            if value == []:
                expected_corrections.append({
                    "path": f"/candidates/0/scope/conditions/{field}", "from": [], "to": None,
                    "reason": "empty_array_for_optional_scope_condition"})
        repairs = [event["data"] for event in events if event["kind"] == "output_normalization"]
        assert len(repairs) == int(bool(expected_corrections)), "unexpected normalization event count"
        if repairs:
            repair = repairs[0]
            assert repair["stage"] == "extraction"
            raw_text = outputs[0]["response"]["output"][0]["content"][0]["text"]
            assert repair["original_output"] == raw_text, "raw model output was modified"
            assert json.loads(repair["original_output"]) == original
            assert sorted(repair["corrections"], key=lambda item: item["path"]) == sorted(
                expected_corrections, key=lambda item: item["path"]), repair
    except AssertionError as error:
        row["passed"] = False
        row["failures"].append(str(error) or "trace assertion failed")
    row["normalization_events"] = sum(event["kind"] == "output_normalization" for event in events)
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=grounding.ROOT / "target/debug/pmr-mcp")
    parser.add_argument("--modes", nargs="+", choices=["inline", "deferred", "explicit", "legacy"],
                        default=["inline", "deferred", "explicit", "legacy"])
    parser.add_argument("--cases", nargs="+", choices=list(CASES), default=list(CASES))
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    assert binary.is_file(), f"build the MCP binary first: {binary}"
    grounding.raw_candidate = raw_candidate
    grounding.source_support_review = source_support_review
    grounding.Fixture.requests, grounding.Fixture.errors = [], []
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-empty-compat-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), grounding.Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        clean = {key: value for key, value in os.environ.items()
                 if not key.startswith(("DEEPSEEK_", "OPENAI_", "PMR_"))}
        env = dict(clean, PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
                   DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
                   NO_PROXY="127.0.0.1,localhost")
        rows = [run_case(binary, directory, env, mode, name)
                for mode in args.modes for name in args.cases]
    expected = len(args.modes) * sum(
        1 if grounding.CASES[CASES[name][0]][-1] == "format_reject" else 2 for name in args.cases)
    report = dict(passed=all(row["passed"] for row in rows), cases=len(rows),
                  expected_calls=expected, actual_calls=len(grounding.Fixture.requests),
                  real_provider_requests=0, results=rows)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    for row in rows:
        print(f"{'PASS' if row['passed'] else 'FAIL'} {row['mode']}/{row['case']}: "
              f"Active={row['active']}, calls={row['actual_calls']}/{row['expected_calls']}, "
              f"normalizations={row['normalization_events']}"
              + ("; " + "; ".join(row["failures"]) if row["failures"] else ""))
    print(json.dumps({key: value for key, value in report.items() if key != "results"}, ensure_ascii=False))
    assert report["passed"] and report["actual_calls"] == expected, "empty compatibility process failed"


if __name__ == "__main__":
    main()
