#!/usr/bin/env python3
"""Shared stdio MCP client and synthetic Responses fixture. Active checks use m1_host.py."""
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
