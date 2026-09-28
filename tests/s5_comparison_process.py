#!/usr/bin/env python3
"""S5 two-relation proposal contract through stdio MCP and a local Responses fixture."""
import argparse
import contextlib
import http.server
import json
import os
import tempfile
import threading
from pathlib import Path

from mcp_smoke import Client, Responses
from s5_normal_intake import candidate, settings
from source_support_fixture import FIXTURE_VERSION


QUESTION = "松石47的技术选择和部署限制是否一致？"
STATEMENTS = {
    "choice": ("项目松石47选用 SQLite。", "技术选择", "SQLite"),
    "denied": ("松石47选用 SQLite 的说法已被否定。", "技术选择", "SQLite"),
    "sqlite3": ("松石47选用 SQLite3。", "技术选择", "SQLite"),
    "abandoned": ("松石47选择放弃 SQLite。", "技术选择", "SQLite"),
    "negated": ("松石47选择不使用 SQLite。", "技术选择", "SQLite"),
    "limit": ("松石47只能在一台机器部署。", "部署限制", "一台机器"),
    "more_than_one": ("松石47只能在多于一台机器部署。", "部署限制", "一台机器"),
    "current": ("松石47当前只部署在一台机器上。", "部署限制", "一台机器"),
}
PREDICATES = [{"predicate": relation, "cardinality": "Single"}
              for relation in ("技术选择", "部署限制", "code_example_language")]
PREDICATES.append({"predicate": "fact", "cardinality": "Multiple"})


class Fixture(Responses):
    calls = 0
    formats = []
    proposal = "valid"
    newer_mode = "no_facts"

    def output(self, body):
        name = body["text"]["format"]["name"]
        type(self).formats.append(name)
        prompt = json.loads(body["input"])
        if name == "personal_memory_source_support_v1":
            return super().output(body)
        if name == "personal_memory_comparison_v1":
            quotes = [item["quote"] for item in prompt["candidates"]]
            choice = next(i for i, quote in enumerate(quotes)
                          if any(phrase in quote for phrase in (
                              "选用 SQLite", "选择不使用 SQLite", "选择放弃 SQLite")))
            deployment = next(i for i, quote in enumerate(quotes)
                              if "一台机器" in quote)
            if self.proposal == "ambiguous":
                return {"ambiguous": True, "roles": []}
            choice_relation = {
                "negated": "选择不使用",
                "abandoned": "选择",
                "long_valid": "选用 SQLite",
                "long_unsafe": "选用 SQLite",
                "long_negated": "选择不使用 SQLite",
            }.get(self.proposal, "选用")
            deployment_relation = {
                "unsafe": "当前只部署在",
                "long_valid": "只能在一台机器部署",
                "long_unsafe": "当前只部署在一台机器上",
                "long_negated": "只能在一台机器部署",
                "long_more_than_one": "只能在多于一台机器部署",
            }.get(self.proposal, "只能在")
            return {"ambiguous": False, "roles": [
                {"query_relation": "技术选择", "candidate_index": choice,
                 "evidence_relation": choice_relation},
                {"query_relation": "部署限制",
                 "candidate_index": 99 if self.proposal == "wrong_index" else deployment,
                 "evidence_relation": deployment_relation},
            ]}
        if name == "personal_memory_assistance_queries_v1":
            return {"queries": []}
        statement = prompt["current_user_turn"]
        if statement == "松石47现在只能在两台机器部署。":
            if self.newer_mode == "no_facts":
                return {"omission_reason": "no_personal_assertion", "candidates": []}
            return {"omission_reason": None,
                    "candidates": [candidate("松石47", statement, predicate="fact")]}
        _, relation, value = next(row for row in STATEMENTS.values()
                                  if row[0] == statement)
        return {"omission_reason": None,
                "candidates": [candidate("松石47", value, predicate=relation)]}


def ingest(binary, db, env, root, label, keys):
    config = settings(root, f"{label}-intake", retrieval_mode="local",
                      enrichment_mode="inline", fact_index_enabled=True,
                      predicates=PREDICATES,
                      model_budget={"group_id": f"{label}-intake", "max_calls": 4})
    source_ids = {}
    with contextlib.closing(Client(binary, db, env, "s5-comparison", config=config)) as client:
        client.call("memory_entity_upsert", {
            "id": "松石47", "canonical_name": "松石47", "aliases": [],
            "entity_type": "Other"})
        for key in keys:
            text, _, _ = STATEMENTS[key]
            result = client.call("memory_remember", {
                "text": text, "session_id": "s5-comparison",
                "turn_id": f"{label}-{key}", "capture_source": True})
            assert result["source"]["status"] == "stored", result
            assert result["enrichment"]["status"] == "committed", result
            source_ids[key] = result["source"]["source_id"]
        memories = client.stored_memories()
        assert len(memories) == 2, memories
        assert {memory["predicate"] for memory in memories} == {
            "技术选择", "部署限制"}, memories
        assert {memory["evidence"][0]["quote"] for memory in memories} == {
            STATEMENTS[key][0] for key in keys}, memories
        local = client.call("memory_recall", {"query": QUESTION})
        assert local["source_evidence"] == [], local
    return source_ids


