#!/usr/bin/env python3
"""Isolated original-N05 intake/recall acceptance. Preflight never contacts a model."""
import argparse
import contextlib
import hashlib
import json
import os
import re
import sqlite3
import tempfile
import time
import uuid
from pathlib import Path
from unittest.mock import patch

from mcp_smoke import Client
from s5_normal_intake import settings


ROOT = Path(__file__).resolve().parents[1]
RUNNER = Path(__file__).resolve()
FIXTURE = ROOT / "docs/refactor/s4/inputs.json"
LABELS = ROOT / "docs/refactor/s5/assisted/original-n05-acceptance-20260926/labels.json"
ARK = "https://ark.cn-beijing.volces.com/api/v3"
PREDICATES = [{"predicate": name, "cardinality": cardinality} for name, cardinality in (
    ("技术选择", "Single"), ("部署状态", "Single"), ("部署限制", "Single"),
    ("暂缓技术", "Multiple"), ("暂缓原因", "Multiple"),
    ("code_example_language", "Single"), ("fact", "Multiple"))]
LIMITS = {"intake": 8, "recall": 2}
OUTPUT_TOKENS = 32768
INPUT_BYTES = 65536


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save(path, value, exclusive=False):
    serialized = json.dumps(value, ensure_ascii=False, indent=2)
    secret = os.environ.get("DEEPSEEK_API_KEY", "")
    if secret:
        serialized = serialized.replace(secret, "[redacted]")
    with path.open("x" if exclusive else "w") as stream:
        stream.write(serialized + "\n")


class LongClient(Client):
    """An intake can spend 2 × 360 seconds inside one MCP call."""

    def rpc(self, method, params):
        self.number += 1
        self.send({"jsonrpc": "2.0", "id": self.number, "method": method, "params": params})
        deadline = time.monotonic() + 900
        while True:
            response = self.responses.get(timeout=max(0.01, deadline - time.monotonic()))
            if not isinstance(response, dict):
                raise RuntimeError("MCP process ended or returned invalid JSON")
            if response.get("id") == self.number:
                if "error" in response:
                    raise RuntimeError("MCP JSON-RPC error")
                return response["result"]


def prepare(client, row, session, output):
    arguments = {"user_turn": {"text": row["text"], "session_id": session,
                               "turn_id": row["id"], "capture_source": True}}
    deadline = time.monotonic() + 900
    number = 0
    while time.monotonic() < deadline:
        number += 1
        raw = client.rpc("tools/call", {"name": "memory_prepare", "arguments": arguments})
        path = output / f"intake-{row['id']}-{number:02d}.json"
        save(path, raw, exclusive=True)
        if raw.get("isError"):
            raise RuntimeError(f"memory_prepare returned an error; see {path.name}")
        result = raw.get("structuredContent") or json.loads(raw["content"][0]["text"])
        if result.get("status") != "in_progress":
            return result
        time.sleep(min(2, max(0, deadline - time.monotonic())))
    raise RuntimeError(f"{row['id']} remained in progress after 900 seconds of same-argument waits")


def check_prepare_poll():
    class FakeClient:
        def __init__(self):
            self.calls = []

        def rpc(self, method, params):
            self.calls.append((method, params))
            return {"structuredContent": {"status": "in_progress" if len(self.calls) == 1
                                          else "completed"}}

    with tempfile.TemporaryDirectory(prefix="pmr-s5-poll-") as directory, patch("time.sleep"):
        client = FakeClient()
        assert prepare(client, {"id": "test", "text": "test"}, "same-session", Path(directory)) == {
            "status": "completed"}
        assert len(client.calls) == 2 and client.calls[0] == client.calls[1]
        assert len(list(Path(directory).glob("intake-test-*.json"))) == 2


def fixture():
    labels = json.loads(LABELS.read_text())
    assert digest(FIXTURE) == labels["source_fixture"]["sha256"]
    rows = json.loads(FIXTURE.read_text())
    n05 = next(row for row in rows if row["id"] == "N05")
    negative = next(row for row in rows if row["id"] == "negative")
    assert [row["id"] for row in n05["sources"]] == labels["source_fixture"]["source_ids"]
    assert negative["sources"] == n05["sources"]
    questions = {row["id"]: row for row in labels["cases"]}
    for row in (n05, negative):
        assert row["query"] == questions[row["id"]]["query"]
        assert row["context"] == questions[row["id"]]["context"] == []
    return n05["sources"], {row["id"]: row["query"] for row in (n05, negative)}


