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

    def test_relation_scope_shape_is_strict(self):
        base = self.samples["w4_07_conflict"]
        cases = {
            "空对象": {},
            "缺 times": {"conditions": []},
            "缺 conditions": {"times": []},
            "未知字段": {"conditions": [], "times": [], "note": "x"},
            "引用多余字段": {"conditions": [{"of": "candidate", "index": 0, "x": 1}], "times": []},
            "引用越界": {"conditions": [{"of": "candidate", "index": 5}], "times": []},
            "引用非目标记录": {"conditions": [{"of": 999, "index": 0}], "times": []},
        }
        for name, scope in cases.items():
            with self.subTest(case=name):
                s = copy.deepcopy(base)
                s["expect"]["decisions"][0]["relation_scope"] = scope
                self.assertNotEqual(check_samples.check(s), [])
        # 目标带有效条件时，记录 ID 也必须是整数：50.0、True 与 50 在 Python 里相等，不能放过。
        for name, of in {"浮点 ID": 50.0, "布尔": True, "字符串 ID": "50"}.items():
            with self.subTest(case=name):
                s = copy.deepcopy(base)
                s["input"]["records"][0]["candidate"]["conditions"] = [{"quote": {"text": "屏幕"}, "resolved": True}]
                s["expect"]["decisions"][0]["relation_scope"] = {"conditions": [{"of": of, "index": 0}], "times": []}
                self.assertTrue(any("relation_scope" in e for e in check_samples.check(s)))
        # 引用新候选自己的不确定表达以外的限定语：本例候选没有条件，空范围合法。
        self.assertEqual(check_samples.check(base), [])

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
            "非 conflict 带 relation_scope": lambda s: s["expect"]["decisions"][0].update(
                relation_scope={"conditions": [], "times": []}),
            "缺 relation_scope": lambda s: s["expect"]["decisions"][0].pop("relation_scope"),
            "未结束却有 end": lambda s: s["input"]["records"][0].update(
                end={"status": "resolved", "start": 1, "end_exclusive": 2, "precision": "day"}),
            "缺 end": lambda s: s["input"]["records"][0].pop("end"),
            "契约版本不对": lambda s: s.update(contract="w4.v1"),
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
            "多出未标注的时间": ("w4_09_new", lambda s: (
                s["input"]["candidates"][0]["candidate"].update(times=[{"role": "validity", "quote": {"text": "字节"},
                                                                        "interp": {"kind": "unknown"}, "anchor": "said_on"}]),
                s["input"]["candidates"][0].update(time_resolutions=[{"status": "unknown", "start": None,
                                                                      "end_exclusive": None, "precision": "unknown"}]))),
            "候选截取片段": ("w4_21_confirm_conflict_side",
                         lambda s: s["input"]["candidates"][1]["candidate"].update(sentence={"text": "7英寸那份是记错的"})),
            "fact 值与原句不同": ("w4_21_confirm_conflict_side",
                              lambda s: s["input"]["candidates"][0]["candidate"]["value"].update(
                                  quote={"text": "栖风读书机的屏幕是8英寸"})),
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
            "败方改成强化": ("w4_21_confirm_conflict_side",
                         lambda s: s["expect"]["decisions"][1].update(action="reinforce")),
            "终止动作缺执行后状态": ("w4_01_state_change", lambda s: s["expect"]["after"]["records"].pop("17")),
            "替代后成员被迁移": ("w4_22_replace_conflict_members",
                           lambda s: rel(s).update(members=["new", "new"])),
            "替代后关系被当作已解决": ("w4_22_replace_conflict_members", lambda s: rel(s).update(status="resolved")),
            "替代记录缺 relation_unclear": ("w4_22_replace_conflict_members",
                                      lambda s: s["expect"]["after"]["records"]["new"].update(update={"status": "judged", "reasons": []})),
            "终止原因与动作不符": ("w4_21_confirm_conflict_side",
                            lambda s: s["expect"]["after"]["records"]["50"].update(termination_reason="Refinement")),
        }
        for name, (sid, mutate) in cases.items():
            s = copy.deepcopy(self.samples[sid])
            mutate(s)
            with self.subTest(case=name):
                self.assertTrue(check_samples.check(s))


# W5 D2: with 110 stale, candidate 0's group needs re-judge and writes nothing;
# only candidate 1's new record is inserted.
D2_EXPECTED_NEW_RECORDS = 1


