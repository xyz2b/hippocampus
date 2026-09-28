#!/usr/bin/env python3
"""Replay the frozen real 11 Planner responses through the current loopback-only runner."""
import argparse
import contextlib
import hashlib
import http.server
import json
import os
import subprocess
import threading
from pathlib import Path

from s5_paired_offline import score, source_rows


ROOT = Path(__file__).resolve().parents[1]
ARCHIVE = ROOT / "docs/refactor/s5/assisted/live-11-20260924"
DEFAULT_OUTPUT = ROOT / "docs/refactor/s5/assisted/post-real-safety-20260924/recorded-replay"


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def check_archive():
    manifest_path = ARCHIVE / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    expected = manifest["archive_files"]
    actual = {str(path.relative_to(ARCHIVE)) for path in ARCHIVE.rglob("*") if path.is_file()
              and path != manifest_path}
    assert actual == set(expected) and len(expected) == manifest["archive_file_count"] == 32
    for name, item in expected.items():
        path = ARCHIVE / name
        assert path.stat().st_size == item["bytes"] and sha256(path) == item["sha256"], name
    for name in ("docs/refactor/s4/inputs.json", "docs/refactor/s4/labels.json"):
        assert sha256(ROOT / name) == manifest["code_and_fixture_files_at_send"][name]["sha256"]
    return sha256(manifest_path)


def fnv1a64(raw):
    value = 0xcbf29ce484222325
    for byte in raw:
        value = ((value ^ byte) * 0x100000001b3) & 0xffffffffffffffff
    return f"fnv1a64:{value:016x}"


