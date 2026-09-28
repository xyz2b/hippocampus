#!/usr/bin/env python3
"""S5-07: fixed-clock 11-case local/assisted pairing with a loopback-only synthetic Planner."""
import argparse
import contextlib
import hashlib
import http.server
import json
import os
import subprocess
import tempfile
import threading
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
QUERIES = ["Redis"]  # Same synthetic output for every question; not selected from labels.


class Planner(http.server.BaseHTTPRequestHandler):
    requests = []

    def log_message(self, *_):
        pass

    def do_POST(self):
        raw = self.rfile.read(int(self.headers["Content-Length"]))
        request = json.loads(raw)
        assert self.path == "/v1/responses"
        assert self.headers["Authorization"] == "Bearer local-fixture-key"
        assert request["model"] == "ep-20260923215039-vxgxg"
        assert request["reasoning"]["effort"] == "medium"
        assert request["max_output_tokens"] == 512
        assert request["store"] is False
        assert request["text"]["format"]["name"] == "personal_memory_assistance_queries_v1"
        assert request["text"]["format"]["strict"] is True
        assert len(raw) <= 65536
        prompt = json.loads(request["input"])
        assert set(prompt) == {"question", "user_context", "trusted_at", "trusted_task_id", "candidate_snippets"}
        type(self).requests.append({"request_sha256": hashlib.sha256(raw).hexdigest(),
                                    "input_bytes": len(raw), "question": prompt["question"],
                                    "trusted_at": prompt["trusted_at"], "queries": QUERIES})
        body = json.dumps({"status": "completed", "usage": {"input_tokens": 10, "output_tokens": 4},
                           "output": [{"type": "message", "content": [{"type": "output_text",
                               "text": json.dumps({"queries": QUERIES}, ensure_ascii=False)}]}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def source_rows(result):
    return result["source_evidence"] + [item["source"] for item in result["historical_or_conflicting"]]


def no_answer_evidence(result):
    lanes = ("mandatory", "constraints", "preferences", "facts", "episodes", "conflicts")
    empty_facts = all(all(snapshot.get(lane) == [] for lane in lanes)
                      for snapshot in (result.get("result", {}),
                                       result.get("global_context", {}).get("result", {})))
    return (result.get("recall_status") == "completed"
            and result.get("source_evidence") == []
            and result.get("historical_or_conflicting") == []
            and empty_facts)


def unresolved_without_evidence(result):
    return (no_answer_evidence(result)
            and result.get("evidence_status") == "sufficiency_not_verified"
            and result.get("reference_resolution", {}).get("status") == "unconfirmed")


def score(result, label, source_map, candidates):
    required = set(label["required_sources"])
    candidate_ids = {source_map[item["source_id"]] for item in candidates}
    final = {source_map[item["source_id"]]: item["text"] for item in source_rows(result)}
    final_ids = set(final)
    point_coverage = {sid: {point: point in final.get(sid, "") for point in points}
                      for sid, points in label["required_points"].items() if sid in required}
    available_complete = required <= final_ids and all(all(points.values()) for points in point_coverage.values())
    answerability = label["answerability"]
    clarification = (result.get("reference_resolution", {}).get("needs_clarification") is True
                     and no_answer_evidence(result))
    safety = ((result["evidence_status"] == "ambiguous" and not final_ids) or clarification
              if answerability == "ambiguous" else not final_ids if answerability == "absent"
              else result["evidence_status"] == "sufficiency_not_verified" or clarification)
    forbidden = sorted(final_ids & set(label["forbidden_sources"]))
    safety = safety and not forbidden
    runtime_ambiguous = (result["evidence_status"] == "ambiguous"
                         and result["reference_resolution"]["status"] == "ambiguous")
    return {"answerability": answerability, "candidate_sources": sorted(candidate_ids),
            "candidate_required_sources": sorted(candidate_ids & required),
            "candidate_required_ids_complete": required <= candidate_ids if required else None,
            "final_sources": sorted(final_ids), "final_required_sources": sorted(final_ids & required),
            "required_point_coverage": point_coverage,
            "available_required_evidence_complete": available_complete if required else None,
            "complete_answerable_evidence": available_complete if answerability == "answerable" else None,
            "unnecessary_sources": sorted(final_ids - required),
            "forbidden_found": forbidden,
            "evidence_status": result["evidence_status"], "safety_behavior_passed": safety,
            "unresolved_without_evidence": unresolved_without_evidence(result),
            "runtime_ambiguous_status": runtime_ambiguous,
            "clarification_handoff_proxy": (no_answer_evidence(result)
                                             and (clarification or unresolved_without_evidence(result) or runtime_ambiguous)),
            "semantic_resolution": result["reference_resolution"]["semantic_resolution"],
            "host_asked_project": None,
            "recall_status": result["recall_status"], "result_bytes": len(json.dumps(result, ensure_ascii=False).encode()),
            "duration_ms": result["duration_ms"], "budget_bytes": result["budget"]["content_bytes"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/examples/s5_assisted_eval")
    parser.add_argument("--output", type=Path, help="New directory for raw runs, ledger, trace and scored report")
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error("build the runner first: cargo build --offline --example s5_assisted_eval")
    with contextlib.ExitStack() as stack:
        if args.output:
            run_dir = args.output.resolve()
            run_dir.mkdir(parents=True, mode=0o700)
        else:
            run_dir = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="s5-paired-")))
        budget = {"group_id": "s5-synthetic-paired-11", "max_calls": 11, "max_operation_calls": 1,
                  "max_input_bytes": 65536, "query_output_tokens": 512}
        budget_path = run_dir / "budget.json"
        budget_path.write_text(json.dumps(budget) + "\n")
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Planner)
        stack.callback(server.server_close)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        stack.callback(server.shutdown)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("DEEPSEEK_", "OPENAI_", "PMR_"))}
        env.update(PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
                   DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
                   NO_PROXY="127.0.0.1,localhost")
        command = [str(binary), "--run-dir", str(run_dir / "raw"), "--budget", str(budget_path),
                   "--ledger", str(run_dir / "model-ledger.sqlite3"), "--send", "--synthetic-all"]
        completed = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True, timeout=120)
        assert completed.returncode == 0, completed.stderr + completed.stdout
        original = json.loads((run_dir / "raw/report.json").read_text())
        assert original["planned_cases"] == original["completed_final_reads"] == 11
        raw_cases = {case["id"]: json.loads((run_dir / "raw" / f"{case['id']}.json").read_text())
                     for case in original["cases"]}
        expected_calls = sum(raw["local"]["reference_resolution"]["needs_clarification"] is not True
                             for raw in raw_cases.values())
        assert len(Planner.requests) == expected_calls, Planner.requests
        assert len(original["ledger_rows"]) == expected_calls, original["ledger_rows"]
        assert all(row["stage"] == "assistance" and row["outcome"] == "completed"
                   and row["output_token_limit"] == 512 for row in original["ledger_rows"])
        events = [json.loads(line) for path in (run_dir / "raw/trace").glob("*.jsonl")
                  for line in path.read_text().splitlines()]
        assert sum(event["kind"] == "model_start" for event in events) == expected_calls
        labels = {row["id"]: row for row in json.loads((ROOT / "docs/refactor/s4/labels.json").read_text())}
        rows = []
        for case in original["cases"]:
            case_id = case["id"]
            raw = raw_cases[case_id]
            snapshots = [event["data"] for event in events if event["run_id"] == f"s5-assisted-eval-{case_id}"
                         and event["kind"] == "assistance_candidates"]
            local, assisted = raw["local"], raw["assisted"]
            clarification = local["reference_resolution"]["needs_clarification"] is True
            assert len(snapshots) == (1 if clarification else 2), case_id
            assert all(s["stage"] == "pre_filter" for s in snapshots), case_id
            assert local["read_revision"] == assisted["read_revision"]
            assert local["source_revision"] == assisted["source_revision"]
            assert local["budget"]["config"] == assisted["budget"]["config"] == original["local_budget"]
            assert raw["fact_index_enabled"] is False and raw["trusted_at"] == original["trusted_at"]
            if clarification:
                assert raw["planner_status"] == "not_requested_clarification"
                assert raw["model_calls"] == []
                assert assisted["assistance"] == {"status": "not_requested", "reason": "clarification_required"}
            else:
                assert len(raw["model_calls"]) == 1 and raw["model_calls"][0]["stage"] == "assistance"
                assert assisted["assistance"]["status"] == "completed", raw
                assert assisted["assistance"]["queries"] == QUERIES
            local_source_rows, assisted_source_rows = source_rows(local), source_rows(assisted)
            for item in local_source_rows:
                assert item in assisted_source_rows, (case_id, "local evidence displaced", item)
            source_map = raw["source_map"]
            local_candidates = snapshots[0]["original"]
            assisted_candidates = (snapshots[-1]["original"] + snapshots[-1]["supplemental"])
            local_score = score(local, labels[case_id], source_map, local_candidates)
            assisted_score = score(assisted, labels[case_id], source_map, assisted_candidates)
            rows.append({"id": case_id, "local": local_score, "assisted": assisted_score,
                         "supplemental_candidate_count": len(snapshots[-1]["supplemental"]),
                         "added_candidate_sources": sorted(set(assisted_score["candidate_sources"]) - set(local_score["candidate_sources"])),
                         "added_final_sources": sorted(set(assisted_score["final_sources"]) - set(local_score["final_sources"])),
                         "lost_final_sources": sorted(set(local_score["final_sources"]) - set(assisted_score["final_sources"])),
                         "planner_calls": len(raw["model_calls"]),
                         "usage": raw["model_calls"][0] if raw["model_calls"] else None})
        report = {"scope": "11 S4 synthetic development questions; retrieval engineering only",
                  "synthetic_planner_queries": QUERIES, "real_provider_requests": 0,
                  "synthetic_requests": len(Planner.requests), "release_acceptance": False,
                  "effect_claim": "none; synthetic output cannot establish real Planner benefit",
                  "fixture_sha256": {"inputs": original["inputs_sha256"], "labels": original["labels_sha256"]},
                  "trusted_at": original["trusted_at"], "fact_index_enabled": False,
                  "local_budget": original["local_budget"], "model_budget": budget,
                  "ledger_rows": original["ledger_rows"], "requests": Planner.requests, "cases": rows}
        (run_dir / "paired-report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps({"status": "passed", "case_count": len(rows), "synthetic_requests": len(Planner.requests),
                          "real_provider_requests": 0, "local_evidence_displaced": 0,
                          "output": str(run_dir / "paired-report.json") if args.output else "temporary"}, ensure_ascii=False))


if __name__ == "__main__":
    main()
