"""Report tests use labelled adapter fixtures, never claim they are model recordings."""
import copy
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import compare
import trial


class EndToEndReport(unittest.TestCase):
    def test_empty_is_not_recorded_not_pass_or_model_failure(self):
        report = compare.run()
        self.assertEqual(report["not_recorded"], 63)
        self.assertEqual(report["new_summary"]["questions"], 0)
        self.assertEqual(report["new_summary"]["common"]["items"], 0)
        self.assertEqual(report["reference"]["regressions"], [])
        self.assertEqual(report["reference"]["summary"], compare.baseline.summarize(compare.baseline.run()))
        self.assertEqual(report["absolute_summary"]["not_recorded"],63)
        self.assertEqual(sum(report["absolute_summary"][s] for s in ("pass","fail","pending_review")),0)
        self.assertIn("未录制", compare.render(report))

    def document(self):
        case, q, *_ = trial.resolve_question("person_sketch_pen", "person_q4")
        fixture = trial.new_fixture(case["id"], q["id"])
        raw = copy.deepcopy(fixture["recall"])
        at = compare.trusted_question(q)["at"]
        raw["request"]["at"] = at
        return {"label": "adapter-fixture-only", "cases": [{"case": case["id"], "steps": [
            {"step": "question", "question": q["id"], "state": "completed", "at": at,
             "result": raw, "source_ids": fixture["source_ids"]}]}]}

    def test_output_uses_common_adapter_and_same_denominator(self):
        report = compare.run(self.document())
        self.assertEqual(report["not_recorded"], 62)
        row = next(r for r in report["questions"] if r["state"] == "completed")
        ref = next(r for r in report["reference"]["questions"] if r["question"] == row["question"])
        self.assertEqual(ref["old"]["common"]["denominator"], row["new"]["common"]["denominator"])
        self.assertTrue(ref["transitions"])
        self.assertEqual(row["new_view"]["adapter"], "recall_v3@1")
        self.assertEqual(compare.trusted_question({"after": 2, "at": None})["at"],
                         compare.CLOCK + 90)

    def test_pass_to_fail_is_separate_and_time_binding_is_not_relaxed(self):
        doc = self.document()
        step = doc["cases"][0]["steps"][0]
        step["result"]["status"] = "insufficient"
        step["result"]["facts"] = []
        report = compare.run(doc)
        row = next(r for r in report["questions"] if r["state"] == "completed")
        self.assertEqual(row["absolute"],"fail")
        self.assertTrue(report["reference"]["regressions"])
        step["at"] += 1
        with self.assertRaisesRegex(ValueError, "trusted time"):
            compare.run(doc)

    def test_absolute_score_does_not_depend_on_reference_availability(self):
        available = compare.run(self.document())
        self.assertEqual(available["absolute_summary"]["pending_review"],1)
        with patch.object(compare.baseline,"run",side_effect=FileNotFoundError("reference absent")):
            absent = compare.run(self.document())
        self.assertEqual(available["absolute_summary"],absent["absolute_summary"])
        self.assertEqual(available["new_summary"],absent["new_summary"])
        self.assertEqual(absent["reference"]["state"],"unavailable")
        self.assertFalse(absent["reference"]["acceptance_gate"])
        text=compare.render(available)
        self.assertLess(text.index("逐题"),text.index("附录：0517000"))

    def test_case_stage_metrics_keep_replay_cost_and_failures_separate(self):
        doc=self.document()
        doc.update(jobs=8,elapsed_ms=500,real_calls=2)
        case=doc["cases"][0]
        case["elapsed_ms"]=400
        case["calls"]=[
            {"operation":"write:m1","stage":"W1","origin":"live","sends":1,"elapsed_ms":21,
             "recorded_elapsed_ms":20,"http_status":200,"response_status":"incomplete",
             "incomplete_details":{"reason":"max_output_tokens"},"usage":{"output_tokens":4096}},
            {"operation":"write:m1","stage":"W3","origin":"replay","sends":0,"elapsed_ms":1,
             "recorded_elapsed_ms":22000,"http_status":429,"response_status":None,"error":"rate limit"},
            {"operation":"write:m2","stage":"W4","origin":"live","sends":1,"elapsed_ms":22,
             "recorded_elapsed_ms":22,"http_status":503,"response_status":None,"transport_error":"connection closed"},
            {"operation":"recall:q","stage":"R1","origin":"not_recorded","sends":0,"elapsed_ms":0,
             "reason":"额度耗尽（EVAL_RECORD）"},
            {"operation":"recall:q","stage":"R4","origin":"replay","sends":0,"elapsed_ms":1,
             "recorded_elapsed_ms":2000,"http_status":200,"response_status":"completed",
             "usage":{"input_tokens":10,"output_tokens":20}},
        ]
        case["steps"].append({"step":"message","message":"m1","result":{"Err":"attempt_failed"}})
        report=compare.run(doc)
        metrics=report["execution"]["cases"][0]
        self.assertEqual(sum(s["sends"] for s in metrics["stages"].values()),2)
        self.assertEqual(metrics["stages"]["W3"]["elapsed_ms"],1)
        self.assertEqual(metrics["stages"]["W3"]["recorded_elapsed_ms"],22000)
        self.assertEqual(metrics["stages"]["W1"]["diagnostics"][0]["incomplete_details"]["reason"],"max_output_tokens")
        self.assertEqual(metrics["stages"]["W4"]["diagnostics"][0]["transport_error"],"connection closed")
        self.assertEqual(metrics["stages"]["R1"]["not_recorded"],1)
        self.assertEqual(metrics["stages"]["R4"]["diagnostics"],[])
        self.assertEqual(metrics["runtime_diagnostics"][0]["error"],"attempt_failed")
        self.assertIn("max_output_tokens",compare.render(report))

    def test_local_outcomes_do_not_follow_remote_completed_or_replay_latency(self):
        doc = self.document()
        doc["execution_mode"] = "replay"
        case = doc["cases"][0]
        case["calls"] = []
        for index, outcome in enumerate(("timeout", "output_limit", "decode_failed", "validation_failed", "completed")):
            mid = f"m{index}"
            call_id = str(index)
            events = [{"kind": "model_start", "data": {"stage": "w3_v1", "call_id": call_id, "timeout_ms": 360000}},
                      {"kind": "model_end", "data": {"call_id": call_id,
                       "local_outcome": outcome if index < 2 else "completed", "duration_ms": 1}}]
            if index in (2, 3):
                events.append({"kind": "model_check", "data": {"stage": "W3", "status": "failed",
                               "phase": "decode" if index == 2 else "validate", "error": "specific rule"}})
            case["steps"].append({"step": "message", "message": mid, "model_diagnostics": events})
            case["calls"].append({"operation": f"write:{mid}", "stage": "W3", "origin": "replay",
                "sends": 0, "elapsed_ms": 1, "recorded_elapsed_ms": 242154,
                "http_status": 200, "response_status": "completed"})
        report = compare.run(doc)
        calls = report["execution"]["cases"][0]["calls"]
        self.assertEqual([c["local_outcome"] for c in calls],
                         ["timeout", "output_limit", "decode_failed", "validation_failed", "completed"])
        self.assertEqual(len(report["execution"]["cases"][0]["stages"]["W3"]["diagnostics"]), 4)
        text = compare.render(report)
        self.assertIn("【回放】", text)
        self.assertIn("不是线上成功率", text)
        self.assertIn("specific rule", text)
        self.assertIn("timeout", text)
        case["steps"] = case["steps"][:1]
        self.assertEqual(compare.execution(doc)["cases"][0]["calls"][0]["local_outcome"], "unknown")

    def test_replay_cannot_overwrite_a_live_report(self):
        with tempfile.TemporaryDirectory(prefix="eval-live-report-") as directory:
            root = pathlib.Path(directory)
            doc = self.document()
            doc["execution_mode"] = "replay"
            run_path = root / "run-replay.json"
            run_path.write_text(json.dumps(doc))
            prefix = root / "live-report"
            before = json.dumps({"execution": {"real_calls": 552}})
            prefix.with_suffix(".json").write_text(before)
            prefix.with_suffix(".md").write_text("original live report")
            cli = subprocess.run([sys.executable, str(pathlib.Path(compare.__file__)), "--run", str(run_path),
                                  "--output", str(prefix)], capture_output=True, text=True)
            self.assertNotEqual(cli.returncode, 0)
            self.assertIn("不得覆盖 live", cli.stderr)
            self.assertEqual(prefix.with_suffix(".json").read_text(), before)
            self.assertEqual(prefix.with_suffix(".md").read_text(), "original live report")

    def test_replay_cannot_attach_historical_send_counts(self):
        with tempfile.TemporaryDirectory(prefix="eval-replay-binding-") as directory:
            root = pathlib.Path(directory)
            run_path = root / "run-replay.json"
            doc = self.document()
            doc["execution_mode"] = "replay"
            run_path.write_text(json.dumps(doc))
            log = root / "sends.json"
            log.write_text(json.dumps({"version":"eval.e2e.sends@1", "run_id":"old-live",
                "attempts":[{"id":0,"state":"recorded"}]}))
            cli = subprocess.run([sys.executable, str(pathlib.Path(compare.__file__)), "--run", str(run_path),
                                  "--send-log", str(log), "--output", str(root / "report")], capture_output=True, text=True)
            self.assertNotEqual(cli.returncode, 0)
            self.assertIn("回放不得绑定真实发送日志", cli.stderr)
            self.assertFalse((root / "report.json").exists())

    def test_interrupted_send_log_reports_counts_without_completed_run(self):
        with tempfile.TemporaryDirectory(prefix="eval-send-report-") as directory:
            root=pathlib.Path(directory)
            (root/"runs").mkdir()
            path=root/"runs"/"run-a.json"
            attempts=[{"id":i,"case":"case","operation":"write:m1","position":i,
                       "stage":"w1_v1","state":"started","request":f"fp-{i}"} for i in range(2)]
            log={"version":"eval.e2e.sends@1","run_id":"run-a","attempts":attempts,
                 "proxy_errors":[{"case":"case","error":"response write failed"}]}
            path.write_text(json.dumps(log))
            # First recording reached disk; its journal completion did not. The second
            # row belongs to another send, so it cannot close this run's in-flight entry.
            (root/"case.json").write_text(json.dumps({"operations":{"write:m1":[
                {"request":"fp-0","send_run":"run-a","send_index":0},
                {"request":"fp-1","send_run":"another-run","send_index":1}]}}))
            recovered=compare.load_send_log(path)
            self.assertEqual([a["state"] for a in recovered["attempts"]],["recorded","started"])
            self.assertEqual(json.loads(path.read_text())["attempts"][0]["state"],"started")
            prefix=root/"report"
            cli=subprocess.run([sys.executable,str(pathlib.Path(compare.__file__)),"--send-log",str(path),
                                "--output",str(prefix)],capture_output=True,text=True)
            self.assertEqual(cli.returncode,0,cli.stderr)
            report=json.loads(prefix.with_suffix(".json").read_text())
            self.assertEqual(report["execution"]["real_calls"],2)
            self.assertEqual(report["execution"]["recorded_calls"],1)
            self.assertEqual(len(report["execution"]["uncompleted_calls"]),1)
            self.assertEqual(report["absolute_summary"]["not_recorded"],63)
            text=prefix.with_suffix(".md").read_text()
            self.assertIn("未完成或未落盘 1",text)
            self.assertIn("response write failed",text)
            self.assertIn("不能证明远端收到",text)
            run_path=root/"run.json"
            run_path.write_text(json.dumps({"run_id":"another-run","cases":[]}))
            wrong=subprocess.run([sys.executable,str(pathlib.Path(compare.__file__)),"--run",str(run_path),
                                  "--send-log",str(path),"--output",str(prefix)],capture_output=True,text=True)
            self.assertNotEqual(wrong.returncode,0)
            self.assertIn("不是同一次运行",wrong.stderr)


if __name__ == "__main__":
    unittest.main()
