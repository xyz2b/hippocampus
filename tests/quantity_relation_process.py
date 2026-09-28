#!/usr/bin/env python3
"""Fee/quantity certificates through real MCP intake, recall and final validation.

Extraction and independent source review use deterministic loopback fixtures.
This tests protocol and source binding, not live model semantic accuracy.
"""
import argparse
import contextlib
import hashlib
import http.server
import json
import os
import sqlite3
import tempfile
import threading
from collections import Counter
from pathlib import Path

from mcp_smoke import Client
from s5_normal_intake import ROOT, candidate, settings
from source_support_fixture import source_support_review


OWNER = "quantity-relation-process"
# Each value is an entire source proposition; no precomputed answer is indexed.
SOURCES = {
    "event_s1": ("海盐折纸工作坊", "Other", "海盐折纸工作坊的报名费为36元。"),
    "event_s2": ("山茶拓印工作坊", "Other", "山茶拓印工作坊的报名费为58元。"),
    "fee_renamed": ("星港摄影课", "Other", "星港摄影课的报名费为72元。"),
    "organization_s1": ("白鹭读书会", "Organization", "白鹭读书会规定，每名会员同时最多可借3册书。"),
    "organization_s2": ("赤枫读书会", "Organization", "赤枫读书会规定，每名会员同时最多可借5册书。"),
    "camera": ("柳岸影像馆", "Organization", "柳岸影像馆规定，每名访客同时最多可借2台相机。"),
    "role_member": ("樟桥器材室", "Organization", "樟桥器材室规定，每名会员同时最多可借4台相机。"),
    "role_visitor": ("樟桥器材室", "Organization", "樟桥器材室规定，每名访客同时最多可借1台相机。"),
}
# id, query, context, source ref, answer. None means no source may be released.
QUERIES = [
    ("event_q5", "参加海盐折纸工作坊，报名要交多少钱？", [], "event_s1", "36元"),
    ("fee_other_subject", "参加山茶拓印工作坊，报名要交多少钱？", [], "event_s2", "58元"),
    ("fee_renamed", "参加星港摄影课，报名要交多少钱？", [], "fee_renamed", "72元"),
    ("organization_q3", "每人同时最多能借几本书？", ["我问的是白鹭读书会。"], "organization_s1", "3册"),
    ("organization_q4", "赤枫读书会每名会员同时最多能借几册书？", [], "organization_s2", "5册"),
    ("organization_q5", "在白鹭读书会，一名会员手上最多可以有几本尚未归还的书？", [], "organization_s1", "3册"),
    ("camera_renamed", "柳岸影像馆每名访客同时最多能借几台相机？", [], "camera", "2台"),
    ("role_specific_member", "樟桥器材室每名会员同时最多能借几台相机？", [], "role_member", "4台"),
    ("role_specific_visitor", "樟桥器材室每名访客同时最多能借几台相机？", [], "role_visitor", "1台"),
    ("role_generic_ambiguous", "樟桥器材室每人同时最多能借几台相机？", [], None, None),
    ("fee_other_relation", "海盐折纸工作坊的押金是多少钱？", [], None, None),
    ("fee_fronted_condition", "周末参加海盐折纸工作坊，报名要交多少钱？", [], None, None),
    ("fee_conditional", "如果参加海盐折纸工作坊，报名要交多少钱？", [], None, None),
    ("fee_extra_clause", "参加海盐折纸工作坊，周末报名要交多少钱？", [], None, None),
    ("fee_missing_subject", "报名要交多少钱？", [], None, None),
    ("quantity_missing_subject", "每名会员同时最多能借几册书？", [], None, None),
    ("quantity_lower_bound", "白鹭读书会每名会员同时至少能借几册书？", [], None, None),
    ("quantity_missing_person", "白鹭读书会同时最多能借几册书？", [], None, None),
    ("quantity_missing_simultaneous", "白鹭读书会每名会员最多能借几册书？", [], None, None),
    ("quantity_cumulative", "白鹭读书会每名会员一共能借几册书？", [], None, None),
    ("quantity_total", "白鹭读书会总共有几册书？", [], None, None),
    ("quantity_duration", "白鹭读书会每名会员最多能借几天书？", [], None, None),
    ("quantity_wrong_unit", "白鹭读书会每名会员同时最多能借几台书？", [], None, None),
    ("quantity_wrong_object", "白鹭读书会每名会员同时最多能借几册杂志？", [], None, None),
    ("quantity_wrong_object_alias", "白鹭读书会每名会员同时最多能借几本杂志？", [], None, None),
    ("quantity_camera_wrong_unit", "柳岸影像馆每名访客同时最多能借几件相机？", [], None, None),
    ("quantity_wrong_role", "白鹭读书会每名工作人员同时最多能借几册书？", [], None, None),
    ("quantity_wrong_role_visitor", "白鹭读书会每名访客同时最多能借几册书？", [], None, None),
    ("quantity_wrong_role_member", "柳岸影像馆每名会员同时最多能借几台相机？", [], None, None),
    ("quantity_fronted_condition", "周末在白鹭读书会，每名会员同时最多能借几册书？", [], None, None),
    ("quantity_uncertain_outstanding", "在白鹭读书会，一名会员手上最多可以有几本可能尚未归还的书？", [], None, None),
    ("quantity_returned", "在白鹭读书会，一名会员手上最多可以有几本已归还的书？", [], None, None),
    ("quantity_unknown_subject", "青禾读书会每名会员同时最多能借几册书？", [], None, None),
]