def recording():
    report = json.loads((ARCHIVE / "run/report.json").read_text())
    trace = [json.loads(line) for path in (ARCHIVE / "run/trace").glob("*.jsonl")
             for line in path.read_text().splitlines()]
    starts = [event for event in trace if event["kind"] == "model_start"]
    ends = {event["run_id"]: event for event in trace if event["kind"] == "model_end"}
    cases = [item["id"] for item in report["cases"]]
    assert len(starts) == len(ends) == len(cases) == 11
    assert [event["run_id"] for event in starts] == [f"s5-assisted-eval-{case}" for case in cases]
    ledger = {row["operation_id"]: row for row in report["ledger_rows"]}
    expected = []
    for case, start in zip(cases, starts):
        operation = start["run_id"]
        end = ends[operation]
        request = start["data"]["request"]
        raw = json.dumps(request, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
        row = ledger[operation]
        assert row["input_bytes"] == len(raw) and row["request_fingerprint"] == fnv1a64(raw)
        assert start["data"]["call_id"] == end["data"]["call_id"]
        assert end["data"]["status"] == "completed" and end["data"]["http_status"] == 200
        reply = end["data"]["response"]
        assert reply["status"] == "completed" and reply["output"]
        assert request["max_output_tokens"] == row["output_token_limit"] == 32768
        expected.append({"id": case, "operation": operation, "question": json.loads(request["input"])["question"],
                         "body": raw, "reply": reply, "recorded_fingerprint": row["request_fingerprint"],
                         "recorded_call_id": start["data"]["call_id"]})
    return expected


class RecordedPlanner(http.server.BaseHTTPRequestHandler):
    expected = []
    received = []
    errors = []
    lock = threading.Lock()

    def log_message(self, *_):
        pass

    def do_POST(self):
        raw = self.rfile.read(int(self.headers["Content-Length"]))
        with self.lock:
            index = len(self.received)
            entry = self.expected[index] if index < len(self.expected) else None
            match = (self.path == "/v1/responses"
                     and self.headers.get("Authorization") == "Bearer local-fixture-key"
                     and entry is not None and raw == entry["body"])
            self.received.append({"ordinal": index + 1, "id": entry["id"] if entry else None,
                                  "request_sha256": hashlib.sha256(raw).hexdigest(),
                                  "request_fingerprint": fnv1a64(raw), "input_bytes": len(raw),
                                  "complete_request_match": match})
            if not match:
                self.errors.append(f"request {index + 1} differs from frozen {entry['operation'] if entry else 'end'}")
        if match:
            body = json.dumps(entry["reply"], ensure_ascii=False, separators=(",", ":")).encode()
            status = 200
        else:
            body = b'{"error":{"message":"recorded request mismatch"}}'
            status = 409
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def changes(before, after):
    before, after = set(before), set(after)
    return {"added": sorted(after - before), "lost": sorted(before - after)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/examples/s5_assisted_eval")
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    args = parser.parse_args()
    assert args.binary.is_file(), "build with cargo build --offline --example s5_assisted_eval"
    archive_sha = check_archive()
    expected = recording()
    output = args.output.resolve()
    assert not output.exists(), f"new output directory required: {output}"
    output.mkdir(parents=True, mode=0o700)
    budget = {"group_id": "s5-07-recorded-replay-20260924-v1", "max_calls": 11,
              "max_operation_calls": 1, "max_input_bytes": 65536, "query_output_tokens": 32768}
    (output / "budget.json").write_text(json.dumps(budget, indent=2) + "\n")
    RecordedPlanner.expected, RecordedPlanner.received, RecordedPlanner.errors = expected, [], []
    with contextlib.ExitStack() as stack:
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), RecordedPlanner)
        stack.callback(server.server_close)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        stack.callback(server.shutdown)
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(("DEEPSEEK_", "OPENAI_", "PMR_"))}
        env.update(PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
                   DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
                   NO_PROXY="127.0.0.1,localhost")
        command = [str(args.binary.resolve()), "--run-dir", str(output / "run"),
                   "--budget", str(output / "budget.json"), "--ledger", str(output / "model-ledger.sqlite3"),
                   "--send", "--synthetic-all"]
        process = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True, timeout=120)
    (output / "execution.stdout.log").write_text(process.stdout)
    (output / "execution.stderr.log").write_text(process.stderr)
    assert process.returncode == 0, process.stderr + process.stdout
    assert not RecordedPlanner.errors and len(RecordedPlanner.received) == 11, RecordedPlanner.errors
    assert all(row["complete_request_match"] for row in RecordedPlanner.received)
    assert check_archive() == archive_sha, "frozen archive changed during replay"

    run_report = json.loads((output / "run/report.json").read_text())
    assert run_report["mode"] == "synthetic_all" and run_report["completed_final_reads"] == 11
    assert len(run_report["ledger_rows"]) == 11
    assert all(row["outcome"] == "completed" and row["output_token_limit"] == 32768
               for row in run_report["ledger_rows"])
    events = [json.loads(line) for path in (output / "run/trace").glob("*.jsonl")
              for line in path.read_text().splitlines()]
    assert sum(event["kind"] == "model_start" for event in events) == 11
    assert sum(event["kind"] == "model_end" for event in events) == 11
    labels = {row["id"]: row for row in json.loads((ROOT / "docs/refactor/s4/labels.json").read_text())}
    old = {row["id"]: row for row in json.loads((ARCHIVE / "paired-report.json").read_text())["cases"]}
    rows = []
    for case in run_report["cases"]:
        case_id = case["id"]
        raw = json.loads((output / "run" / f"{case_id}.json").read_text())
        snapshots = [event["data"] for event in events if event["run_id"] == f"s5-assisted-eval-{case_id}"
                     and event["kind"] == "assistance_candidates"]
        assert len(snapshots) == 2 and all(item["stage"] == "pre_filter" for item in snapshots)
        assert snapshots[0]["original"] == snapshots[1]["original"]
        local, assisted = raw["local"], raw["assisted"]
        assert local["read_revision"] == assisted["read_revision"]
        assert local["source_revision"] == assisted["source_revision"]
        assert local["budget"]["config"] == assisted["budget"]["config"] == run_report["local_budget"]
        assert raw["fact_index_enabled"] is False and raw["trusted_at"] == run_report["trusted_at"]
        assert len(raw["model_calls"]) == 1 and raw["model_calls"][0]["stage"] == "assistance"
        assert assisted["assistance"]["status"] in ("completed", "completed_empty")
        local_rows, assisted_rows = source_rows(local), source_rows(assisted)
        retained = all(item in assisted_rows for item in local_rows)
        assert retained, f"{case_id}: local final evidence displaced"
        source_map = raw["source_map"]
        local_score = score(local, labels[case_id], source_map, snapshots[0]["original"])
        assisted_score = score(assisted, labels[case_id], source_map,
                               snapshots[1]["original"] + snapshots[1]["supplemental"])
        rows.append({"id": case_id, "local": local_score, "assisted": assisted_score,
                     "local_final_retained": retained,
                     "current_pair_candidate": changes(local_score["candidate_sources"], assisted_score["candidate_sources"]),
                     "current_pair_final": changes(local_score["final_sources"], assisted_score["final_sources"]),
                     "versus_frozen": {mode: {
                         "candidate": changes(old[case_id][mode]["candidate_sources"], scored["candidate_sources"]),
                         "final": changes(old[case_id][mode]["final_sources"], scored["final_sources"]),
                         "safety_before": old[case_id][mode]["safety_behavior_passed"],
                         "safety_after": scored["safety_behavior_passed"]}
                         for mode, scored in (("local", local_score), ("assisted", assisted_score))}})
    n03 = next(row for row in rows if row["id"] == "N03")
    report = {"scope": "current code with frozen real Planner business responses; offline retrieval diagnostics only",
              "real_provider_requests": 0, "loopback_replay_requests": len(RecordedPlanner.received),
              "archive_manifest_sha256": archive_sha, "full_request_matches": 11,
              "response_source": "frozen model_end.response allowlisted business fields; no new model output",
              "release_acceptance": False, "model_budget": budget, "requests": RecordedPlanner.received,
              "local_final_retained": sum(row["local_final_retained"] for row in rows),
              "n03_d1": {mode: {"candidate_present": "D1" in n03[mode]["candidate_sources"],
                                   "final_present": "D1" in n03[mode]["final_sources"]}
                          for mode in ("local", "assisted")},
              "safety_behavior_passed": {mode: sum(row[mode]["safety_behavior_passed"] for row in rows)
                                          for mode in ("local", "assisted")},
              "cases": rows}
    (output / "paired-report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    files = {str(path.relative_to(output)): {"bytes": path.stat().st_size, "sha256": sha256(path)}
             for path in output.rglob("*") if path.is_file() and path.name != "manifest.json"}
    (output / "manifest.json").write_text(json.dumps({"archive_manifest_sha256": archive_sha,
                                                       "script_sha256": sha256(Path(__file__)),
                                                       "binary_sha256": sha256(args.binary),
                                                       "files": files}, indent=2) + "\n")
    print(json.dumps({"status": "passed", "cases": len(rows), "local_final_retained": 11,
                      "n03_d1": report["n03_d1"], "real_provider_requests": 0,
                      "output": str(output / "paired-report.json")}, ensure_ascii=False))


if __name__ == "__main__":
    main()
