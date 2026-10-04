"""Score an actual eval.e2e.v3 run against frozen 0517000; never calls a model.

python3 tests/eval/scoring/compare.py --run <run.json> --output <report-prefix>
Absent recordings are unavailable comparisons, not model failures or passes.
"""
from __future__ import annotations

import argparse
import collections
import copy
import json
import pathlib

import baseline
import trial
from adapters import adapt_recall, question_input
from ledger import load_ledger
from scoring import MISSING

CLOCK = 1_790_568_000


def trusted_question(question):
    q = copy.deepcopy(question)
    if q.get("at") is None:
        q["at"] = CLOCK + 60 * (q["after"] - 1) + 30
    return q


def common_row(case, q, result):
    """Exactly baseline.py's common counters, without inventing a new original score."""
    return {
        "question": f"{case['id']}/{q['id']}", "query": q["query"],
        "category": baseline.category(q["expect_common"]),
        "common": {k: result[k] for k in ("denominator", "auto_pass", "final_pass",
                   "question_auto_only_pass", "question_final_pass")},
        "counts": dict(collections.Counter(i["result"] for i in result["items"])),
        "pending_review": [i["id"] for i in result["items"] + result["extras"]
                           if i["final"] == "pending_review"],
        "failed": [i["id"] for i in result["items"] + result["extras"] if i["final"] == "fail"],
        "gaps": [{"item": i["id"], "flag": i.get("flag")} for i in result["items"] if i["result"] == MISSING],
        "extras": [e["id"] for e in result["extras"]], "blockers": result["blockers"],
    }


def run(document=None):
    document = document or {"cases": []}
    ledger = load_ledger(trial.LEDGER)
    old_rows = baseline.run(ledger)
    old_by_id = {r["question"]: r for r in old_rows}
    steps = {(c["case"], s["question"]): s for c in document["cases"]
             for s in c["steps"] if s["step"] == "question"}
    questions, new_rows, regressions = [], [], []
    for path in sorted(trial.CASES.glob("*.json")):
        case = trial._load(path)
        for question in case["questions"]:
            name = f"{case['id']}/{question['id']}"
            old_row = old_by_id[name]
            row = {"question": name, "query": question["query"], "category": old_row["category"],
                   "old": old_row, "state": "not_recorded", "reason": "未录制", "new": None,
                   "transitions": [], "regressions": [], "incomplete": []}
            step = steps.get((case["id"], question["id"]))
            if step and step["state"] == "completed":
                q = trusted_question(question)
                # Exported time cannot redefine the trusted question or weaken binding checks.
                if step.get("at") != q["at"]:
                    raise ValueError(f"{name}: exported trusted time differs")
                raw = step["result"]
                texts = trial.source_texts(case, q)
                view = adapt_recall(question_input(case, q), q["query_meta"], raw,
                                    step["source_ids"], source_texts=texts, source_state=step.get("source_state"))
                new = trial.evaluate("new", case, q, q["query_meta"], q["expect_common"], view, ledger)
                _, old_q, meta, expect, old_view = trial.run_old(case["id"], q["id"])
                old = trial.evaluate("old", case, old_q, meta, expect, old_view, ledger)
                transitions, regs, incomplete = trial.transitions(old, new, expect["status"])
                row.update(state="completed", reason=None, new=common_row(case, q, new),
                           new_view=view, scored=new, transitions=transitions,
                           regressions=regs, incomplete=incomplete,
                           question_transition={"old": old["question_final_pass"],
                                                "new": new["question_final_pass"]})
                new_rows.append(row["new"])
                regressions.extend({"question": name, **r} for r in regs)
            elif step:
                row["reason"] = step.get("reason") or "未录制"
            questions.append(row)
    # Reuse baseline's summary so counts/denominators/gaps cannot drift between implementations.
    # Its original-score fields only describe legacy outputs and are excluded for the new side.
    summary_input = [dict(r, original={"dimension": old_by_id[r["question"]]["original"]["dimension"],
                                      "pass": False}) for r in new_rows]
    new_summary = baseline.summarize(summary_input)
    for key in ("original_recall", "original_refusal", "divergent"):
        new_summary.pop(key, None)
    for category in new_summary["by_category"].values():
        category.pop("original_pass", None)
    return {"scoring_version": "common.v1", "baseline": "0517000", "label": document.get("label"),
            "old_summary": baseline.summarize(old_rows), "new_summary": new_summary,
            "not_recorded": sum(r["state"] == "not_recorded" for r in questions),
            "regressions": regressions, "questions": questions,
            "coverage_note": "仅问答共同评分；27 个 LongMemEval 用例缺逐条入库标注，不能据此评价漏提或错误更新。未录制不计通过、不判模型失败；语义待复核不计最终通过。"}


def render(report):
    old, new = report["old_summary"], report["new_summary"]
    lines = ["# 端到端新旧对照（common.v1）", "", f"配置：{report['label'] or '未录制'}；旧基线：0517000。", "",
             f"已评分 {new['questions']} / {len(report['questions'])}；未录制 {report['not_recorded']}。", "",
             "未录制不进入新版评分分母。新旧覆盖不同，部分录制时不得将汇总分数作直接优劣比较。", "",
             f"旧共同评分：{json.dumps(old['common'], ensure_ascii=False)}", "",
             f"新共同评分：{json.dumps(new['common'], ensure_ascii=False)}", "",
             report["coverage_note"], "", "## 通过→失败及其他退步", ""]
    for r in report["regressions"]:
        lines.append(f"- {r['question']} / {r['item']}：{r['note']}")
    if not report["regressions"]:
        lines.append("暂无可判定退步。未录制或待复核不代表无退步。")
    lines += ["", "## 逐题", "", "| 题 | 旧共同 自动/最终/分母 | 新共同 自动/最终/分母 | 结果/缺口/待复核 |", "|---|---|---|---|"]
    for row in report["questions"]:
        old = row["old"]["common"]
        new = row["new"]["common"] if row["new"] else None
        fmt = lambda r: f"{r['auto_pass']}/{r['final_pass']}/{r['denominator']}"
        detail = row["reason"] if not new else f"缺口 {len(row['new']['gaps'])}；待复核 {len(row['new']['pending_review'])}；退步 {len(row['regressions'])}"
        lines.append(f"| {row['question']} | {fmt(old)} | {fmt(new) if new else '未录制'} | {detail} |")
    lines += ["", "## 逐项转移", ""]
    for row in report["questions"]:
        for item in row["transitions"]:
            lines.append(f"- {row['question']} / {item['item']}：{item['old']} → {item['new']}；{item['note'] or ''}")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", type=pathlib.Path, help="不存在的文件明确报告未录制")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    report = run(trial._load(args.run) if args.run and args.run.exists() else None)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.with_suffix(".json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    args.output.with_suffix(".md").write_text(render(report))
    print(f"common.v1：已评分 {report['new_summary']['questions']}/63，未录制 {report['not_recorded']}；报告 {args.output}")


if __name__ == "__main__":
    main()
