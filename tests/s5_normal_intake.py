#!/usr/bin/env python3
"""Normal prepare -> committed quote -> MCP recall matrix, with raw-only fallback measured separately."""
import argparse
import contextlib
import http.server
import json
import os
import tempfile
import threading
from pathlib import Path

from mcp_smoke import Client
from source_support_fixture import FIXTURE_VERSION, source_support_review


ROOT = Path(__file__).resolve().parents[1]
STATEMENTS = {
    "budget_a": ("松石47的维护预算是 500 元。", "松石47", "500 元"),
    "budget_b": ("蓝鲸82的维护预算是 700 元。", "蓝鲸82", "700 元"),
    "phone": ("王阿姨的手机坏了，因为进水。", "王阿姨", "坏了，因为进水"),
    "shelf": ("书架甲的材质是橡木。", "书架甲", "橡木"),
    "appointment": ("牙医预约改到了周二上午。", "牙医预约", "周二上午"),
    "budget_corrected": ("更正：松石47的维护预算是 600 元，之前的 500 元作废。", "松石47", "600 元"),
}
CASES = [
    ("project", "松石47的维护预算是多少？", "budget_a", None, []),
    ("project_reworded", "松石47维护预算为多少？", "budget_a", None, []),
    ("project_advice_diagnostic", "松石47要留多少维护资金？", None, "diagnostic_safe", []),
    ("other_project", "蓝鲸82的维护预算是多少？", "budget_b", None, []),
    ("person", "王阿姨的手机为什么坏了？", "phone", None, []),
    ("object", "书架甲的材质是什么？", "shelf", None, []),
    ("event", "牙医预约改到什么时候？", "appointment", None, []),
    ("missing_property", "松石47的负责人是谁？", None, None, []),
    ("wrong_subject", "松石47的手机为什么坏了？", None, None, []),
    ("unbound_project", "那个项目的维护预算是多少？", None, "clarify", []),
    ("unbound_object", "它为什么坏了？", None, "clarify", ["继续聊王阿姨。"]),
    ("unbound_event", "那次预约改到什么时候？", None, "clarify", ["继续聊王阿姨。"]),
    ("missing_phone_owner", "手机为什么坏了？", None, "clarify", []),
    ("missing_event", "改到什么时候？", None, "clarify", []),
]


def candidate(subject, value, target=None, predicate="fact"):
    axes = dict(companion_roles=[], companion_entity_ids=[], geography=None, country=None,
                city=None, date=None, time_of_day=None, activities=[], task_id=None)
    return dict(subject_id=subject, unresolved_subject=None, memory_type="fact", predicate=predicate,
                value=dict(kind="text", text=value, integer=None, boolean=None, entity_id=None),
                polarity=None, strength=None, scope=dict(domain=None, conditions=axes),
                retention_tier="durable", valid_from=None, valid_to=None, expires_at=None,
                temporal_expression=None, extraction_confidence="high", certainty="high",
                source="user_statement", intent=dict(kind="supersede" if target else "assert",
                                                     target_memory_id=target,
                                                     reason="correction" if target else None),
                explicit_memory_request=False, cross_turn_useful=True, tags=[])


