#!/usr/bin/env python3
"""Check recorded web/UI/RPC/SQLite evidence; never synthesize a web turn."""
import hashlib
import json
import sys
from pathlib import Path


def audit(index_path):
    index = json.loads(index_path.read_text())
    root = Path(index["private_root"])
    failures = []

    def need(ok, message):
        if not ok:
            failures.append(message)

    for name, digest in index["artifacts"].items():
        need(hashlib.sha256((root / name).read_bytes()).hexdigest() == digest, "artifact changed: " + name)
    for name, digest in index["sources"].items():
        need(hashlib.sha256(Path(name).read_bytes()).hexdigest() == digest, "source changed: " + name)
    read = lambda name: json.loads((root / name).read_text())
    a, b = read("web-A.result.json"), read("web-B.result.json")
    for result in (a, b):
        need(result["status"] == "passed", str(result["failures"]))
        need(result["observation"]["new_chat"] is True, "not observed as a fresh chat")
        need(result["observation"]["builtin_memory"] == "off", "built-in memory was not isolated")
        need(result["observation"]["completed"] is True, "no final UI response")
        need(result["observation"]["host_internal_input"] == "unobservable", "host observability overstated")
    need(a["observation"]["chat_url"] != b["observation"]["chat_url"], "same chat reused")
    need(a["finished_at"] <= b["started_at"], "recall began before save evidence was captured")
    before, saved = read("web-A.before.json")["state"], read("web-A.after.json")
    need(not before["repository"]["memories"], "save database was not fresh")
    memories = {m["id"]: m for m in saved["repository"]["memories"].values()
                if m["status"] == "Active" and any(e["quote"] == a["case"]["prompt"] for e in m["evidence"])}
    expected = ("无糖乌龙茶", "雾蓝", "420")
    for term in expected:
        need(term in json.dumps(list(memories.values()), ensure_ascii=False), "unsaved fact: " + term)
        need(term in b["observation"]["answer"], "answer missed fact: " + term)
        need(term not in b["case"]["prompt"], "recall question leaks fact: " + term)
    returned = []
    for call in b["calls"]:
        if call["tool"] == "memory_recall" and not call["arguments"].get("bootstrap"):
            result = call["result"]["structured_content"].get("result", {})
            returned.extend(m for lane in ("mandatory", "constraints", "preferences", "facts", "episodes")
                            for m in result.get(lane, []) if m["id"] in memories)
    for term in expected:
        need(term in json.dumps(returned, ensure_ascii=False), "absent from actual semantic recall: " + term)
    pairs = []
    for path in root.glob("rpc-*.jsonl"):
        requests = {}
        for line in path.read_text().splitlines():
            row = json.loads(line)
            if row["direction"] == "request" and row.get("call_id"):
                requests[row["call_id"]] = row
            elif row.get("prepare_receipt"):
                request = requests.get(row.get("call_id"), {})
                arguments = request.get("body", {}).get("params", {}).get("arguments", {})
                if arguments.get("user_turn", {}).get("text") != a["case"]["prompt"]:
                    continue
                need(not arguments["user_turn"].get("history"), "new chat included previous user history")
                receipt = row["prepare_receipt"]
                output = row["body"].get("result", {}).get("structuredContent", {})
                if output.get("status") == "in_progress":
                    continue
                need(receipt["status"] == "present", "no observed processing receipt")
                need((receipt.get("value") or {}).get("request", {}).get("text") == a["case"]["prompt"], "receipt text mismatch")
                need(all(output.get(part, {}).get("status") == "completed" for part in ("remember", "recall")), "prepare did not finish both parts")
                pairs.append(row["call_id"])
    need(bool(pairs), "no correlated final prepare response")
    restored = read("restoration.json")
    need(all(restored.get(k) is True for k in ("route_restored", "formal_unchanged", "builtin_memory_restored")), "restoration incomplete")
    outcome = "fail" if failures else "pass"
    print(json.dumps({"acf_assertion": {"id": "web-two-chats", "outcome": outcome,
          "observed": json.dumps({"failures": failures, "prepare_calls": pairs, "recalled_ids": [m["id"] for m in returned],
                       "scope": "two observed web chats; human comparison pending; host internal input unobservable"}, ensure_ascii=False)}}, ensure_ascii=False))
    return bool(failures)


if __name__ == "__main__":
    raise SystemExit(audit(Path(sys.argv[1])))
