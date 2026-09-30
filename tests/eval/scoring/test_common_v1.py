"""Checks for the four-question common.v1 trial (review checklist 1-3).

    python3 -m unittest discover -s tests/eval/scoring -v
"""
import copy
import inspect
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import adapters  # noqa: E402
import ledger as ledger_mod  # noqa: E402
import trial  # noqa: E402
from scoring import FAIL, MISSING, PASS, REVIEW, score  # noqa: E402

EMPTY = {"version": 1, "entries": {}}


def _result(side, case_id, qid, *, raw=None, source_state=None, ledger=EMPTY, mutate_q=None, mutate_expect=None):
    if side == "old":
        case, q, meta, expect, view = trial.run_old(case_id, qid, raw=raw, source_state=source_state)
    else:
        fx = raw if raw is not None else trial.new_fixture(case_id, qid)
        case, q, meta, expect, view = trial.run_new(case_id, qid, fx, source_state=source_state)
    q, meta, expect = copy.deepcopy(q), copy.deepcopy(meta), copy.deepcopy(expect)
    if mutate_q:
        mutate_q(q, meta)
    if mutate_expect:
        mutate_expect(expect)
    return trial.evaluate(side, case, q, meta, expect, view, ledger)


def _key(res, item_id):
    return next(i for i in res["items"] + res["extras"] if i["id"] == item_id)["key"]


class AdapterBoundary(unittest.TestCase):
    def test_adapters_take_no_labels(self):
        for fn in (adapters.adapt_old, adapters.adapt_recall):
            params = inspect.signature(fn).parameters
            self.assertFalse({p for p in params if "expect" in p}, fn.__name__)
        case, q, _, _ = trial.resolve_question("person_sketch_pen", "person_q4")
        qi = adapters.question_input(case, q)
        self.assertNotIn("expect", qi)
        self.assertNotIn("expect_common", qi)

    def test_view_independent_of_annotation(self):
        case, q, meta, expect, v1 = trial.run_old("lme_59524333", "q")
        poisoned = dict(q, expect={"answer": ["7:00 pm"]}, expect_common={"status": "conflict"})
        base, real = trial.old_raw("lme_59524333", "q")
        store = adapters.old_store_context(base, "q")
        v2 = adapters.adapt_old(adapters.question_input(case, poisoned), meta, real, base["source_ids"], store,
                                source_texts=trial.source_texts(case, q))
        self.assertEqual(v1, v2)


