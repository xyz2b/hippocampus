#!/usr/bin/env python3
"""Thin E1 runner checks using the existing synthetic Responses fixture; no provider calls."""
import argparse
import hashlib
import http.server
import json
import os
import sqlite3
import subprocess
import tempfile
import threading
from pathlib import Path

from s5_process import Fixture as ResponsesFixture


class Fixture(ResponsesFixture):
    def do_POST(self):
        if self.mode not in ("http_401", "http_403"):
            return super().do_POST()
        assert self.path == "/v1/responses"
        type(self).requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        body = json.dumps({"error": {"message": "arbitrary provider account detail"}}).encode()
        self.send_response(int(self.mode.removeprefix("http_")))
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/examples/longmemeval_e1"))
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    env = {k: v for k, v in os.environ.items() if not k.startswith("PMR_")}
    env.update(PMR_MODEL_NETWORK="offline", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
               DEEPSEEK_API_KEY="local-fixture-key")
    checks = []
    try:
        with tempfile.TemporaryDirectory(prefix="lme-e1-runner-") as temporary:
            root = Path(temporary)
            for mode, limit, calls, status in (("success", 2, 2, "committed"),
                                              ("review_incomplete", 2, 2, "review_incomplete"),
                                              ("budget", 1, 1, "failed")):
                Fixture.mode, Fixture.requests = mode, []
                db, intake, budget = root / f"{mode}.sqlite3", root / f"{mode}.jsonl", root / f"{mode}-budget.json"
                turn = dict(case_id="c0", owner_id="c0", session_id="s0", turn_id="t0",
                            text="I want all responses in Chinese.", observed_at=1700000000,
                            history=[], utc_offset_seconds=0)
                intake.write_text(json.dumps(turn) + "\n")
                budget.write_text(json.dumps(dict(group_id="fixture", max_calls=limit)))
                command = [str(binary), "ingest", "--db", str(db), "--input", str(intake),
                           "--inline", "--budget", str(budget)]
                run = subprocess.run(command, env=env, text=True, capture_output=True, timeout=30)
                assert run.returncode == (0 if mode == "success" else 1), (run.stderr, run.stdout)
                result = json.loads(run.stdout)
                assert result["receipt"]["enrichment"]["status"] == status, result
                assert len(Fixture.requests) == len(result["model_calls"]) == calls
                with sqlite3.connect(db) as connection:
                    state = json.loads(connection.execute(
                        "SELECT state_json FROM memory_profiles WHERE owner_id='c0'").fetchone()[0])
                    assert len(state["repository"]["memories"]) == (1 if mode == "success" else 0)
                    source = next(iter(state["sources"].values()))
                    assert source["request"]["text"] == turn["text"] and source["observed_at"] == turn["observed_at"]
                    assert connection.execute("SELECT count(*) FROM model_calls").fetchone()[0] == calls
                rerun = subprocess.run(command, env=env, text=True, capture_output=True, timeout=30)
                replay = json.loads(rerun.stdout)
                assert replay["replayed"] and replay["model_calls"] == [] and len(Fixture.requests) == calls
                for request in Fixture.requests:
                    actual = json.loads(request["input"])
                    assert actual["current_user_turn"] == turn["text"] and actual["now"] == turn["observed_at"]
                    assert "question" not in actual and "answer" not in actual
                queries = root / f"{mode}-queries.jsonl"
                queries.write_text(json.dumps(dict(case_id="c0", question_id="c0", owner_id="c0",
                                                  query="Chinese", at=1700000001, utc_offset_seconds=0)) + "\n")
                pair = subprocess.run([str(binary), "pair", "--db", str(db), "--input", str(queries)],
                                      env=env, text=True, capture_output=True, timeout=30)
                assert pair.returncode == 0, pair.stderr
                paired = json.loads(pair.stdout)
                assert paired["shared_snapshot_verified"] and paired["backend_generation_calls"] == 0
                assert len(Fixture.requests) == calls, "paired retrieval issued a model request"
                checks.append(dict(mode=mode, physical_attempts=calls, enrichment=status,
                                   raw_retained=True, replay_attempts=0, paired_attempts=0))
            for status in (401, 403):
                mode = f"http_{status}"
                Fixture.mode, Fixture.requests = mode, []
                db, intake, budget = root / f"{mode}.sqlite3", root / f"{mode}.jsonl", root / f"{mode}-budget.json"
                turns = [dict(owner_id="c0", session_id="s0", turn_id=f"t{i}",
                              text=f"Synthetic raw statement {i}.", observed_at=1700000000 + i)
                         for i in range(2)]
                intake.write_text("".join(json.dumps(turn) + "\n" for turn in turns))
                budget.write_text(json.dumps(dict(group_id="fixture", max_calls=4)))
                command = [str(binary), "ingest", "--db", str(db), "--input", str(intake),
                           "--inline", "--budget", str(budget)]
                run = subprocess.run(command, env=env, text=True, capture_output=True, timeout=30)
                assert run.returncode == 1, (run.stderr, run.stdout)
                results = [json.loads(line) for line in run.stdout.splitlines()]
                reasons = [f"model_http_{status}", f"batch_blocked_after_http_{status}"]
                assert len(results) == 2 and len(Fixture.requests) == 1, results
                for result, reason, calls in zip(results, reasons, (1, 0)):
                    assert result["receipt"]["source"]["status"] == "stored", result
                    assert result["receipt"]["enrichment"]["status"] == "failed", result
                    assert result["enrichment_mode"] == "inline" and result["error"] == reason, result
                    assert len(result["model_calls"]) == calls and not result["replayed"], result
                assert "arbitrary provider account detail" not in run.stdout
                with sqlite3.connect(db) as connection:
                    state = json.loads(connection.execute(
                        "SELECT state_json FROM memory_profiles WHERE owner_id='c0'").fetchone()[0])
                    sources = sorted(state["sources"].values(), key=lambda source: source["turn_id"])
                    assert len(sources) == 2 and not state["repository"]["memories"], state
                    assert [source["request"]["text"] for source in sources] == [turn["text"] for turn in turns]
                    assert [source["error"] for source in sources] == reasons
                    assert "arbitrary provider account detail" not in json.dumps(state)
                    assert connection.execute("SELECT count(*) FROM model_calls").fetchone()[0] == 1
                replay = subprocess.run(command, env=env, text=True, capture_output=True, timeout=30)
                repeated = [json.loads(line) for line in replay.stdout.splitlines()]
                assert replay.returncode == 1 and len(repeated) == 2 and len(Fixture.requests) == 1
                assert all(result["replayed"] and result["model_calls"] == [] for result in repeated)
                assert [result["error"] for result in repeated] == reasons
                checks.append(dict(mode=mode, physical_attempts=1, sources_stored=2,
                                   enrichment="failed", blocked_reason=reasons[1],
                                   raw_retained=True, replay_attempts=0))

            def fixture_case(name, limit, count=2):
                Fixture.requests = []
                db = root / f"{name}.sqlite3"
                intake, budget = root / f"{name}.jsonl", root / f"{name}-budget.json"
                turns = [dict(owner_id="c0", session_id="s0", turn_id=f"t{i}",
                              text="I want all responses in Chinese.", observed_at=1700000000 + i)
                         for i in range(count)]
                intake.write_text("".join(json.dumps(turn) + "\n" for turn in turns))
                budget.write_text(json.dumps(dict(group_id=name, max_calls=limit, max_operation_calls=2)))
                command = [str(binary), "ingest", "--db", str(db), "--input", str(intake),
                           "--inline", "--budget", str(budget)]
                return db, intake, turns, command

            def run_rows(command, code):
                result = subprocess.run(command, env=env, text=True, capture_output=True, timeout=30)
                assert result.returncode == code, (result.stderr, result.stdout)
                return [json.loads(line) for line in result.stdout.splitlines() if line.strip()]

            def snapshot(db):
                with sqlite3.connect(db) as connection:
                    state = json.loads(connection.execute(
                        "SELECT state_json FROM memory_profiles WHERE owner_id='c0'").fetchone()[0])
                    calls = connection.execute("SELECT count(*) FROM model_calls").fetchone()[0]
                    return state, calls

            def sources(state):
                return {source["turn_id"]: source for source in state["sources"].values()}

            def raw_identity(state):
                return {turn: {key: source[key] for key in
                               ("id", "session_id", "turn_id", "observed_at", "received_sequence", "request")}
                        for turn, source in sources(state).items()}

            # An explicit attempt gets one chance; historical HTTP failures do not disable a new attempt.
            db, intake, turns, command = fixture_case("resume-http", 8)
            Fixture.mode = "http_403"
            run_rows(command, 1)
            original, calls = snapshot(db)
            assert calls == len(Fixture.requests) == 1
            run_rows(command + ["--resume-attempt", "still-forbidden"], 1)
            denied, calls = snapshot(db)
            assert calls == len(Fixture.requests) == 2
            assert sources(denied)["t1"] == sources(original)["t1"], "stop must not claim or mutate later sources"
            assert raw_identity(denied) == raw_identity(original)
            assert any(a["id"] == "still-forbidden" for a in sources(denied)["t0"]["result"]["resume_attempts"])
            run_rows(command + ["--resume-attempt", "still-forbidden"], 1)
            assert snapshot(db) == (denied, 2) and len(Fixture.requests) == 2
            Fixture.mode = "success"
            run_rows(command + ["--resume-attempt", "account-restored"], 0)
            restored, calls = snapshot(db)
            assert calls == len(Fixture.requests) == 6
            assert raw_identity(restored) == raw_identity(original)
            assert all(s["enrichment"] == "committed" for s in sources(restored).values())
            assert all(any(a["id"] == "account-restored" for a in s["result"]["resume_attempts"])
                       for s in sources(restored).values())
            for attempt in ("account-restored", "successful-sources-skip"):
                results = run_rows(command + ["--resume-attempt", attempt], 0)
                assert all(not r["model_calls"] for r in results)
                assert snapshot(db) == (restored, 6) and len(Fixture.requests) == 6
            # Resume cannot silently become a new source intake operation.
            intake.write_text(json.dumps({**turns[-1], "turn_id": "not-yet-stored"}) + "\n")
            run_rows(command + ["--resume-attempt", "missing-source"], 1)
            assert snapshot(db) == (restored, 6) and len(Fixture.requests) == 6
            checks.append(dict(mode="resume-http", physical_attempts=6, resumed_sources=2,
                               repeated_attempt_calls=0, completed_source_calls=0, raw_identity_preserved=True))

            for limit, resume_calls in ((1, 0), (2, 1)):
                name = f"resume-budget-{limit}"
                db, intake, turns, command = fixture_case(name, limit)
                Fixture.mode = "http_403"
                run_rows(command, 1)
                original, calls = snapshot(db)
                assert calls == 1
                Fixture.mode = "success"
                run_rows(command + ["--resume-attempt", "finite-budget"], 1)
                stopped, calls = snapshot(db)
                assert calls == len(Fixture.requests) == 1 + resume_calls
                assert sources(stopped)["t0"]["error"] == "model_budget_denied"
                assert sources(stopped)["t1"] == sources(original)["t1"]
                assert raw_identity(stopped) == raw_identity(original) and not stopped["repository"]["memories"]
                run_rows(command + ["--resume-attempt", "finite-budget"], 1)
                assert snapshot(db) == (stopped, calls) and len(Fixture.requests) == calls
                # A new attempt ID cannot reset the group's consumed allowance.
                run_rows(command + ["--resume-attempt", "allowance-still-used"], 1)
                again, again_calls = snapshot(db)
                assert again_calls == len(Fixture.requests) == calls
                assert sources(again)["t1"] == sources(original)["t1"]
                assert not again["repository"]["memories"] and raw_identity(again) == raw_identity(original)
                checks.append(dict(mode=name, physical_attempts=calls, resume_attempts=resume_calls,
                                   remaining_group_calls=0, partial_facts=0, later_source_unchanged=True))

            for mode, initial_calls in (("initial_incomplete", 1), ("review_incomplete", 2)):
                name = f"resume-{mode}"
                db, intake, turns, command = fixture_case(name, 4, count=1)
                Fixture.mode = mode
                run_rows(command, 1)
                original, calls = snapshot(db)
                assert calls == initial_calls and not original["repository"]["memories"]
                Fixture.mode = "success"
                run_rows(command + ["--resume-attempt", "complete-output"], 0)
                completed, calls = snapshot(db)
                assert calls == len(Fixture.requests) == initial_calls + 2
                assert raw_identity(completed) == raw_identity(original)
                assert sources(completed)["t0"]["enrichment"] == "committed"
                assert len(completed["repository"]["memories"]) == 1
                run_rows(command + ["--resume-attempt", "complete-output"], 0)
                assert snapshot(db) == (completed, calls) and len(Fixture.requests) == calls
                checks.append(dict(mode=name, physical_attempts=calls, recovered=True, replay_attempts=0))

            # Reprocessing an older source must never overwrite or reinforce a newer matching slot.
            db, intake, turns, command = fixture_case("resume-stale", 8)
            turns[1]["observed_at"] = turns[0]["observed_at"]
            intake.write_text(json.dumps(turns[0]) + "\n")
            Fixture.mode = "http_403"
            run_rows(command, 1)
            intake.write_text(json.dumps(turns[1]) + "\n")
            Fixture.mode = "success"
            run_rows(command, 0)
            newer, calls = snapshot(db)
            assert calls == len(Fixture.requests) == 3 and len(newer["repository"]["memories"]) == 1
            intake.write_text(json.dumps(turns[0]) + "\n")
            run_rows(command + ["--resume-attempt", "historical-review"], 1)
            stale, calls = snapshot(db)
            assert calls == len(Fixture.requests) == 5
            assert stale["repository"] == newer["repository"] and stale["revision"] == newer["revision"]
            assert raw_identity(stale) == raw_identity(newer)
            old_source = sources(stale)["t0"]
            assert old_source["enrichment"] == "stale_requires_review"
            candidates = old_source["result"]["stale_candidates"]
            assert len(candidates) == 1 and candidates[0]["evidence"][0]["turn_id"] == "t0"
            assert candidates[0]["evidence"][0]["quote"] == turns[0]["text"]
            for attempt in ("historical-review", "do-not-retry-stale"):
                run_rows(command + ["--resume-attempt", attempt], 1)
                assert snapshot(db) == (stale, 5) and len(Fixture.requests) == 5
            checks.append(dict(mode="resume-stale", physical_attempts=5, stale_candidates_preserved=True,
                               newer_memory_unchanged=True, replay_attempts=0))
    finally:
        server.shutdown()
        server.server_close()
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == binary_hash, "binary changed during checks"
    report = dict(status="passed", scope="synthetic literal-loopback checks; no provider or dataset generation",
                  reproduce="cargo build --example longmemeval_e1 && python3 tests/s5_e1_runner.py --output docs/refactor/s5/e1/runner-check.json",
                  binary_sha256=binary_hash, script_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                  physical_attempts=sum(c["physical_attempts"] for c in checks), provider_attempts=0,
                  checks=checks)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(report, ensure_ascii=False))


if __name__ == "__main__":
    main()