class Fixture(http.server.BaseHTTPRequestHandler):
    requests = []
    errors = []

    def log_message(self, *_):
        pass

    def do_POST(self):
        try:
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert self.path == "/v1/responses"
            assert self.headers["Authorization"] == "Bearer local-fixture-key"
            stage = request["text"]["format"]["name"]
            prompt = json.loads(request["input"])
            source = next(value for value in SOURCES.values() if value[2] == prompt["current_user_turn"])
            assert prompt["conversation_history"] == []
            assert stage in ("personal_memory_candidates_v1", "personal_memory_source_support_v1")
            if stage == "personal_memory_source_support_v1":
                frozen = prompt["frozen_candidates"]
                assert len(frozen) == 1
                assert frozen[0]["subject"] == {"Resolved": source[0]}
                assert frozen[0]["value"] == {"Text": source[2]}
                assert frozen[0]["evidence"][0]["quote"] == source[2]
                output = source_support_review(request)
            else:
                output = {"omission_reason": None, "candidates": [candidate(source[0], source[2])]}
            type(self).requests.append(stage)
            body = {"status": "completed", "usage": {"input_tokens": 10, "output_tokens": 10},
                    "output": [{"type": "message", "content": [{"type": "output_text",
                               "text": json.dumps(output, ensure_ascii=False)}]}]}
            status = 200
        except Exception as error:
            type(self).errors.append(repr(error))
            status, body = 500, {"error": {"message": "local fixture assertion failed"}}
        data = json.dumps(body, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def check_query(client, case, source_ids):
    name, query, context, ref, answer = case
    recalled = client.call("memory_recall", {"query": query, "context": context})
    contract = recalled["answer_contract"]
    expected_ids = [source_ids[ref]] if ref else []
    expected_sources = [{"source_id": source_ids[ref], "quote": SOURCES[ref][2]}] if ref else []
    failures = []
    if contract["query"] != query or contract["context"] != context:
        failures.append("contract did not preserve original question/context")
    if contract["safe_source_ids"] != expected_ids or contract.get("sources", []) != expected_sources:
        failures.append("wrong or absent certified source")
    if ref and contract["safe_answer"] != answer + "。":
        failures.append("wrong or absent certified value")
    evidence = recalled["source_evidence"]
    if name == "role_generic_ambiguous":
        # Existing recall contract keeps related committed quotes, but gives neither
        # quote an answer certificate when role-dependent values disagree.
        related = {source_ids[key]: SOURCES[key][2] for key in ("role_member", "role_visitor")}
        if ({item["source_id"]: item["text"] for item in evidence} != related
                or any("verified_fact" in item or "verified_identity" in item
                       or "unique_in_snapshot" in json.dumps(item) for item in evidence)):
            failures.append("ambiguous roles exposed unrelated or answer-certified evidence")
    elif [item["source_id"] for item in evidence] != expected_ids:
        failures.append("answer evidence does not match the certified source")
    if recalled["model_calls"]:
        failures.append("local query performed model work")
    fallback_ref, fallback_draft = ("role_member", "4台") if name == "role_generic_ambiguous" else (
        ("event_s1", "36元") if name.startswith("fee_") or name == "pending_fee" else (
            ("camera", "2台") if "camera" in name or name == "quantity_wrong_role_member"
            else ("organization_s1", "3册")))
    validation = client.call("memory_validate_answer", {
        "query": query, "context": context, "draft_answer": (answer or fallback_draft) + "。",
        "source_ids": [source_ids[ref or fallback_ref]]})
    if validation["approved"] != bool(ref) or validation["approved_source_ids"] != expected_ids:
        failures.append("final answer validation did not enforce the same certificate")
    if validation["model_calls"]:
        failures.append("local final validation performed model work")
    if not ref and (validation["safe_source_ids"] or validation["sources"]):
        failures.append("final validation released sources for an unsupported answer")
    alternative = None
    if name == "role_generic_ambiguous":
        alternative = client.call("memory_validate_answer", {
            "query": query, "context": context, "draft_answer": "1台。",
            "source_ids": [source_ids["role_visitor"]]})
        if (alternative["approved"] or alternative["approved_source_ids"] or
                alternative["safe_source_ids"] or alternative["sources"] or alternative["model_calls"]):
            failures.append("generic role question accepted one of the role-dependent values")
    return dict(id=name, query=query, context=context, expected_value=answer,
                expected_source_ids=expected_ids, passed=not failures, failures=failures,
                contract=contract, source_evidence=evidence, validation=validation,
                alternative_validation=alternative)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/pmr-mcp")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    Fixture.requests, Fixture.errors = [], []
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-quantity-relation-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        clean = {key: value for key, value in os.environ.items()
                 if not key.startswith(("DEEPSEEK_", "OPENAI_", "ARK_", "PMR_"))}
        env = dict(clean, PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
                   DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1", NO_PROXY="127.0.0.1,localhost")
        db = directory / "isolated.sqlite3"
        budget = dict(group_id="quantity-relation-loopback", max_calls=2 * len(SOURCES), max_operation_calls=2)
        config = settings(directory, "inline", retrieval_mode="local", enrichment_mode="inline",
                          fact_index_enabled=True, model_budget=budget, memory_types=[], value_kinds=[],
                          predicates=[{"predicate": "fact", "cardinality": "Multiple"}])
        source_ids, intake, rows = {}, [], []
        with contextlib.closing(Client(binary, db, env, OWNER, config=config)) as client:
            client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
            for subject, kind, _ in SOURCES.values():
                client.call("memory_entity_upsert", {"id": subject, "canonical_name": subject,
                                                     "aliases": [], "entity_type": kind})
            for name, (subject, _, text) in SOURCES.items():
                prepared = client.call("memory_prepare", {"user_turn": {
                    "text": text, "history": [], "session_id": "quantity-process",
                    "turn_id": name, "capture_source": True}})
                receipt = prepared["remember"]["result"]
                assert receipt["source"]["status"] == "stored", receipt
                assert receipt["index"]["status"] == "ready", receipt
                assert receipt["enrichment"]["status"] == "committed", receipt
                source_ids[name] = receipt["source"]["source_id"]
                memories = [m for m in client.stored_memories()
                            if m["subject_id"] == subject and m["value"] == {"Text": text}]
                assert len(memories) == 1 and memories[0]["value"] == {"Text": text}, memories
                assert memories[0]["evidence"][0]["quote"] == text, memories
                intake.append(dict(id=name, source_id=source_ids[name], passed=True))
            for case in QUERIES:
                rows.append(check_query(client, case, source_ids))
            # These are the original positive/negative final-gate gaps, not just recall tests.
            for name, query_id, draft, ref in [
                ("organization_v1", "organization_q4", "5册。", "organization_s2"),
                ("organization_v2", "organization_q4", "3册。", "organization_s2"),
                ("fee_wrong_amount", "event_q5", "58元。", "event_s1"),
                ("quantity_other_source", "organization_q4", "5册。", "organization_s1"),
                ("fee_other_source", "event_q5", "36元。", "event_s2"),
            ]:
                _, query, context, expected_ref, value = next(case for case in QUERIES if case[0] == query_id)
                result = client.call("memory_validate_answer", {
                    "query": query, "context": context, "draft_answer": draft, "source_ids": [source_ids[ref]]})
                expected = name == "organization_v1"
                fallback = result
                passed = (result["approved"] == expected and result["approved_source_ids"] ==
                          ([source_ids[ref]] if expected else []) and not result["model_calls"]
                          and fallback["safe_answer"] == value + "。"
                          and fallback["safe_source_ids"] == [source_ids[expected_ref]])
                rows.append(dict(id=name, passed=passed, validation=result))
        # Newer raw evidence has not been reviewed: a previously certified old value is stale.
        off = settings(directory, "off", retrieval_mode="local", enrichment_mode="off",
                       fact_index_enabled=True, model_budget=budget, memory_types=[], value_kinds=[],
                       predicates=[{"predicate": "fact", "cardinality": "Multiple"}])
        with contextlib.closing(Client(binary, db, env, OWNER, config=off)) as client:
            client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
            for name, text, query in [
                ("pending_quantity", "白鹭读书会规定，每名会员同时最多可借7册书。",
                 "白鹭读书会每名会员同时最多能借几册书？"),
                ("pending_fee", "海盐折纸工作坊的报名费为49元。", "参加海盐折纸工作坊，报名要交多少钱？"),
            ]:
                receipt = client.call("memory_remember", {"text": text, "history": [],
                    "session_id": "quantity-process", "turn_id": name, "capture_source": True})
                assert receipt["source"]["status"] == "stored", receipt
                rows.append(check_query(client, (name, query, [], None, None), source_ids))
        with sqlite3.connect(db) as connection:
            ledger = connection.execute("SELECT stage, outcome FROM model_calls").fetchall()
        stages = Counter(stage for stage, _ in ledger)
        assert stages == {"extraction": len(SOURCES), "extraction_review": len(SOURCES)}, stages
        assert all(status == "completed" for _, status in ledger), ledger
        assert len(Fixture.requests) == 2 * len(SOURCES) and not Fixture.errors, Fixture.errors
        report = dict(status="passed" if all(row["passed"] for row in rows) else "failed",
                      binary=str(binary), binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                      real_provider_requests=0, loopback_model_requests=len(Fixture.requests),
                      fixture_note="Protocol fixture; semantic review is deterministic, not a live-model accuracy result.",
                      assertion_note="The initial generic-role test incorrectly required no recalled related quotes. "
                                     "It now follows the existing contract: exactly the two committed role-specific "
                                     "quotes may be recalled, with no answer certificate or approved source; neither "
                                     "role-specific value may pass final validation. Other no-answer cases still "
                                     "require empty source_evidence.",
                      intake=intake, results=rows, failed_cases=[row["id"] for row in rows if not row["passed"]])
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({key: report[key] for key in ("status", "real_provider_requests", "loopback_model_requests", "failed_cases")}, ensure_ascii=False))
    return int(report["status"] != "passed")


if __name__ == "__main__":
    raise SystemExit(main())