class Checklist1_ErrorsAreFound(unittest.TestCase):
    """Every counterexample category ends in auto_fail or review, never a pass."""
    REQUIRED = {
        "错误主体": ["ce_old_subject", "ce_new_subject"],
        "错误来源": ["ce_old_source", "ce_new_source"],
        "条件扩大": ["ce_old_condition", "ce_new_condition"],
        "错误当前结论": ["ce_old_current", "ce_new_current", "ce_old_conflict_answered", "ce_new_conflict_answered"],
        "正确值出现在否定句中": ["ce_old_negation", "ce_new_negation"],
        "正确事实旁附带错误事实": ["ce_old_extra", "ce_new_extra_forbidden", "ce_new_extra_unlabeled"],
        "旧版弃答而新版给错": ["ce_old_abstain", "ce_new_after_abstain"],
        "计算端点": ["ce_new_elapsed_endpoint", "ce_new_elapsed_source", "ce_new_elapsed_no_evidence",
                     "ce_new_elapsed_validity", "ce_new_elapsed_bad_quote"],
        "旧答案推定边界": ["ce_old_question_mark", "ce_old_wrong_binding", "ce_old_wrong_version",
                          "ce_old_unmapped_source"],
        "新版绑定": ["ce_new_wrong_binding", "ce_new_wrong_version"],
        "历史中的额外结论": ["ce_new_history_extra"],
        "来源错误不推断替换": ["ce_new_source_newvalue"],
        "额外冲突成员": ["ce_new_extra_member"],
        "冲突成员来源失效": ["ce_new_member_deleted"],
        "引用片段核验": ["ce_old_fake_quote", "ce_new_fake_quote"],
        "省略号": ["ce_old_ellipsis"],
        "可信提问时刻": ["ce_new_request_time_bad", "ce_new_fake_request_quote", "ce_new_request_operand_position"],
        "计算条目 ID": ["ce_new_duplicate_item_id", "ce_new_forged_request_item"],
        "取整口径（r4.v3）": ["ce_new_weeks_claims_exact", "ce_new_weeks_wrong_basis", "ce_new_days_off_by_one",
                             "ce_new_month_no_basis", "ce_new_month_precision"],
    }

    @classmethod
    def setUpClass(cls):
        cls.ces = {c["id"]: c for c in trial.load_counterexamples()}
        # Real ledger: proposed entries must not turn anything into a pass either.
        cls.ledger = ledger_mod.load_ledger(trial.LEDGER)

    def test_all_categories_present(self):
        for cat, ids in self.REQUIRED.items():
            for cid in ids:
                self.assertIn(cid, self.ces, cat)

    def test_targets_and_no_pass(self):
        self.assertEqual(len(self.ces), 48)
        for cid, ce in self.ces.items():
            res = trial.run_counterexample(ce, self.ledger)
            by_id = {i["id"]: i for i in res["items"] + res["extras"]}
            for t in ce["targets"]:
                with self.subTest(ce=cid, item=t["item"]):
                    self.assertIn(by_id[t["item"]]["result"], t["results"])
            if cid.endswith("_control"):
                continue
            with self.subTest(ce=cid):
                self.assertFalse(res["question_final_pass"], "反例不能最终通过")
                self.assertFalse(res["question_auto_only_pass"])
                bad = [i for i in res["items"] + res["extras"] if i["result"] in (FAIL, REVIEW, MISSING)]
                self.assertTrue(bad, "反例必须至少有一项自动判错、待复核或缺失")

    def test_blockers_on_new_side(self):
        # (id, blockers decided automatically, blocker-kind items awaiting review)
        for cid, hit, pending in [("ce_new_condition", {"条件扩大"}, set()),
                                  ("ce_new_current", set(), {"错误替换"}),
                                  ("ce_new_conflict_answered", set(), {"错误替换"}),
                                  ("ce_new_source_newvalue", set(), {"错误替换"})]:
            res = trial.run_counterexample(self.ces[cid], EMPTY)
            self.assertEqual({b["type"] for b in res["blockers"]}, hit, cid)
            self.assertEqual({b["type"] for b in res["blockers_pending"]}, pending, cid)

    def test_approvals_of_correct_outputs_do_not_carry_over(self):
        """Confirm every review item of the correct outputs; no counterexample
        may pass on those verdicts (stale approvals must not apply)."""
        base = trial.run_trial(EMPTY)
        led = {"version": 1, "entries": {
            i["key"]: {"verdict": "pass", "status": "confirmed", "reviewer": "test"}
            for qr in base["questions"] for side in ("old", "new")
            for i in qr[side]["items"] + qr[side]["extras"] if i.get("key")}}
        for cid, ce in self.ces.items():
            res = trial.run_counterexample(ce, led)
            with self.subTest(ce=cid):
                self.assertFalse(res["question_final_pass"])

    def test_injected_compute_items_change_view_and_key(self):
        """Reproduces the review finding: an item inserted before the real one
        (same id, or the reserved @request.at id) must not be swallowed."""
        fx = trial.new_fixture("lme_5e1b23de", "q")
        base = _result("new", "lme_5e1b23de", "q")
        led = {"version": 1, "entries": {i["key"]: {"verdict": "pass", "status": "confirmed", "reviewer": "test"}
                                         for i in base["items"] + base["extras"] if i.get("key")}}
        self.assertTrue(_result("new", "lme_5e1b23de", "q", ledger=led)["question_final_pass"])
        for cid in ("ce_new_duplicate_item_id", "ce_new_forged_request_item"):
            res = trial.run_counterexample(self.ces[cid], led)
            with self.subTest(ce=cid):
                self.assertFalse(res["question_final_pass"])
                ep = next(i for i in res["items"] if i["id"] == "computed.endpoints")
                self.assertEqual(ep["result"], FAIL)
                self.assertIn("计算结构非法", ep["basis"])

    def test_abstain_then_wrong_is_regression(self):
        report = trial.run_trial(EMPTY)
        pair = report["pairs"][0]
        self.assertTrue(any("新增语义错误" in r["note"] for r in pair["regressions"]))


