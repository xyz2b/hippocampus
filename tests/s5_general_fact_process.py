#!/usr/bin/env python3
"""Frozen general-fact examples via actual MCP and loopback model fixtures; no real provider calls."""
import argparse
import contextlib
import hashlib
import http.server
import json
import os
import tempfile
import threading
from collections import Counter
from pathlib import Path

from mcp_smoke import Client
from s5_normal_intake import candidate, settings
from source_support_fixture import FIXTURE_VERSION, source_support_review


ROOT = Path(__file__).resolve().parents[1]
CASES = ROOT / "docs/refactor/s5/generalization-preflight-20260926"
INPUTS = json.loads((CASES / "inputs.json").read_text())
LABELS = json.loads((CASES / "labels.json").read_text())


class Fixture(http.server.BaseHTTPRequestHandler):
    requests = []
    errors = []

    def log_message(self, *_):
        pass

    def do_POST(self):
        try:
            assert self.path == "/v1/responses"
            assert self.headers["Authorization"] == "Bearer local-fixture-key"
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            name = request["text"]["format"]["name"]
            type(self).requests.append(name)
            prompt = json.loads(request["input"])
            if name == "personal_memory_source_support_v1":
                output = source_support_review(request)
            elif name == "personal_memory_assistance_queries_v1":
                output = {"queries": []}
            else:
                statement = prompt["current_user_turn"]
                source = next(source for source in INPUTS["sources"] if source["text"] == statement)
                label = LABELS["intake"][source["id"]]
                output = {"omission_reason": None, "candidates": [
                    candidate(label["subject"], statement, predicate="fact")]}
            body = json.dumps({"status": "completed", "usage": {"input_tokens": 10, "output_tokens": 10},
                               "output": [{"type": "message", "content": [{"type": "output_text",
                                   "text": json.dumps(output, ensure_ascii=False)}]}]}).encode()
            self.send_response(200)
        except Exception as error:
            type(self).errors.append(repr(error))
            body = json.dumps({"error": str(error)}).encode()
            self.send_response(500)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def check_recall(result, query, label, source_ids):
    contract = result.get("answer_contract", {})
    sources = contract.get("safe_source_ids", [])
    expected_ids = [source_ids[ref] for ref in label["required_source_refs"]]
    expected_sources = [{"source_id": source_ids[ref], "quote": LABELS["intake"][ref]["supporting_quote"]}
                        for ref in label["required_source_refs"]]
    safe_answer = contract.get("safe_answer") or ""
    if label["expected"] == "answer":
        passed = (sources == expected_ids and safe_answer == label["answer_values"][0] + "。"
                  and contract.get("sources") == expected_sources)
    else:
        status = "clarification_required" if label["expected"] == "clarify" else "insufficient_evidence"
        passed = contract.get("status") == status and not sources and not contract.get("sources")
    bound = (contract.get("version") == 1 and contract.get("query") == query["query"]
             and contract.get("context") == query["context"])
    evidence = result.get("source_evidence", [])
    safe = (all(source in expected_ids for source in sources)
            and all(source["source_id"] in expected_ids for source in evidence))
    return {"id": query["id"], "expected": label["expected"], "passed": passed and bound and safe,
            "safe": safe,
            "contract": contract, "source_evidence": result.get("source_evidence", []),
            "assisted_resolution": result.get("assisted_resolution"),
            "assistance_search": result.get("assistance_search"),
            "model_calls": result.get("model_calls", [])}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/pmr-mcp")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    assert binary.is_file(), f"build MCP binary first: {binary}"
    Fixture.requests, Fixture.errors = [], []
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-s5-general-fact-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        clean = {key: value for key, value in os.environ.items()
                 if not key.startswith(("DEEPSEEK_", "OPENAI_", "ARK_", "PMR_"))}
        env = dict(clean, PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
                   DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
                   NO_PROXY="127.0.0.1,localhost")
        configs = json.loads((CASES / "budgets.json").read_text())["phases"]
        paths = {phase: settings(directory, phase, **config) for phase, config in configs.items()}
        db = directory / "memory.sqlite3"
        source_ids, intake = {}, []
        with contextlib.closing(Client(binary, db, env, "s5-general-fact", config=paths["intake"])) as client:
            client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
            for entity in INPUTS["entities"]:
                client.call("memory_entity_upsert", entity)
            for source in INPUTS["sources"]:
                result = client.call("memory_prepare", {"user_turn": {
                    "text": source["text"], "session_id": "general-fact", "turn_id": source["id"],
                    "capture_source": True}})
                saved = result["remember"]["result"]
                passed = (saved["source"]["status"] == "stored"
                          and saved["enrichment"]["status"] == "committed"
                          and len(saved["outcomes"]) == 1)
                assert passed, (source["id"], result)
                source_ids[source["id"]] = saved["source"]["source_id"]
                intake.append({"id": source["id"], "passed": passed, "remember": saved})
            assert client.call("memory_list", {})["total"] == 8
        recalls = []
        with contextlib.closing(Client(binary, db, env, "s5-general-fact", config=paths["recall"])) as client:
            client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
            for query in INPUTS["queries"]:
                result = client.call("memory_recall", {key: query[key] for key in ("query", "context")})
                recalls.append(check_recall(result, query, LABELS["queries"][query["id"]], source_ids))
        validations = []
        queries = {query["id"]: query for query in INPUTS["queries"]}
        with contextlib.closing(Client(binary, db, env, "s5-general-fact", config=paths["validate"])) as client:
            client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
            for case in INPUTS["validations"]:
                query = queries[case["query_id"]]
                request = {
                    "query": query["query"], "context": query["context"], "draft_answer": case["draft_answer"],
                    "source_ids": [source_ids[ref] for ref in case["source_refs"]]}
                assert request["source_ids"], "validation pairs must cite the actual saved source"
                result = client.call("memory_validate_answer", request)
                expected = LABELS["validations"][case["id"]]["expected_approved"]
                approved = result["approved"] is True
                approved_ids = result.get("approved_source_ids", [])
                passed = (approved == expected and (approved_ids == [source_ids[ref] for ref in case["source_refs"]]
                                                     if expected else not approved_ids))
                validations.append({"id": case["id"], "expected_approved": expected,
                                    "passed": passed, "request": request, "result": result})
        failed = [row["id"] for row in recalls + validations if not row["passed"]]
        report = {"status": "quality_gaps" if failed else "passed", "scope": "actual MCP with correct loopback model fixtures",
                  "fixture_version": FIXTURE_VERSION, "semantic_review": "mocked, not live model quality",
                  "fixture_inputs": {name: hashlib.sha256((CASES / name).read_bytes()).hexdigest()
                                     for name in ("inputs.json", "labels.json")},
                  "real_provider_requests": 0, "loopback_model_requests": len(Fixture.requests),
                  "loopback_stages": dict(Counter(Fixture.requests)), "fixture_errors": Fixture.errors,
                  "intake": intake, "recalls": recalls, "validations": validations, "failed_cases": failed}
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps({"status": report["status"], "intake_passed": len(intake),
                          "recall_passed": sum(row["passed"] for row in recalls),
                          "validation_passed": sum(row["passed"] for row in validations),
                          "real_provider_requests": 0, "loopback_model_requests": len(Fixture.requests),
                          "fixture_errors": Fixture.errors, "failed_cases": failed}, ensure_ascii=False))
        assert not Fixture.errors, "loopback model fixture failed"
        assert not failed, "general fact acceptance has gaps; inspect --output report"


if __name__ == "__main__":
    main()
