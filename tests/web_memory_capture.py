#!/usr/bin/env python3
"""Capture real web MCP traffic; UI is driven separately through CUA.

The checker reuses domain assertions, normalizing captured RPCs for that checker.
These are web observations, never presented as Codex CLI execution records.
"""
import argparse
from collections import defaultdict, deque
import hashlib
import json
import os
import subprocess
import sys
import threading
import time
import tomllib
from pathlib import Path

from auto_memory_acceptance import CASES, evaluate, snapshot

UNAVAILABLE = "ACCEPTANCE_TEST_MEMORY_UNAVAILABLE"
BOOTSTRAP_UNAVAILABLE = "ACCEPTANCE_TEST_BOOTSTRAP_UNAVAILABLE: only startup recall is unavailable in this test; other memory tools remain available."


def bootstrap_failures(case, calls):
    if case.get("resume") or case.get("unavailable"):
        return []
    bootstrap = [c["tool"] == "memory_recall" and c.get("arguments", {}).get("bootstrap") is True for c in calls]
    if not any(bootstrap):
        return ["new chat did not execute offline bootstrap"]
    return [] if bootstrap[0] else ["new chat did not execute offline bootstrap before other memory tools"]


def evaluate_unavailable(calls, before, after, observation):
    failures = []
    if not observation["completed"]:
        failures.append("client turn did not complete")
    if before != after:
        failures.append("unavailable service changed persistent state")
    if not calls or any(c["status"] != "failed" or
            c["result"]["structured_content"].get("error") != UNAVAILABLE for c in calls):
        failures.append("expected injected MCP errors were not observed")
    if observation.get("reports_save_failure") is not True:
        failures.append("UI did not explicitly report that saving failed")
    return dict(status="failed" if failures else "passed", failures=failures,
                tools=[dict(tool=c["tool"], status=c["status"]) for c in calls],
                answer=observation["answer"], retrieval_path=None,
                evaluation_note="Web fault injection: tools remain discoverable, every call fails; unlike Codex case 33 where tools are disabled.")


