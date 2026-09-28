#!/usr/bin/env python3
"""Single-use S6 runner: one frozen operation per step; no request retries."""
import argparse
import contextlib
import datetime
import fcntl
import importlib.util
import json
import os
import sqlite3
import sys
import time
from collections import Counter
from pathlib import Path

from s5_original_n05_fullchain import LongClient, clean_env, digest, receipt, save
from s5_normal_intake import settings
from s6_holdout_scoring import score_intake, score_contract, score_validate

if sys.flags.optimize:
    raise RuntimeError("run without Python optimization; acceptance guards must remain enabled")
ROOT = Path(__file__).resolve().parents[1]
PACK = ROOT / "docs/refactor/s6/holdout-v1-20260927"
OWNER = "s6-holdout-v1-20260927"
SPEC = importlib.util.spec_from_file_location("s6_preflight", PACK / "preflight.py")
PREP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PREP)


def read(path):
    return json.loads(path.read_text())


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def plan(inputs):
    operations = []
    for scenario in inputs["scenarios"]:
        for count, source in enumerate(scenario["sources"], 1):
            operations.append(dict(scenario=scenario["id"], phase="intake", id=source["id"], after_source_count=count))
            for query in scenario["queries"]:
                if query["after_source_count"] != count:
                    continue
                operations.append(dict(scenario=scenario["id"], phase="recall", id=query["id"], after_source_count=count))
                operations.extend(dict(scenario=scenario["id"], phase="validate", id=v["id"], after_source_count=count)
                                  for v in scenario["validations"] if v["query_id"] == query["id"])
    return operations


def frozen():
    runtime, dataset = read(PACK / "runtime-freeze.json"), read(PACK / "dataset-freeze.json")
    for path, fingerprint in {**runtime["runtime_files"], **dataset["files"]}.items():
        assert digest(ROOT / path) == fingerprint, f"frozen file changed: {path}"
    for key in ("binary", "source_archive"):
        assert digest(Path(runtime[key]["path"])) == runtime[key]["sha256"]
    assert PREP.formal_state() == runtime["formal_baseline"], "formal S4 baseline changed"
    return runtime


def profile(db):
    with sqlite3.connect(db.as_uri() + "?mode=ro", uri=True) as connection:
        return json.loads(connection.execute("SELECT state_json FROM memory_profiles WHERE owner_id=?", (OWNER,)).fetchone()[0])


def model_reports(value):
    reports = []
    if isinstance(value, dict):
        if isinstance(value.get("model_calls"), list):
            reports.extend(value["model_calls"])
        for key, child in value.items():
            if key != "model_calls":
                reports.extend(model_reports(child))
    elif isinstance(value, list):
        for child in value:
            reports.extend(model_reports(child))
    return reports