def ledger(db, group):
    if not db.exists():
        return []
    with sqlite3.connect(db.resolve().as_uri() + "?mode=ro", uri=True) as connection:
        tables = {row[0] for row in connection.execute("SELECT name FROM sqlite_master")}
        if "model_calls" not in tables:
            return []
        return [dict(zip(("stage", "outcome", "input_bytes", "output_token_limit",
                          "input_tokens", "output_tokens"), row))
                for row in connection.execute(
                    "SELECT stage,outcome,input_bytes,output_token_limit,input_tokens,output_tokens "
                    "FROM model_calls WHERE group_id=? ORDER BY created_at,call_id", (group,))]


def clean_env(mode):
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("DEEPSEEK_", "OPENAI_", "PMR_"))}
    env["PMR_MODEL_NETWORK"] = mode
    if mode == "live":
        if os.environ.get("DEEPSEEK_BASE_URL") != ARK or not os.environ.get("DEEPSEEK_API_KEY"):
            raise ValueError("exact Ark HTTPS URL and a provider key are required")
        env.update(DEEPSEEK_BASE_URL=ARK, DEEPSEEK_API_KEY=os.environ["DEEPSEEK_API_KEY"])
    return env


def config(output, phase, manifest):
    values = dict(retrieval_mode="assisted" if phase == "recall" else "local",
                  enrichment_mode="inline" if phase == "intake" else "off",
                  fact_index_enabled=True, predicates=PREDICATES,
                  local_recall_budget={"candidate_k": 30, "source_limit": 10,
                                       "content_bytes": 24000})
    if phase != "preflight":
        values["model_budget"] = dict(group_id=manifest["groups"][phase],
            max_calls=LIMITS[phase], max_operation_calls=2 if phase == "intake" else 1,
            max_input_bytes=INPUT_BYTES, extraction_output_tokens=OUTPUT_TOKENS,
            review_output_tokens=OUTPUT_TOKENS, query_output_tokens=OUTPUT_TOKENS)
    return settings(output, f"{phase}-config", **values)


def receipt(prepared):
    remembered = prepared["remember"]
    return remembered.get("result", remembered)


def source_ok(remembered, required_fact):
    return (remembered["source"]["status"] == "stored"
            and remembered["index"]["status"] == "ready"
            and remembered["enrichment"]["status"] in
            ({"committed"} if required_fact else {"committed", "completed_no_facts"}))


def relevant_memories(memories, turn, quote):
    return [memory for memory in memories if memory["subject_id"] == "松石47"
            and any(evidence["turn_id"] == turn and evidence["quote"] == quote
                    for evidence in memory["evidence"])]


def fact_gate(memories, sources):
    source = {row["id"]: row["text"] for row in sources}
    e1 = relevant_memories(memories, "E1", source["E1"])
    e2 = relevant_memories(memories, "E2", source["E2"])
    e1_choice = any(memory["predicate"] in ("技术选择", "fact")
                    and "SQLite" in json.dumps(memory["value"], ensure_ascii=False)
                    for memory in e1)
    current = [memory for memory in e2 if memory["predicate"] in ("部署状态", "fact")
               and "一台机器" in json.dumps(memory["value"], ensure_ascii=False)]
    false_limit = any(
        re.search(r"部署|机器|单机|deploy|machine", text, re.I)
        and re.search(r"限制|上限|只能|仅能|limit|restrict|maximum", text, re.I)
        for memory in e2
        if (text := memory["predicate"] + json.dumps(memory["value"], ensure_ascii=False)))
    return {"E1_sqlite_committed": e1_choice, "E2_current_machine_committed": bool(current),
            "E2_false_limit_detected": false_limit,
            "passed": e1_choice and bool(current) and not false_limit,
            "E1_memories": e1, "E2_memories": e2}


def check_fact_gate(sources):
    original = {row["id"]: row["text"] for row in sources}
    def memory(turn, predicate, value):
        return {"subject_id": "松石47", "predicate": predicate, "value": {"Text": value},
                "evidence": [{"turn_id": turn, "quote": original[turn]}]}
    choice = memory("E1", "技术选择", "SQLite")
    state = memory("E2", "部署状态", "一台机器")
    assert fact_gate([choice, state], sources)["passed"]
    assert not fact_gate([choice, memory("E2", "部署限制", "一台机器")], sources)["passed"]
    assert not fact_gate([state], sources)["passed"]


