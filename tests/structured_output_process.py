#!/usr/bin/env python3
"""Replay archived output degeneration through four MCP intake paths, loopback only."""
import argparse
import contextlib
import hashlib
import http.server
import io
import json
import os
import tempfile
import threading
from pathlib import Path

import intake_grounding_process as grounding


TRACE = grounding.ROOT / "docs/refactor/s5/scope-paraphrase-live-20260927/evidence/event_registration_fee/trace"
BASE_FIXTURE = grounding.Fixture
BASE_TOOL = grounding.tool
PROTOCOL = ("complete, compact JSON object", "JSON Schema", "whitespace padding",
            "Stop immediately after the closing brace", "Preserve whitespace inside string values")


class Fixture(BASE_FIXTURE):
    bodies = []
    receipts = []
    replay = False
    envelope = None

    def do_POST(self):
        raw = self.rfile.read(int(self.headers["Content-Length"]))
        type(self).bodies.append(json.loads(raw))
        self.rfile = io.BytesIO(raw)
        super().do_POST()

    def respond(self, status, body):
        if status == 200 and self.replay:
            body = self.envelope
        super().respond(status, body)


def tool(client, name, args):
    result = BASE_TOOL(client, name, args)
    Fixture.receipts.append(result)
    return result


def archived_response():
    events = [json.loads(line) for path in sorted(TRACE.glob("*.jsonl"))
              for line in path.read_text().splitlines()]
    requests = [event["data"]["request"] for event in events if event["kind"] == "model_start"]
    responses = [event["data"]["response"] for event in events if event["kind"] == "model_end"]
    assert len(requests) == len(responses) == 1, "archive must contain one physical request"
    envelope = responses[0]
    assert envelope["status"] == "incomplete" and envelope["incomplete_details"]["reason"] == "length"
    original = json.loads(requests[0]["input"])["current_user_turn"]
    return original, envelope


def run_case(binary, directory, env, mode, name, expect_completion):
    case_dir = directory / f"{mode}-{name}"
    case_dir.mkdir()
    trace = case_dir / "trace"
    trace.mkdir(mode=0o700)
    Fixture.replay = name == "archived_incomplete"
    before, receipts_before = len(Fixture.bodies), len(Fixture.receipts)
    row = grounding.run_case(binary, case_dir, dict(env, PMR_TRACE_DIR=str(trace)), mode, name)
    try:
        if Fixture.replay:
            events = [json.loads(line) for path in trace.glob("*.jsonl")
                      for line in path.read_text().splitlines()]
            ends = [event["data"] for event in events if event["kind"] == "model_end"]
            assert len(ends) == 1, "incomplete extraction was retried or reviewed"
            assert ends[0]["response"] == Fixture.envelope, "archived provider envelope changed"
            assert ends[0]["parsed_result"] is None, "incomplete JSON was parsed"
            assert ends[0]["parsing_status"] == "not_run", "incomplete JSON reached parser"
            db = case_dir / f"{mode}-{name}.sqlite3"
            assert not grounding.snapshot(db)["repository"]["memories"], "partial output created facts"
            if mode != "legacy":
                receipts = Fixture.receipts[receipts_before:]
                receipt = receipts[-1]
                if mode == "inline":
                    receipt = receipt["remember"].get("result", receipt["remember"])
                assert receipt["enrichment"]["status"] == "extraction_incomplete", "wrong failure receipt"
            row["completed_extraction"] = False
            if expect_completion:
                assert row["active"] == 1, "archived degenerate output cannot complete normal intake"
        else:
            bodies = Fixture.bodies[before:]
            assert len(bodies) == 2, "expected extraction plus independent source review"
            for body in bodies:
                instructions = body["instructions"]
                missing = [part for part in PROTOCOL if part not in instructions]
                assert not missing, f"{body['text']['format']['name']} missing shared protocol: {missing}"
                assert body["text"]["format"]["strict"] is True
                assert body["text"]["format"]["type"] == "json_schema"
    except AssertionError as error:
        row["passed"] = False
        row["failures"].append(str(error))
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=grounding.ROOT / "target/debug/pmr-mcp")
    parser.add_argument("--modes", nargs="+", choices=["inline", "deferred", "explicit", "legacy"],
                        default=["inline", "deferred", "explicit", "legacy"])
    parser.add_argument("--cases", nargs="+", choices=["archived_incomplete", "fact_sentence"],
                        default=["archived_incomplete", "fact_sentence"])
    parser.add_argument("--expect-replay-completion", action="store_true",
                        help="Red reproduction: require the archived incomplete response to finish intake")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    assert binary.is_file(), f"build the MCP binary first: {binary}"
    original, Fixture.envelope = archived_response()
    grounding.CASES["archived_incomplete"] = (original, "fact", "text", original, "format_reject")
    grounding.Fixture, grounding.tool = Fixture, tool
    Fixture.requests, Fixture.errors, Fixture.bodies, Fixture.receipts = [], [], [], []
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pmr-structured-output-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        clean = {key: value for key, value in os.environ.items()
                 if not key.startswith(("DEEPSEEK_", "OPENAI_", "PMR_"))}
        env = dict(clean, PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
                   DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
                   NO_PROXY="127.0.0.1,localhost")
        rows = [run_case(binary, directory, env, mode, name, args.expect_replay_completion)
                for mode in args.modes for name in args.cases]
    expected = len(args.modes) * sum(1 if name == "archived_incomplete" else 2 for name in args.cases)
    report = dict(passed=all(row["passed"] for row in rows), cases=len(rows),
                  expected_calls=expected, actual_calls=len(Fixture.requests), real_provider_requests=0,
                  archived_envelope_sha256=hashlib.sha256(json.dumps(
                      Fixture.envelope, sort_keys=True, ensure_ascii=False).encode()).hexdigest(), results=rows)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    for row in rows:
        print(f"{'PASS' if row['passed'] else 'FAIL'} {row['mode']}/{row['case']}: "
              f"Active={row['active']}, calls={row['actual_calls']}/{row['expected_calls']}"
              + ("; " + "; ".join(row["failures"]) if row["failures"] else ""))
    print(json.dumps({key: value for key, value in report.items() if key != "results"}, ensure_ascii=False))
    assert report["passed"] and len(Fixture.requests) == expected, "structured output process regression failed"


if __name__ == "__main__":
    main()
