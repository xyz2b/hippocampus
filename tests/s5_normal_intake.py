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