def audit(output, budgets):
    calls, parse_errors, events = [], [], []
    for scenario in budgets["groups"]:
        db = output / scenario / "business.sqlite3"
        if db.exists():
            with sqlite3.connect(db.as_uri() + "?mode=ro", uri=True) as connection:
                connection.row_factory = sqlite3.Row
                calls.extend({**dict(row), "scenario": scenario} for row in connection.execute("SELECT * FROM model_calls"))
    for path in sorted(output.glob("*/trace/*.jsonl")):
        for line in path.read_text().splitlines():
            try:
                events.append(json.loads(line))
            except ValueError:
                parse_errors.append(str(path.relative_to(output)))
    starts = [e["data"] for e in events if e["kind"] == "model_start"]
    ends = [e["data"] for e in events if e["kind"] == "model_end"]
    result = {"reserved_calls": len(calls), "physical_requests": len(starts), "ledger": calls,
              "completed_request_events": len(ends), "trace_parse_errors": parse_errors,
              "known_input_tokens": sum(row["input_tokens"] or 0 for row in calls),
              "known_output_tokens": sum(row["output_tokens"] or 0 for row in calls),
              "unknown_usage_requests": sum(row["usage_status"] != "known" for row in calls)}
    # Preserve reservations and attempted sends even when the checks below stop the run.
    save(output / "accounting.json", result)
    assert not parse_errors, "trace contains incomplete JSON"
    for scenario, phases in budgets["groups"].items():
        rows = [row for row in calls if row["scenario"] == scenario]
        configs = {row["group_id"]: row for row in phases.values()}
        assert all(row["group_id"] in configs and row["owner_id"] == OWNER for row in rows)
        for phase, config in phases.items():
            group = [row for row in rows if row["group_id"] == config["group_id"]]
            assert len(group) <= config["max_calls"], "scenario/phase cap exceeded"
            assert len({(r["operation_id"], r["stage"]) for r in group}) == len(group), "duplicate operation stage"
            assert all(n <= config["max_operation_calls"] for n in Counter(r["operation_id"] for r in group).values())
            cap = 16384 if phase == "intake" else 8192
            stages = {"extraction", "extraction_review"} if phase == "intake" else {"assistance"}
            assert all(row["stage"] in stages and row["output_token_limit"] == cap
                       and row["input_bytes"] <= 65536 for row in group)
    assert len(calls) <= budgets["total_max_physical_requests"]
    assert sum(row["input_bytes"] for row in calls) <= 5505024
    assert sum(row["output_token_limit"] for row in calls) <= 950272
    ids = {row["call_id"] for row in calls}
    assert len(ids) == len(calls) == len(starts) == len(ends), "trace/ledger counts differ"
    assert ids == {e["call_id"] for e in starts} == {e["call_id"] for e in ends}
    reports = [report for path in output.glob("*/*-poll-*.json") for report in model_reports(read(path).get("structuredContent", {}))]
    assert ids == {r["call_id"] for r in reports}, "tool receipts/ledger differ"
    assert all(row["outcome"] == "completed" for row in calls), "provider request failed"
    by_id = {row["call_id"]: row for row in calls}
    for start in starts:
        request, row = start["request"], by_id[start["call_id"]]
        assert start["attempt"] == 1 and start["timeout_ms"] == 180000
        assert start["provider"] == "ark.cn-beijing.volces.com" and start["budget_group"] == row["group_id"]
        assert request["max_output_tokens"] == row["output_token_limit"]
        assert request["model"] == budgets["model"] and request["reasoning"]["effort"] == budgets["reasoning"]
    for end in ends:
        assert end["http_status"] == 200 and end["status"] == "completed"
        assert end["response"]["status"] == "completed" and end["parsing_status"] == "valid_json"
        assert end.get("error") is None
    assert all(e.get("data", {}).get("evidence_status") != "evidence_incomplete" for e in events)
    return result


def runtime_checks(value):
    if isinstance(value, dict):
        assert value.get("diagnostics", {}).get("status") != "evidence_incomplete", "trace evidence incomplete"
        failure_reasons = {"model_credentials_missing", "model_network_disabled", "model_budget_rejected",
                           "model_request_failed", "model_refused", "planner_incomplete", "invalid_plan",
                           "model_client_unavailable", "model_budget_missing", "candidate_search_failed",
                           "soft_search_failed", "invalid_soft_queries"}
        assert value.get("assistance", {}).get("reason") not in failure_reasons, "assistance failed"
        assert value.get("status") not in ("recall_failed", "failed"), "tool stage failed"
        for key, child in value.items():
            if key not in ("content", "text", "quote"):
                runtime_checks(child)
    elif isinstance(value, list):
        for child in value:
            runtime_checks(child)


def check_task_visibility(result, query, sources, source_ids):
    wrong_refs = {s["id"] for s in sources.values() if s.get("task_id") and s["task_id"] != query.get("task_id")}
    wrong_ids = {source_ids[s["id"]] for s in sources.values() if s.get("task_id")
                 and s["task_id"] != query.get("task_id") and s["id"] in source_ids}
    exposed = []
    def walk(value):
        if isinstance(value, dict):
            for key, child in value.items():
                if key == "source_id" and child in wrong_ids:
                    exposed.append(child)
                elif key == "turn_id" and child in wrong_refs:
                    exposed.append(child)
                elif key == "Task" and isinstance(child, str) and child != query.get("task_id"):
                    exposed.append(child)
                elif key in ("source_ids", "safe_source_ids", "approved_source_ids") and isinstance(child, list):
                    exposed.extend(item for item in child if item in wrong_ids)
                else:
                    walk(child)
        elif isinstance(value, list):
            for child in value:
                walk(child)
    walk(result)
    return list(set(exposed))


def source_request(scenario, source):
    earlier = scenario["sources"][:scenario["sources"].index(source)]
    turn = {"text": source["text"], "session_id": f"{OWNER}-{scenario['id']}",
            "turn_id": source["id"], "capture_source": True,
            "history": [row["text"] for row in earlier if row.get("task_id") == source.get("task_id")]}
    if "task_id" in source:
        turn["task_id"] = source["task_id"]
    return {"user_turn": turn}


