#!/usr/bin/env python3
"""Audit captured CUA web results. This script never sends chats or fabricates observations."""
import argparse
import copy
import json
from pathlib import Path


def injected_bootstrap_error(call):
    return (call["tool"] == "memory_recall" and call["arguments"].get("bootstrap")
            and call["status"] == "failed"
            and str(call["result"]["structured_content"].get("error", "")).startswith("ACCEPTANCE_TEST_BOOTSTRAP_UNAVAILABLE:"))


def audit_case(row):
    calls = row["calls"]
    bootstrap = any(c["tool"] == "memory_recall" and c["arguments"].get("bootstrap")
                    and c["status"] == "completed" for c in calls)
    errors, checked = [], 0
    for call in calls:
        if row["case"].get("unavailable"):
            continue  # The existing fault replaces the entire service response.
        if row["case"].get("global_context_test") and injected_bootstrap_error(call):
            continue  # Separately reported controlled startup failure, before the real service.
        checked += 1
        context = call["result"]["structured_content"].get("global_context", {})
        if context.get("status") != "available":
            errors.append(f"{call['tool']}: global requirements not delivered")
            continue
        for lane in ("mandatory", "constraints", "preferences", "facts", "episodes"):
            for memory in context["result"][lane]:
                if (memory["owner_id"] != "auto-test-user" or memory["subject_id"] != "self"
                        or memory["scope"] != {"domain": None, "conditions": []}
                        or memory["status"] != "Active"):
                    errors.append(f"{call['tool']}: unrelated/inactive requirement {memory['id']}")
    return dict(bootstrap_succeeded=bootstrap, delivery_errors=errors,
                delivery_checked_calls=checked,
                original_status=row["status"], original_failures=row["failures"],
                chat_url=row["observation"]["chat_url"])