class Checklist2_ReviewIsNotPass(unittest.TestCase):
    def test_pending_review_not_counted(self):
        report = trial.run_trial(EMPTY)
        for qr in report["questions"]:
            for side in ("old", "new"):
                r = qr[side]
                self.assertEqual(r["final_pass"], r["auto_pass"])
                pending = [i for i in r["items"] if i["result"] == REVIEW]
                if pending or r["extras"]:
                    self.assertFalse(r["question_final_pass"], qr["question"])

    def test_confirmed_counts_proposed_does_not(self):
        base = trial.run_trial(EMPTY)
        keys = [(i["key"], i) for qr in base["questions"] for s in ("old", "new")
                for i in qr[s]["items"] + qr[s]["extras"] if i["final"] == "pending_review"]
        self.assertTrue(keys)
        proposed = {"version": 1, "entries": {k: {"verdict": "pass", "status": "proposed"} for k, _ in keys}}
        confirmed = {"version": 1, "entries": {k: {"verdict": "pass", "status": "confirmed", "reviewer": "test"}
                                               for k, _ in keys}}
        rejected = {"version": 1, "entries": {k: {"verdict": "fail", "status": "confirmed", "reviewer": "test"}
                                              for k, _ in keys}}
        p, c, x = trial.run_trial(proposed), trial.run_trial(confirmed), trial.run_trial(rejected)
        for b, pq, cq, xq in zip(base["questions"], p["questions"], c["questions"], x["questions"]):
            for side in ("old", "new"):
                d = b[side]["denominator"]
                self.assertEqual(d, pq[side]["denominator"])
                self.assertEqual(d, cq[side]["denominator"])
                self.assertEqual(pq[side]["final_pass"], b[side]["final_pass"])
                n_review = sum(1 for i in b[side]["items"] if i["result"] == REVIEW)
                self.assertEqual(cq[side]["final_pass"], b[side]["final_pass"] + n_review)
                self.assertEqual(xq[side]["final_pass"], b[side]["final_pass"])
                # all review confirmed pass: question passes iff nothing failed or went missing
                clean = all(i["result"] in (PASS, REVIEW) for i in b[side]["items"])
                self.assertEqual(cq[side]["question_final_pass"], clean, b["question"] + side)

    def test_same_items_both_sides(self):
        for qr in trial.run_trial(EMPTY)["questions"]:
            self.assertEqual([i["id"] for i in qr["old"]["items"]], [i["id"] for i in qr["new"]["items"]])


