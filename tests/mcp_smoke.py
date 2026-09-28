#!/usr/bin/env python3
"""Actual stdio MCP roundtrip. Default: loopback Responses fixture. --live: configured API."""
import argparse
import contextlib
import http.server
import json
import os
import queue
import sqlite3
import subprocess
import tempfile
import threading
import time
from pathlib import Path
from source_support_fixture import source_support_review


class Responses(http.server.BaseHTTPRequestHandler):
    calls = 0

    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        assert self.path == "/v1/responses"
        assert self.headers["Authorization"] == "Bearer local-fixture-key"
        assert body["model"] == "ep-20260923215039-vxgxg"
        assert body["reasoning"]["effort"] == "medium"
        assert body["max_output_tokens"] == 4096
        assert body["text"]["format"]["type"] == "json_schema"
        assert body["text"]["format"]["strict"] is True
        type(self).calls += 1
        output = self.output(body)
        data = json.dumps({"status": "completed", "output": [{"type": "message", "content": [
            {"type": "output_text", "text": json.dumps(output, ensure_ascii=False)}]}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def output(self, body):
        if body["text"]["format"]["name"] == "personal_memory_source_support_v1":
            return source_support_review(body)
        axes = dict(companion_roles=[], companion_entity_ids=[], geography=None,
                    country=None, city=None, date=None, time_of_day=None, activities=[], task_id=None)
        if "runtime_context" in body["text"]["format"]["name"]:
            output = dict(domain=None, entity_ids=["self"], task_id=None, conditions=axes,
                          predicates=["response_language"], targets=[], temporal_intent="current", temporal_expression=None)
        else:
            output = {"candidates": [dict(
                subject_id="self", unresolved_subject=None, memory_type="constraint", predicate="response_language",
                value=dict(kind="text", text="中文", integer=None, boolean=None, entity_id=None),
                polarity=None, strength=None, scope=dict(domain=None, conditions=axes), retention_tier="durable",
                valid_from=None, valid_to=None, expires_at=None, temporal_expression=None,
                extraction_confidence="high", certainty="high", source="user_statement",
                intent=dict(kind="assert", target_memory_id=None, reason=None), explicit_memory_request=True,
                cross_turn_useful=True, tags=[])]}
        return output



class Client:
    def __init__(self, binary, db, env, owner="smoke-user", config=None, budget_calls=None):
        self.db = db
        self.owner = owner
        if budget_calls is not None:
            assert config is None, "provide a budget in the explicit config instead"
            config = Path(str(db) + ".fixture-config.json")
            config.write_text(json.dumps({"model_budget": {
                "group_id": "isolated-fixture", "max_calls": budget_calls}}))
        self.errors = tempfile.TemporaryFile(mode="w+t")
        self.proc = subprocess.Popen([str(binary), "--db", str(db), "--owner", owner] + (["--config", str(config)] if config else []),
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.errors,
                                     text=True, encoding="utf-8", env=env)
        self.responses = queue.Queue()
        self.reader = threading.Thread(target=self.read_lines, daemon=True)
        self.reader.start()
        self.number = 0
        result = self.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                        "clientInfo": {"name": "pmr-smoke", "version": "1"}})
        assert result["capabilities"]["tools"] == {}, result
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def read_lines(self):
        try:
            for line in self.proc.stdout:
                self.responses.put(json.loads(line))
        except Exception as error:
            self.responses.put(error)
        finally:
            self.responses.put(None)

    def send(self, body):
        self.proc.stdin.write(json.dumps(body, ensure_ascii=False) + "\n")
        self.proc.stdin.flush()

    def rpc(self, method, params):
        self.number += 1
        self.send({"jsonrpc": "2.0", "id": self.number, "method": method, "params": params})
        deadline = time.monotonic() + 420
        while True:
            response = self.responses.get(timeout=max(0.01, deadline - time.monotonic()))
            assert isinstance(response, dict), f"MCP process ended or produced invalid JSON: {response}"
            if response.get("id") == self.number:
                assert "error" not in response, response
                return response["result"]

    def call(self, name, args, error=False):
        result = self.rpc("tools/call", {"name": name, "arguments": args})
        assert bool(result.get("isError")) == error, result
        return result.get("structuredContent") or json.loads(result["content"][0]["text"])

    def stored_memories(self, include_history=False):
        """Test-only inspection of the isolated SQLite profile, outside the MCP answer surface."""
        with sqlite3.connect(self.db) as connection:
            row = connection.execute("SELECT state_json FROM memory_profiles WHERE owner_id=?",
                                     (self.owner,)).fetchone()
        if not row:
            return []
        memories = json.loads(row[0])["repository"]["memories"]
        memories = memories.values() if isinstance(memories, dict) else memories
        return sorted((memory for memory in memories if memory["status"] != "Deleted"
                       and (include_history or memory["status"] == "Active")),
                      key=lambda memory: memory["id"])

    def close(self):
        self.proc.stdin.close()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
            raise AssertionError("MCP server did not stop after stdin EOF")
        finally:
            self.reader.join(timeout=2)
            self.proc.stdout.close()
            self.errors.close()
        assert self.proc.returncode == 0, self.proc.returncode


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/pmr-mcp"))
    parser.add_argument("--live", action="store_true")
    args = parser.parse_args()
    env = dict(os.environ)
    env.pop("PMR_OPENAI_TRACE_CONTENT", None)
    env.pop("PMR_OPENAI_TRACE", None)
    started = time.monotonic()
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-mcp-smoke-")))
        if not args.live:
            server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Responses)
            threading.Thread(target=server.serve_forever, daemon=True).start()
            stack.callback(server.server_close)
            stack.callback(server.shutdown)
            env.update(PMR_MODEL_NETWORK="offline", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1", DEEPSEEK_API_KEY="local-fixture-key")
        else:
            assert env.get("DEEPSEEK_API_KEY") and env.get("DEEPSEEK_BASE_URL"), "configure DeepSeek API environment first"
            # This test uses only the synthetic statement below; retain provider output on failures.
            env.update(PMR_OPENAI_TRACE="1", PMR_OPENAI_TRACE_CONTENT="1")
        db = directory / "memory.sqlite3"
        request = dict(text="请记住：无论讨论什么主题，以后所有回答都必须使用中文。", session_id="mcp-smoke-session", turn_id="one")
        client = Client(args.binary.resolve(), db, env, budget_calls=None if args.live else 100)
        try:
            tools = client.rpc("tools/list", {})["tools"]
            names = {tool["name"] for tool in tools}
            assert names == {"memory_prepare", "memory_remember", "memory_recall",
                             "memory_validate_answer", "memory_list", "memory_entity_upsert",
                             "memory_clarify", "memory_forget"}, names
            assert client.call("memory_list", {})["total"] == 0
            client.call("memory_list", {"owner_id": "another-user"}, error=True)
            client.call("memory_remember", {**request, "session_id": " "}, error=True)
            print("MCP initialized; 8 tools and parameter validation passed", flush=True)
            result = client.call("memory_remember", request)
            assert result["replayed"] is False, result
            listing = client.call("memory_list", {})
            assert listing["total"] == 1, listing
            memory = client.stored_memories()[0]
            assert listing["memories"] == [{"id": memory["id"], "status": memory["status"]}], listing
            assert memory["predicate"] == "response_language", memory
            assert memory["memory_type"] == "Constraint", memory
            saved_retention_tier = memory["retention_tier"]
            assert memory["scope"] == {"domain": None, "conditions": []}, memory
            assert memory["owner_id"] == "smoke-user" and memory["evidence"][0]["quote"] == request["text"], memory
            memory_id = memory["id"]
            print(f"Saved memory {memory_id}; restarting server", flush=True)
        finally:
            if args.live:
                client.errors.seek(0)
                print(client.errors.read(), end="", flush=True)
            client.close()
        client = Client(args.binary.resolve(), db, env, budget_calls=None if args.live else 100)
        try:
            assert client.call("memory_remember", request)["replayed"] is True
            client.call("memory_remember", {**request, "text": "改为英语"}, error=True)
            result = client.call("memory_recall", {"query": "根据你记住的要求，你以后必须用什么语言回答我？"})
            assert any(memory["id"] == memory_id for memory in result["result"]["mandatory"]), result
            assert client.stored_memories()[0]["retrieval_count"] == 1
            print("Restart, idempotent replay and semantic recall passed", flush=True)
        finally:
            if args.live:
                client.errors.seek(0)
                print(client.errors.read(), end="", flush=True)
            client.close()
        # Local management and deletion must work even when cloud credentials are absent.
        offline = dict(env)
        offline.pop("DEEPSEEK_API_KEY", None)
        offline.pop("DEEPSEEK_BASE_URL", None)
        # Legacy provider credentials must never mask missing DeepSeek configuration.
        offline.update(OPENAI_BASE_URL="http://127.0.0.1:1", OPENAI_API_KEY="unused-legacy-key")
        client = Client(args.binary.resolve(), db, offline, owner="someone-else")
        try:
            assert client.call("memory_list", {})["total"] == 0
            client.call("memory_forget", {"memory_id": memory_id}, error=True)
        finally:
            client.close()
        client = Client(args.binary.resolve(), db, offline)
        try:
            missing = client.call("memory_recall", {"query": "回答语言"}, error=True)
            assert missing["error"] == "DEEPSEEK_BASE_URL is not set", missing
            assert client.stored_memories()[0]["retrieval_count"] == 1
            bootstrap = client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
            assert bootstrap["result"]["mandatory"][0]["id"] == memory_id, bootstrap
            assert client.stored_memories()[0]["retrieval_count"] == 2
            client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True, "task_id": "task"}, error=True)
            assert client.call("memory_remember", request)["replayed"] is True
            client.call("memory_forget", {"memory_id": memory_id})
        finally:
            client.close()
        client = Client(args.binary.resolve(), db, offline)
        try:
            assert client.call("memory_list", {"include_history": True})["total"] == 0
        finally:
            client.close()
        if not args.live:
            assert Responses.calls == 3, f"extraction + review + recall; replay must not send again: {Responses.calls}"
        # Check classification after recall so its failure does not hide provider compatibility.
        assert saved_retention_tier == "Durable", {"retention_tier": saved_retention_tier}
        print(json.dumps({"status": "passed", "mode": "live" if args.live else "offline-fixture",
                          "seconds": round(time.monotonic() - started, 3),
                          "checks": ["MCP handshake", "tools/list", "validation", "remember", "restart", "replay",
                                     "recall", "persisted counters", "owner isolation", "offline management", "forget restart"]}))
        print(json.dumps({"acf_assertion": {"id": "deepseek-mcp-roundtrip", "outcome": "pass",
                          "observed": "Actual isolated MCP save, restart, replay, recall and offline checks passed; mode="
                                      + ("live" if args.live else "fixture")}}))


if __name__ == "__main__":
    main()
