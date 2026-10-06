#!/usr/bin/env python3
"""MCP 在线召回：事件屏障下的并发、去重、删除、快照和取消；仅 loopback 合成响应。"""
import argparse
import contextlib
import http.server
import io
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import threading

from mcp_smoke import Client
from recall_pipeline_process import AT, OWNER, TEXT, Fixture


class ConcurrentClient(Client):
    def __init__(self, *args, **kwargs):
        self.pending = {}
        super().__init__(*args, **kwargs)

    def start(self, method, params):
        self.number += 1
        self.send({"jsonrpc": "2.0", "id": self.number, "method": method, "params": params})
        return self.number

    def finish(self, number, timeout=20):
        while number not in self.pending:
            response = self.responses.get(timeout=timeout)
            assert isinstance(response, dict), response
            if "id" in response:
                self.pending[response["id"]] = response
        response = self.pending.pop(number)
        assert "error" not in response, response
        return response["result"]

    def rpc(self, method, params):
        return self.finish(self.start(method, params))

    def start_call(self, name, args):
        return self.start("tools/call", {"name": name, "arguments": args})

    def finish_call(self, number, error=False, timeout=20):
        result = self.finish(number, timeout)
        assert bool(result.get("isError")) == error, result
        return result.get("structuredContent") or json.loads(result["content"][0]["text"])


class HeldFixture(Fixture):
    gate = None
    requests = []

    def do_POST(self):
        raw = self.rfile.read(int(self.headers["Content-Length"]))
        body = json.loads(raw)
        gate = type(self).gate
        if gate and body["text"]["format"]["name"] == gate[0]:
            gate[1].set()
            assert gate[2].wait(20), "test did not release the model response"
        stream = self.rfile
        try:
            self.rfile = io.BytesIO(raw)
            super().do_POST()
        finally:
            self.rfile = stream


def query(number):
    return {"query": "我的灯是什么颜色？", "request_id": f"concurrent-{number}", "at": AT}


def gate(stage):
    entered, release = threading.Event(), threading.Event()
    HeldFixture.gate = stage, entered, release
    return entered, release


