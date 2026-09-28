#!/usr/bin/env python3
"""Rescore frozen N04 retrievals for project clarification without model calls."""
import argparse
import copy
import hashlib
import json
from pathlib import Path

from s5_paired_offline import no_answer_evidence, score, unresolved_without_evidence


ROOT = Path(__file__).resolve().parents[1]
ARCHIVE = ROOT / "docs/refactor/s5/assisted/live-11-20260924"
CASES = ("N04", "N04-ambiguous")
ASSISTED_ROOT = ROOT / "docs/refactor/s5/assisted"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def checked_archive_file(manifest, name):
    path = ARCHIVE / name
    expected = manifest["archive_files"][name]
    require(path.stat().st_size == expected["bytes"] and sha256(path) == expected["sha256"],
            f"frozen archive hash mismatch: {name}")
    return json.loads(path.read_text())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="New directory outside frozen archives")
    args = parser.parse_args()
    output = args.output.resolve()
    require(not output.is_relative_to(ROOT) or output.parent == ASSISTED_ROOT,
            "repository output must be a new direct child of the assisted directory")
    require(not output.exists(), "new output directory required")

    manifest_path = ARCHIVE / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    old = checked_archive_file(manifest, "paired-report.json")
    labels_path = ROOT / "docs/refactor/s4/labels.json"
    inputs_path = ROOT / "docs/refactor/s4/inputs.json"
    require(sha256(labels_path) == manifest["code_and_fixture_files_at_send"]["docs/refactor/s4/labels.json"]["sha256"],
            "frozen labels hash mismatch")
    require(sha256(inputs_path) == manifest["code_and_fixture_files_at_send"]["docs/refactor/s4/inputs.json"]["sha256"],
            "frozen inputs hash mismatch")
    labels = {row["id"]: row for row in json.loads(labels_path.read_text())}
    inputs = {row["id"]: row for row in json.loads(inputs_path.read_text())}
    old_cases = {row["id"]: row for row in old["cases"]}

    cases = []
    for case_id in CASES:
        require(inputs[case_id]["query"] == "先搁置的那种内存缓存服务叫什么？"
                and inputs[case_id]["context"] == [], f"changed N04 question/context: {case_id}")
        raw = checked_archive_file(manifest, f"run/{case_id}.json")
        modes = {}
        for mode in ("local", "assisted"):
            scored = score(raw[mode], labels[case_id], raw["source_map"], [])
            baseline = old_cases[case_id][mode]
            require(scored["final_sources"] == baseline["final_sources"]
                    and scored["evidence_status"] == baseline["evidence_status"]
                    and scored["safety_behavior_passed"] == baseline["safety_behavior_passed"],
                    f"legacy score changed: {case_id}/{mode}")
            require(scored["unresolved_without_evidence"] is True
                    and scored["runtime_ambiguous_status"] is False
                    and scored["clarification_handoff_proxy"] is True
                    and scored["semantic_resolution"] == "not_performed",
                    f"unexpected clarification state: {case_id}/{mode}")
            modes[mode] = {"final_sources": scored["final_sources"],
                           "required_evidence_complete": scored["complete_answerable_evidence"],
                           "legacy_safety_behavior_passed": scored["safety_behavior_passed"],
                           "unresolved_without_evidence": scored["unresolved_without_evidence"],
                           "runtime_ambiguous_status": scored["runtime_ambiguous_status"],
                           "clarification_handoff_proxy": scored["clarification_handoff_proxy"],
                           "semantic_resolution": scored["semantic_resolution"],
                           "host_asked_project": scored["host_asked_project"]}
        cases.append({"id": case_id, "frozen_answerability": labels[case_id]["answerability"],
                      "expected_final_action_for_this_unnamed_query": "ask_which_project",
                      "local": modes["local"], "assisted": modes["assisted"]})

    negative = checked_archive_file(manifest, "run/negative.json")
    require(not unresolved_without_evidence(negative["local"])
            and not unresolved_without_evidence(negative["assisted"]),
            "wrong-source negative cannot count as empty evidence")
    for mode in ("local", "assisted"):
        negative_score = score(negative[mode], labels["negative"], negative["source_map"], [])
        require(not negative_score["clarification_handoff_proxy"],
                "wrong-source negative cannot pass clarification handoff")
    one_sided = copy.deepcopy(checked_archive_file(manifest, "run/N04-ambiguous.json")["assisted"])
    one_sided["source_evidence"] = [{"source_id": "one-sided-candidate"}]
    require(not no_answer_evidence(one_sided) and not unresolved_without_evidence(one_sided),
            "one-sided evidence cannot count as empty")
    known_ambiguous = checked_archive_file(manifest, "run/N06-no-context.json")
    n06 = score(known_ambiguous["local"], labels["N06-no-context"],
                known_ambiguous["source_map"], [])
    require(n06["runtime_ambiguous_status"] is True
            and n06["clarification_handoff_proxy"] is True
            and n06["semantic_resolution"] == "not_performed",
            "N06 runtime ambiguity status changed")

    report = {"scope": "S5 N04 clarification rubric; frozen raw-only retrieval outputs, no final answer",
              "real_provider_requests": 0, "host_answer_observed": False,
              "source_manifest_sha256": sha256(manifest_path),
              "frozen_fixture_sha256": {"inputs": sha256(inputs_path), "labels": sha256(labels_path)},
              "historical_metrics_unchanged": True,
              "project_clarification_handoff_proxy_count": {
                  mode: sum(case[mode]["clarification_handoff_proxy"] for case in cases)
                  for mode in ("local", "assisted")},
              "rubric": {"retrieval": "Keep frozen source recall and point coverage separate",
                         "runtime_ambiguous_status": "A runtime status, not proof of two project relations",
                         "semantic_multi_project_resolution": "Not performed in these retrievals",
                         "unresolved_without_evidence": "Completed recall, unconfirmed referent, unverified sufficiency, and no final source or structured fact",
                         "clarification_handoff_proxy": "No answer evidence plus either an unconfirmed referent or a runtime ambiguous status; not an observed host question",
                         "final_action": "For this unnamed question with no user context, ask which project; only a host answer can verify it"},
              "counterchecks": {"negative_with_source_is_unresolved_without_evidence": False,
                                "one_sided_source_is_unresolved_without_evidence": False,
                                "N06_runtime_ambiguous_status": True,
                                "N06_semantic_resolution": "not_performed"},
              "cases": cases}
    output.mkdir(parents=True, mode=0o700)
    with (output / "report.json").open("x") as file:
        json.dump(report, file, ensure_ascii=False, indent=2)
        file.write("\n")
    require(sha256(manifest_path) == report["source_manifest_sha256"],
            "frozen manifest changed during scoring")
    print(json.dumps({"status": "passed", "cases": len(cases), "real_provider_requests": 0,
                      "output": str(output / "report.json")}, ensure_ascii=False))


if __name__ == "__main__":
    main()
