"""Absolute expect_common scoring of eval.e2e.v3; 0517000 is reference only.

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
STAGES = ("W1", "W3", "W4", "R1", "R4")


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


def absolute_state(scored):
    if scored["question_final_pass"]:
        return "pass"
    finals = [i["final"] for i in scored["items"] + scored["extras"]]
    if any(s in ("fail", "missing") for s in finals):
        return "fail"
    if "pending_review" in finals:
        return "pending_review"
    return "fail"


def execution(document):
    cases = []
    for case in document.get("cases", []):
        stages = {s: {"requests": 0, "sends": 0, "replays": 0, "not_recorded": 0,
                      "elapsed_ms": 0, "recorded_elapsed_ms": None, "usage": {}, "diagnostics": []}
                  for s in STAGES}
        for call in case.get("calls", []):
            s = stages[call["stage"]]
            s["requests"] += 1
            s["sends"] += call["sends"]
            s["replays"] += call["origin"] == "replay"
            s["not_recorded"] += call["origin"] == "not_recorded"
            s["elapsed_ms"] += call["elapsed_ms"]
            if call.get("recorded_elapsed_ms") is not None:
                s["recorded_elapsed_ms"] = (s["recorded_elapsed_ms"] or 0) + call["recorded_elapsed_ms"]
            for token in ("input_tokens", "output_tokens"):
                value = (call.get("usage") or {}).get(token)
                if value is not None:
                    s["usage"][token] = s["usage"].get(token, 0) + value
            if (call.get("reason") or call.get("transport_error") or call.get("error")
                    or (call.get("http_status") or 0) >= 400
                    or call.get("response_status") != "completed"):
                s["diagnostics"].append({k: call.get(k) for k in
                    ("operation", "origin", "http_status", "response_status", "incomplete_details",
                     "transport_error", "error", "reason", "usage")})
        runtime = []
        for step in case.get("steps", []):
            result = step.get("result") or {}
            body = result.get("Ok", result)
            status = (body.get("enrichment") or {}).get("status")
            if result.get("Err") or body.get("error") or body.get("degraded_reasons") or status in (
                    "failed", "budget_exhausted", "pending", "needs_rejudge", "coverage_gaps"):
                runtime.append({"step": step["step"], "id": step.get("message") or step.get("question"),
                                "error": result.get("Err") or body.get("error"),
                                "enrichment": body.get("enrichment"),
                                "degraded_reasons": body.get("degraded_reasons")})
        cases.append({"case": case["case"], "elapsed_ms": case.get("elapsed_ms"),
                      "stages": stages, "runtime_diagnostics": runtime})
    return {"jobs": document.get("jobs"), "elapsed_ms": document.get("elapsed_ms"),
            "real_calls": document.get("real_calls", 0), "cases": cases}


def reference(questions, ledger):
    """Reference failures cannot prevent reporting the new path's absolute score."""
    try:
        rows = baseline.run(ledger)
        by_id = {r["question"]: r for r in rows}
        comparisons, regressions = [], []
        for row in questions:
            ref = {"question": row["question"], "old": by_id[row["question"]],
                   "transitions": [], "regressions": [], "incomplete": []}
            if row["state"] == "completed":
                case, q, meta, expect, view = trial.run_old(*row["question"].split("/"))
                old = trial.evaluate("old", case, q, meta, expect, view, ledger)
                transitions, regs, incomplete = trial.transitions(old, row["scored"], expect["status"])
                ref.update(transitions=transitions, regressions=regs, incomplete=incomplete)
                regressions.extend({"question": row["question"], **r} for r in regs)
            comparisons.append(ref)
        return {"baseline": "0517000", "state": "available", "summary": baseline.summarize(rows),
                "questions": comparisons, "regressions": regressions, "acceptance_gate": False}
    except (OSError, KeyError, ValueError, TypeError) as error:
        return {"baseline": "0517000", "state": "unavailable", "reason": str(error), "acceptance_gate": False}


def run(document=None):
    document = document or {"cases": []}
    ledger = load_ledger(trial.LEDGER)
    steps = {(c["case"], s["question"]): s for c in document["cases"]
             for s in c["steps"] if s["step"] == "question"}
    questions, new_rows = [], []
    for path in sorted(trial.CASES.glob("*.json")):
        case = trial._load(path)
        for question in case["questions"]:
            name = f"{case['id']}/{question['id']}"
            row = {"question": name, "query": question["query"], "category": baseline.category(question["expect_common"]),
                   "state": "not_recorded", "absolute": "not_recorded", "reason": "未录制", "new": None}
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
                row.update(state="completed", reason=None, new=common_row(case, q, new),
                           new_view=view, scored=new, absolute=absolute_state(new))
                new_rows.append(row["new"])
            elif step:
                row["reason"] = step.get("reason") or "未录制"
            questions.append(row)
    # Reuse baseline's summary so counts/denominators/gaps cannot drift between implementations.
    # Its original-score fields only describe legacy outputs and are excluded for the new side.
    summary_input = [dict(r, original={"dimension": "recall", "pass": False}) for r in new_rows]
    new_summary = baseline.summarize(summary_input)
    for key in ("original_recall", "original_refusal", "divergent"):
        new_summary.pop(key, None)
    for category in new_summary["by_category"].values():
        category.pop("original_pass", None)
    states = ("pass", "fail", "pending_review", "not_recorded")
    absolute = {s: sum(r["absolute"] == s for r in questions) for s in states}
    absolute["questions"] = len(questions)
    by_case = {}
    for row in questions:
        case = row["question"].split("/")[0]
        by_case.setdefault(case, {s: 0 for s in states})[row["absolute"]] += 1
    absolute["by_case"] = by_case
    return {"scoring_version": "common.v1", "label": document.get("label"),
            "absolute_summary": absolute, "new_summary": new_summary,
            "not_recorded": sum(r["state"] == "not_recorded" for r in questions),
            "questions": questions, "execution": execution(document), "reference": reference(questions, ledger),
            "coverage_note": "仅问答共同评分；27 个 LongMemEval 用例缺逐条入库标注，不能据此评价漏提或错误更新。未录制不计通过、不判模型失败；语义待复核不计最终通过。"}