def query_request(query):
    return {key: query[key] for key in ("query", "context", "task_id") if key in query}


def call(client, method, arguments, artifact):
    """Only in-progress prepare receipts may be waited on, with identical arguments."""
    deadline = time.monotonic() + 900
    count = 0
    while True:
        count += 1
        poll_path = artifact.with_name(artifact.stem + f"-poll-{count:02d}.json")
        assert not poll_path.exists(), "existing receipt: do not replay the operation"
        raw = client.rpc("tools/call", {"name": method, "arguments": arguments})
        save(poll_path, raw, exclusive=True)
        assert not raw.get("isError"), "MCP tool returned an error; inspect raw receipt"
        result = raw.get("structuredContent") or json.loads(raw["content"][0]["text"])
        if result.get("status") != "in_progress":
            return result
        assert method == "memory_prepare" and time.monotonic() < deadline, "operation did not finish"
        time.sleep(2)


def summary(report):
    counts = {phase: dict(Counter(row["status"] for row in report["rows"] if row["phase"] == phase))
              for phase in ("intake", "recall", "validate")}
    return {"status": report["status"], "completed_steps": len(report["rows"]),
            "total_steps": 68, "counts": counts,
            "physical_requests": report.get("accounting", {}).get("physical_requests"),
            "last": report["rows"][-1] if report["rows"] else None}


def preflight(output):
    runtime = frozen()
    inputs, labels, budgets = [read(PACK / name) for name in ("inputs.json", "labels.json", "budgets.json")]
    PREP.static_check(inputs, labels, budgets)
    check = read(ROOT / "docs/refactor/s6/runner-v1-20260927/offline-check.json")
    assert check["status"] == "passed" and check["real_model_requests"] == 0
    dependencies = [Path(__file__), ROOT / "tests/s6_holdout_scoring.py", ROOT / "tests/s6_holdout_runner_check.py",
                    ROOT / "tests/s5_original_n05_fullchain.py", ROOT / "tests/mcp_smoke.py", ROOT / "tests/s5_normal_intake.py"]
    assert check["fingerprints"] == {str(path.relative_to(ROOT)): digest(path) for path in dependencies}
    output.mkdir(parents=True, mode=0o700, exist_ok=False)
    operations = plan(inputs)
    assert Counter(op["phase"] for op in operations) == {"intake": 16, "recall": 36, "validate": 16}
    configs = {}
    for scenario in inputs["scenarios"]:
        directory = output / scenario["id"]
        directory.mkdir(mode=0o700)
        (directory / "trace").mkdir(mode=0o700)
        for phase in ("intake", "recall", "validate"):
            path = settings(directory, phase, **PREP.phase_config(budgets, scenario["id"], phase))
            configs[str(path.relative_to(output))] = digest(path)
    manifest = {"created_at_utc": now(), "output": str(output), "owner": OWNER,
                "dataset_freeze_sha256": digest(PACK / "dataset-freeze.json"), "runtime": runtime,
                "fingerprints": check["fingerprints"], "offline_check_sha256": digest(ROOT / "docs/refactor/s6/runner-v1-20260927/offline-check.json"),
                "budgets": budgets, "operations": operations, "configs": configs, "preflight": "passed"}
    save(output / "manifest.json", manifest, exclusive=True)
    save(output / "report.json", {"status": "ready", "rows": [], "source_ids": {}, "blocked_scenarios": [],
                                  "fixed_denominators": {"intake": 16, "recall": 36, "validate": 16}}, exclusive=True)
    print(json.dumps({"status": "preflight_passed", "operations": len(operations), "real_model_requests": 0}))


def verify(output, manifest):
    assert str(output) == manifest["output"] and manifest["owner"] == OWNER
    assert digest(PACK / "dataset-freeze.json") == manifest["dataset_freeze_sha256"]
    assert frozen() == manifest["runtime"]
    for path, fingerprint in manifest["fingerprints"].items():
        assert digest(ROOT / path) == fingerprint, f"runner changed: {path}"
    for path, fingerprint in manifest["configs"].items():
        assert digest(output / path) == fingerprint, f"configuration changed: {path}"
    assert manifest["budgets"] == read(PACK / "budgets.json")
    authorization = read(output / "authorization.json")
    assert authorization["manifest_sha256"] == digest(output / "manifest.json")
    assert authorization["output"] == str(output) and authorization["max_physical_requests"] == 84
    assert authorization["provider_base_url"] == manifest["budgets"]["provider_base_url"]
    assert authorization["automatic_retries"] == 0 and authorization["user_reply"]
    window = manifest["budgets"]["execution_window"]
    assert datetime.datetime.fromisoformat(window["start_inclusive"]) <= datetime.datetime.now(datetime.timezone.utc) < datetime.datetime.fromisoformat(window["end_exclusive"])


