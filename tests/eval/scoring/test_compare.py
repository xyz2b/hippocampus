"""Report tests use labelled adapter fixtures, never claim they are model recordings."""
import copy
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


if __name__ == "__main__":
    unittest.main()
