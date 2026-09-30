"""Full 0517000 baseline under common.v1 (all 63 questions, old side only).

Offline: reads the archived baseline results, the reviewed annotations and
the review ledger; writes reports/baseline-0517000.{md,json}. Lists, side
by side, the original score (tests/eval.rs score_question: answer
substring + sources), the common score, and coverage gaps. No model calls;
the new implementation does not exist yet, so there is no new side here.

    python3 tests/eval/scoring/baseline.py
"""
from __future__ import annotations

import collections
import re
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import trial  # noqa: E402
from ledger import load_ledger  # noqa: E402
from scoring import FAIL, MISSING, NOT_MEASURABLE, PASS, REVIEW  # noqa: E402


def _compact(text: str) -> str:
    return "".join(ch for ch in text if not ch.isspace()).lower()


def original_score(expect: dict, contract: dict, source_ids: dict):
    """Port of tests/eval.rs score_question (the pre-common scoring)."""
    status, answer = contract.get("status", "missing"), contract.get("safe_answer") or ""
    cited = contract.get("safe_source_ids") or []
    problems = []
    for name in expect.get("forbid_cite", []):
        if source_ids.get(name) in cited:
            problems.append(f"引用了禁止来源 {name}")
    kind = expect["kind"]
    if kind == "answer":
        if not cited:
            problems.append("未核准任何来源")
        for v in expect["answer"]:
            if _compact(str(v)) not in _compact(answer):
                problems.append(f"答案缺少 {v}")
        for name in expect.get("cite", []):
            if source_ids.get(name) not in cited:
                problems.append(f"未引用 {name}")
    else:
        ok = not cited and {"clarify": status == "clarification_required",
                            "insufficient_evidence": status == "insufficient_evidence",
                            "conflict": "冲突" in answer}[kind]
        if not ok:
            problems.append(f"期望 {kind}")
    dim = "recall" if kind == "answer" else "refusal"
    return dim, (not problems), problems


def category(expect: dict) -> str:
    if expect.get("computed"):
        return "日期计算"
    st = expect["status"]
    if st == "insufficient":
        return "证据不足"
    if st == "clarify":
        return "需澄清"
    if st == "conflict":
        return "冲突"
    if any(f.get("kind") == "value_as_current" for f in expect.get("forbidden", [])) and \
            any(f.get("current") == "yes" for f in expect.get("facts", [])):
        return "作答（存在旧值或他人/他物的值）"
    return "作答（取值）"


def run(ledger=None):
    ledger = ledger if ledger is not None else load_ledger(trial.LEDGER)
    rows = []
    for p in sorted(trial.CASES.glob("*.json")):
        c = trial._load(p)
        base = trial.baseline_for(c["id"])
        for q in c["questions"]:
            case, q_, meta, exp, view = trial.run_old(c["id"], q["id"])
            res = trial.evaluate("old", case, q_, meta, exp, view, ledger)
            _, real = trial.old_raw(c["id"], q["id"])
            dim, opass, oprob = original_score(q["expect"], real["answer_contract"], base["source_ids"])
            gaps = [i for i in res["items"] if i["result"] == MISSING]
            rows.append({
                "question": f"{c['id']}/{q['id']}", "query": q["query"], "category": category(exp),
                "label_status": exp.get("label_status"), "annotator_note": exp.get("annotator_note"),
                "old_status": view["status_raw"],
                "old_answer": view["assertions"][0]["text"] if view["assertions"] else None,
                "original": {"dimension": dim, "pass": opass, "problems": oprob},
                "common": {k: res[k] for k in ("denominator", "auto_pass", "final_pass",
                                                "question_auto_only_pass", "question_final_pass")},
                "counts": dict(collections.Counter(i["result"] for i in res["items"])),
                "pending_review": [i["id"] for i in res["items"] + res["extras"] if i["final"] == "pending_review"],
                "failed": [i["id"] for i in res["items"] + res["extras"] if i["final"] == "fail"],
                "gaps": [{"item": i["id"], "flag": i.get("flag")} for i in gaps],
                "extras": [e["id"] for e in res["extras"]],
                "blockers": res["blockers"],
            })
    return rows


def summarize(rows):
    s = {"questions": len(rows)}
    for dim in ("recall", "refusal"):
        rs = [r for r in rows if r["original"]["dimension"] == dim]
        s[f"original_{dim}"] = {"pass": sum(r["original"]["pass"] for r in rs), "total": len(rs)}
    s["common"] = {
        "items": sum(r["common"]["denominator"] for r in rows),
        "auto_pass": sum(r["common"]["auto_pass"] for r in rows),
        "final_pass": sum(r["common"]["final_pass"] for r in rows),
        "questions_auto_only_pass": sum(r["common"]["question_auto_only_pass"] for r in rows),
        "questions_final_pass": sum(r["common"]["question_final_pass"] for r in rows),
        "questions_with_pending_review": sum(bool(r["pending_review"]) for r in rows),
        "pending_review_items": sum(len(r["pending_review"]) for r in rows),
        "questions_failed": sum(bool(r["failed"]) for r in rows),
        "questions_with_gaps": sum(bool(r["gaps"]) for r in rows),
        "blockers": sum(len(r["blockers"]) for r in rows),
    }
    gaps = collections.Counter()
    for r in rows:
        for g in r["gaps"]:
            gaps[re.sub(r"\[\d+\]", "", g["item"])] += 1
    s["gap_fields"] = dict(gaps)
    cats = collections.defaultdict(lambda: {"n": 0, "original_pass": 0, "auto_only": 0, "final": 0})
    for r in rows:
        c = cats[r["category"]]
        c["n"] += 1
        c["original_pass"] += r["original"]["pass"]
        c["auto_only"] += r["common"]["question_auto_only_pass"]
        c["final"] += r["common"]["question_final_pass"]
    s["by_category"] = dict(cats)
    s["divergent"] = [r["question"] for r in rows if r["original"]["pass"] != r["common"]["question_final_pass"]]
    return s


