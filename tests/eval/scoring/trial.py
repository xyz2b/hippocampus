"""Four-question common.v1 trial (common-scoring-view.md 8, steps 1-2).

Offline: reads case annotations, the archived 0517000 baseline, hand-written
recall.v2 fixtures, counterexamples and the review ledger; writes
reports/trial-4q.json and reports/trial-4q.md. No model calls.

    python3 tests/eval/scoring/trial.py
"""
from __future__ import annotations

import copy
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from adapters import adapt_old, adapt_recall, old_store_context, question_input  # noqa: E402
from ledger import finalize, load_ledger  # noqa: E402
from scoring import FAIL, MISSING, NOT_MEASURABLE, PASS, REVIEW, blockers, score  # noqa: E402

EVAL = HERE.parent
CASES, BASELINE = EVAL / "cases", EVAL / "baseline" / "0517000"
FIXTURES = HERE / "fixtures"
LEDGER = HERE / "review_ledger.json"
TRIAL = [("lme_59524333", "q"), ("lme_5e1b23de", "q"),
         ("unresolved_conflict", "conflict_q2"), ("person_sketch_pen", "person_q4")]


def _load(p):
    with open(p, encoding="utf-8") as fh:
        return json.load(fh)


_case_cache = {}


def load_case(case_id):
    if not _case_cache:
        for p in CASES.glob("*.json"):
            c = _load(p)
            _case_cache[c["id"]] = c
    return _case_cache[case_id]


def resolve_question(case_id, qid):
    """Returns (case, question, query_meta, expect_common). qid may name a probe."""
    if "~" in qid:
        probe = _load(FIXTURES / "probes.json")[qid]
        case = load_case(probe["base"]["case"])
        base = next(q for q in case["questions"] if q["id"] == probe["base"]["question"])
        q = dict(base, id=qid, query=probe["query"])
        return case, q, probe["query_meta"], probe["expect_common"]
    case = load_case(case_id)
    q = next(q for q in case["questions"] if q["id"] == qid)
    return case, q, q["query_meta"], q["expect_common"]


def source_texts(case, question):
    return {m["id"]: m["text"] for m in case["messages"]}


def baseline_for(case_id):
    return _load(BASELINE / f"{case_id}.json")


def old_raw(case_id, qid):
    base = baseline_for(case_id)
    real_q = qid.split("~")[0]
    step = next(s for s in base["steps"] if s["step"] == "question" and s["question"] == real_q)
    return base, step["result"]


def run_old(case_id, qid, raw=None, source_state=None):
    case, q, meta, expect = resolve_question(case_id, qid)
    base, real = old_raw(case_id, qid)
    store = old_store_context(base, qid.split("~")[0])
    view = adapt_old(question_input(case, q), meta, raw if raw is not None else real,
                     base["source_ids"], store, source_texts=source_texts(case, q), source_state=source_state)
    return case, q, meta, expect, view


def run_new(case_id, qid, fixture, source_state=None):
    case, q, meta, expect = resolve_question(case_id, qid)
    view = adapt_recall(question_input(case, q), meta, fixture["recall"], fixture["source_ids"],
                        source_texts=source_texts(case, q), source_state=source_state)
    return case, q, meta, expect, view


def evaluate(side, case, q, meta, expect, view, ledger):
    texts = source_texts(case, q)
    scored = score(expect, meta, view, texts)
    fin = finalize(scored=scored, view=view, question=question_input(case, q), query_meta=meta,
                   expect=expect, source_texts=texts, ledger=ledger, side=side)
    b = blockers(fin["items"])
    fin["blockers"], fin["blockers_pending"] = b["hit"], b["pending"]
    fin["status"] = view["status"]
    fin["status_raw"] = view["status_raw"]
    return fin


def new_fixture(case_id, qid):
    return _load(FIXTURES / "recall_v2" / f"{case_id}.{qid}.json")


def run_counterexample(ce, ledger):
    case_id, qid = ce["question"]["case"], ce["question"]["id"]
    if ce["side"] == "old":
        case, q, meta, expect, view = run_old(case_id, qid, raw=ce["output"])
    else:
        case, q, meta, expect, view = run_new(case_id, qid, {"recall": ce["output"], "source_ids": ce["source_ids"]},
                                              source_state=ce.get("source_state"))
    return evaluate(ce["side"], case, q, meta, expect, view, ledger)