def authorized(path, output, manifest):
    if not path or not path.is_file():
        raise ValueError("send phases require an authorization JSON file")
    required = dict(authorized_scope="S5 original N05 isolated fullchain intake and recall",
                    output_dir=str(output), manifest_sha256=digest(output / "manifest.json"),
                    provider_base_url=ARK, intake_max_calls=8, recall_max_calls=2,
                    max_operation_calls_intake=2, max_operation_calls_recall=1,
                    output_tokens=OUTPUT_TOKENS, max_input_bytes=INPUT_BYTES)
    actual = json.loads(path.read_text())
    if any(actual.get(key) != value or type(actual.get(key)) is not type(value)
           for key, value in required.items()):
        raise ValueError("authorization scope, output, manifest, provider or limits mismatch")
    if digest(Path(manifest["binary"])) != manifest["binary_sha256"]:
        raise ValueError("MCP binary changed since preflight")
    if digest(RUNNER) != manifest["runner_sha256"]:
        raise ValueError("runner changed since preflight")
    if digest(FIXTURE) != manifest["fixture_sha256"] or digest(LABELS) != manifest["labels_sha256"]:
        raise ValueError("inputs or labels changed since preflight")
    if json.loads((output / "preflight-report.json").read_text())["status"] != "passed":
        raise ValueError("zero-send preflight did not pass")


def preflight(output, binary, sources, questions):
    check_fact_gate(sources)
    check_prepare_poll()
    if output.exists():
        raise ValueError("preflight output directory must be new")
    os.umask(0o077)
    output.mkdir(parents=True, mode=0o700)
    run = uuid.uuid4().hex
    manifest = dict(binary=str(binary), binary_sha256=digest(binary), runner_sha256=digest(RUNNER),
                    fixture_sha256=digest(FIXTURE), labels_sha256=digest(LABELS),
                    owner=f"s5-original-n05-{run}", session_id=f"s5-original-n05-{run}",
                    groups={phase: f"s5-original-n05-{run}-{phase}" for phase in LIMITS},
                    sources=sources, questions=questions, budget_limits=LIMITS)
    save(output / "manifest.json", manifest)
    env = clean_env("deny")
    config_path = config(output, "preflight", manifest)
    (output / "preflight-receipts").mkdir(mode=0o700)
    with contextlib.closing(LongClient(binary, output / "preflight.sqlite3", env,
                                   manifest["owner"], config=config_path)) as client:
        client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
        for entity in ("松石47", "赤铜82"):
            client.call("memory_entity_upsert", {"id": entity, "canonical_name": entity,
                                                 "aliases": [], "entity_type": "Other"})
        for row in sources:
            prepared = prepare(client, row, manifest["session_id"], output / "preflight-receipts")
            remembered = receipt(prepared)
            assert remembered["source"]["status"] == "stored"
            assert remembered["index"]["status"] == "ready"
            assert remembered["enrichment"]["status"] == "not_requested"
        for query in questions.values():
            result = client.call("memory_recall", {"query": query, "context": []})
            assert result["source_evidence"] == []
    assert ledger(output / "preflight.sqlite3", manifest["groups"]["intake"]) == []
    assert ledger(output / "preflight.sqlite3", manifest["groups"]["recall"]) == []
    save(output / "preflight-report.json", {"status": "passed", "real_provider_requests": 0,
         "sources_stored": len(sources), "new_business_db_exists": (output / "business.sqlite3").exists(),
         "intake_max_calls": 8, "recall_max_calls": 2,
         "same_argument_poll_selfcheck": "passed"})
    return manifest


