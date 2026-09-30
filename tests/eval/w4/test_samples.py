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


    def test_checker_rejects_legacy_and_relation_errors(self):
        def legacy(s):
            return next(r for r in s["input"]["records"] if r["format"] == "legacy")
        cases = {
            "旧格式记录缺 gaps": ("w4_18_legacy_gaps", lambda s: legacy(s).update(gaps=[])),
            "旧格式记录带 update": ("w4_18_legacy_gaps", lambda s: legacy(s).update(update={"status": "judged", "reasons": []})),
            "关系只有一个成员": ("w4_15_resolve_conflict", lambda s: s["input"]["relations"][0].update(members=[50])),
            "关系成员不在输入中": ("w4_15_resolve_conflict", lambda s: s["input"]["relations"][0].update(members=[50, 99])),
            "关系依据不是原文": ("w4_15_resolve_conflict",
                           lambda s: s["input"]["relations"][0]["basis"][0]["quote"].update(text="屏幕是7英寸")),
            "关系成员重复": ("w4_15_resolve_conflict", lambda s: s["input"]["relations"][0].update(members=[50, 50])),
            "旧 Memory 缺必填字段": ("w4_18_legacy_gaps", lambda s: legacy(s)["data"].pop("owner_id")),
            "旧 Evidence 格式不符": ("w4_18_legacy_gaps",
                                  lambda s: legacy(s)["data"].update(evidence=[{"quote": "广州", "turn_id": None}])),
            "旧记录无依据却不标 source_missing": ("w4_18_legacy_gaps", lambda s: legacy(s).update(gaps=["sentence_missing"])),
        }
        for name, (sid, mutate) in cases.items():
            s = copy.deepcopy(self.samples[sid])
            mutate(s)
            with self.subTest(case=name):
                self.assertTrue(check_samples.check(s))


    def test_checker_rejects_dropped_qualifiers(self):
        def drop_times(i):
            def f(s):
                s["input"]["candidates"][i]["candidate"]["times"] = []
                s["input"]["candidates"][i]["time_resolutions"] = []
            return f
        cases = {
            "省略“最近”": ("w4_17_pending_unclear_relation", drop_times(0)),
            "省略“现在”": ("w4_13_state_change_date_unknown", drop_times(0)),
            "强度表达置空": ("w4_11_preference_strength",
                           lambda s: s["input"]["candidates"][0]["candidate"].update(strength=None)),
        }
        for name, (sid, mutate) in cases.items():
            s = copy.deepcopy(self.samples[sid])
            mutate(s)
            with self.subTest(case=name):
                self.assertTrue(check_samples.check(s))

    def test_checker_rejects_resolution_leaving_both_sides_current(self):
        def rel(s):
            return s["expect"]["after"]["relations"][0]
        cases = {
            "败方未结束": ("w4_21_confirm_conflict_side",
                        lambda s: s["expect"]["after"]["records"]["50"].update(stored_status="active")),
            "双方都列为保留方": ("w4_15_resolve_conflict", lambda s: rel(s).update(survivors=[50, 51])),
            "未写保留方": ("w4_15_resolve_conflict", lambda s: rel(s).pop("survivors")),
            "决定解决了关系但执行后未标 resolved": ("w4_15_resolve_conflict",
                                         lambda s: s["expect"]["after"].update(relations=[])),
            "标 resolved 却没有决定解决": ("w4_21_confirm_conflict_side",
                                    lambda s: s["expect"]["decisions"][1].update(resolves_relations=[])),
        }
        for name, (sid, mutate) in cases.items():
            s = copy.deepcopy(self.samples[sid])
            mutate(s)
            with self.subTest(case=name):
                self.assertTrue(check_samples.check(s))


class ReviewStatusRule(unittest.TestCase):
    def test_status_rule(self):
        from review_status import status_for
        d = {"id": "x", "label_status": "draft", "note": "a"}
        self.assertEqual(status_for(d, None), "draft")
        self.assertEqual(status_for(d, {"id": "x", "label_status": "reviewed", "note": "a"}), "reviewed")
        self.assertEqual(status_for(d, {"id": "x", "label_status": "reviewed", "note": "b"}), "draft")
        self.assertEqual(status_for(d, {"id": "x", "label_status": "draft", "note": "a"}), "draft")
        self.assertEqual(status_for(d, {"id": "x", "note": "a"}), "draft")


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