def load_counterexamples():
    return [_load(p) for p in sorted((FIXTURES / "counterexamples").glob("ce_*.json"))]


def old_abstained(old):
    return old["status"] in ("insufficient", "clarify")


def transitions(old, new, expect_status):
    """Per-item old -> new transitions and regressions (common-scoring-view.md 5)."""
    rows, regressions, incomplete = [], [], []
    new_items = {i["id"]: i for i in new["items"]}
    abstain = old_abstained(old) and expect_status not in ("insufficient", "clarify")
    for oi in old["items"]:
        ni = new_items[oi["id"]]
        if abstain:
            ocls = "abstain"
        elif oi["final"] == "pass":
            ocls = "pass"
        elif oi["result"] == FAIL or oi["final"] == "fail":
            ocls = "fail"
        elif oi["result"] == MISSING:
            ocls = "missing" + (f"({oi['flag']})" if oi.get("flag") else "")
        else:
            ocls = oi["final"]
        ncls = ni["final"]
        note = None
        if "pending_review" in (ocls, ncls):
            note = "比较未完成：待复核"
            incomplete.append({"item": oi["id"], "old": ocls, "new": ncls})
        elif ocls == "pass" and ncls != "pass":
            note = "退步：旧版最终通过，新版未通过"
        elif ocls == "abstain" and ncls == "fail":
            note = "退步：新增语义错误（旧版弃答，新版给错）"
        elif ocls.startswith("missing"):
            note = "新增正确覆盖" if ncls == "pass" else "新增错误" if ncls in ("fail", "missing") else "新版：" + ncls
        rows.append({"item": oi["id"], "old": ocls, "new": ncls, "note": note})
        if note and note.startswith("退步"):
            regressions.append({"item": oi["id"], "note": note})
    # Extra conclusions are not paired items but gate the new side's final
    # pass; unresolved ones leave the comparison incomplete as well.
    for e in new["extras"]:
        if e["final"] == "pending_review":
            incomplete.append({"item": e["id"], "old": None, "new": "pending_review"})
        elif e["final"] == "fail":
            regressions.append({"item": e["id"], "note": "新版额外结论复核未通过"})
    return rows, regressions, incomplete


def _summ(res):
    return {"denominator": res["denominator"], "auto_pass": res["auto_pass"], "final_pass": res["final_pass"],
            "question_auto_only_pass": res["question_auto_only_pass"],
            "question_final_pass": res["question_final_pass"],
            "counts": {k: sum(1 for i in res["items"] if i["result"] == k)
                       for k in (PASS, FAIL, REVIEW, MISSING, NOT_MEASURABLE)},
            "extras": len(res["extras"]), "blockers": res["blockers"],
            "blockers_pending": res["blockers_pending"]}


def run_trial(ledger=None):
    ledger = ledger if ledger is not None else load_ledger(LEDGER)
    questions = []
    for case_id, qid in TRIAL:
        case, q, meta, expect, oview = run_old(case_id, qid)
        old = evaluate("old", case, q, meta, expect, oview, ledger)
        _, _, _, _, nview = run_new(case_id, qid, new_fixture(case_id, qid))
        new = evaluate("new", case, q, meta, expect, nview, ledger)
        assert [i["id"] for i in old["items"]] == [i["id"] for i in new["items"]], "判分项必须一致"
        rows, regs, inc = transitions(old, new, expect["status"])
        questions.append({"question": f"{case_id}/{qid}", "query": q["query"], "query_meta": meta,
                          "expect_common": expect, "old_view": oview, "new_view": nview,
                          "old": old, "new": new, "transitions": rows, "regressions": regs,
                          "incomplete": inc})
    ces = []
    for ce in load_counterexamples():
        res = run_counterexample(ce, ledger)
        by_id = {i["id"]: i for i in res["items"] + res["extras"]}
        checks = []
        for t in ce["targets"]:
            got = by_id.get(t["item"], {}).get("result")
            checks.append({"item": t["item"], "want": t["results"], "got": got, "ok": got in t["results"]})
        ces.append({"id": ce["id"], "side": ce["side"], "probe": ce["probe"],
                    "question": f"{ce['question']['case']}/{ce['question']['id']}",
                    "checks": checks, "question_final_pass": res["question_final_pass"],
                    "blockers": res["blockers"], "result": res})
    pairs = []
    pair_path = FIXTURES / "counterexamples" / "pairs.json"
    if pair_path.exists():
        by = {c["id"]: c for c in ces}
        for p in _load(pair_path):
            o, n = by[p["old"]]["result"], by[p["new"]]["result"]
            _, _, _, expect = resolve_question(*p["question"].split("/"))
            rows, regs, inc = transitions(o, n, expect["status"])
            pairs.append({**p, "regressions": regs, "incomplete": inc, "transitions": rows})
    return {"scoring_version": "common.v1", "questions": questions, "counterexamples": ces, "pairs": pairs}