def render(report):
    new, absolute = report["new_summary"], report["absolute_summary"]
    lines = ["# 新路径端到端绝对评分（expect_common / common.v1）", "", f"配置：{report['label'] or '未录制'}。", "",
             f"通过 {absolute['pass']} / 失败 {absolute['fail']} / 待复核 {absolute['pending_review']} / 未录制 {absolute['not_recorded']}（共 {absolute['questions']} 题）。", "",
             f"已评分 {new['questions']} / {len(report['questions'])}；未录制 {report['not_recorded']}。", "",
             "已知失败或缺失必需项归为失败；没有已知失败但仍需语义确认的题归为待复核。未录制不计通过，待复核不计最终通过。", "",
             f"共同判分项：{json.dumps(new['common'], ensure_ascii=False)}", "",
             report["coverage_note"], "", "## 按用例", "",
             "| 用例 | 通过 | 失败 | 待复核 | 未录制 |", "|---|---|---|---|---|"]
    for case, scores in absolute["by_case"].items():
        lines.append(f"| {case} | {scores['pass']} | {scores['fail']} | {scores['pending_review']} | {scores['not_recorded']} |")
    lines += ["", "## 逐题", "", "| 题 | 绝对结果 | 自动/最终/分母 | 缺口/待复核/判错 |", "|---|---|---|---|"]
    for row in report["questions"]:
        new = row["new"]["common"] if row["new"] else None
        count = f"{new['auto_pass']}/{new['final_pass']}/{new['denominator']}" if new else "未录制"
        detail = row["reason"] if not new else f"缺口 {len(row['new']['gaps'])}；待复核 {len(row['new']['pending_review'])}；判错 {'、'.join(row['new']['failed'])}"
        lines.append(f"| {row['question']} | {row['absolute']} | {count} | {detail} |")
    metrics = report["execution"]
    lines += ["", "## 调用与耗时", "", f"EVAL_JOBS={metrics['jobs']}；本轮真实发送 {metrics['real_calls']}；批次墙钟 {metrics['elapsed_ms']} ms。", "",
              "当前耗时为本轮处理时间；历史录制耗时在回放时仍保留，两者分列。回放 usage 是历史账单，不是本轮费用。", "",
              "| 用例 | 用例墙钟 ms | 阶段 | 请求/发送/回放/未录制 | 当前耗时 ms | 历史录制耗时 ms | usage 输入/输出 | 失败诊断 |", "|---|---|---|---|---|---|---|---|"]
    for case in metrics["cases"]:
        for stage, stats in case["stages"].items():
            usage = stats["usage"]
            diagnostic = json.dumps(stats['diagnostics'], ensure_ascii=False).replace('|', '\\|')
            lines.append(f"| {case['case']} | {case['elapsed_ms']} | {stage} | {stats['requests']}/{stats['sends']}/{stats['replays']}/{stats['not_recorded']} | {stats['elapsed_ms']} | {stats['recorded_elapsed_ms']} | {usage.get('input_tokens', '未知')}/{usage.get('output_tokens', '未知')} | {diagnostic} |")
    for case in metrics["cases"]:
        for diagnostic in case["runtime_diagnostics"]:
            lines.append(f"\n{case['case']} 运行时诊断：{json.dumps(diagnostic, ensure_ascii=False)}\n")
    ref = report["reference"]
    lines += ["", "## 附录：0517000 旧基线参考", "", "旧基线与转移仅作参考，不作为新路径验收门槛。", ""]
    if ref["state"] != "available":
        lines.append(f"参考不可用：{ref['reason']}")
        return "\n".join(lines) + "\n"
    lines.append(f"旧共同评分：{json.dumps(ref['summary']['common'], ensure_ascii=False)}")
    for r in ref["regressions"]:
        lines.append(f"- {r['question']} / {r['item']}：{r['note']}")
    for row in ref["questions"]:
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
    absolute = report["absolute_summary"]
    print(f"expect_common：通过 {absolute['pass']} / 失败 {absolute['fail']} / 待复核 {absolute['pending_review']} / 未录制 {absolute['not_recorded']}；报告 {args.output}")


if __name__ == "__main__":
    main()