def evaluate_source_capture(case, calls, before, after, observation):
    """Opt-in web source checks, independent of model enrichment success.

    Case files use source_capture: true/false. Negative cases are read-only by
    default; readonly: false permits fact writes but never source retention.
    A before/after instance represents one fresh user message, not a replay.
    """
    failures, enrichment = [], []
    result = dict(tools=[dict(tool=c.get("tool"), status=c.get("status")) for c in calls],
                  answer=observation.get("answer"), retrieval_path=None, enrichment=enrichment)
    expected = case.get("source_capture")
    prompt = case.get("prompt")
    if type(expected) is not bool or not isinstance(prompt, str) or not prompt.strip():
        failures.append("source_capture must be boolean and prompt must be nonempty text")
    if observation.get("completed") is not True:
        failures.append("client turn did not complete")
    if not isinstance(observation.get("answer"), str):
        failures.append("web answer was not captured")
    old = before.get("sources", {}) if isinstance(before, dict) else None
    new = after.get("sources", {}) if isinstance(after, dict) else None
    if not isinstance(old, dict) or not isinstance(new, dict) or any(
            not isinstance(record, dict) for records in (old, new) if isinstance(records, dict)
            for record in records.values()):
        failures.append("invalid source snapshot")
    elif type(expected) is bool:
        writes = [c for c in calls if c.get("tool") in ("memory_prepare", "memory_remember")]
        groups = {}
        for call in writes:
            arguments = call.get("arguments")
            turn = arguments.get("user_turn") if isinstance(arguments, dict) and call["tool"] == "memory_prepare" else arguments
            if not isinstance(turn, dict) or any(not isinstance(turn.get(field), str) or not turn[field].strip()
                                                  for field in ("session_id", "turn_id", "text")):
                failures.append("invalid source call arguments")
                continue
            if expected and (turn.get("capture_source") is not True or turn["text"] != prompt):
                failures.append("source call must explicitly retain the verbatim web message")
            if not expected and turn.get("capture_source") is True:
                failures.append("source capture attempted on a non-capture turn")
            key = json.dumps(["memory_remember", turn["session_id"], turn["turn_id"]],
                             ensure_ascii=False, separators=(",", ":"))
            group = groups.setdefault(key, [])
            if group and (call["tool"] != group[0]["tool"] or arguments != group[0]["arguments"]):
                failures.append("polling changed tool or arguments for the same turn")
            group.append(call)
        if expected:
            if len(groups) != 1:
                failures.append("expected exactly one fresh source turn")
            for key, group in groups.items():
                # Polls and retries share one logical source; only final output proves reception.
                body = (group[-1].get("result") or {}).get("structured_content")
                if isinstance(body, dict) and group[-1]["tool"] == "memory_prepare":
                    body = body.get("remember")
                    if isinstance(body, dict) and body.get("status") == "completed":
                        body = body.get("result")
                source = body.get("source") if isinstance(body, dict) else None
                part = body.get("enrichment") if isinstance(body, dict) else None
                if not isinstance(source, dict) or source.get("status") != "stored" or not isinstance(source.get("source_id"), str) or not source["source_id"]:
                    failures.append("final source receipt did not confirm stored")
                    continue
                if not isinstance(part, dict) or not isinstance(part.get("status"), str):
                    failures.append("final enrichment outcome was not recorded")
                enrichment.append(dict(key=key, source_id=source["source_id"],
                                       status=part.get("status") if isinstance(part, dict) else "unknown",
                                       error=part.get("error") if isinstance(part, dict) else None))
                record = new.get(key, {})
                request = record.get("request")
                args = group[-1]["arguments"]
                turn = args["user_turn"] if group[-1]["tool"] == "memory_prepare" else args
                if (key in old or source["source_id"] in {s.get("id") for s in old.values()}
                        or record.get("id") != source["source_id"] or record.get("session_id") != turn["session_id"]
                        or record.get("turn_id") != turn["turn_id"] or not isinstance(request, dict)
                        or request.get("text") != prompt or request.get("capture_source") is not True
                        or request.get("session_id") != turn["session_id"] or request.get("turn_id") != turn["turn_id"]):
                    failures.append("stored source is missing, old, or differs from the current web turn")
                if isinstance(part, dict) and record.get("enrichment") != part.get("status"):
                    failures.append("source and enrichment receipt disagree with the database")
            if set(new) - set(old) != set(groups) or any(new.get(key) != value for key, value in old.items()):
                failures.append("source snapshot does not contain exactly the new captured turn")
        else:
            if old != new:
                failures.append("non-capture turn changed persisted sources")
            if case.get("readonly", True):
                if any(c.get("tool") not in ("memory_recall", "memory_list") for c in calls):
                    failures.append("attempted write on non-save turn")
                for field in ("receipts", "staging", "repository"):
                    if field not in before or field not in after:
                        failures.append("missing read-only snapshot field: " + field)
                    elif field == "repository":
                        def without_read_counters(value):
                            # Recall may update counters; it may not create or alter a fact.
                            value = json.loads(json.dumps(value))
                            memories = value.get("memories")
                            if not isinstance(memories, dict):
                                raise ValueError("invalid memory snapshot")
                            for memory in memories.values():
                                memory.pop("retrieval_count", None)
                                memory.pop("last_retrieved_at", None)
                            return value
                        try:
                            if without_read_counters(before[field]) != without_read_counters(after[field]):
                                failures.append("non-save turn changed memory content")
                        except (AttributeError, TypeError, ValueError):
                            failures.append("invalid read-only memory snapshot")
                    elif before[field] != after[field]:
                        failures.append("non-save turn changed " + field)
    if case.get("no_tools") and calls:
        failures.append("unnecessary tool call")
    for tool in case.get("tools", []):
        if not any(c.get("tool") == tool for c in calls):
            failures.append("missing " + tool)
    result.update(status="failed" if failures else "passed", failures=failures,
                  source_capture=dict(expected=expected, status="failed" if failures else "passed"))
    return result


