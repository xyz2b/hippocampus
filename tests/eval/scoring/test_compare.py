"""Report tests use labelled adapter fixtures, never claim they are model recordings."""
import copy
import unittest

import compare
import trial


class EndToEndReport(unittest.TestCase):
    def test_empty_is_not_recorded_not_pass_or_model_failure(self):
        report = compare.run()
        self.assertEqual(report["not_recorded"], 63)
        self.assertEqual(report["new_summary"]["questions"], 0)
        self.assertEqual(report["new_summary"]["common"]["items"], 0)
        self.assertEqual(report["regressions"], [])
        self.assertEqual(report["old_summary"], compare.baseline.summarize(compare.baseline.run()))
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
        self.assertEqual(row["old"]["common"]["denominator"], row["new"]["common"]["denominator"])
        self.assertTrue(row["transitions"])
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
        self.assertTrue(row["regressions"])
        self.assertTrue(report["regressions"])
        step["at"] += 1
        with self.assertRaisesRegex(ValueError, "trusted time"):
            compare.run(doc)


if __name__ == "__main__":
    unittest.main()
