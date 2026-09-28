#!/usr/bin/env python3
"""One frozen event intake: at most extraction + source review, no retry."""
import argparse
import contextlib
import copy
import datetime
import fcntl
import json
import os
from pathlib import Path

import s5_current_regression_live as runner

runner.PACK = runner.ROOT / "docs/refactor/s5/structured-output-regression-20260927"
runner.OWNER = "s5-structured-output-canary-20260927"
runner.DEPENDENCIES += ("tests/s5_structured_canary.py",)
SCENARIO = "event_registration_fee"
SOURCE = "event_s1"


def case():
    inputs, labels = [runner.read(runner.PACK / name) for name in ("inputs.json", "labels.json")]
    scenario = next(row for row in inputs["scenarios"] if row["id"] == SCENARIO)
    source = scenario["sources"][0]
    assert source == {"id": SOURCE, "text": "海盐折纸工作坊的报名费为36元。"}
    assert labels["intake"][SOURCE] == dict(subject="海盐折纸工作坊", relation="报名费",
                                            value="36元", supporting_quote=source["text"])
    return scenario, source, labels


def preflight(output):
    runtime = runner.frozen()
    case()
    budgets = copy.deepcopy(runner.read(runner.PACK / "budgets.json"))
    assert budgets["provider_base_url"] == "https://ark.cn-beijing.volces.com/api/v3"
    assert budgets["timeout_ms"] == 180000 and budgets["automatic_retries"] == 0
    policy = budgets["runtime_defaults"]["model_budget"]
    assert policy["max_input_bytes"] == 65536
    assert policy["extraction_output_tokens"] == policy["review_output_tokens"] == 16384
    budgets["groups"] = {SCENARIO: {"intake": dict(group_id=runner.OWNER + "-intake",
                                                   max_calls=2, max_operation_calls=2)}}
    budgets.update(total_max_physical_requests=2, max_reserved_output_tokens=32768,
                   max_aggregate_input_bytes=131072, phase_max_physical_requests={"intake": 2})
    output.mkdir(parents=True, mode=0o700, exist_ok=False)
    directory = output / SCENARIO
    directory.mkdir(mode=0o700)
    (directory / "trace").mkdir(mode=0o700)
    config = runner.settings(directory, "intake", **runner.phase_config(budgets, SCENARIO, "intake"))
    manifest = dict(created_at_utc=runner.now(), output=str(output), owner=runner.OWNER,
                    runtime=runtime, dataset_freeze_sha256=runner.digest(runner.PACK / "dataset-freeze.json"),
                    budgets=budgets, configs={str(config.relative_to(output)): runner.digest(config)},
                    source_id=SOURCE, scenario=SCENARIO, preflight="passed")
    runner.save(output / "manifest.json", manifest, exclusive=True)
    runner.save(output / "report.json", {"status": "ready", "fixed_denominator": 1}, exclusive=True)
    print(json.dumps({"status": "preflight_passed", "operations": 1, "real_model_requests": 0}))


def verify(output, manifest):
    assert manifest["output"] == str(output) and manifest["owner"] == runner.OWNER
    assert manifest["preflight"] == "passed" and manifest["source_id"] == SOURCE
    assert manifest["scenario"] == SCENARIO and manifest["runtime"] == runner.frozen()
    assert manifest["dataset_freeze_sha256"] == runner.digest(runner.PACK / "dataset-freeze.json")
    for path, fingerprint in manifest["configs"].items():
        assert runner.digest(output / path) == fingerprint, "configuration changed"
    auth = runner.read(output / "authorization.json")
    assert auth["manifest_sha256"] == runner.digest(output / "manifest.json")
    assert auth["output"] == str(output) and auth["user_reply"].strip()
    assert auth["max_physical_requests"] == manifest["budgets"]["total_max_physical_requests"] == 2
    assert auth["provider_base_url"] == manifest["budgets"]["provider_base_url"]
    assert auth["automatic_retries"] == 0
    window = manifest["budgets"]["execution_window"]
    assert (datetime.datetime.fromisoformat(window["start_inclusive"])
            <= datetime.datetime.now(datetime.timezone.utc)
            < datetime.datetime.fromisoformat(window["end_exclusive"]))