class Checklist3_CacheInvalidation(unittest.TestCase):
    """Each component change invalidates a verdict; revision/duration do not."""

    NEW = ("lme_59524333", "q", "facts[0].semantic")
    OLD = ("lme_5e1b23de", "q", "computed.value")

    def new_key(self, raw=None, **kw):
        fx = copy.deepcopy(trial.new_fixture(*self.NEW[:2]))
        if raw:
            raw(fx)
        return _key(_result("new", *self.NEW[:2], raw=fx, **kw), self.NEW[2])

    def old_key(self, raw=None, **kw):
        base, real = trial.old_raw(*self.OLD[:2])
        real = copy.deepcopy(real)
        if raw:
            raw(real)
        return _key(_result("old", *self.OLD[:2], raw=real, **kw), self.OLD[2])

    def test_changes_invalidate(self):
        k0, o0 = self.new_key(), self.old_key()

        def add_conflict(fx):
            r = fx["recall"]
            other = copy.deepcopy(r["records"][1])
            other["id"] = 30
            other["open_relations"] = ["rel-9"]
            r["records"][1]["open_relations"] = ["rel-9"]
            r["records"].append(other)
            r["conflicts"].append({"id": "rel-9", "kind": "conflict", "members": [21, 30], "conditions": [],
                                   "times": [], "basis": [], "status": "open", "resolution": None})

        def add_extra(fx):
            f = copy.deepcopy(fx["recall"]["facts"][0])
            f["statement"] = "每周一、三、五去健身房"
            fx["recall"]["facts"].append(f)

        def pending_raw(fx):
            fx["recall"]["raw"].append({"source_id": "src-a07", "quote": {"text": "I usually go to the gym"},
                                        "said_on": "2023-05-30", "sequence": 7, "grade": "uncommitted",
                                        "enrichment": "pending", "memory_requests": []})
            fx["recall"]["completeness"]["pending_related_sources"] = 1

        def pending_update(fx):
            fx["recall"]["records"][1]["update"] = {"status": "pending", "reasons": ["update_scope_incomplete"]}

        def changed_statement(fx):
            fx["recall"]["facts"][0]["statement"] = "通常 6 点去健身房"

        def changed_quote(fx):
            fx["recall"]["facts"][0]["evidence"][0]["quote"]["text"] = "usually at 6:00 pm"

        def fact_field(field, value):
            def f(fx):
                fx["recall"]["facts"][0][field] = value
            return f

        def record_condition(fx):
            fx["recall"]["records"][1]["candidate"]["conditions"] = [{"quote": {"text": "on Tuesday"}, "resolved": True}]

        def changed_history(fx):
            fx["recall"]["history"][0]["statement"] = "以前通常晚上 8:00 去健身房"

        cases = {
            "评分版本": lambda: self._with_attr(ledger_mod, "SCORING_VERSION", "common.v2", self.new_key),
            "适配器版本": self._adapter_version,
            "标注 expect_common": lambda: self.new_key(mutate_expect=lambda e: e["facts"][0].update(relation_note="x")),
            "标注 query_meta": lambda: self.new_key(mutate_q=lambda q, m: m.update(time_focus="unspecified")),
            "问题文本": lambda: self.new_key(mutate_q=lambda q, m: q.update(query="What time do I go to the gym?")),
            "上下文": lambda: self.new_key(mutate_q=lambda q, m: q.update(context=["上周说的"])),
            "提问时间": lambda: self.new_key(mutate_q=lambda q, m: q.update(at=q["at"] + 86400)),
            "task_id": lambda: self.new_key(mutate_q=lambda q, m: q.update(task_id="t-1")),
            "被复核输出 statement": lambda: self.new_key(raw=changed_statement),
            "被复核输出引用片段": lambda: self.new_key(raw=changed_quote),
            "额外结论": lambda: self.new_key(raw=add_extra),
            "额外结论：history": lambda: self.new_key(raw=changed_history),
            "事实时间限定": lambda: self.new_key(raw=fact_field("times", [{"role": "validity", "quote": {"text": "usually"},
                                                                       "interp": {"kind": "unknown"}, "anchor": "said_on"}])),
            "事实强度": lambda: self.new_key(raw=fact_field("strength", "strong")),
            "事实不确定性": lambda: self.new_key(raw=fact_field("uncertainty", {"quote": {"text": "usually"}, "resolved": True})),
            "事实更新状态": lambda: self.new_key(raw=fact_field("update", {"status": "pending", "reasons": ["update_scope_incomplete"]})),
            "事实 grade": lambda: self.new_key(raw=fact_field("grade", "uncommitted")),
            "语义状态：记录候选的条件": lambda: self.new_key(raw=record_condition),
            "语义状态：新增未解决冲突": lambda: self.new_key(raw=add_conflict),
            "语义状态：来源删除": lambda: self.new_key(source_state={"s2_t3": {"deleted": True}}),
            "语义状态：待整理来源": lambda: self.new_key(raw=pending_raw),
            "语义状态：更新待判定": lambda: self.new_key(raw=pending_update),
        }
        for name, fn in cases.items():
            with self.subTest(change=name):
                self.assertNotEqual(fn(), k0)

        old_cases = {
            "旧版：被复核答案": lambda: self.old_key(raw=lambda r: r["answer_contract"].update(
                safe_answer="3 months ago（2023-11-02）。")),
            "旧版：反证 historical_or_conflicting": lambda: self.old_key(raw=lambda r: r.update(
                historical_or_conflicting=[{"source_id": "83715-1790753231625448000-319"}])),
            "旧版：完整性 degraded_reasons": lambda: self.old_key(raw=lambda r: r.update(degraded_reasons=[])),
            "旧版：来源失效": lambda: self.old_key(source_state={"s1_t4": {"deleted": True}}),
            "旧版：标注": lambda: self.old_key(mutate_expect=lambda e: e["computed"].update(value="4")),
        }
        for name, fn in old_cases.items():
            with self.subTest(change=name):
                self.assertNotEqual(fn(), o0)

    def test_record_state_change_invalidates_old(self):
        o0 = self.old_key()
        base, _ = trial.old_raw(*self.OLD[:2])
        orig = adapters.old_store_context
        def patched(b, qid):
            # a record linked to the reviewed source s1_t4 appears in the store
            st = copy.deepcopy(orig(b, qid))
            m = copy.deepcopy(next(iter(st["memories"].values())))
            m["id"], m["evidence"] = 99, [{"turn_id": "s1_t4", "quote": "today"}]
            st["memories"]["99"] = m
            return st
        trial.old_store_context = patched
        try:
            self.assertNotEqual(self.old_key(), o0)
        finally:
            trial.old_store_context = orig

    def test_forbidden_and_current_message_keys(self):
        """The forbidden verdict depends on the conclusions it assessed, and the
        current message text is part of the verdict's evidence."""
        def key(mut, item):
            fx = copy.deepcopy(trial.new_fixture(*self.NEW[:2]))
            mut(fx)
            return _key(_result("new", *self.NEW[:2], raw=fx), item)
        same = lambda fx: None
        def seven(fx):
            fx["recall"]["facts"][0]["statement"] = "通常晚上 7:00 去健身房"
        def cm_text(fx):
            fx["recall"]["current_message"]["text"] = "What time do I go to the gym now?"
        f0 = key(same, "forbidden[0]")
        self.assertNotEqual(key(seven, "forbidden[0]"), f0)
        self.assertNotEqual(key(cm_text, "forbidden[0]"), f0)
        self.assertNotEqual(key(cm_text, self.NEW[2]), key(same, self.NEW[2]))
        # and a confirmed approval of the 6 pm answer no longer applies to 7 pm
        res0 = _result("new", *self.NEW[:2])
        led = {"version": 1, "entries": {i["key"]: {"verdict": "pass", "status": "confirmed", "reviewer": "test"}
                                         for i in res0["items"] + res0["extras"] if i.get("key")}}
        fx = copy.deepcopy(trial.new_fixture(*self.NEW[:2]))
        seven(fx)
        res = _result("new", *self.NEW[:2], raw=fx, ledger=led)
        self.assertEqual(next(i for i in res["items"] if i["id"] == "forbidden[0]")["final"], "pending_review")
        self.assertFalse(res["question_final_pass"])

    def test_confirmed_run_fails_after_qualifier_change(self):
        """Reproduces the review finding: confirm the correct output, then change
        a time qualifier or the update state; the old verdict must not apply."""
        fx0 = trial.new_fixture(*self.NEW[:2])
        res0 = _result("new", *self.NEW[:2])
        led = {"version": 1, "entries": {i["key"]: {"verdict": "pass", "status": "confirmed", "reviewer": "test"}
                                         for i in res0["items"] + res0["extras"] if i.get("key")}}
        self.assertTrue(_result("new", *self.NEW[:2], ledger=led)["question_final_pass"])
        for field, value in (("times", [{"role": "validity", "quote": {"text": "usually"},
                                         "interp": {"kind": "unknown"}, "anchor": "said_on"}]),
                             ("update", {"status": "pending", "reasons": ["update_scope_incomplete"]})):
            fx = copy.deepcopy(fx0)
            fx["recall"]["facts"][0][field] = value
            with self.subTest(field=field):
                self.assertFalse(_result("new", *self.NEW[:2], raw=fx, ledger=led)["question_final_pass"])

    def test_revision_and_duration_do_not(self):
        k0, o0 = self.new_key(), self.old_key()

        def rev(fx):
            fx["recall"]["snapshot"] = {"read_revision": 99, "source_revision": 120}
            fx["recall"]["request"]["request_id"] = "op-other"

        def remap(fx):
            m = {old: old + "-x" for old in fx["source_ids"].values()}
            fx["source_ids"] = {k: m[v] for k, v in fx["source_ids"].items()}
            def walk(o):
                if isinstance(o, dict):
                    return {k: (m.get(v, v) if k == "source_id" else walk(v)) for k, v in o.items()}
                if isinstance(o, list):
                    return [walk(x) for x in o]
                return o
            fx["recall"] = walk(fx["recall"])

        self.assertEqual(self.new_key(raw=rev), k0)
        self.assertEqual(self.new_key(raw=remap), k0, "运行时 ID 映射为稳定引用后不应影响键")
        self.assertEqual(self.old_key(raw=lambda r: r.update(duration_ms=12345, read_revision=77, source_revision=88)), o0)

    def test_ledger_entry_stops_applying(self):
        k0 = self.new_key()
        led = {"version": 1, "entries": {k0: {"verdict": "pass", "status": "confirmed", "reviewer": "test"}}}
        ok = _result("new", *self.NEW[:2], ledger=led)
        item = next(i for i in ok["items"] if i["id"] == self.NEW[2])
        self.assertEqual(item["final"], "pass")
        fx = copy.deepcopy(trial.new_fixture(*self.NEW[:2]))
        fx["recall"]["completeness"]["pending_updates"] = 1
        stale = _result("new", *self.NEW[:2], raw=fx, ledger=led)
        item = next(i for i in stale["items"] if i["id"] == self.NEW[2])
        self.assertEqual(item["final"], "pending_review")

    # helpers
    def _with_attr(self, mod, name, value, fn):
        old = getattr(mod, name)
        setattr(mod, name, value)
        try:
            return fn()
        finally:
            setattr(mod, name, old)

    def _adapter_version(self):
        old = adapters.NEW_ADAPTER
        adapters.NEW_ADAPTER = "recall_v2@2"
        try:
            return self.new_key()
        finally:
            adapters.NEW_ADAPTER = old