def summary(report):
    out = {"questions": []}
    for qr in report["questions"]:
        out["questions"].append({"question": qr["question"], "old": _summ(qr["old"]), "new": _summ(qr["new"]),
                                 "regressions": qr["regressions"], "incomplete": qr["incomplete"]})
    tot = {}
    for side in ("old", "new"):
        rs = [qr[side] for qr in report["questions"]]
        tot[side] = {"denominator": sum(r["denominator"] for r in rs),
                     "auto_pass": sum(r["auto_pass"] for r in rs),
                     "final_pass": sum(r["final_pass"] for r in rs),
                     "questions_auto_only_pass": sum(r["question_auto_only_pass"] for r in rs),
                     "questions_final_pass": sum(r["question_final_pass"] for r in rs),
                     "pending_review": sum(1 for r in rs for i in r["items"] + r["extras"] if i["final"] == "pending_review"),
                     "blockers": sum(len(r["blockers"]) for r in rs),
                     "blockers_pending": sum(len(r["blockers_pending"]) for r in rs)}
    regs = sum(len(qr["regressions"]) for qr in report["questions"])
    inc = sum(len(qr["incomplete"]) for qr in report["questions"])
    tot["comparison"] = {
        "regressions": regs, "incomplete": inc,
        "blockers_new": tot["new"]["blockers"], "blockers_new_pending": tot["new"]["blockers_pending"],
        "verdict": ("有退步或阻断项" if regs or tot["new"]["blockers"]
                    else "比较未完成，不能判定不退步" if inc or tot["new"]["blockers_pending"]
                    else "无退步、新版阻断项为零（新版为手写正确夹具，只说明判分流程成立，不代表新实现的效果）"),
    }
    out["totals"] = tot
    out["counterexamples"] = {"total": len(report["counterexamples"]),
                              "targets_ok": all(c["ok"] for ce in report["counterexamples"] for c in ce["checks"]),
                              "any_final_pass": any(ce["question_final_pass"] for ce in report["counterexamples"])}
    return out


RES_CN = {PASS: "自动通过", FAIL: "自动判错", REVIEW: "待复核", MISSING: "缺失", NOT_MEASURABLE: "不可测"}
FIN_CN = {"pass": "通过", "fail": "未通过", "missing": "缺失", "pending_review": "待复核", "not_measurable": "不可测"}


def _cell(i):
    s = RES_CN[i["result"]]
    if i.get("flag"):
        s += f" `{i['flag']}`"
    if i["result"] == REVIEW:
        rv = i.get("review")
        s += f" → {FIN_CN[i['final']]}" + (f"（台账 {rv['status']}: {rv['verdict']}）" if rv else "")
    return s


