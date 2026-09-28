#!/usr/bin/env python3
"""Score the frozen S5 real 11-case run offline; never invoke the model."""
import argparse
import hashlib
import json
import sqlite3
from pathlib import Path

from s5_paired_offline import score


ROOT = Path(__file__).resolve().parents[1]
ARCHIVE = ROOT / "docs/refactor/s5/assisted/live-11-20260924"
RUN = ARCHIVE / "run"
ASSISTED_ROOT = ROOT / "docs/refactor/s5/assisted"


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def changes(local, assisted, key):
    before, after = set(local[key]), set(assisted[key])
    return {"added": sorted(after - before), "lost": sorted(before - after)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="New report path outside the frozen archive")
    args = parser.parse_args()
    output = args.output.resolve()
    if output.is_relative_to(ROOT) and output.parent != ASSISTED_ROOT:
        raise ValueError("repository output must be a new file directly under the assisted directory")
    original = json.loads((RUN / "report.json").read_text())
    labels_path = ROOT / "docs/refactor/s4/labels.json"
    inputs_path = ROOT / "docs/refactor/s4/inputs.json"
    assert sha256(labels_path) == original["labels_sha256"]
    assert sha256(inputs_path) == original["inputs_sha256"]
    labels = {row["id"]: row for row in json.loads(labels_path.read_text())}
    assert original["planned_cases"] == original["completed_final_reads"] == 11
    assert len(original["ledger_rows"]) == 11
    assert {row["id"] for row in original["cases"]} == set(labels)
    assert original["mode"] == "real_all" and original["send_requested"] is True

    trace_paths = sorted((RUN / "trace").glob("*.jsonl"))
    assert [path.name for path in trace_paths] == original["trace"]["files"]
    events = [json.loads(line) for path in trace_paths for line in path.read_text().splitlines()]
    assert sum(event["kind"] == "model_start" for event in events) == 11
    assert sum(event["kind"] == "model_end" for event in events) == 11

    ledger_path = ROOT / original["ledger_path"]
    with sqlite3.connect(f"file:{ledger_path}?mode=ro", uri=True) as db:
        assert db.execute("pragma integrity_check").fetchone() == ("ok",)
        physical_calls = db.execute(
            "select operation_id, stage, outcome, input_bytes, output_token_limit, "
            "input_tokens, output_tokens, request_fingerprint from model_calls"
        ).fetchall()
    assert len(physical_calls) == 11
    physical = {row[0]: row[1:] for row in physical_calls}
    ledger = {row["operation_id"]: row for row in original["ledger_rows"]}
    assert len(ledger) == 11 and set(physical) == set(ledger)
    for operation, row in ledger.items():
        assert physical[operation] == tuple(row[key] for key in (
            "stage", "outcome", "input_bytes", "output_token_limit", "input_tokens",
            "output_tokens", "request_fingerprint"))
        assert row["stage"] == "assistance" and row["outcome"] == "completed"
        assert row["usage_status"] == "known" and row["output_token_limit"] == 32768
        assert row["input_bytes"] <= 65536

    cases = []
    file_hashes = {"run/report.json": sha256(RUN / "report.json"),
                   "run/trace/" + trace_paths[0].name: sha256(trace_paths[0]),
                   "model-ledger.sqlite3": sha256(ledger_path)}
    for case in original["cases"]:
        case_id = case["id"]
        raw_path = RUN / f"{case_id}.json"
        raw = json.loads(raw_path.read_text())
        file_hashes[f"run/{case_id}.json"] = sha256(raw_path)
        assert raw["id"] == case_id and raw["trusted_at"] == original["trusted_at"]
        assert raw["fact_index_enabled"] is original["fact_index_enabled"] is False
        assert raw["local"]["read_revision"] == raw["assisted"]["read_revision"]
        assert raw["local"]["source_revision"] == raw["assisted"]["source_revision"]
        assert raw["local"]["budget"]["config"] == raw["assisted"]["budget"]["config"] == original["local_budget"]
        assert raw["planner_status"] == "completed" and len(raw["model_calls"]) == 1
        operation = f"s5-assisted-eval-{case_id}"
        assert raw["model_calls"][0]["operation_id"] == operation
        assert raw["model_calls"][0]["stage"] == "assistance"
        assert raw["assisted"]["assistance"]["status"] in ("completed", "completed_empty")

        snapshots = [event["data"] for event in events if event["run_id"] == operation
                     and event["kind"] == "assistance_candidates"]
        assert len(snapshots) == 2 and all(s["stage"] == "pre_filter" for s in snapshots)
        assert snapshots[0]["original"] == snapshots[1]["original"]
        local = score(raw["local"], labels[case_id], raw["source_map"], snapshots[0]["original"])
        assisted = score(raw["assisted"], labels[case_id], raw["source_map"],
                         snapshots[1]["original"] + snapshots[1]["supplemental"])
        for mode, scored in (("local", local), ("assisted", assisted)):
            baseline = case[mode]
            assert scored["answerability"] == baseline["answerability"]
            assert scored["final_sources"] == baseline["final_sources"]
            if baseline["available_required_evidence_complete"] is not None:
                assert scored["available_required_evidence_complete"] == baseline["available_required_evidence_complete"]
            assert scored["forbidden_found"] == baseline["forbidden_found"]
            assert scored["evidence_status"] == baseline["evidence_status"]
        required = set(labels[case_id]["required_sources"])
        forbidden = set(labels[case_id]["forbidden_sources"])
        candidate = changes(local, assisted, "candidate_sources")
        final = changes(local, assisted, "final_sources")
        cases.append({"id": case_id, "answerability": labels[case_id]["answerability"],
                      "local": local, "assisted": assisted,
                      "candidate_nonrequired": {
                          mode: sorted(set(scored["candidate_sources"]) - required)
                          for mode, scored in (("local", local), ("assisted", assisted))},
                      "candidate_forbidden": {
                          mode: sorted(set(scored["candidate_sources"]) & forbidden)
                          for mode, scored in (("local", local), ("assisted", assisted))},
                      "candidate": {**candidate,
                                    "added_required": sorted(set(candidate["added"]) & required),
                                    "lost_required": sorted(set(candidate["lost"]) & required),
                                    "added_forbidden": sorted(set(candidate["added"]) & forbidden)},
                      "final": {**final,
                                "added_required": sorted(set(final["added"]) & required),
                                "lost_required": sorted(set(final["lost"]) & required),
                                "added_forbidden": sorted(set(final["added"]) & forbidden)},
                      "supplemental_candidate_rows": len(snapshots[1]["supplemental"]),
                      "planner_query_count": len(raw["assisted"]["assistance"]["queries"]),
                      "planner_status": raw["assisted"]["assistance"]["status"],
                      "model_call": raw["model_calls"][0],
                      "ledger": ledger[operation]})

    answerable = [case for case in cases if case["answerability"] == "answerable"]
    ambiguous = [case for case in cases if case["answerability"] == "ambiguous"]
    absent = [case for case in cases if case["answerability"] == "absent"]
    report = {
        "scope": "S4 fixed 11-question real Planner local/assisted development pair; retrieval evidence only",
        "release_acceptance": False,
        "safety_metric": "Proxy from evidence_status and final source IDs; no generated answer or judge call",
        "nonrequired_metric": "Source IDs outside required_sources; useful context may also count here",
        "real_provider_requests": len(physical_calls), "synthetic_requests": 0,
        "fixture_sha256": {"inputs": original["inputs_sha256"], "labels": original["labels_sha256"]},
        "archive_sha256": file_hashes, "trusted_at": original["trusted_at"],
        "fact_index_enabled": False, "local_budget": original["local_budget"],
        "model": original["model"], "reasoning_effort": original["reasoning_effort"],
        "model_budget": original["model_budget"],
        "aggregate": {
            "case_count": len(cases), "answerable_count": len(answerable),
            "complete_answerable_evidence": {
                mode: sum(case[mode]["complete_answerable_evidence"] is True for case in answerable)
                for mode in ("local", "assisted")},
            "safety_behavior_passed": {
                mode: sum(case[mode]["safety_behavior_passed"] for case in cases)
                for mode in ("local", "assisted")},
            "ambiguous_safety_passed": {
                mode: sum(case[mode]["safety_behavior_passed"] for case in ambiguous)
                for mode in ("local", "assisted")},
            "absent_safety_passed": {
                mode: sum(case[mode]["safety_behavior_passed"] for case in absent)
                for mode in ("local", "assisted")},
            "candidate_added_required": {case["id"]: case["candidate"]["added_required"] for case in cases
                                         if case["candidate"]["added_required"]},
            "candidate_lost_required": {case["id"]: case["candidate"]["lost_required"] for case in cases
                                        if case["candidate"]["lost_required"]},
            "candidate_added_forbidden": {case["id"]: case["candidate"]["added_forbidden"] for case in cases
                                          if case["candidate"]["added_forbidden"]},
            "final_added_required": {case["id"]: case["final"]["added_required"] for case in cases
                                     if case["final"]["added_required"]},
            "final_lost_required": {case["id"]: case["final"]["lost_required"] for case in cases
                                    if case["final"]["lost_required"]},
            "final_added_forbidden": {case["id"]: case["final"]["added_forbidden"] for case in cases
                                      if case["final"]["added_forbidden"]},
            "final_added_sources": {case["id"]: case["final"]["added"] for case in cases
                                    if case["final"]["added"]},
            "final_lost_sources": {case["id"]: case["final"]["lost"] for case in cases
                                   if case["final"]["lost"]},
            "supplemental_candidate_rows": sum(case["supplemental_candidate_rows"] for case in cases),
            "usage": {"calls": len(physical_calls),
                      "input_tokens": sum(row["input_tokens"] for row in ledger.values()),
                      "output_tokens": sum(row["output_tokens"] for row in ledger.values()),
                      "input_bytes": sum(row["input_bytes"] for row in ledger.values()),
                      "pricing_status": "unknown"}},
        "cases": cases}
    with output.open("x") as file:
        file.write(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({"output": str(output), **report["aggregate"]}, ensure_ascii=False))


if __name__ == "__main__":
    main()