class ElapsedRule(unittest.TestCase):
    """r4.v3 rounding rule, checked against the seven dated questions of the
    63 (endpoints and LongMemEval answers) and edge cases."""

    def test_baseline_questions(self):
        from scoring import elapsed_rule
        d = __import__("datetime").date
        cases = [  # (case, start, end, unit, expected value, approximate)
            ("lme_5e1b23de", d(2023, 11, 1), d(2024, 2, 1), "month", "3", False),
            ("lme_gpt4_6dc9b45b", d(2021, 6, 1), d(2021, 10, 2), "month", "4", False),
            ("lme_71017276", d(2023, 3, 4), d(2023, 4, 1), "week", "4", False),
            ("lme_af082822", d(2022, 11, 17), d(2022, 12, 1), "week", "2", False),
            ("lme_bcbe585f", d(2023, 4, 1), d(2023, 5, 1), "week", "4", True),
            ("lme_gpt4_e072b769", d(2023, 4, 16), d(2023, 5, 6), "week", "3", True),  # Ibotta: 20 天约 3 周
            ("lme_gpt4_b5700ca9", d(2023, 4, 6), d(2023, 4, 10), "day", "4", False),
        ]
        for case, a, b, unit, value, approx in cases:
            with self.subTest(case=case):
                v, rounding, days, ap = elapsed_rule(a, b, unit)
                self.assertEqual((v, ap), (value, approx))
                self.assertEqual(days, (b - a).days)

    def test_edges(self):
        from scoring import elapsed_rule
        d = __import__("datetime").date
        self.assertEqual(elapsed_rule(d(2023, 1, 1), d(2023, 1, 18), "week")[0], "2")   # 17 天
        self.assertEqual(elapsed_rule(d(2023, 1, 1), d(2023, 1, 19), "week")[0], "3")   # 18 天
        self.assertEqual(elapsed_rule(d(2023, 1, 31), d(2023, 2, 28), "month")[0], "0")  # 未满一个月
        self.assertEqual(elapsed_rule(d(2020, 2, 29), d(2021, 2, 28), "year")[0], "0")
        self.assertIsNone(elapsed_rule(d(2023, 2, 1), d(2023, 1, 1), "day"))