def render_md(report):
    s = summary(report)
    L = ["# 共同判分视图 common.v1 四题试跑报告", "",
         "自动部分只覆盖结构项；语义正确性（主体、关系、值的表述）依赖人工复核。"
         "“最终通过”只计自动通过项和台账中 **confirmed** 的复核通过项；proposed（草拟）不计。", "",
         "新版一侧为手写 recall.v2 夹具（按 contracts.md 3.4 表达的正确输出），不是新实现的运行结果。"
         "lme_5e1b23de 的计算按已冻结的 r4.v2 / recall.v2 使用保留操作数 `@request.at`：提问时刻由代码按可信 request.at 与用户时区提供（computed.request_time）。", "",
         "## 汇总", "",
         "| 版本 | 判分项（分母） | 仅自动通过 | 含复核通过 | 题目仅自动通过 | 题目最终通过 | 待复核 | 阻断项（已定 / 待复核） |",
         "|---|---|---|---|---|---|---|---|"]
    for side, name in (("old", "旧基线 0517000"), ("new", "recall.v2 夹具")):
        t = s["totals"][side]
        L.append(f"| {name} | {t['denominator']} | {t['auto_pass']} | {t['final_pass']} | {t['questions_auto_only_pass']}/4 | "
                 f"{t['questions_final_pass']}/4 | {t['pending_review']} | {t['blockers']} / {t['blockers_pending']} |")
    cmp_ = s["totals"]["comparison"]
    L += ["", f"**新旧比较结论：{cmp_['verdict']}**（退步 {cmp_['regressions']} 项，未完成比较 {cmp_['incomplete']} 项，"
              f"新版阻断项 {cmp_['blockers_new']} 个、待复核的阻断类禁止项 {cmp_['blockers_new_pending']} 个）。"]
    for qr in report["questions"]:
        L += ["", f"## {qr['question']}：{qr['query']}", "",
              f"query_meta：`{json.dumps(qr['query_meta'], ensure_ascii=False)}`", "",
              f"- 旧版：status `{qr['old']['status_raw']}` → {qr['old']['status']}；"
              f"答案 `{(qr['old_view']['assertions'][0]['text'] if qr['old_view']['assertions'] else '（无断言）')}`",
              f"- 新版：status `{qr['new']['status_raw']}` → {qr['new']['status']}；facts {len(qr['new_view']['assertions'])} 条，"
              f"conflicts {len(qr['new_view']['conflicts'])} 个，computed {'有' if qr['new_view']['computed'] else '无'}", "",
              "| 判分项 | 旧版 | 新版 | 转移 |", "|---|---|---|---|"]
        new_items = {i["id"]: i for i in qr["new"]["items"]}
        trans = {r["item"]: r for r in qr["transitions"]}
        for oi in qr["old"]["items"]:
            ni = new_items[oi["id"]]
            t = trans[oi["id"]]
            L.append(f"| `{oi['id']}` | {_cell(oi)} | {_cell(ni)} | {t['old']} → {t['new']}{'；' + t['note'] if t['note'] else ''} |")
        for side in ("old", "new"):
            if qr[side]["extras"]:
                for e in qr[side]["extras"]:
                    L.append(f"| 额外结论 {side} `{e['id']}` | | | {_cell(e)} |")
        L.append("")
        L.append(f"旧版：分母 {qr['old']['denominator']}，仅自动 {qr['old']['auto_pass']}，含复核 {qr['old']['final_pass']}；"
                 f"新版：分母 {qr['new']['denominator']}，仅自动 {qr['new']['auto_pass']}，含复核 {qr['new']['final_pass']}。"
                 f"退步 {len(qr['regressions'])} 项；未完成比较 {len(qr['incomplete'])} 项；"
                 f"新版阻断项 {len(qr['new']['blockers'])} 个，待复核的阻断类禁止项 {len(qr['new']['blockers_pending'])} 个。")
    L += ["", "## 反例", "", "| 反例 | 版本 | 题 | 探测 | 目标项结果 | 题目最终通过 | 阻断项 |", "|---|---|---|---|---|---|---|"]
    for ce in report["counterexamples"]:
        tg = "；".join(f"`{c['item']}` {RES_CN.get(c['got'], c['got'])}{'' if c['ok'] else ' ✗期望 ' + '/'.join(c['want'])}"
                      for c in ce["checks"])
        bl = "、".join(sorted({b["type"] for b in ce["blockers"]})) or "—"
        if ce["result"]["blockers_pending"]:
            bl += "；待复核：" + "、".join(sorted({b["type"] for b in ce["result"]["blockers_pending"]}))
        L.append(f"| {ce['id']} | {ce['side']} | {ce['question']} | {ce['probe']} | {tg} | {'是' if ce['question_final_pass'] else '否'} | {bl} |")
    if report["pairs"]:
        L += ["", "## 新旧成对反例（旧版弃答、新版给错）", ""]
        for p in report["pairs"]:
            L.append(f"- {p['old']} → {p['new']}：" + ("；".join(f"`{r['item']}` {r['note']}" for r in p["regressions"]) or "无退步（异常）"))
    return "\n".join(L) + "\n"


def main():
    report = run_trial()
    (HERE / "reports").mkdir(exist_ok=True)
    with open(HERE / "reports" / "trial-4q.json", "w", encoding="utf-8") as fh:
        json.dump({"summary": summary(report), **report}, fh, ensure_ascii=False, indent=2, sort_keys=True)
        fh.write("\n")
    with open(HERE / "reports" / "trial-4q.md", "w", encoding="utf-8") as fh:
        fh.write(render_md(report))
    print(json.dumps(summary(report), ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