def audit_target(row, before=None, after=None):
    result = audit_case(row)
    test = row["case"]["global_context_test"]
    marker, mode = test["marker"], test["mode"]
    errors = list(result["delivery_errors"])
    remaining = list(row["failures"])
    control_failures = []
    for call in row["calls"]:
        if injected_bootstrap_error(call):
            for failure in (f"tool error: {call['tool']}: {call.get('error')}", f"tool result error: {call['tool']}"):
                if failure in remaining:
                    remaining.remove(failure)
                    control_failures.append(failure)
    # Keep the original check and its result visible. This additional test intentionally
    # has no successful bootstrap and judges the fallback, not the startup requirement.
    missing = "new chat did not execute offline bootstrap"
    if missing in remaining:
        remaining.remove(missing)
        control_failures.append(missing)
    errors.extend(remaining)
    if result["bootstrap_succeeded"]:
        errors.append("a successful bootstrap exposed the rule; fallback was not isolated")
    if not all(row.get(f"fault_flags_{when}", {}).get("simulate-bootstrap-unavailable") for when in ("before", "after")):
        errors.append("controlled startup fault was not recorded throughout the test")
    exposures = []
    for call in row["calls"]:
        content = call["result"]["structured_content"]
        context_has = marker in json.dumps(content.get("global_context", {}), ensure_ascii=False)
        primary_has = marker in json.dumps({k: v for k, v in content.items() if k != "global_context"}, ensure_ascii=False)
        if context_has or primary_has:
            exposures.append(dict(tool=call["tool"], context_has=context_has, primary_has=primary_has))
    answer = row["observation"]["answer"].strip()
    suffix_applied = answer.rstrip("。.!！ \n").endswith(marker)
    if mode == "remember_hidden":
        if not exposures or exposures[0] != dict(tool="memory_remember", context_has=True, primary_has=False):
            errors.append("hidden rule was not first exposed solely through the remember response context")
        if not suffix_applied:
            errors.append("final answer did not apply the hidden suffix rule")
    elif mode == "override":
        if not exposures:
            errors.append("stored rule was not delivered, so precedence was not tested")
        if answer != "收到":
            errors.append("final answer did not obey the explicit current-turn-only reply")
    elif mode == "forget":
        forgets = [c for c in row["calls"] if c["tool"] == "memory_forget" and c["status"] == "completed"]
        if not forgets or marker in json.dumps(forgets[-1]["result"]["structured_content"].get("global_context", {}), ensure_ascii=False):
            errors.append("forget response still contains the deleted global rule")
        if suffix_applied:
            errors.append("final answer applied the rule after forgetting it")
    elif mode == "after_forget":
        if exposures or suffix_applied:
            errors.append("a new chat still received/applied the forgotten global rule")
    else:
        errors.append("unknown additional test mode")
    if mode != "forget" and before is not None and after is not None:
        cities = [m for m in before["repository"]["memories"].values()
                  if m["status"] == "Active" and m["subject_id"] == "self"
                  and m["predicate"] == "lives_in" and m["value"] == {"Text": "苏州"}]
        deltas = {str(m["id"]): after["repository"]["memories"][str(m["id"])]["reinforcement_count"] - m["reinforcement_count"]
                  for m in cities}
        result["sqlite_reinforcement_deltas"] = deltas
        if len(deltas) != 1 or list(deltas.values()) != [1]:
            errors.append("SQLite did not independently confirm exactly one reinforcement")
    result["remember_session_ids"] = [c["arguments"]["session_id"] for c in row["calls"] if c["tool"] == "memory_remember"]
    result.update(mode=mode, expected_control_failures=control_failures,
                  first_marker_exposure=exposures[0] if exposures else None,
                  suffix_applied=suffix_applied, target_errors=errors,
                  target_status="failed" if errors else "passed")
    return result


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--root", type=Path, default=Path("docs/verification/global-context-2026-09-08/web"))
    p.add_argument("--partial", action="store_true")
    p.add_argument("--selfcheck", action="store_true")
    args = p.parse_args()
    if args.selfcheck:
        row = json.loads((args.root / "web-05.result.json").read_text())
        assert not audit_case(row)["delivery_errors"]
        missing = copy.deepcopy(row)
        del missing["calls"][-1]["result"]["structured_content"]["global_context"]
        assert audit_case(missing)["delivery_errors"]
        scoped = copy.deepcopy(row)
        scoped["calls"][-1]["result"]["structured_content"]["global_context"]["result"]["mandatory"][0]["scope"]["conditions"] = [{"Task": "wrong-project"}]
        assert audit_case(scoped)["delivery_errors"]
        no_bootstrap = copy.deepcopy(row)
        no_bootstrap["calls"] = [c for c in row["calls"] if not c["arguments"].get("bootstrap")]
        no_bootstrap.update(status="failed", failures=["new chat did not execute offline bootstrap"])
        r = audit_case(no_bootstrap)
        assert not r["bootstrap_succeeded"] and not r["delivery_errors"] and r["original_status"] == "failed"
        # Artificial verifier inputs only; never emitted as web execution evidence.
        target = copy.deepcopy(no_bootstrap)
        target["calls"] = [copy.deepcopy(next(c for c in row["calls"] if c["tool"] == "memory_remember"))]
        context = target["calls"][0]["result"]["structured_content"]["global_context"]
        target["calls"][0]["result"]["structured_content"] = {"saved": True, "global_context": context}
        target["case"]["global_context_test"] = dict(mode="remember_hidden", marker="中文")
        target["fault_flags_before"] = target["fault_flags_after"] = {"simulate-bootstrap-unavailable": True}
        target["observation"]["answer"] = "已保存。中文"
        assert audit_target(target)["target_status"] == "passed"
        target["observation"]["answer"] = "已保存。"
        assert audit_target(target)["target_status"] == "failed"
        target["observation"]["answer"] = "已保存。中文"
        target["calls"][0]["result"]["structured_content"]["primary_marker"] = "中文"
        assert audit_target(target)["target_status"] == "failed"
        print("Captured response passes; missing context and scoped contamination fail; fallback never erases bootstrap failure.")
        return 0
    results = {}
    for case in range(1, 53):
        path = args.root / f"web-{case:02}.result.json"
        if path.exists():
            results[f"{case:02}"] = audit_case(json.loads(path.read_text()))
    retests = {path.stem.removesuffix(".result").removeprefix("web-"): audit_case(json.loads(path.read_text()))
               for path in sorted(args.root.glob("web-[0-9][0-9]-r*.result.json"))}
    targets = {}
    for path in sorted(args.root.glob("web-fallback-*.result.json")):
        prefix = str(path).removesuffix(".result.json")
        targets[path.stem.removesuffix(".result").removeprefix("web-")] = audit_target(
            json.loads(path.read_text()), json.loads(Path(prefix + ".before.json").read_text())["state"],
            json.loads(Path(prefix + ".after.json").read_text()))
    sessions = []
    for key in (f"fallback-{i:02}" for i in range(1, 4)):
        if key in targets:
            for session in targets[key]["remember_session_ids"]:
                if session in sessions:
                    targets[key]["target_errors"].append("independent new chats reused a remember session ID")
                    targets[key]["target_status"] = "failed"
                sessions.append(session)
    latest = dict(results)
    for key, value in retests.items():
        latest[key.split("-")[0]] = value
    summary = dict(surface="captured ChatGPT web via native CUA", cases=len(results), expected=52,
                   original_passed=sum(r["original_status"] == "passed" for r in results.values()),
                   latest_passed=sum(r["original_status"] == "passed" for r in latest.values()),
                   results=results, retests=retests, targets=targets)
    print(json.dumps(summary, ensure_ascii=False, indent=2))
    if not args.partial:
        passed = (len(results) == 52
                  and all(r["original_status"] == "passed" and not r["delivery_errors"] for r in results.values())
                  and all(targets.get(f"fallback-{i:02}", {}).get("target_status") == "passed" for i in range(1, 7)))
        print(json.dumps({"acf_assertion": {"id": "web-regression", "outcome": "pass" if passed else "fail",
                                           "observed": json.dumps(summary, ensure_ascii=False)}}))
        return 0 if passed else 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
