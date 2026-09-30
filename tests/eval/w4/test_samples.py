"""python3 -m unittest discover -s tests/eval/w4 -v"""
import copy
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import check_samples  # noqa: E402


class Samples(unittest.TestCase):
    def setUp(self):
        self.samples = {s["id"]: s for s in check_samples.load_samples()}

    def test_all_samples_pass_structural_checks(self):
        for sid, s in self.samples.items():
            with self.subTest(sample=sid):
                self.assertEqual(check_samples.check(s), [])

    def test_checker_rejects_broken_samples(self):
        base = self.samples["w4_01_state_change"]
        cases = {
            "目标不在输入中": lambda s: s["expect"]["decisions"][0].update(targets=[99]),
            "coexist 带目标": lambda s: s["expect"]["decisions"][0].update(action="coexist"),
            "pending 带动作": lambda s: s["expect"]["decisions"][0].update(status="pending", pending_reason="scope_incomplete"),
            "依据缺旧来源": lambda s: s["expect"]["decisions"][0].update(basis_sources=["s-205"]),
            "原文不存在": lambda s: s["input"]["candidates"][0]["candidate"]["value"]["quote"].update(text="广州"),
            "时间换算错": lambda s: s["input"]["candidates"][0]["time_resolutions"][0].update(start=0),
            "effective 用保存期限": lambda s: s["expect"]["decisions"][0]["effective"].update(role="retention"),
            "effective 偏移改成 -12": lambda s: s["expect"]["decisions"][0]["effective"]["interp"].update(offset=-12),
            "删除全部时间换算": lambda s: s["input"]["candidates"][0].update(time_resolutions=[]),
            "月份换算精度错": lambda s: s["input"]["candidates"][0]["time_resolutions"][0].update(precision="day"),
        }
        for name, mutate in cases.items():
            s = copy.deepcopy(base)
            mutate(s)
            with self.subTest(case=name):
                self.assertTrue(check_samples.check(s))

    def test_checker_rejects_scope_and_recurring_errors(self):
        cases = {
            "通用 fact 标成范围完整": ("w4_03_refine_whole", lambda s: s["input"]["coverage"].update(complete=True)),
            "范围不完整却标 judged": ("w4_03_refine_whole",
                                  lambda s: s["expect"]["after"]["records"]["new"].update(update={"status": "judged", "reasons": []})),
            "撤回回执标 judged": ("w4_05_retract",
                              lambda s: s["expect"]["after"].update(candidate_update={"status": "judged", "reasons": []})),
            "周期时间生成日期区间": ("w4_05_retract", lambda s: s["input"]["records"][0]["time_resolutions"][0].update(
                status="resolved", start=1, end_exclusive=2, precision="day")),
            "周期时间漏了换算": ("w4_03_refine_whole", lambda s: s["input"]["records"][0].update(time_resolutions=[])),
        }
        for name, (sid, mutate) in cases.items():
            s = copy.deepcopy(self.samples[sid])
            mutate(s)
            with self.subTest(case=name):
                self.assertTrue(check_samples.check(s))


class CorrectOutputsPass(unittest.TestCase):
    """Correct variants the checker must accept (review round 2)."""

    def setUp(self):
        self.samples = {s["id"]: s for s in check_samples.load_samples()}

    def test_effective_time_verified_but_unresolvable(self):
        # "那阵子搬到上海": meaning verified, date not resolvable -> unknown, end_unknown.
        s = copy.deepcopy(self.samples["w4_01_state_change"])
        text = "我那阵子从北京搬到上海了。"
        s["sources"]["s-205"]["text"] = text
        c = s["input"]["candidates"][0]
        t = {"role": "event", "quote": {"text": "那阵子"}, "interp": {"kind": "unknown"}, "anchor": "said_on"}
        c["candidate"]["sentence"]["text"] = text
        c["candidate"]["times"] = [t]
        c["time_resolutions"] = [{"status": "unknown", "start": None, "end_exclusive": None, "precision": "unknown"}]
        s["expect"]["decisions"][0]["effective"] = t
        s["expect"]["after"]["records"]["17"]["ended"] = "end_unknown"
        self.assertEqual(check_samples.check(s), [])

    def test_untouched_old_record_keeps_its_state(self):
        # Coexist leaves record 31 as it was, including a judged update state.
        s = copy.deepcopy(self.samples["w4_04_coexist_partial"])
        s["expect"]["after"]["records"]["31"]["update"] = {"status": "judged", "reasons": []}
        self.assertEqual(check_samples.check(s), [])


if __name__ == "__main__":
    unittest.main()