def write(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def correlate(row, pending, db, call_id):
    """Enrich existing wire rows; an emitted response is not host receipt proof."""
    body = row["body"]
    if row["direction"] == "request" and "id" in body:
        row["call_id"] = call_id
        pending[body["id"]].append(row)
    elif row["direction"] == "response" and pending.get(body.get("id")):
        request = pending[body["id"]].popleft()
        row["call_id"] = request["call_id"]
        params = request["body"].get("params", {})
        if isinstance(params, dict) and params.get("name") == "memory_prepare":
            arguments = params.get("arguments")
            turn = arguments.get("user_turn") if isinstance(arguments, dict) else None
            turn = turn if isinstance(turn, dict) else {}
            key = json.dumps(["memory_remember", turn.get("session_id"), turn.get("turn_id")],
                             ensure_ascii=False, separators=(",", ":"))
            receipt = {"key": key, "observed_at": time.time()}
            try:
                value = snapshot(db).get("receipts", {}).get(key)
                receipt.update(status="present" if value is not None else "absent", value=value)
            except Exception as error:
                # A failed observation must not hide or alter the actual MCP response.
                receipt.update(status="unavailable", error=str(error))
            row["prepare_receipt"] = receipt
            row["visibility"] = "server_stdio_output; host receipt and internal input unobservable"
    return row


def serve(root, model_network=None):
    with (Path.home() / ".codex/config.toml").open("rb") as stream:
        config = tomllib.load(stream)["mcp_servers"]["personal_memory"]
    env = dict(os.environ, **config.get("env", {}))
    # This wrapper serves synthetic acceptance data only. Keep backend query
    # decoding evidence so an empty web recall can be diagnosed after the turn.
    (root / "traces").mkdir(mode=0o700, exist_ok=True)
    env.update(PMR_OPENAI_TRACE="1", PMR_OPENAI_TRACE_CONTENT="1",
               PMR_TRACE_DIR=str(root / "traces"))
    if model_network is not None:
        env["PMR_MODEL_NETWORK"] = model_network
        write(root / f"server-{os.getpid()}.config.json",
              dict(owner="auto-test-user", db=str(root / "web.sqlite3"), model_network=model_network))
    with (root / f"rpc-{os.getpid()}.jsonl").open("a") as log, (root / f"server-{os.getpid()}.stderr").open("a") as err:
        child = subprocess.Popen([config["command"], "--db", str(root / "web.sqlite3"), "--owner", "auto-test-user"],
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err, text=True, env=env)
        lock = threading.Lock()
        pending = defaultdict(deque)
        sequence = 0

        def record(direction, line):
            nonlocal sequence
            with lock:
                sequence += 1
                row = correlate(dict(time=time.time(), direction=direction, body=json.loads(line)),
                                pending, root / "web.sqlite3", f"{os.getpid()}:{sequence}")
                log.write(json.dumps(row, ensure_ascii=False) + "\n")
                log.flush()

        def responses():
            for line in child.stdout:
                record("response", line)
                sys.stdout.write(line)
                sys.stdout.flush()

        reader = threading.Thread(target=responses)
        reader.start()
        try:
            for line in sys.stdin:
                record("request", line)
                request = json.loads(line)
                is_bootstrap = request.get("params", {}).get("name") == "memory_recall" and request.get("params", {}).get("arguments", {}).get("bootstrap") is True
                unavailable = (root / "simulate-unavailable").exists() or (is_bootstrap and (root / "simulate-bootstrap-unavailable").exists())
                if request.get("method") == "tools/call" and unavailable:
                    error = UNAVAILABLE if (root / "simulate-unavailable").exists() else BOOTSTRAP_UNAVAILABLE
                    result = dict(isError=True, structuredContent={"error": error},
                                  content=[dict(type="text", text=json.dumps({"error": error}))])
                    response = json.dumps(dict(jsonrpc="2.0", id=request["id"], result=result)) + "\n"
                    record("response", response)
                    sys.stdout.write(response)
                    sys.stdout.flush()
                    continue
                child.stdin.write(line)
                child.stdin.flush()
        finally:
            child.stdin.close()
            child.wait()
            reader.join()
        return child.returncode


def selfcheck_source_capture():
    from copy import deepcopy
    import sqlite3
    import tempfile

    prompt = "样例用户最近改用中文，想了解如何设置。"
    turn = dict(text=prompt, session_id="source-selfcheck", turn_id="1", capture_source=True)
    key = json.dumps(["memory_remember", turn["session_id"], turn["turn_id"]], separators=(",", ":"))
    before = dict(sources={}, repository={"memories": {}}, receipts={}, staging={"pending": {}})
    source = dict(id="new-source", session_id=turn["session_id"], turn_id=turn["turn_id"],
                  request=turn, enrichment="extraction_incomplete")
    after = dict(before, sources={key: source})
    body = dict(source={"status": "stored", "source_id": source["id"]},
                enrichment={"status": "extraction_incomplete", "error": "extraction_incomplete"})
    final = dict(tool="memory_prepare", arguments={"user_turn": turn}, status="completed",
                 result={"structured_content": {"remember": dict(body, status="failed"),
                                                "recall": {"status": "failed"}}})
    pending = dict(final, result={"structured_content": {"status": "in_progress"}})
    case = dict(prompt=prompt, source_capture=True)
    observation = dict(completed=True, answer="原文已留存，整理失败。")

    def check(calls=None, old=None, new=None, current_case=None):
        return evaluate_source_capture(current_case or case, [pending, final] if calls is None else calls,
                                       before if old is None else old, after if new is None else new, observation)

    passed = check()
    assert passed["status"] == "passed" and len(passed["enrichment"]) == 1
    assert passed["enrichment"][0]["status"] == "extraction_incomplete"
    assert check(calls=[dict(final, tool="memory_remember", arguments=turn,
                            status="failed", result={"structured_content": body})])["status"] == "passed"
    for field, value in (("capture_source", False), ("text", "改写的原话"), ("session_id", None)):
        broken = deepcopy(final)
        broken["arguments"]["user_turn"][field] = value
        assert check(calls=[broken])["status"] == "failed", field
    assert check(calls=[pending])["status"] == "failed"
    assert check(calls=[dict(final, result={"structured_content": {}})])["status"] == "failed"
    assert check(new=before)["status"] == "failed"
    assert check(old=after)["status"] == "failed"
    old_id = dict(before, sources={"other-turn": source})
    assert check(old=old_id, new=dict(after, sources={**old_id["sources"], **after["sources"]}))["status"] == "failed"
    broken_source = deepcopy(after)
    broken_source["sources"][key]["request"]["text"] = "另一条原话"
    assert check(new=broken_source)["status"] == "failed"
    for malformed in ({"sources": []}, {"sources": {key: None}}):
        assert check(new=malformed)["status"] == "failed"
    for malformed in (dict(case, source_capture="true"), dict(case, prompt="")):
        assert check(current_case=malformed)["status"] == "failed"
    negative = dict(case, source_capture=False, readonly=True)
    recall = dict(tool="memory_recall", arguments={"query": prompt}, status="completed")
    assert check(calls=[recall], new=before, current_case=negative)["status"] == "passed"
    assert check(calls=[recall], current_case=negative)["status"] == "failed"
    assert check(calls=[final], new=before, current_case=negative)["status"] == "failed"
    mutated = deepcopy(before)
    mutated["receipts"][key] = {}
    assert check(calls=[recall], new=mutated, current_case=negative)["status"] == "failed"
    assert check(calls=[], new={}, current_case=negative)["status"] == "failed"
    with tempfile.TemporaryDirectory() as directory:
        db = Path(directory) / "web.sqlite3"
        with sqlite3.connect(db) as conn:
            conn.execute("CREATE TABLE memory_profiles(owner_id TEXT, state_json TEXT)")
            conn.executemany("INSERT INTO memory_profiles VALUES (?, ?)",
                             [("decoy-owner", json.dumps(before)), ("auto-test-user", json.dumps(after))])
        assert check(new=snapshot(db))["status"] == "passed"
    print("Source capture checker: stored source survives failed enrichment; polls count once; false flag, rewritten text, missing receipt, missing/old source, malformed evidence and forbidden writes fail. Fixed-owner SQLite snapshot checked; no network.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["serve", "before", "after", "selfcheck"])
    parser.add_argument("root", type=Path)
    parser.add_argument("case", nargs="?")
    parser.add_argument("--instance")
    parser.add_argument("--case-file", type=Path, help="Explicit additional scenario; original 52 cases stay unchanged")
    parser.add_argument("--observation", type=Path, help="CUA-observed answer, chat_url, completed and confirmation_clicks JSON")
    parser.add_argument("--model-network", choices=["deny"], help="serve only: disable paid backend calls while observing real web tool choices")
    args = parser.parse_args()
    if args.model_network is not None and args.action != "serve":
        parser.error("--model-network only applies to serve")
    root = args.root.resolve()
    if args.action == "selfcheck":
        import sqlite3
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            db = Path(directory) / "test.sqlite3"
            turn = dict(text="样例原话", session_id="新聊天", turn_id="1")
            key = json.dumps(["memory_remember", "新聊天", "1"], ensure_ascii=False, separators=(",", ":"))
            receipt = dict(request=turn, result={"outcomes": ["fixture"]})
            with sqlite3.connect(db) as conn:
                conn.execute("CREATE TABLE memory_profiles(owner_id TEXT, state_json TEXT)")
                conn.execute("INSERT INTO memory_profiles VALUES (?,?)", ("auto-test-user", json.dumps({"receipts": {key: receipt}})))
            pending = defaultdict(deque)
            def request(number, call_id):
                return correlate(dict(direction="request", body=dict(id=number, params=dict(name="memory_prepare", arguments={"user_turn": turn}))), pending, db, call_id)
            def response(number, result):
                return correlate(dict(direction="response", body=dict(id=number, result=result)), pending, db, "unused")
            a, b = request(1, "a"), request(2, "b")
            rb, ra = response(2, {"status": "in_progress"}), response(1, {"remember": {"status": "completed"}})
            assert rb["call_id"] == b["call_id"] and ra["call_id"] == a["call_id"]
            assert ra["prepare_receipt"]["value"] == receipt and rb["body"]["result"]["status"] == "in_progress"
            assert "unobservable" in ra["visibility"]
            request(1, "retry")
            assert response(1, {})["call_id"] == "retry"
            turn["turn_id"] = "pure-question"
            request(3, "empty")
            assert response(3, {})["prepare_receipt"]["status"] == "absent"
            db.write_text("unreadable database")
            request(4, "error")
            assert response(4, {})["prepare_receipt"]["status"] == "unavailable"
            assert "call_id" not in response(99, {})
            for invalid in (None, [], {"user_turn": None}, {"user_turn": []}):
                correlate(dict(direction="request", body=dict(id=5, params=dict(name="memory_prepare", arguments=invalid))), pending, db, "invalid")
                assert response(5, {})["body"]["result"] == {}
        print(json.dumps({"acf_assertion": {"id": "prepare-correlation", "outcome": "pass", "observed": "SQLite receipt linked to wire args/output; out-of-order calls, retries, pending output, missing receipt, unavailable DB and unmatched response checked"}}))
        calls = [dict(tool="memory_remember", status="failed", result={"structured_content": {"error": UNAVAILABLE}})]
        obs = dict(completed=True, reports_save_failure=True, answer="保存失败")
        assert evaluate_unavailable(calls, {}, {}, obs)["status"] == "passed"
        assert evaluate_unavailable(calls, {}, {"unexpected": "write"}, obs)["status"] == "failed"
        assert evaluate_unavailable([], {}, {}, obs)["status"] == "failed"
        assert evaluate_unavailable(calls, {}, {}, dict(obs, reports_save_failure=False))["status"] == "failed"
        print("Unavailable-service checker: expected error passes; state mutation, missing RPC and false save claim fail.")
        bootstrap = dict(tool="memory_recall", arguments={"bootstrap": True})
        prepare = dict(tool="memory_prepare", arguments={})
        assert bootstrap_failures({}, [bootstrap, prepare]) == []
        assert bootstrap_failures({}, [prepare, bootstrap]) == ["new chat did not execute offline bootstrap before other memory tools"]
        for calls in ([], [prepare], [dict(bootstrap, arguments={"bootstrap": False})]):
            assert bootstrap_failures({}, calls) == ["new chat did not execute offline bootstrap"]
        for exception in ("resume", "unavailable"):
            assert bootstrap_failures({exception: True}, [prepare]) == []
        print("Bootstrap-order checker: startup recall must be the first memory call; missing and late bootstrap fail; resume and unavailable cases remain exempt.")
        selfcheck_source_capture()
        return 0
    if args.action == "serve":
        return serve(root, args.model_network)
    web_cases = CASES + [dict(id="52", prompt="Please explain in one sentence how to calculate 17 plus 25.",
                             tools=["memory_recall"], answer="42", chinese_progress=True, readonly=True)]
    case = (json.loads(args.case_file.read_text()) if args.case_file
            else next(c for c in web_cases if c["id"] == args.case))
    prefix = root / ("web-" + (args.instance or case["id"]))
    if args.action == "before":
        assert not prefix.with_suffix(".before.json").exists(), "Use a fresh instance for a retest"
        write(prefix.with_suffix(".before.json"), dict(time=time.time(), state=snapshot(root / "web.sqlite3"), case=case,
              fault_flags={name: (root / name).exists() for name in ("simulate-unavailable", "simulate-bootstrap-unavailable")}))
        print(json.dumps(case, ensure_ascii=False))
        return 0
    assert not prefix.with_suffix(".result.json").exists(), "Preserve results; use a fresh instance for a retest"
    finished_at = time.time()
    before = json.loads(prefix.with_suffix(".before.json").read_text())
    if "source_capture" in case:
        assert before["case"] == case, "Source acceptance case changed after the before snapshot"
    observation = json.loads(args.observation.read_text())
    calls = []
    for path in sorted(root.glob("rpc-*.jsonl")):
        rows = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
        pending, pairs = defaultdict(deque), []
        for row in rows:
            body = row["body"]
            if row["direction"] == "request" and "id" in body:
                pending[body["id"]].append(row)
            elif row["direction"] == "response" and pending[body.get("id")]:
                pairs.append((pending[body["id"]].popleft(), body))
        pairs.extend((request, {}) for queue in pending.values() for request in queue)
        for row, response in pairs:
            request = row["body"]
            if not before["time"] <= row["time"] <= finished_at or request.get("method") != "tools/call":
                continue
            result = response.get("result", {})
            structured = result.get("structuredContent")
            if structured is None and result.get("content"):
                structured = json.loads(result["content"][0]["text"])
            calls.append(dict(type="mcp_tool_call", tool=request["params"]["name"], arguments=request["params"].get("arguments", {}),
                              status="completed" if response and not response.get("error") and not result.get("isError") else "failed",
                              error=response.get("error"), result={"structured_content": structured or {}}, wire_source=path.name, time=row["time"]))
    calls.sort(key=lambda c: c["time"])
    normalized = [dict(type="item.completed", item=c) for c in calls]
    if "first_visible_message" in observation:
        normalized.append(dict(type="item.completed", item=dict(type="agent_message", text=observation["first_visible_message"])))
    normalized.append(dict(type="item.completed", item=dict(type="agent_message", text=observation["answer"])))
    if observation["completed"]:
        normalized.append(dict(type="turn.completed"))
    after = snapshot(root / "web.sqlite3")
    result = (evaluate_source_capture(case, calls, before["state"], after, observation) if "source_capture" in case
              else evaluate_unavailable(calls, before["state"], after, observation) if case.get("unavailable")
              else evaluate(case, normalized, before["state"], after, 0))
    if observation["confirmation_clicks"]:
        result["failures"].append("required per-call UI confirmation")
    if case.get("chinese_progress") and "first_visible_message" not in observation:
        result["failures"].append("first visible message was not captured")
    result["failures"].extend(bootstrap_failures(case, calls))
    result["status"] = "failed" if result["failures"] else "passed"
    result.update(surface=observation.get("surface", "ChatGPT web via native CUA"), case=case, observation=observation, calls=calls,
                  fault_flags_before=before.get("fault_flags", {}),
                  fault_flags_after={name: (root / name).exists() for name in ("simulate-unavailable", "simulate-bootstrap-unavailable")},
                  started_at=before["time"], finished_at=finished_at,
                  seconds=round(finished_at-before["time"], 3), binary_sha256=hashlib.sha256((Path.home()/".local/bin/pmr-mcp").read_bytes()).hexdigest())
    write(prefix.with_suffix(".after.json"), after)
    write(prefix.with_suffix(".result.json"), result)
    output_keys = ["status", "failures", "tools", "seconds"]
    if "source_capture" in case:
        output_keys += ["source_capture", "enrichment"]
    print(json.dumps({k:result[k] for k in output_keys}, ensure_ascii=False))
    return int(result["status"] != "passed")


if __name__ == "__main__":
    raise SystemExit(main())
