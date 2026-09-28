#!/usr/bin/env python3
"""Offline checks for the current development runner; no model or socket use."""
import contextlib
import copy
import io
import json
import tempfile
from collections import Counter
from pathlib import Path
from unittest.mock import Mock, patch

import s5_current_regression_live as runner


def rejects(action, error=AssertionError):
    try:
        action()
    except error:
        return
    raise AssertionError("unsafe operation was accepted")


def main():
    inputs, labels, budget = [runner.read(runner.PACK / name)
                             for name in ("inputs.json", "labels.json", "budgets.json")]
    assert runner.static_check(inputs, labels, budget) == {"intake": 8, "recall": 24, "validate": 8}
    assert Counter(op["phase"] for op in runner.plan(inputs)) == {"intake": 8, "recall": 24, "validate": 8}
    bad = copy.deepcopy(budget)
    bad["total_max_physical_requests"] += 1
    rejects(lambda: runner.static_check(inputs, labels, bad))
    bad = copy.deepcopy(budget)
    next(iter(bad["groups"].values()))["intake"]["max_operation_calls"] = 1
    rejects(lambda: runner.static_check(inputs, labels, bad))
    completed = {"enrichment": {"status": "committed"}, "model_calls": [
        {"stage": stage, "outcome": "completed"} for stage in ("extraction", "extraction_review")]}
    runner.check_intake_stages(completed)
    rejects(lambda: runner.check_intake_stages({**completed, "model_calls": completed["model_calls"][:1]}))
    repaired = {**completed, "model_calls": completed["model_calls"] + [
        {"stage": stage, "outcome": "completed"} for stage in ("extraction_repair", "extraction_repair_review")]}
    runner.check_intake_stages(completed, allow_coverage_repair=True)
    runner.check_intake_stages(repaired, allow_coverage_repair=True)
    rejects(lambda: runner.check_intake_stages(repaired))
    rejects(lambda: runner.check_intake_stages({**repaired, "model_calls": repaired["model_calls"][:3]}, allow_coverage_repair=True))
    rejects(lambda: runner.check_intake_stages({**repaired, "model_calls": list(reversed(repaired["model_calls"]))}, allow_coverage_repair=True))
    failed_repair = copy.deepcopy(repaired)
    failed_repair["model_calls"][-1]["outcome"] = "failed"
    rejects(lambda: runner.check_intake_stages(failed_repair, allow_coverage_repair=True))
    bad = copy.deepcopy(budget)
    first_intake = next(iter(bad["groups"].values()))["intake"]
    first_intake["allow_coverage_repair"] = not first_intake.get("allow_coverage_repair", False)
    rejects(lambda: runner.static_check(inputs, labels, bad))
    bad = copy.deepcopy(budget)
    next(iter(bad["groups"].values()))["recall"]["allow_coverage_repair"] = True
    rejects(lambda: runner.static_check(inputs, labels, bad))
    rejects(lambda: runner.runtime_checks({"assistance": {"reason": "model_request_failed"}}))

    with tempfile.TemporaryDirectory(prefix="pmr-current-runner-check-") as temporary:
        output = Path(temporary)
        client = Mock()
        client.rpc.return_value = {"isError": True, "content": [{"text": "fixture failure"}]}
        rejects(lambda: runner.call(client, "memory_recall", {"query": "测试。"}, output / "rpc.json"))
        assert client.rpc.call_count == 1 and (output / "rpc-poll-01.json").exists()
        client.reset_mock()
        rejects(lambda: runner.call(client, "memory_recall", {}, output / "rpc.json"))
        client.rpc.assert_not_called()

        artifact = output / "result.json"
        artifact.write_text('{"synthetic_receipt":true}')
        row = dict(id="Q1", phase="recall", scenario=inputs["scenarios"][0]["id"],
                   after_source_count=2, artifact="result.json", artifact_sha256=runner.digest(artifact),
                   status="review_required")
        report = dict(status="review_required", rows=[row], blocked_scenarios=[],
                      fixed_denominators={"intake": 8, "recall": 24, "validate": 8})
        manifest = dict(operations=[row], runtime={"formal_baseline": {}}, budgets=budget)
        runner.save(output / "manifest.json", manifest)
        runner.save(output / "report.json", report)
        with patch.object(runner, "verify"), patch.object(runner, "LongClient") as connect:
            rejects(lambda: runner.step(output), FileNotFoundError)
            connect.assert_not_called()
        review = dict(id="Q1", phase="recall", artifact_sha256="wrong", verdict="passed", reason="fixture")
        runner.save(output / "result-review.json", review)
        rejects(lambda: runner.apply_review(output, copy.deepcopy(report)))
        review["artifact_sha256"] = runner.digest(artifact)
        review["verdict"] = "quality_gap"
        runner.save(output / "result-review.json", review)
        checked = copy.deepcopy(report)
        runner.apply_review(output, checked)
        assert checked["status"] == "ready" and checked["rows"][0]["status"] == "quality_gap"
        review["verdict"] = "safety_error"
        runner.save(output / "result-review.json", review)
        with patch.object(runner, "verify"), patch.object(runner, "LongClient") as connect, contextlib.redirect_stdout(io.StringIO()):
            runner.step(output)
            assert runner.read(output / "report.json")["status"] == "stopped"
            rejects(lambda: runner.step(output))
            connect.assert_not_called()

        # A prior attempt marker stops before constructing a client, even with no receipt.
        query = inputs["scenarios"][0]["queries"][0]
        op = dict(id=query["id"], scenario=inputs["scenarios"][0]["id"], phase="recall",
                  after_source_count=query["after_source_count"])
        directory = output / op["scenario"]
        directory.mkdir()
        (directory / "business.sqlite3").touch()
        (directory / f"00-{op['id']}.started").touch()
        manifest["operations"] = [op]
        runner.save(output / "manifest.json", manifest)
        runner.save(output / "report.json", {**report, "status": "ready", "rows": []})
        with patch.object(runner, "verify"), patch.object(runner, "audit", return_value={"reserved_calls": 0}), \
             patch.object(runner, "formal_state", return_value={}), patch.object(runner, "clean_env", return_value={}), \
             patch.object(runner, "LongClient") as connect, contextlib.redirect_stdout(io.StringIO()):
            runner.step(output)
            stopped = runner.read(output / "report.json")
            assert stopped["status"] == "stopped" and stopped["rows"][0]["status"] == "runtime_error"
            assert "FileExistsError" in stopped["rows"][0]["error"]
            connect.assert_not_called()
        source = inputs["scenarios"][0]["sources"][0]
        manifest["operations"] = [dict(id=source["id"], scenario=inputs["scenarios"][0]["id"], phase="intake", after_source_count=1)]
        runner.save(output / "manifest.json", manifest)
        runner.save(output / "report.json", {**report, "status": "ready", "rows": []})
        cap = budget["groups"][inputs["scenarios"][0]["id"]]["intake"]["max_operation_calls"]
        remaining = budget["total_max_physical_requests"] - cap + 1
        with patch.object(runner, "verify"), patch.object(runner, "audit", return_value={"reserved_calls": remaining}), \
             patch.object(runner, "formal_state", return_value={}), patch.object(runner, "clean_env") as environment, \
             patch.object(runner, "LongClient") as connect, contextlib.redirect_stdout(io.StringIO()):
            runner.step(output)
            stopped = runner.read(output / "report.json")
            assert stopped["status"] == "stopped" and "insufficient remaining budget for this operation" in stopped["rows"][0]["error"]
            connect.assert_not_called()
            environment.assert_not_called()
    print(json.dumps({"status": "passed", "counts": {"intake": 8, "recall": 24, "validate": 8},
                      "max_physical_requests": budget["total_max_physical_requests"], "real_model_requests": 0,
                      "checks": ["frozen_plan_and_caps", "independent_review_required", "provider_failure_no_retry",
                                 "existing_receipt_no_rpc", "missing_or_stale_review_blocks_send",
                                 "quality_gap_continues", "safety_error_stops", "attempt_marker_blocks_replay",
                                 "coverage_repair_requires_explicit_intake_policy_and_complete_ordered_stages",
                                 "full_operation_budget_reserved_before_client_creation"]}))


if __name__ == "__main__":
    main()