def send(output):
    with (output / "runner.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        manifest, report = [runner.read(output / name) for name in ("manifest.json", "report.json")]
        verify(output, manifest)
        assert report["status"] == "ready", "canary already attempted; do not restart"
        scenario, source, labels = case()
        directory = output / SCENARIO
        db, artifact = directory / "business.sqlite3", directory / "00-event_s1.json"
        assert not db.exists(), "canary requires a fresh database"
        assert not artifact.exists(), "receipt already exists"
        artifact.with_suffix(".started").touch(exist_ok=False)
        report.update(status="running", started_at_utc=runner.now())
        runner.save(output / "report.json", report)
        try:
            env = {key: value for key, value in runner.clean_env("live").items() if not key.startswith("ARK_")}
            env["PMR_TRACE_DIR"] = str(directory / "trace")
            with contextlib.closing(runner.LongClient(manifest["runtime"]["binary"]["path"], db, env,
                    runner.OWNER, config=directory / "intake.json")) as client:
                bootstrap = client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
                assert bootstrap["model_calls"] == []
                for entity in scenario["entities"]:
                    client.call("memory_entity_upsert", entity)
                request = runner.source_request(scenario, source)
                result = runner.call(client, "memory_prepare", request, artifact)
                profile = runner.profile(db)
                client.errors.seek(0)
                stderr = client.errors.read()
                runner.save(artifact, dict(request=request, result=result, profile=profile,
                            completed_at_utc=runner.now(), stderr=stderr), exclusive=True)
            report.update(artifact=str(artifact.relative_to(output)), artifact_sha256=runner.digest(artifact))
            report["accounting"] = runner.audit(output, manifest["budgets"])
            assert report["accounting"]["physical_requests"] == 2
            assert "evidence_incomplete" not in stderr
            runner.runtime_checks(result)
            saved = runner.receipt(result)
            runner.check_intake_stages(saved)
            assert (saved["source"]["status"], saved["index"]["status"], saved["enrichment"]["status"]) == (
                "stored", "ready", "committed"), "intake did not commit supported facts"
            assert result["recall"]["status"] == "completed" and result["recall"]["result"]["model_calls"] == []
            record = next(row for row in profile["sources"].values() if row["turn_id"] == SOURCE)
            assert record["request"] == {**request["user_turn"], "task_id": request["user_turn"].get("task_id")}
            memories = profile["repository"]["memories"]
            memories = list(memories.values()) if isinstance(memories, dict) else memories
            report["score"] = runner.score_intake(scenario, source, saved, memories, labels,
                source_record={**record["request"], "source_id": record["id"], "enrichment": record["enrichment"]})
            report["status"] = "stopped" if report["score"]["safety_error"] else "review_required"
        except Exception as error:
            report.update(status="stopped", error=type(error).__name__ + ": " + str(error))
        finally:
            try:
                report["accounting"] = runner.audit(output, manifest["budgets"])
            except Exception as error:
                report.update(status="stopped", accounting_error=str(error))
                if (output / "accounting.json").exists():
                    report["accounting"] = runner.read(output / "accounting.json")
            report["formal_baseline_unchanged"] = runner.formal_state() == manifest["runtime"]["formal_baseline"]
            if not report["formal_baseline_unchanged"]:
                report["status"] = "stopped"
            report["completed_at_utc"] = runner.now()
            runner.save(output / "report.json", report)
    status(output)


def status(output):
    report = runner.read(output / "report.json")
    print(json.dumps({key: report.get(key) for key in ("status", "fixed_denominator", "error", "score",
                     "formal_baseline_unchanged")}, ensure_ascii=False))


def check():
    """Exercise the canary's send guards without constructing an MCP client."""
    import tempfile
    from unittest.mock import patch

    original = runner.PACK
    fixture = runner.ROOT / "docs/refactor/s5/scope-paraphrase-regression-20260927"
    with tempfile.TemporaryDirectory(prefix="s5-canary-check-") as temporary:
        directory = Path(temporary)
        pack, output = directory / "pack", directory / "run"
        pack.mkdir()
        for name in ("inputs.json", "labels.json", "dataset-freeze.json"):
            (pack / name).write_bytes((fixture / name).read_bytes())
        budget = runner.read(fixture / "budgets.json")
        current = datetime.datetime.now(datetime.timezone.utc)
        budget["execution_window"] = dict(start_inclusive=(current - datetime.timedelta(hours=1)).isoformat(),
                                            end_exclusive=(current + datetime.timedelta(hours=1)).isoformat())
        runner.save(pack / "budgets.json", budget)
        with patch.object(runner, "PACK", pack), patch.object(runner, "frozen", return_value={"formal_baseline": {}}), \
                patch.object(runner, "LongClient") as client:
            preflight(output)
            manifest = runner.read(output / "manifest.json")
            config = runner.read(output / SCENARIO / "intake.json")
            assert config["retrieval_mode"] == "local" and config["enrichment_mode"] == "inline"
            assert config["model_budget"]["max_calls"] == config["model_budget"]["max_operation_calls"] == 2

            def rejects(action, error=AssertionError):
                try:
                    action()
                except error:
                    return
                raise AssertionError("unsafe canary action was accepted")

            rejects(lambda: send(output), FileNotFoundError)
            auth = dict(manifest_sha256=runner.digest(output / "manifest.json"), output=str(output),
                        user_reply="offline fixture", max_physical_requests=2, automatic_retries=0,
                        provider_base_url=manifest["budgets"]["provider_base_url"])
            runner.save(output / "authorization.json", auth)
            verify(output, manifest)
            for key, value in (("max_physical_requests", 3), ("manifest_sha256", "changed"),
                               ("output", str(output) + "-other"), ("user_reply", ""), ("automatic_retries", 1)):
                runner.save(output / "authorization.json", {**auth, key: value})
                rejects(lambda: verify(output, manifest))
            runner.save(output / "authorization.json", auth)
            (output / SCENARIO / "00-event_s1.started").touch()
            rejects(lambda: send(output), FileExistsError)
            runner.save(output / "report.json", {"status": "stopped"})
            rejects(lambda: send(output))
            (output / SCENARIO / "intake.json").write_text("{}")
            rejects(lambda: verify(output, manifest))
            client.assert_not_called()
    assert runner.PACK == original
    print(json.dumps({"status": "checks_passed", "real_model_requests": 0,
                      "checks": ["two_call_cap", "authorization_binding", "no_replay", "configuration_hash"]}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("preflight", "send", "status", "check"))
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    if args.phase == "check":
        check()
    else:
        assert args.output is not None, "--output required"
        globals()[args.phase](args.output.resolve())