def apply_review(output, report):
    last = report["rows"][-1]
    raw_path = output / last["artifact"]
    review = read(raw_path.with_name(raw_path.stem + "-review.json"))
    assert review["artifact_sha256"] == digest(raw_path) and review["reason"].strip()
    assert review["id"] == last["id"] and review["phase"] == last["phase"]
    assert review["verdict"] in ("passed", "quality_gap", "safety_error")
    last["review"] = review
    last["status"] = review["verdict"]
    if last["status"] == "safety_error":
        report["status"] = "stopped"
    else:
        report["status"] = "ready"
        if last["phase"] == "intake" and last["status"] == "quality_gap":
            report["blocked_scenarios"].append(last["scenario"])


def step(output):
    with (output / "runner.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        manifest, report = read(output / "manifest.json"), read(output / "report.json")
        verify(output, manifest)
        assert report["status"] in ("ready", "review_required"), "run already stopped or complete; do not restart"
        for index, row in enumerate(report["rows"]):
            assert all(row[key] == manifest["operations"][index][key] for key in ("id", "scenario", "phase"))
            if "artifact" in row:
                assert digest(output / row["artifact"]) == row["artifact_sha256"], "receipt changed"
        if report["status"] == "review_required":
            apply_review(output, report)
        if report["status"] == "stopped":
            save(output / "report.json", report)
            print(json.dumps(summary(report), ensure_ascii=False))
            return
        index = len(report["rows"])
        if index == len(manifest["operations"]):
            report["status"] = "passed" if all(r["status"] == "passed" for r in report["rows"]) else "quality_gaps"
            save(output / "report.json", report)
            print(json.dumps(summary(report), ensure_ascii=False))
            return
        op = manifest["operations"][index]
        inputs, labels = read(PACK / "inputs.json"), read(PACK / "labels.json")
        scenario = next(s for s in inputs["scenarios"] if s["id"] == op["scenario"])
        directory = output / scenario["id"]
        db = directory / "business.sqlite3"
        artifact = directory / f"{index:02d}-{op['id']}.json"
        row = {**op, "started_at_utc": now()}
        try:
            report["accounting"] = audit(output, manifest["budgets"])
            if op["phase"] != "intake" and scenario["id"] in report["blocked_scenarios"]:
                row["status"] = "blocked_by_upstream"
            else:
                cap = 2 if op["phase"] == "intake" else 1
                assert report["accounting"]["reserved_calls"] + cap <= 84
                if op["phase"] == "intake" and op["after_source_count"] == 1:
                    assert not db.exists(), "first source requires a fresh scenario database"
                else:
                    assert db.exists(), "never recreate a missing scenario database"
                env = {k: v for k, v in clean_env("live").items() if not k.startswith("ARK_")}
                env["PMR_TRACE_DIR"] = str(directory / "trace")
                # A crash after this marker can never replay the operation automatically.
                artifact.with_suffix(".started").touch(exist_ok=False)
                with contextlib.closing(LongClient(manifest["runtime"]["binary"]["path"], db, env, OWNER,
                                                   config=directory / f"{op['phase']}.json")) as client:
                    bootstrap = client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
                    assert bootstrap["model_calls"] == []
                    if op["phase"] == "intake" and op["after_source_count"] == 1:
                        for entity in scenario["entities"]:
                            client.call("memory_entity_upsert", entity)
                    collection = {"intake": "sources", "recall": "queries", "validate": "validations"}[op["phase"]]
                    case = next(item for item in scenario[collection] if item["id"] == op["id"])
                    if op["phase"] == "intake":
                        request, method = source_request(scenario, case), "memory_prepare"
                    else:
                        query = case if op["phase"] == "recall" else next(q for q in scenario["queries"] if q["id"] == case["query_id"])
                        request, method = query_request(query), "memory_recall"
                        if op["phase"] == "validate":
                            request.update(draft_answer=case["draft_answer"], source_ids=[report["source_ids"][ref] for ref in case["source_refs"]])
                            method = "memory_validate_answer"
                    before = profile(db)
                    result = call(client, method, request, artifact)
                    state = profile(db)
                    client.errors.seek(0)
                    stderr = client.errors.read()
                    data = {"request": request, "result": result, "before_revision": before["revision"],
                            "before_source_revision": before["source_revision"], "completed_at_utc": now(), "stderr": stderr}
                    if op["phase"] == "intake":
                        data["profile"] = state
                    save(artifact, data, exclusive=True)
                row.update(artifact=str(artifact.relative_to(output)), artifact_sha256=digest(artifact))
                report["accounting"] = audit(output, manifest["budgets"])
                assert "evidence_incomplete" not in stderr, "trace write failed"
                runtime_checks(result)
                sources = {s["id"]: {**s, "label": labels["intake"][s["id"]]} for s in scenario["sources"]}
                if op["phase"] == "intake":
                    saved = receipt(result)
                    record = next(s for s in state["sources"].values() if s["turn_id"] == case["id"])
                    assert record["request"] == {**request["user_turn"], "task_id": request["user_turn"].get("task_id")}
                    report["source_ids"][case["id"]] = saved["source"]["source_id"]
                    memories = state["repository"]["memories"]
                    memories = list(memories.values()) if isinstance(memories, dict) else memories
                    score = score_intake(scenario, case, saved, memories, labels,
                                         source_record={**record["request"], "source_id": record["id"], "enrichment": record["enrichment"]})
                    assert result["recall"]["status"] == "completed" and result["recall"]["result"]["model_calls"] == []
                    row["observed"] = {"remember": saved, "memories": [m for m in memories if any(e["turn_id"] == case["id"] for e in m.get("evidence", []))]}
                else:
                    assert (before["revision"], before["source_revision"]) == (state["revision"], state["source_revision"]), "query changed profile"
                    snapshot = {"read_revision": state["revision"], "source_revision": state["source_revision"]}
                    if op["phase"] == "recall":
                        score = score_contract(result["answer_contract"], query, labels["queries"][case["id"]], report["source_ids"], sources, expected_revision=snapshot)
                    else:
                        prior = next(r for r in report["rows"] if r["id"] == case["query_id"])
                        prior_contract = read(output / prior["artifact"])["result"]["answer_contract"]
                        revision = {key: prior_contract[key] for key in ("read_revision", "source_revision")}
                        assert revision == snapshot, "validation not at the query's actual snapshot"
                        score = score_validate(result, case, query, labels["queries"][query["id"]], labels["validations"][case["id"]],
                                               report["source_ids"], sources, expected_revision=revision)
                    if check_task_visibility(result, query, sources, report["source_ids"]):
                        score["safety_error"].append("task_leak: out-of-scope source exposed outside answer_contract")
                    contract = result["answer_contract"] if op["phase"] == "recall" else result
                    row["observed"] = {key: contract.get(key) for key in ("status", "safe_answer", "safe_source_ids", "sources", "approved", "approved_source_ids")}
                row["score"] = score
                # Every actual result is inspected before the next frozen operation.
                row["status"] = "safety_error" if score["safety_error"] else "review_required"
                if row["status"] == "safety_error":
                    report["status"] = "stopped"
                elif row["status"] == "review_required":
                    report["status"] = "review_required"
                elif row["status"] == "quality_gap" and op["phase"] == "intake":
                    report["blocked_scenarios"].append(op["scenario"])
        except Exception as error:
            row["status"] = "runtime_error"
            row["error"] = type(error).__name__ + ": " + str(error)
            report["status"] = "stopped"
        finally:
            row["completed_at_utc"] = now()
            report["rows"].append(row)
            report["formal_baseline_unchanged"] = PREP.formal_state() == manifest["runtime"]["formal_baseline"]
            if not report["formal_baseline_unchanged"]:
                report["status"] = "stopped"
            try:
                report["accounting"] = audit(output, manifest["budgets"])
            except Exception as error:
                report["status"] = "stopped"
                report["accounting_error"] = str(error)
                if (output / "accounting.json").exists():
                    report["accounting"] = read(output / "accounting.json")
            save(output / "report.json", report)
        print(json.dumps(summary(report), ensure_ascii=False), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("preflight", "step", "status"))
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    os.umask(0o077)
    output = args.output.resolve()
    if args.phase == "preflight":
        preflight(output)
    elif args.phase == "step":
        step(output)
    else:
        print(json.dumps(summary(read(output / "report.json")), ensure_ascii=False))