def assisted(binary, db, env, root, label, expected, sends=1):
    Fixture.proposal = label
    config = settings(root, label, retrieval_mode="assisted", enrichment_mode="off",
                      fact_index_enabled=True, predicates=PREDICATES,
                      local_recall_budget={"candidate_k": 30, "source_limit": 2,
                                           "content_bytes": 8192},
                      model_budget={"group_id": label, "max_calls": 1,
                                    "max_operation_calls": 1})
    before = Fixture.calls
    with contextlib.closing(Client(binary, db, env, "s5-comparison", config=config)) as client:
        result = client.call("memory_recall", {"query": QUESTION})
    observed_sends = Fixture.calls - before
    assert observed_sends in ((0, 1) if sends is None else (sends,)), (label, result)
    if observed_sends:
        assert Fixture.formats[-1] == "personal_memory_comparison_v1", (label, result)
    actual = {(item["source_id"], item["text"])
              for item in result["source_evidence"]}
    assert actual == expected, (label, result)
    assert len(result["model_calls"]) == observed_sends, (label, result)
    if sends == 0:
        assert result["assistance"] == {
            "status": "not_requested", "reason": "uncommitted_newer_revision"}, result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/pmr-mcp"))
    args = parser.parse_args()
    binary = args.binary.resolve()
    assert binary.is_file(), f"build the MCP binary first: {binary}"
    clean = {key: value for key, value in os.environ.items()
             if not key.startswith(("DEEPSEEK_", "OPENAI_", "PMR_"))}
    Fixture.calls = 0
    Fixture.formats = []
    with contextlib.ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-s5-comparison-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        env = dict(clean, PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
                   DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
                   NO_PROXY="127.0.0.1,localhost")
        good_db = root / "good.sqlite3"
        good = ingest(binary, good_db, env, root, "good", ("choice", "limit"))
        assisted(binary, good_db, env, root, "valid", {
            (good["choice"], STATEMENTS["choice"][0]),
            (good["limit"], STATEMENTS["limit"][0])})
        assisted(binary, good_db, env, root, "long_valid", {
            (good["choice"], STATEMENTS["choice"][0]),
            (good["limit"], STATEMENTS["limit"][0])})
        for label in ("wrong_index", "ambiguous"):
            assisted(binary, good_db, env, root, label, set())
        off = settings(root, "pending-intake", retrieval_mode="local",
                       enrichment_mode="off", predicates=PREDICATES)
        with contextlib.closing(Client(binary, good_db, env, "s5-comparison", config=off)) as client:
            pending = client.call("memory_remember", {
                "text": "更正：松石47的部署限制是两台机器。",
                "session_id": "s5-comparison", "turn_id": "pending",
                "capture_source": True})
            assert pending["enrichment"]["status"] == "not_requested", pending
        assisted(binary, good_db, env, root, "pending", set(), sends=0)
        unsafe_db = root / "unsafe.sqlite3"
        ingest(binary, unsafe_db, env, root, "unsafe", ("choice", "current"))
        assisted(binary, unsafe_db, env, root, "unsafe", set())
        assisted(binary, unsafe_db, env, root, "long_unsafe", set())
        negated_db = root / "negated.sqlite3"
        ingest(binary, negated_db, env, root, "negated", ("negated", "limit"))
        assisted(binary, negated_db, env, root, "negated", set())
        assisted(binary, negated_db, env, root, "long_negated", set())
        for key in ("denied", "sqlite3"):
            db = root / f"{key}.sqlite3"
            ingest(binary, db, env, root, key, (key, "limit"))
            assisted(binary, db, env, root, key, set())
        for key, rows in (("abandoned", ("abandoned", "limit")),
                          ("more_than_one", ("choice", "more_than_one"))):
            db = root / f"{key}.sqlite3"
            ingest(binary, db, env, root, key, rows)
            assisted(binary, db, env, root, key, set())
            if key == "more_than_one":
                assisted(binary, db, env, root, "long_more_than_one", set())
        no_facts_db = root / "no-facts.sqlite3"
        ingest(binary, no_facts_db, env, root, "no-facts", ("choice", "limit"))
        Fixture.newer_mode = "no_facts"
        no_facts = settings(root, "no-facts-followup", retrieval_mode="local",
                            enrichment_mode="inline", predicates=PREDICATES,
                            model_budget={"group_id": "no-facts-followup", "max_calls": 2})
        with contextlib.closing(Client(binary, no_facts_db, env, "s5-comparison",
                                       config=no_facts)) as client:
            newer = client.call("memory_remember", {
                "text": "松石47现在只能在两台机器部署。",
                "session_id": "s5-comparison", "turn_id": "no-facts-newer",
                "capture_source": True})
            assert newer["enrichment"]["status"] == "completed_no_facts", newer
        assisted(binary, no_facts_db, env, root, "no-facts", set(), sends=0)
        wrong_db = root / "wrong-committed.sqlite3"
        ingest(binary, wrong_db, env, root, "wrong-committed", ("choice", "limit"))
        Fixture.newer_mode = "wrong_fact"
        wrong_intake = settings(root, "wrong-committed-followup", retrieval_mode="local",
                                enrichment_mode="inline", predicates=PREDICATES,
                                model_budget={"group_id": "wrong-committed-followup", "max_calls": 2})
        with contextlib.closing(Client(binary, wrong_db, env, "s5-comparison",
                                       config=wrong_intake)) as client:
            newer = client.call("memory_remember", {
                "text": "松石47现在只能在两台机器部署。",
                "session_id": "s5-comparison", "turn_id": "wrong-committed-newer",
                "capture_source": True})
            assert newer["enrichment"]["status"] == "committed", newer
            listing = client.call("memory_list", {})
            assert len(listing["memories"]) == 3, listing
            assert any(memory["predicate"] == "fact" for memory in client.stored_memories()), listing
            local = client.call("memory_recall", {"query": QUESTION})
            assert local["source_evidence"] == [], local
        assisted(binary, wrong_db, env, root, "wrong-committed", set(), sends=None)
    print(json.dumps({"status": "passed", "cases": 16,
                      "fixture_version": FIXTURE_VERSION, "semantic_review": "mocked, not live model quality",
                      "synthetic_requests": Fixture.calls,
                      "provider_requests": 0}, ensure_ascii=False))


if __name__ == "__main__":
    main()