def render(rows, s):
    c = s["common"]
    L = ["# 0517000 基线：原始评分与共同评分（common.v1，全部 63 题）", "",
         "只有旧版一侧：新实现尚未完成，没有新版输出。全部离线，未调用模型。", "",
         "- **原始评分**：tests/eval.rs 的旧口径（答案子串 + 来源），即严格回放的召回 42 / 拒答 21。",
         "- **共同评分**：common.v1。“仅自动”只算结构检查和整串等值；“最终”另计台账中 confirmed 的复核结论，proposed 不计。",
         "- **覆盖缺口**：旧格式无法表达的判分项（`format_cannot_express`），记为缺失、不补齐。", "",
         "## 汇总", "",
         "| 口径 | 结果 |", "|---|---|",
         f"| 原始：召回 | {s['original_recall']['pass']} / {s['original_recall']['total']} |",
         f"| 原始：拒答 | {s['original_refusal']['pass']} / {s['original_refusal']['total']} |",
         f"| 共同：判分项 仅自动 / 最终 / 分母 | {c['auto_pass']} / {c['final_pass']} / {c['items']} |",
         f"| 共同：题目 仅自动通过 / 最终通过 | {c['questions_auto_only_pass']} / {c['questions_final_pass']}（共 {s['questions']}） |",
         f"| 共同：有待复核项的题 | {c['questions_with_pending_review']}（{c['pending_review_items']} 项） |",
         f"| 共同：有判错项的题 | {c['questions_failed']} |",
         f"| 共同：有覆盖缺口的题 | {c['questions_with_gaps']} |",
         f"| 阻断项 | {c['blockers']} |", "",
         "## 按题型", "", "| 题型 | 题数 | 原始通过 | 共同仅自动通过 | 共同最终通过 |", "|---|---|---|---|---|"]
    for k, v in sorted(s["by_category"].items(), key=lambda kv: -kv[1]["n"]):
        L.append(f"| {k} | {v['n']} | {v['original_pass']} | {v['auto_only']} | {v['final']} |")
    L += ["", "## 覆盖缺口（旧格式无法表达）", "",
          "按判分项字段计：" + "，".join(f"`{k}` {v}" for k, v in sorted(s["gap_fields"].items())) + "。",
          "",
          "- 日期计算题：旧答案是一段文字，给不出取整口径、精确天数和结构化端点。",
          "- 冲突题：旧格式只报告“存在冲突”，给不出冲突双方。",
          "- 其余不可测范围：LongMemEval 的 27 个用例没有逐条入库期望，入库、漏提和错误更新在这 27 个用例上不可测（phase1-baseline 第 5 节），本报告只覆盖问答。", "",
          "## 原始评分与共同评分不一致的题", ""]
    for q in s["divergent"]:
        r = next(x for x in rows if x["question"] == q)
        why = []
        if r["failed"]:
            why.append("判错：" + "、".join(r["failed"]))
        if r["gaps"]:
            why.append("缺失：" + "、".join(g["item"] for g in r["gaps"]))
        if r["pending_review"]:
            why.append("待复核：" + "、".join(r["pending_review"]))
        L.append(f"- `{q}`：原始{'通过' if r['original']['pass'] else '未通过'}，共同{'通过' if r['common']['question_final_pass'] else '未通过'}。"
                 + "；".join(why) + (f"（{r['annotator_note']}）" if r["annotator_note"] and r["failed"] else ""))
    L += ["", "## 逐题", "", "| 题 | 题型 | 旧答案 | 原始 | 共同 仅自动/最终/分母 | 待复核 | 缺失 | 判错 |", "|---|---|---|---|---|---|---|---|"]
    for r in rows:
        L.append(f"| `{r['question']}` | {r['category']} | {r['old_answer'] or r['old_status']} | "
                 f"{'通过' if r['original']['pass'] else '未通过'} | {r['common']['auto_pass']}/{r['common']['final_pass']}/{r['common']['denominator']} | "
                 f"{len(r['pending_review']) or ''} | {len(r['gaps']) or ''} | {'、'.join(r['failed'])} |")
    return "\n".join(L) + "\n"


def main():
    rows = run()
    s = summarize(rows)
    out = HERE / "reports"
    with open(out / "baseline-0517000.json", "w", encoding="utf-8") as fh:
        json.dump({"summary": s, "questions": rows}, fh, ensure_ascii=False, indent=2, sort_keys=True)
        fh.write("\n")
    with open(out / "baseline-0517000.md", "w", encoding="utf-8") as fh:
        fh.write(render(rows, s))
    print(json.dumps(s, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
