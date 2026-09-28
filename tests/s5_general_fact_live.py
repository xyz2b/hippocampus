#!/usr/bin/env python3
"""Frozen 8/20/8 acceptance against Ark; isolated, single use, no automatic retries."""
import argparse
import contextlib
import json
import os
from pathlib import Path

from s5_general_fact_process import CASES, INPUTS, LABELS, ROOT, check_recall
from s5_original_n05_fullchain import LongClient, clean_env, digest, ledger, prepare, receipt, save
from s5_normal_intake import settings


BUDGET = json.loads((CASES / "budgets.json").read_text())
BASELINE = ROOT / "docs/refactor/s5/general-fact-answer-20260927/summary.json"
RUNNER = Path(__file__).resolve()


def formal():
    paths = {"profile": Path.home() / ".config/tunnel-client/personal-memory.yaml",
             "wrapper": Path.home() / ".local/bin/pmr-mcp-configured",
             "database": Path.home() / ".local/share/personal-memory-runtime/memory.sqlite3"}
    return {name: {"sha256": digest(path), "mtime_ns": path.stat().st_mtime_ns}
            for name, path in paths.items()}


def intake_score(memories, source):
    label = LABELS["intake"][source["id"]]
    normalize = lambda value: value.strip().rstrip("。.!！").strip()
    text = source["text"]
    allowed = {normalize(label["value"]), normalize(text),
               normalize(text.removeprefix(label["subject"]).removeprefix("的"))}
    selected = [memory for memory in memories if any(
        evidence["turn_id"] == source["id"] for evidence in memory["evidence"])]
    supported = [memory for memory in selected if (
        memory["subject_id"] == label["subject"] and memory["status"] == "Active"
        and memory["predicate"] in ({"fact", "likes"} if source["id"].startswith("P-") else {"fact"})
        and memory["memory_type"] in {"Fact", "Preference", "Episode"}
        and memory.get("polarity") != "Negative"
        and normalize(memory["value"].get("Text", "")) in allowed
        and any(evidence["quote"] == label["supporting_quote"] for evidence in memory["evidence"]))]
    return {"passed": bool(selected) and len(supported) == len(selected),
            "memories": selected, "unsupported_memories": len(selected) - len(supported)}


def snapshot_ledger(db):
    return {phase: ledger(db, config["model_budget"]["group_id"])
            for phase, config in BUDGET["phases"].items()}


def check_calls(db):
    rows = snapshot_ledger(db)
    for phase, calls in rows.items():
        policy = BUDGET["phases"][phase]["model_budget"]
        assert len(calls) <= policy["max_calls"], "phase budget exceeded"
        for call in calls:
            assert call["outcome"] == "completed", f"model request failed in {phase}"
            cap = policy["query_output_tokens"] if phase != "intake" else policy["extraction_output_tokens"]
            assert call["output_token_limit"] == cap and call["input_bytes"] <= policy["max_input_bytes"]
    assert sum(map(len, rows.values())) <= 44
    return rows


def preflight(output):
    baseline = json.loads(BASELINE.read_text())
    for name, fingerprint in baseline["fingerprints"].items():
        assert digest(ROOT / name) == fingerprint, f"tested version changed: {name}"
    # Exercise the live intake scorer independently of a model request.
    source = INPUTS["sources"][0]
    label = LABELS["intake"][source["id"]]
    memory = {"subject_id": label["subject"], "status": "Active", "predicate": "fact", "memory_type": "Fact", "value": {"Text": label["value"]},
              "evidence": [{"turn_id": source["id"], "quote": source["text"]}]}
    assert intake_score([memory], source)["passed"]
    assert not intake_score([], source)["passed"]
    assert not intake_score([{**memory, "value": {"Text": "错误值"}}], source)["passed"]
    assert not intake_score([{**memory, "subject_id": "错误主体"}], source)["passed"]
    output.mkdir(parents=True, exist_ok=False, mode=0o700)
    manifest = {"binary": str(ROOT / "target/debug/pmr-mcp"), "fingerprints": baseline["fingerprints"],
                "runner_sha256": digest(RUNNER), "formal_baseline": formal(),
                "provider_base_url": BUDGET["provider_base_url"], "model": BUDGET["model"],
                "budget": BUDGET, "preflight": "passed", "real_provider_requests": 0}
    save(output / "manifest.json", manifest)
    for phase, config in BUDGET["phases"].items():
        settings(output, phase, **config)
    save(output / "authorization.json", {
        "user_reply": "继续", "authorized_scope": "同版本真实模型小批验收：8条入库、20问、8个草稿",
        "output_dir": str(output), "manifest_sha256": digest(output / "manifest.json"),
        "provider_base_url": BUDGET["provider_base_url"], "max_physical_requests": 44,
        "automatic_retries": 0, "formal_environment": "S4 unchanged"})
    return manifest