class ReplacementBinding(unittest.TestCase):
    """Replacement records are bound to the candidate that produced them (w4_22)."""

    def setUp(self):
        self.base = {s["id"]: s for s in check_samples.load_samples()}["w4_22_replace_conflict_members"]

    def rejudged(self):
        # Same decision, re-judge path: candidate already committed as 120, reused.
        s = copy.deepcopy(self.base)
        s["input"]["candidates"][0]["committed_id"] = 120
        recs = s["expect"]["after"]["records"]
        recs["120"] = recs.pop("new")
        return s

    def with_unrelated_new_fact(self):
        s = copy.deepcopy(self.base)
        s["sources"]["s-511"] = {"text": "我在腾讯工作。", "said_on": "2026-09-28"}
        c = copy.deepcopy(s["input"]["candidates"][0])
        c["index"] = 1
        c["source"] = {"kind": "stored", "source_id": "s-511"}
        c["candidate"].update(relation="works_at", sentence={"text": "我在腾讯工作。"},
                              value={"kind": "text", "text": "腾讯", "quote": {"text": "腾讯"}})
        s["input"]["candidates"].append(c)
        s["expect"]["decisions"].append({"candidate": 1, "status": "decided", "action": "new", "targets": [],
                                         "resolves_relations": [], "basis_sources": ["s-511"], "effective": None,
                                         "relation_scope": None, "pending_reason": None})
        s["expect"]["after"]["records"]["new_work"] = {"from_candidate": 1, "stored_status": "active",
                                                       "update": {"status": "judged", "reasons": []}}
        s["expect"]["after"]["new_records"] = 2  # both candidates are first commits
        return s

    def same_source_two_groups(self):
        # One source, one claim, two independent dependency groups (W5 D2/D3):
        # candidate 0 replaces the conflicting 110/111, candidate 1 is an
        # unrelated new fact from the same sentence.
        s = copy.deepcopy(self.base)
        text = "我已经搬到深圳了，现在在腾讯工作。"
        s["sources"]["s-510"]["text"] = text
        c0 = s["input"]["candidates"][0]
        c0["candidate"]["sentence"] = {"text": text}
        c1 = copy.deepcopy(c0)
        c1["index"] = 1
        c1["candidate"].update(relation="works_at", value={"kind": "text", "text": "腾讯", "quote": {"text": "腾讯"}},
                               times=[{"role": "validity", "quote": {"text": "现在"}, "interp": {"kind": "unknown"},
                                       "anchor": "said_on"}])
        c1["time_resolutions"] = [{"status": "unknown", "start": None, "end_exclusive": None, "precision": "unknown"}]
        s["input"]["candidates"].append(c1)
        s["qualifier_marks"]["candidate:1"] = {"times": ["现在"]}
        s["expect"]["decisions"].append({"candidate": 1, "status": "decided", "action": "new", "targets": [],
                                         "resolves_relations": [], "basis_sources": ["s-510"], "effective": None,
                                         "relation_scope": None, "pending_reason": None})
        s["expect"]["after"]["records"]["new_work"] = {"from_candidate": 1, "stored_status": "active",
                                                       "update": {"status": "judged", "reasons": []}}
        s["expect"]["after"]["new_records"] = 2  # both candidates are first commits
        return s

    def rejudged_member_in_input(self):
        # 120 was committed earlier and is itself a member of rel-2, so code
        # passes it in the input records; the re-judge still reuses 120.
        s = self.rejudged()
        c = s["input"]["candidates"][0]
        rec = copy.deepcopy(s["input"]["records"][0])
        rec.update(id=120, candidate=copy.deepcopy(c["candidate"]), said_on=c["said_on"],
                   evidence=[{"source": c["source"], "quote": {"text": c["candidate"]["sentence"]["text"]}}])
        s["input"]["records"].append(rec)
        s["input"]["relations"][0]["members"].append(120)
        s["expect"]["after"]["relations"][0]["members"].append(120)
        return s

    def w21_numbered(self):
        s = copy.deepcopy({x["id"]: x for x in check_samples.load_samples()}["w4_21_confirm_conflict_side"])
        recs = s["expect"]["after"]["records"]
        recs["130"] = recs.pop("new")
        return s

    def merged(self):
        # Semantically valid merge positive: w4_23 (same value and qualifiers, different source).
        return copy.deepcopy({x["id"]: x for x in check_samples.load_samples()}["w4_23_rejudge_merge"])

    def test_correct_variants_pass(self):
        for name, s in [("首次提交", copy.deepcopy(self.base)), ("重判复用 120", self.rejudged()),
                        ("同批无关新事实不受影响", self.with_unrelated_new_fact()),
                        ("同一原文两组（W5 D2/D3 夹具）", self.same_source_two_groups()),
                        ("重判复用的 120 也在输入中", self.rejudged_member_in_input()),
                        ("21 号新记录用数字编号", self.w21_numbered()),
                        ("重判为强化另一记录（归并例外，w4_23）", self.merged())]:
            with self.subTest(case=name):
                self.assertEqual(check_samples.check(s), [])

    def test_broken_variants_fail(self):
        def rec(s, k):
            return s["expect"]["after"]["records"][k]
        cases = {
            "重判：删前身链": (self.rejudged, lambda s: rec(s, "120").pop("history")),
            "重判：清掉待判定": (self.rejudged, lambda s: rec(s, "120").update(update={"status": "judged", "reasons": []})),
            "重判：另插一条新记录": (self.rejudged, lambda s: s["expect"]["after"]["records"].update(
                new=dict(rec(s, "120")))),
            "首次：删 history": (lambda: copy.deepcopy(self.base), lambda s: rec(s, "new").pop("history")),
            "首次：history 不全": (lambda: copy.deepcopy(self.base), lambda s: rec(s, "new").update(history=[110])),
            "首次：judged 只留原因字符串": (lambda: copy.deepcopy(self.base), lambda s: rec(s, "new").update(
                update={"status": "judged", "reasons": ["update_relation_unclear"]})),
            "首次：未绑定候选": (lambda: copy.deepcopy(self.base), lambda s: rec(s, "new").pop("from_candidate")),
            "输入中的 120：删前身链": (self.rejudged_member_in_input, lambda s: rec(s, "120").pop("history")),
            "输入中的旧记录被绑定为产物": (lambda: copy.deepcopy(self.base),
                                   lambda s: rec(s, "110").update(from_candidate=0)),
            "21 号数字编号：清掉待判定": (self.w21_numbered, lambda s: rec(s, "130").update(
                update={"status": "judged", "reasons": []})),
            "21 号数字编号：删掉更新状态": (self.w21_numbered, lambda s: rec(s, "130").pop("update")),
            "归并例外：没写归并目标": (self.merged, lambda s: rec(s, "70").pop("merged_into")),
            "归并例外：仍在投影中": (self.merged, lambda s: rec(s, "70").update(projected=True)),
            "非归并动作借 merged_into 跳过：judged": (self.w21_numbered, lambda s: rec(s, "130").update(
                merged_into=None, update={"status": "judged", "reasons": []})),
            "非归并动作借 merged_into 跳过：删更新状态": (self.w21_numbered, lambda s: (
                rec(s, "130").update(merged_into=None), rec(s, "130").pop("update"))),
        }
        for name, (make, mutate) in cases.items():
            s = make()
            mutate(s)
            with self.subTest(case=name):
                self.assertTrue(check_samples.check(s))


    def test_two_first_commits_expect_two_new_records(self):
        # D3 (both groups succeed) inserts 2; D2 (candidate 0's group needs
        # re-judge) inserts only candidate 1's record.
        for name, s in [("同批无关新事实", self.with_unrelated_new_fact()),
                        ("同一原文两组 D3", self.same_source_two_groups())]:
            with self.subTest(case=name):
                self.assertEqual(s["expect"]["after"]["new_records"], 2)
        self.assertEqual(D2_EXPECTED_NEW_RECORDS, 1)

    def test_merge_into_self_hits_its_own_guard(self):
        # Target 70 is also outside the input, so other rules fire too; assert
        # the dedicated guard itself reports the self-merge.
        s = self.merged()
        s["expect"]["decisions"][0].update(targets=[70])
        s["expect"]["after"]["records"]["70"].update(merged_into=70)
        errs = check_samples.check(s)
        self.assertTrue(any("重判不能强化或归并到自身" in e for e in errs), errs)


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
        s["qualifier_marks"]["candidate:0"] = {"times": ["那阵子"]}
        c = s["input"]["candidates"][0]
        t = {"role": "event", "quote": {"text": "那阵子"}, "interp": {"kind": "unknown"}, "anchor": "said_on"}
        c["candidate"]["sentence"]["text"] = text
        c["candidate"]["times"] = [t]
        c["time_resolutions"] = [{"status": "unknown", "start": None, "end_exclusive": None, "precision": "unknown"}]
        s["expect"]["decisions"][0]["effective"] = t
        s["expect"]["after"]["records"]["17"]["ended"] = "end_unknown"
        self.assertEqual(check_samples.check(s), [])

    def test_unmarked_lookalike_words_are_not_qualifiers(self):
        # "特别版" is not a strength and a title containing "以前" is not a time:
        # with no reviewer mark on the source, nothing has to be kept.
        s = copy.deepcopy(self.samples["w4_10_reinforce"])
        text = "我还是喜欢《以前的我们》里提到的特别版咖啡。"
        s["sources"]["s-320"]["text"] = text
        c = s["input"]["candidates"][0]["candidate"]
        c.update(sentence={"text": text}, value={"kind": "text", "text": text, "quote": {"text": text}})
        self.assertEqual(check_samples.check(s), [])

    def _two_assertions_one_sentence(self):
        # "我上个月…，妹妹去年…": one complete sentence, two assertions, each
        # with its own time. Marks are per assertion, so neither borrows the other's.
        s = copy.deepcopy(self.samples["w4_01_state_change"])
        text = "我上个月从北京搬到上海了，妹妹去年搬到杭州。"
        s["sources"]["s-205"]["text"] = text
        c0 = s["input"]["candidates"][0]
        c0["candidate"]["sentence"] = {"text": text}
        c1 = copy.deepcopy(c0)
        c1["index"] = 1
        last_year = {"role": "event", "quote": {"text": "去年"}, "interp": {"kind": "relative", "unit": "year", "offset": -1},
                     "anchor": "said_on"}
        c1["candidate"].update(subject={"entity_id": "sister"}, times=[last_year],
                               value={"kind": "text", "text": "杭州", "quote": {"text": "杭州"}})
        c1["time_resolutions"] = [{"status": "resolved", "start": 1735660800, "end_exclusive": 1767196800,
                                   "precision": "year"}]
        s["input"]["candidates"].append(c1)
        s["expect"]["decisions"].append({"candidate": 1, "status": "decided", "action": "new", "targets": [],
                                         "resolves_relations": [], "basis_sources": ["s-205"], "effective": None,
                                         "relation_scope": None, "pending_reason": None})
        s["qualifier_marks"]["candidate:1"] = {"times": ["去年"]}
        return s

    def test_qualifiers_bound_to_each_assertion(self):
        self.assertEqual(check_samples.check(self._two_assertions_one_sentence()), [])

    def test_borrowed_qualifier_is_rejected(self):
        s = self._two_assertions_one_sentence()
        s["qualifier_marks"]["candidate:0"] = {"times": ["上个月", "去年"]}
        self.assertTrue(check_samples.check(s))

    def test_marked_sentences_allow_any_punctuation_and_spacing(self):
        for text, sents, first in [
            ("I work at ByteDance. I hike on weekends.", ["I work at ByteDance.", "I hike on weekends."],
             "I work at ByteDance."),
            ("我在ByteDance工作\n周末会去爬山", ["我在ByteDance工作", "周末会去爬山"], "我在ByteDance工作"),
        ]:
            s = copy.deepcopy(self.samples["w4_09_new"])
            s["sources"]["s-310"].update(text=text, sentences=sents)
            c = s["input"]["candidates"][0]["candidate"]
            c["sentence"] = {"text": first}
            c["value"] = {"kind": "text", "text": "ByteDance", "quote": {"text": "ByteDance"}}
            with self.subTest(text=text):
                self.assertEqual(check_samples.check(s), [])
            c["sentence"] = {"text": first[:-3]}
            with self.subTest(text=text, fragment=True):
                self.assertTrue(check_samples.check(s))

    def test_untouched_old_record_keeps_its_state(self):
        # Coexist leaves record 31 as it was, including a judged update state.
        s = copy.deepcopy(self.samples["w4_04_coexist_partial"])
        s["expect"]["after"]["records"]["31"]["update"] = {"status": "judged", "reasons": []}
        self.assertEqual(check_samples.check(s), [])


if __name__ == "__main__":
    unittest.main()