def while_held(client):
    result = client.finish_call(client.start_call("memory_list", {}), timeout=5)
    assert "global_context" in result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    binary = parser.parse_args().binary
    with contextlib.ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="mcp-recall-concurrent-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), HeldFixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("PMR_", "OPENAI_", "DEEPSEEK_"))}
        env.update(PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="fixture-key",
                   DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1")
        config = root / "config.json"
        config.write_text(json.dumps({"recall_pipeline": "recall_v3", "enrichment_mode": "off",
            "recall_models": {"r1": "pro-fixture", "r4": "pro-fixture",
                              "reasoning_effort": {"r1": "minimal", "r4": "minimal"}},
            "model_budget": {"group_id": "mcp-concurrency", "max_calls": 40}}))
        db = root / "memory.sqlite3"
        client = ConcurrentClient(binary, db, env, OWNER, config=config)
        seed_number = 0

        def seed():
            nonlocal seed_number
            seed_number += 1
            saved = client.call("memory_remember", {"session_id": "concurrent", "turn_id": f"seed-{seed_number}",
                "text": TEXT, "capture_source": True, "observed_at": AT})
            HeldFixture.source_id = saved["source"]["source_id"]

        try:
            seed()
            for stage in ["r1_v1", "r4_v4"]:
                entered, release = gate(stage)
                before = len(HeldFixture.requests)
                args = query(f"same-{stage}")
                first = client.start_call("memory_recall", args)
                try:
                    assert entered.wait(10)
                    duplicate = client.start_call("memory_recall", args)
                    while_held(client)
                    assert not release.is_set()
                finally:
                    release.set()
                one = client.finish_call(first)
                assert client.finish_call(duplicate) == one
                assert one["recall"]["status"] == "answered", one
                assert len(HeldFixture.requests) == before + 2
                assert client.call("memory_recall", args) == one
                checked = client.call("memory_validate_answer", dict(args, draft_answer=TEXT,
                    source_ids=[HeldFixture.source_id]))
                assert checked["approved"] is True, checked

            for action in ["change", "delete", "cancel"]:
                for stage in ["r1_v1", "r4_v4"]:
                    entered, release = gate(stage)
                    before = len(HeldFixture.requests)
                    args = query(f"{action}-{stage}")
                    first = client.start_call("memory_recall", args)
                    try:
                        assert entered.wait(10)
                        if action == "change":
                            saved = client.finish_call(client.start_call("memory_remember", {
                                "session_id": "concurrent", "turn_id": f"change-{stage}", "text": "我有一把绿伞。",
                                "capture_source": True, "observed_at": AT}), timeout=5)
                        elif action == "delete":
                            client.finish_call(client.start_call("memory_forget", {
                                "source_id": HeldFixture.source_id}), timeout=5)
                        else:
                            client.send({"jsonrpc": "2.0", "method": "notifications/cancelled",
                                         "params": {"requestId": first, "reason": "fixture cancellation"}})
                        while_held(client)
                    finally:
                        release.set()
                    if action == "change":
                        original = client.finish_call(first)
                        replayed = client.call("memory_recall", args)
                        assert replayed == original, (original, replayed)
                        # R3 includes arrivals during R1, but not arrivals during R4.
                        assert (json.dumps(saved["source"]["source_id"]) in json.dumps(original)) == (stage == "r1_v1"), original
                        assert len(HeldFixture.requests) == before + 2
                        continue
                    # The cancelled RPC may have no response; repeating its exact request joins/checks the same execution.
                    failed = client.call("memory_recall", args, error=True)
                    if action != "cancel":
                        original = client.finish_call(first, error=True)
                        expected = "recall_snapshot_changed" if action == "change" else "recall_claim_lost"
                        assert expected in original["error"], original
                    else:
                        assert "cancelled" in failed["error"] or "not_retained" in failed["error"], failed
                    assert TEXT not in json.dumps(failed, ensure_ascii=False)
                    assert len(HeldFixture.requests) == before + (1 if stage == "r1_v1" else 2)
                    with sqlite3.connect(db) as c:
                        state = c.execute("SELECT state_json FROM memory_profiles WHERE owner_id=?", (OWNER,)).fetchone()[0]
                    assert TEXT not in json.dumps(json.loads(state)["receipts"], ensure_ascii=False)
                    if action == "delete":
                        assert TEXT not in state
                        seed()

            entered, release = gate("r1_v1")
            before = len(HeldFixture.requests)
            args = {"user_turn": {"session_id": "concurrent", "turn_id": "prepared", "text": TEXT,
                                  "capture_source": True, "observed_at": AT}}
            first = client.start_call("memory_prepare", args)
            try:
                assert entered.wait(10)
                client.send({"jsonrpc": "2.0", "method": "notifications/cancelled",
                             "params": {"requestId": first, "reason": "accepted prepare continues"}})
                duplicate = client.start_call("memory_prepare", args)
                while_held(client)
                with sqlite3.connect(db) as c:
                    state = json.loads(c.execute("SELECT state_json FROM memory_profiles WHERE owner_id=?", (OWNER,)).fetchone()[0])
                assert any(s["turn_id"] == "prepared" and s["request"] for s in state["sources"].values())
            finally:
                release.set()
            prepared = client.finish_call(duplicate)
            assert prepared["source"]["status"] == "stored" and prepared["recall"]["status"] == "answered", prepared
            assert len(HeldFixture.requests) == before + 2
            assert client.call("memory_prepare", args) == prepared
            print("MCP barriers passed: R1/R4 concurrent list, same-ID replay, snapshot change, deletion, cancellation, accepted Prepare")
        finally:
            HeldFixture.gate = None
            client.close()


if __name__ == "__main__":
    main()