def _na(aid, text, sources, current="yes", polarity="affirm"):
    """A recall.v3-style adapted assertion (free-text statement)."""
    return {"id": aid, "text": text, "value": None, "sources": sorted(sources), "unmapped_sources": [],
            "quotes": [], "polarity": polarity, "current": current, "conditions": [], "uncertainty": None,
            "plain": False}


def _nview(assertions, status="answer", history=()):
    return {"adapter": "test", "status": status, "status_raw": status, "binding": [], "assertions": assertions,
            "conflicts": [], "computed": None, "history": list(history)}


def _items(expect, meta, view, texts=None):
    res = score(expect, meta, view, texts)
    return {i["id"]: i for i in res["items"]}, res["extras"]


class ForbiddenKinds(unittest.TestCase):
    """wrong_value / wrong_source forbid a value or source AS THE ANSWER;
    correct extra context is reviewed, not failed. source_out_of_scope is
    global."""

    def test_extra_context_from_other_source_is_not_an_error(self):
        # time_q2: "4月15日19:00；此前4月1日至14日为17:30" must not be auto-failed.
        _, _, meta, exp = trial.resolve_question("absolute_date_ranges", "time_q2")
        view = _nview([_na("facts[0]", "4月15日每天19:00闭馆", ["time_s2"]),
                       _na("facts[1]", "此前4月1日至14日为17:30闭馆", ["time_s1"], current="no")])
        items, extras = _items(exp, meta, view)
        self.assertFalse([i for i in items.values() if i["result"] == FAIL], items)
        self.assertEqual([e["result"] for e in extras], [REVIEW])

    def test_wrong_source_as_answer_fails(self):
        _, _, meta, exp = trial.resolve_question("explicit_correction", "correction_q1")
        view = _nview([_na("facts[0]", "存储卡是64GB", ["correction_s2"])])
        items, _ = _items(exp, meta, view)
        ws = next(i for i in items.values() if i.get("kind") == "wrong_source")
        self.assertEqual(ws["result"], FAIL)
        self.assertEqual(items["facts[0].sources"]["result"], FAIL)

    def test_wrong_value_only_as_the_answer(self):
        case_id, qid = "lme_af8d2e46", "q"
        _, _, meta, exp = trial.resolve_question(case_id, qid)
        k = next(i for i, f in enumerate(exp["forbidden"]) if f["kind"] == "wrong_value")
        base, real = trial.old_raw(case_id, qid)

        def old(answer):
            raw = copy.deepcopy(real)
            raw["answer_contract"]["safe_answer"] = answer
            return _result("old", case_id, qid, raw=raw)

        def fb(res):
            return next(i for i in res["items"] if i["id"] == f"forbidden[{k}]")["result"]

        self.assertEqual(fb(old("3 shirts。")), FAIL)       # distractor given as the answer
        self.assertEqual(fb(old("7 shirts。")), PASS)
        self.assertEqual(fb(old("7 shirts, but I only wore 3。")), REVIEW)  # correct supplement: review, not fail
        view = _nview([_na("facts[0]", "带了 7 件衬衫", ["s1_t4"]),
                       _na("facts[1]", "实际只穿了 3 件", ["s1_t4"], current="no")])
        items, _ = _items(exp, meta, view)
        self.assertNotEqual(items[f"forbidden[{k}]"]["result"], FAIL)

    def test_wrong_value_on_insufficient_question(self):
        case_id, qid = "lme_0862e8bf_abs", "q"
        real_res = _result("old", case_id, qid)  # real old output: insufficient
        self.assertEqual(next(i for i in real_res["items"] if i.get("kind") == "wrong_value")["result"], PASS)
        base, real = trial.old_raw(case_id, qid)
        raw = copy.deepcopy(real)
        ac = raw["answer_contract"]
        ac.update(status="source_linked_model_answer", safe_answer="Luna。",
                  safe_source_ids=[base["source_ids"]["s1_t6"]])
        case, q, _, _ = trial.resolve_question(case_id, qid)
        ac["sources"] = [{"grade": "uncommitted", "source_id": base["source_ids"]["s1_t6"],
                          "quote": trial.source_texts(case, q)["s1_t6"]}]
        res = _result("old", case_id, qid, raw=raw)
        by = {i["id"]: i for i in res["items"]}
        self.assertEqual(by["status"]["result"], FAIL)
        self.assertEqual(next(i for i in res["items"] if i.get("kind") == "wrong_value")["result"], FAIL)

    def test_out_of_scope_source_is_global(self):
        _, _, meta, exp = trial.resolve_question("task_isolation", "task_q1")
        view = _nview([_na("facts[0]", "开机显示西班牙语", ["task_s1"])],
                      history=[{"id": "history[0]", "text": "高原路演任务设为意大利语", "until": None,
                                "sources": ["task_s2"], "unmapped_sources": [], "quotes": []}])
        items, _ = _items(exp, meta, view)
        oos = next(i for i in items.values() if i.get("kind") == "source_out_of_scope")
        self.assertEqual(oos["result"], FAIL)


