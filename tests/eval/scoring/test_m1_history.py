"""AF27: recompute the accepted history; never treat it as a current strict replay."""
import hashlib
import json
import unittest

import compare
import trial


class M1AcceptedHistory(unittest.TestCase):
    def test_ac2_and_both_targeted_runs_with_confirmed_ledger_are_59_4(self):
        root = trial.EVAL / "end_to_end"
        base = root / "ac2-pro-medium-pro-minimal-pro-minimal-r4-compute-low" / "run.json"
        digest = hashlib.sha256(base.read_bytes()).hexdigest()
        merged = json.loads(base.read_text())
        for name in ("ac2-af-targeted", "af14-targeted"):
            directory = root / name
            plan = json.loads((directory / "target-plan.json").read_text())
            self.assertEqual(plan["version"], "eval.e2e.targeted@1")
            self.assertEqual(plan["source_run_sha256"], digest)
            merged = compare.merge_runs(merged, json.loads((directory / "run.json").read_text()),
                                        plan["questions"], "accepted history", name)
        report = compare.run(merged)  # Existing labels, adapters and confirmed review ledger.
        summary = report["absolute_summary"]
        self.assertEqual({key: summary[key] for key in
                          ("pass", "fail", "pending_review", "not_recorded", "questions")},
                         {"pass": 59, "fail": 4, "pending_review": 0,
                          "not_recorded": 0, "questions": 63})
        self.assertEqual({q["question"] for q in report["questions"] if q["absolute"] == "fail"},
                         {"unresolved_conflict/conflict_q2", "lme_7a87bd0c/q",
                          "lme_a2f3aa27/q", "lme_59524333/q"})
        print("历史合并离线重算：63题，59通过 / 4失败 / 0待复核；本次模型发送0。")


if __name__ == "__main__":
    unittest.main()