class Fixture(http.server.BaseHTTPRequestHandler):
    requests = []
    correction_target = None

    def log_message(self, *_):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        assert self.path == "/v1/responses"
        assert self.headers["Authorization"] == "Bearer local-fixture-key"
        name = request["text"]["format"]["name"]
        if name == "personal_memory_source_support_v1":
            output = source_support_review(request)
        elif name == "personal_memory_assistance_queries_v1":
            output = {"queries": []}
        else:
            prompt = json.loads(request["input"])
            statement = prompt["current_user_turn"]
            key = next(key for key, (text, _, _) in STATEMENTS.items() if text == statement)
            _, subject, value = STATEMENTS[key]
            target = type(self).correction_target if key == "budget_corrected" else None
            assert key != "budget_corrected" or target, "correction needs an existing memory"
            predicate = "maintenance_budget" if key.startswith("budget_") else "fact"
            if predicate == "fact":
                value = statement
            output = {"omission_reason": None,
                      "candidates": [candidate(subject, value, target, predicate)]}
        type(self).requests.append(name)
        body = json.dumps({"status": "completed", "usage": {"input_tokens": 10, "output_tokens": 10},
                           "output": [{"type": "message", "content": [{"type": "output_text",
                               "text": json.dumps(output, ensure_ascii=False)}]}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def settings(directory, name, **values):
    path = directory / f"{name}.json"
    path.write_text(json.dumps(values))
    return path


def call_case(client, case, source_ids):
    name, query, expected, mode, context = case
    args = {"query": query}
    if context:
        args["context"] = context
    result = client.call("memory_recall", args)
    evidence = result["source_evidence"]
    ids = [item["source_id"] for item in evidence]
    expected_id = source_ids.get(expected)
    exact = bool(expected and len(evidence) == 1 and ids == [expected_id]
                 and evidence[0]["text"] == STATEMENTS[expected][0])
    clarification = result["reference_resolution"]["needs_clarification"] is True
    current_memories = [memory for lane in ("mandatory", "constraints", "preferences", "facts", "episodes")
                        for memory in result["result"].get(lane, [])]
    safe = not evidence if not expected else all(item["source_id"] == expected_id for item in evidence)
    if not expected:
        safe &= not current_memories
    passed = (exact if expected else safe and (clarification if mode == "clarify" else True))
    return {"case": name, "query": query, "expected": expected, "source_ids": ids,
            "source_quotes": [item["text"] for item in evidence],
            "source_decisions": result["source_decisions"],
            "candidate_exclusions": result["budget"]["excluded"],
            "clarification": clarification,
            "clarification_question": result["reference_resolution"].get("clarification_question"),
            "safe": safe, "answerable": exact, "passed": passed,
            "current_memories": [{"id": memory["id"], "status": memory["status"],
                                  "subject_id": memory["subject_id"],
                                  "predicate": memory["predicate"], "value": memory["value"]}
                                 for memory in current_memories],
            "evidence_status": result["evidence_status"],
            "model_calls": len(result["model_calls"])}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/pmr-mcp")
    parser.add_argument("--output", type=Path, help="Optional JSON report path")
    args = parser.parse_args()
    binary = args.binary.resolve()
    assert binary.is_file(), f"build the MCP binary first: {binary}"
    Fixture.requests = []
    Fixture.correction_target = None
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-s5-normal-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        clean = {key: value for key, value in os.environ.items()
                 if not key.startswith(("DEEPSEEK_", "OPENAI_", "PMR_"))}
        loopback = dict(clean, PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
                        DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
                        NO_PROXY="127.0.0.1,localhost")
        denied = dict(clean, PMR_MODEL_NETWORK="deny")
        inline = settings(directory, "inline", retrieval_mode="local", enrichment_mode="inline",
                          fact_index_enabled=True,
                          predicates=[{"predicate": "fact", "cardinality": "Multiple"},
                                      {"predicate": "maintenance_budget", "cardinality": "Single"},
                                      {"predicate": "code_example_language", "cardinality": "Single"}],
                          model_budget=dict(group_id="normal-intake", max_calls=30))
        off = settings(directory, "off", retrieval_mode="local", enrichment_mode="off")
        source_ids = {}
        intake_rows = []
        with contextlib.closing(Client(binary, directory / "inline.sqlite3", loopback,
                                       "s5-normal", config=inline)) as client:
            client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
            for entity, kind in [("松石47", "Other"), ("蓝鲸82", "Other"),
                                 ("王阿姨", "Person"), ("书架甲", "Other"), ("牙医预约", "Other")]:
                client.call("memory_entity_upsert", {"id": entity, "canonical_name": entity,
                                                      "aliases": [], "entity_type": kind})
            for key in STATEMENTS:
                if key == "budget_corrected":
                    break
                text, _, _ = STATEMENTS[key]
                prepared = client.call("memory_prepare", {"user_turn": {
                    "text": text, "session_id": "normal", "turn_id": key, "capture_source": True}})
                remembered = prepared["remember"]["result"]
                assert remembered["enrichment"]["status"] == "committed", (key, prepared)
                assert remembered["source"]["status"] == "stored", (key, prepared)
                source_ids[key] = remembered["source"]["source_id"]
                intake_rows.append({"case": key, "source_id": source_ids[key],
                                    "enrichment": remembered["enrichment"]["status"],
                                    "outcomes": remembered["outcomes"]})
            listing = client.call("memory_list", {})
            assert listing["total"] == 5, listing
            memories = client.stored_memories()
            assert {memory["evidence"][0]["quote"] for memory in memories} == {
                STATEMENTS[key][0] for key in source_ids}, listing
            Fixture.correction_target = next(memory["id"] for memory in memories
                                             if memory["evidence"][0]["quote"] == STATEMENTS["budget_a"][0])
            inline_cases = [call_case(client, case, source_ids) for case in CASES]
            correction = STATEMENTS["budget_corrected"][0]
            updated = client.call("memory_prepare", {"user_turn": {
                "text": correction, "session_id": "normal", "turn_id": "budget_corrected",
                "history": [STATEMENTS["budget_a"][0]], "capture_source": True}})
            assert updated["remember"]["result"]["enrichment"]["status"] == "committed", updated
            source_ids["budget_corrected"] = updated["remember"]["result"]["source"]["source_id"]
            correction_state = {
                "outcomes": updated["remember"]["result"]["outcomes"],
                "memories": [{"id": memory["id"], "status": memory["status"],
                              "superseded_by": memory["superseded_by"],
                              "source_quote": memory["evidence"][0]["quote"]}
                             for memory in client.stored_memories(include_history=True)]
            }
            assert len(correction_state["outcomes"]) == 1, correction_state
            applied = correction_state["outcomes"][0]["Ingested"]["Applied"]
            assert applied["action"] == "Supersede" and applied["target"] == Fixture.correction_target, correction_state
            assert {memory["source_quote"]: memory["status"] for memory in correction_state["memories"]}[
                STATEMENTS["budget_a"][0]] == "Superseded", correction_state
            for name, query in [("correction_exact", "松石47的维护预算是多少？"),
                                ("correction_now", "松石47现在的维护预算是多少？")]:
                corrected = call_case(client, (name, query, "budget_corrected", None, []), source_ids)
                corrected["safe"] &= source_ids["budget_a"] not in corrected["source_ids"]
                corrected["passed"] &= corrected["safe"]
                inline_cases.append(corrected)
        with contextlib.closing(Client(binary, directory / "raw-only.sqlite3", denied,
                                       "s5-normal", config=off)) as client:
            client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
            for key in STATEMENTS:
                text, _, _ = STATEMENTS[key]
                prepared = client.call("memory_prepare", {"user_turn": {
                    "text": text, "session_id": "normal", "turn_id": key, "capture_source": True}})
                remembered = prepared["remember"]["result"]
                assert remembered["source"]["status"] == "stored" and remembered["enrichment"]["status"] == "not_requested"
            assert client.call("memory_list", {})["total"] == 0
            raw_cases = [call_case(client, case, {}) for case in CASES]
        report = {"scope": "synthetic extraction + actual stdio MCP; no host answer observed",
                  "fixture_version": FIXTURE_VERSION, "semantic_review": "mocked, not live model quality",
                  "real_provider_requests": 0, "synthetic_requests": len(Fixture.requests),
                  "intake": intake_rows,
                  "correction_state": correction_state,
                  "diagnostic_note": "The historical '要留多少维护资金' wording may ask for advice, not the recorded budget; it is a safety diagnostic, not an answerability requirement.",
                  "inline": {"cases": inline_cases, "passed": sum(c["passed"] for c in inline_cases),
                             "answerable_cases": sum(c["expected"] is not None for c in inline_cases),
                             "answerable": sum(c["answerable"] for c in inline_cases)},
                  "raw_only": {"cases": raw_cases,
                               "safe_empty": sum(not c["source_ids"] for c in raw_cases),
                               "answerable": sum(c["answerable"] for c in raw_cases)}}
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps({"status": "passed" if all(c["passed"] for c in inline_cases) else "quality_gaps",
                          "inline_cases": len(inline_cases), "inline_passed": report["inline"]["passed"],
                          "inline_answerable": report["inline"]["answerable"],
                          "raw_safe_empty": report["raw_only"]["safe_empty"],
                          "raw_answerable": report["raw_only"]["answerable"],
                          "synthetic_requests": len(Fixture.requests), "real_provider_requests": 0,
                          "failed_cases": [c["case"] for c in inline_cases if not c["passed"]],
                          "output": str(args.output.resolve()) if args.output else None}, ensure_ascii=False))
        assert all(c["safe"] for c in inline_cases), "wrong source returned; inspect JSON report"
        assert all(c["passed"] for c in inline_cases), "normal intake acceptance has quality gaps"


if __name__ == "__main__":
    main()