class FullBaseline(unittest.TestCase):
    """All 63 questions: every one annotated and reviewed; the ported
    original scoring reproduces the strict replay (recall 42/42, refusal
    21/21, phase1-baseline.md section 4); the old side has no blockers."""

    def test_full_baseline(self):
        import baseline
        rows = baseline.run(EMPTY)
        self.assertEqual(len(rows), 63)
        self.assertTrue(all(r["label_status"] == "reviewed" for r in rows))
        s = baseline.summarize(rows)
        self.assertEqual(s["original_recall"], {"pass": 42, "total": 42})
        self.assertEqual(s["original_refusal"], {"pass": 21, "total": 21})
        self.assertEqual(s["common"]["blockers"], 0)
        self.assertEqual(s["common"]["final_pass"], s["common"]["auto_pass"])  # empty ledger: reviews don't count


class TrialResults(unittest.TestCase):
    def test_ledger_covers_every_pending_item(self):
        led = ledger_mod.load_ledger(trial.LEDGER)
        for qr in trial.run_trial(led)["questions"]:
            for side in ("old", "new"):
                for i in qr[side]["items"] + qr[side]["extras"]:
                    if i["result"] == REVIEW:
                        self.assertTrue(i["key"] in led["entries"], f"{qr['question']} {side} {i['id']} 台账键已失效")

    def test_new_fixture_has_no_blockers_and_no_auto_fail(self):
        for qr in trial.run_trial(EMPTY)["questions"]:
            self.assertEqual(qr["new"]["blockers"], [], qr["question"])
            self.assertFalse([i for i in qr["new"]["items"] if i["result"] in (FAIL, MISSING)], qr["question"])


if __name__ == "__main__":
    unittest.main()