def intake(output, manifest, sources):
    db = output / "business.sqlite3"
    if db.exists() or (output / "intake-report.json").exists():
        raise ValueError("intake is single-use and requires a fresh business database")
    env = dict(clean_env("live"), PMR_TRACE_DIR=str(output / "intake-trace"))
    report = {"status": "running", "rows": [], "max_physical_requests": 8}
    save(output / "intake-report.json", report)
    trace = output / "intake-trace"
    trace.mkdir(mode=0o700)
    try:
        with contextlib.closing(LongClient(manifest["binary"], db, env, manifest["owner"],
                                       config=config(output, "intake", manifest))) as client:
            client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
            for entity in ("松石47", "赤铜82"):
                client.call("memory_entity_upsert", {"id": entity, "canonical_name": entity,
                                                     "aliases": [], "entity_type": "Other"})
            for row in sources:
                prepared = prepare(client, row, manifest["session_id"], output)
                remembered = receipt(prepared)
                report["rows"].append({"id": row["id"], "source_id": remembered["source"].get("source_id"),
                                       "source": remembered["source"]["status"],
                                       "index": remembered["index"]["status"],
                                       "enrichment": remembered["enrichment"]["status"],
                                       "outcomes": remembered.get("outcomes", [])})
                report["ledger"] = ledger(db, manifest["groups"]["intake"])
                save(output / "intake-report.json", report)
                if not source_ok(remembered, row["id"] in ("E1", "E2")):
                    raise AssertionError(f"intake failed for {row['id']}")
                if row["id"] in ("E1", "E2"):
                    memories = client.stored_memories()
                    gate = fact_gate(memories, sources)
                    if row["id"] == "E1" and not gate["E1_sqlite_committed"]:
                        raise AssertionError("E1 SQLite choice was not committed")
                    if row["id"] == "E2" and not gate["passed"]:
                        raise AssertionError("E2 current state absent or falsely encoded as limit")
            directory = client.call("memory_list", {"limit": 100})
            listing = {"source": "isolated_sqlite_profile", "total": directory["total"],
                       "memories": client.stored_memories()}
        save(output / "intake-memories.json", listing)
        report["fact_gate"] = fact_gate(listing["memories"], sources)
        report["ledger"] = ledger(db, manifest["groups"]["intake"])
        assert len(report["ledger"]) <= 8
        report["status"] = "passed" if report["fact_gate"]["passed"] else "failed"
    except Exception as error:
        report["status"] = "failed"
        report["error"] = f"{type(error).__name__}: {error}"
        report["ledger"] = ledger(db, manifest["groups"]["intake"])
        raise
    finally:
        save(output / "intake-report.json", report)


def recall(output, manifest, questions):
    db = output / "business.sqlite3"
    if (output / "recall-report.json").exists():
        raise ValueError("recall is single-use")
    previous = json.loads((output / "intake-report.json").read_text())
    if previous["status"] != "passed" or not previous["fact_gate"]["passed"]:
        raise ValueError("intake fact gate did not pass")
    report = {"status": "running", "cases": [], "max_physical_requests": 2}
    save(output / "recall-report.json", report)
    local_config = settings(output, "recall-local-config", retrieval_mode="local",
                            enrichment_mode="off", fact_index_enabled=True,
                            predicates=PREDICATES, local_recall_budget={
                                "candidate_k": 30, "source_limit": 10, "content_bytes": 24000})
    try:
        with contextlib.closing(LongClient(manifest["binary"], db, clean_env("deny"),
                                       manifest["owner"], config=local_config)) as client:
            client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
            for case in ("negative", "N05"):
                result = client.call("memory_recall", {"query": questions[case], "context": []})
                save(output / f"local-{case}.json", result)
                if result["source_evidence"] or result["historical_or_conflicting"]:
                    raise AssertionError(f"local {case} returned a final source")
        trace = output / "recall-trace"
        trace.mkdir(mode=0o700)
        env = dict(clean_env("live"), PMR_TRACE_DIR=str(trace))
        with contextlib.closing(LongClient(manifest["binary"], db, env, manifest["owner"],
                                       config=config(output, "recall", manifest))) as client:
            client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
            for case in ("negative", "N05"):
                result = client.call("memory_recall", {"query": questions[case], "context": []})
                save(output / f"assisted-{case}.json", result)
                safe = not result["source_evidence"] and not result["historical_or_conflicting"]
                report["cases"].append({"id": case, "safe_empty_final_sources": safe,
                                        "assistance": result.get("assistance"),
                                        "model_calls": result.get("model_calls", [])})
                report["ledger"] = ledger(db, manifest["groups"]["recall"])
                save(output / "recall-report.json", report)
                if not safe:
                    raise AssertionError(f"assisted {case} returned a final source")
        report["status"] = "passed"
    except Exception as error:
        report["status"] = "failed"
        report["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        report["ledger"] = ledger(db, manifest["groups"]["recall"])
        assert len(report["ledger"]) <= 2
        save(output / "recall-report.json", report)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("preflight", "intake", "recall"))
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/pmr-mcp")
    parser.add_argument("--authorization", type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    sources, questions = fixture()
    output = args.output.resolve()
    binary = args.binary.resolve()
    if not binary.is_file():
        raise ValueError("MCP binary is missing")
    if args.phase == "preflight":
        if args.authorization:
            raise ValueError("preflight does not use authorization")
        manifest = preflight(output, binary, sources, questions)
    else:
        manifest = json.loads((output / "manifest.json").read_text())
        authorized(args.authorization, output, manifest)
        if args.phase == "intake":
            intake(output, manifest, sources)
        else:
            recall(output, manifest, questions)
    print(json.dumps({"phase": args.phase, "output": str(output),
                      "groups": manifest["groups"], "limits": LIMITS}, ensure_ascii=False))


if __name__ == "__main__":
    main()
