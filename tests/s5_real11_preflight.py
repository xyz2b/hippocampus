#!/usr/bin/env python3
"""S5-07 real 11-case plan: offline dry-run and pre-send guards only."""
import argparse
import json
import os
import subprocess
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def run(binary, root, budget, ledger, *mode, env):
    return subprocess.run(
        [str(binary), "--run-dir", str(root), "--budget", str(budget),
         "--ledger", str(ledger), *mode], cwd=ROOT, env=env,
        capture_output=True, text=True, timeout=30,
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/examples/s5_assisted_eval")
    args = parser.parse_args()
    assert args.binary.is_file(), "build with cargo build --offline --example s5_assisted_eval"
    clean = {k: v for k, v in os.environ.items() if not k.startswith(("DEEPSEEK_", "OPENAI_", "PMR_"))}
    with tempfile.TemporaryDirectory(prefix="s5-real11-preflight-") as temporary:
        root = Path(temporary)
        budget_path = root / "budget.json"
        ledger = root / "new-ledger.sqlite3"
        budget = {"group_id": "s5-07-real-11-offline-preflight", "max_calls": 11,
                  "max_operation_calls": 1, "max_input_bytes": 65536,
                  "query_output_tokens": 32768}
        budget_path.write_text(json.dumps(budget))
        deny = dict(clean, PMR_MODEL_NETWORK="deny")
        planned = run(args.binary, root / "plan", budget_path, ledger, "--dry-run", "--real-all", env=deny)
        assert planned.returncode == 0, planned.stderr
        report = json.loads((root / "plan/report.json").read_text())
        ids = [case["id"] for case in json.loads((ROOT / "docs/refactor/s4/inputs.json").read_text())]
        assert report["mode"] == "real_all" and report["send_requested"] is False
        assert report["planned_cases"] == report["completed_final_reads"] == len(ids) == 11
        assert [case["id"] for case in report["cases"]] == ids
        assert report["ledger_rows"] == [] and report["trace"]["model_start"] == 0
        assert not ledger.exists() and (root / "plan").stat().st_mode & 0o777 == 0o700
        assert (root / "plan/trace").stat().st_mode & 0o777 == 0o700
        for case_id in ids:
            raw = json.loads((root / "plan" / f"{case_id}.json").read_text())
            prompt = raw["planner_input"]
            assert set(prompt) == {"question", "user_context", "trusted_at", "trusted_task_id", "candidate_snippets"}
            assert prompt["question"] == raw["query"] and prompt["user_context"] == raw["context"]
            expected = ("not_requested_clarification"
                        if raw["local"]["reference_resolution"]["needs_clarification"] is True
                        else "not_sent_dry_run")
            assert raw["planner_status"] == expected and raw["model_calls"] == []
        rejected = 0

        def reject(changes, env, *mode):
            nonlocal rejected
            budget_path.write_text(json.dumps(dict(budget, **changes)))
            outcome = run(args.binary, root / f"reject-{rejected}", budget_path, ledger,
                          *mode, env=env)
            assert outcome.returncode != 0, outcome.stdout
            assert not (root / f"reject-{rejected}").exists() and not ledger.exists()
            rejected += 1

        reject({}, deny, "--send", "--real-all")
        reject({}, dict(clean, PMR_MODEL_NETWORK="live", DEEPSEEK_BASE_URL="http://127.0.0.1:1/v1",
                        DEEPSEEK_API_KEY="dummy"), "--send", "--real-all")
        reject({}, dict(clean, PMR_MODEL_NETWORK="live",
                        DEEPSEEK_BASE_URL="https://ark.cn-beijing.volces.com/api/v3"),
               "--send", "--real-all")
        for changed in ({"group_id": "s5-assisted-n04-pair-20260924-v2"},
                        {"max_calls": 12}, {"max_operation_calls": 2},
                        {"max_input_bytes": 65537}, {"query_output_tokens": 4096}):
            reject(changed, deny, "--dry-run", "--real-all")
        reject({}, deny, "--dry-run", "--real-all", "--synthetic-all")
        reject({}, deny, "--dry-run", "--real-all", "--case", "N04")
        reject({}, deny, "--dry-run", "--case")
        ledger.touch()
        outcome = run(args.binary, root / "existing-ledger", budget_path, ledger,
                      "--dry-run", "--real-all", env=deny)
        assert outcome.returncode != 0 and not (root / "existing-ledger").exists()
        rejected += 1
        print(json.dumps({"status": "passed", "dry_run_cases": 11,
                          "pre_send_rejections": rejected, "provider_requests": 0}))


if __name__ == "__main__":
    main()