def send(output):
    manifest = json.loads((output / "manifest.json").read_text())
    auth = json.loads((output / "authorization.json").read_text())
    assert auth["manifest_sha256"] == digest(output / "manifest.json")
    assert auth["output_dir"] == str(output) and auth["max_physical_requests"] == 44
    assert auth["provider_base_url"] == BUDGET["provider_base_url"] and auth["automatic_retries"] == 0
    assert manifest["preflight"] == "passed" and manifest["budget"] == BUDGET
    assert digest(RUNNER) == manifest["runner_sha256"]
    for name, fingerprint in manifest["fingerprints"].items():
        assert digest(ROOT / name) == fingerprint, f"frozen version changed: {name}"
    assert formal() == manifest["formal_baseline"]
    env = clean_env("live")
    db = output / "business.sqlite3"
    assert not db.exists(), "business database already exists; do not reset budget"
    (output / "started").touch(exist_ok=False)
    trace = output / "trace"
    trace.mkdir(mode=0o700)
    env["PMR_TRACE_DIR"] = str(trace)
    report = {"status": "running", "intake": [], "recalls": [], "validations": [],
              "fixed_denominators": {"intake": 8, "recalls": 20, "validations": 8},
              "max_physical_requests": 44, "automatic_retries": 0}
    source_ids = {}

    def checkpoint():
        report["ledger"] = snapshot_ledger(db)
        report["reserved_calls"] = sum(map(len, report["ledger"].values()))
        save(output / "report.json", report)

    try:
        for phase in ("intake", "recall", "validate"):
            config = output / f"{phase}.json"
            assert json.loads(config.read_text()) == BUDGET["phases"][phase]
            with contextlib.closing(LongClient(manifest["binary"], db, env, "s5-generalization-live",
                                              config=config)) as client:
                client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
                if phase == "intake":
                    for entity in INPUTS["entities"]:
                        client.call("memory_entity_upsert", entity)
                    for row in INPUTS["sources"]:
                        saved = receipt(prepare(client, row, "s5-generalization-20260927", output))
                        scored = intake_score(client.stored_memories(), row)
                        scored.update(id=row["id"], source=saved.get("source"),
                                      index=saved.get("index"), enrichment=saved.get("enrichment"))
                        scored["passed"] &= (saved["source"]["status"] == "stored"
                            and saved["index"]["status"] == "ready"
                            and saved["enrichment"]["status"] == "committed")
                        report["intake"].append(scored)
                        checkpoint()
                        check_calls(db)
                        if not scored["passed"]:
                            raise RuntimeError(f"intake needs inspection: {row['id']}")
                        source_ids[row["id"]] = saved["source"]["source_id"]
                        save(output / "source-ids.json", source_ids)
                        print(json.dumps({"phase": phase, "id": row["id"], "passed": True}, ensure_ascii=False), flush=True)
                    save(output / "memories.json", client.stored_memories(include_history=True))
                elif phase == "recall":
                    for row in INPUTS["queries"]:
                        result = client.call("memory_recall", {key: row[key] for key in ("query", "context")})
                        save(output / f"recall-{row['id']}.json", result, exclusive=True)
                        scored = check_recall(result, row, LABELS["queries"][row["id"]], source_ids)
                        report["recalls"].append(scored)
                        checkpoint()
                        check_calls(db)
                        if not scored["safe"]:
                            raise RuntimeError(f"wrong evidence: {row['id']}")
                        print(json.dumps({"phase": phase, "id": row["id"], "passed": scored["passed"]}), flush=True)
                else:
                    queries = {row["id"]: row for row in INPUTS["queries"]}
                    for row in INPUTS["validations"]:
                        query = queries[row["query_id"]]
                        request = {"query": query["query"], "context": query["context"],
                                   "draft_answer": row["draft_answer"],
                                   "source_ids": [source_ids[ref] for ref in row["source_refs"]]}
                        result = client.call("memory_validate_answer", request)
                        save(output / f"validate-{row['id']}.json", {"request": request, "result": result}, exclusive=True)
                        expected = LABELS["validations"][row["id"]]["expected_approved"]
                        contract = check_recall({"answer_contract": result}, query,
                                               LABELS["queries"][row["query_id"]], source_ids)
                        passed = (result["approved"] == expected and (contract["passed"] if expected else True)
                                  and result["approved_source_ids"] == (request["source_ids"] if expected else []))
                        report["validations"].append({"id": row["id"], "expected_approved": expected,
                                                      "passed": passed, "result": result})
                        checkpoint()
                        check_calls(db)
                        if not contract["safe"] or (not expected and result["approved"]):
                            raise RuntimeError(f"unsafe validation: {row['id']}")
                        print(json.dumps({"phase": phase, "id": row["id"], "passed": passed}), flush=True)
        report["status"] = "passed" if all(row["passed"] for phase in ("intake", "recalls", "validations")
                                           for row in report[phase]) else "quality_gaps"
    except Exception as error:
        report["status"] = "stopped"
        report["error"] = type(error).__name__ + ": " + str(error)
    finally:
        report["formal_baseline_unchanged"] = formal() == manifest["formal_baseline"]
        checkpoint()
    print(json.dumps({key: report[key] for key in ("status", "reserved_calls", "formal_baseline_unchanged")}), flush=True)
    if report["status"] != "passed":
        raise SystemExit(1)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("preflight", "send"))
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    os.umask(0o077)
    output = args.output.resolve()
    preflight(output) if args.phase == "preflight" else send(output)
