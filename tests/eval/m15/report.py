"""Read MVP run output; reuse common.v1 mechanics, never auto-pass semantics."""
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "scoring"))
from adapters import adapt_recall, question_input
from trial import evaluate, source_texts
from compare import execution, absolute_state
from ledger import load_ledger


def report(document, ledger=None):
    case = json.loads((HERE / "mvp.json").read_text())
    run = next(c for c in document["cases"] if c["case"] == case["id"])
    rows = []
    for q in case["questions"]:
        step = next(s for s in run["steps"] if s.get("question") == q["id"])
        if step.get("state") != "completed":
            rows.append({"question":q["id"], "absolute":"not_recorded", "reason":step.get("reason")})
            continue
        view = adapt_recall(question_input(case, q), q["query_meta"], step["result"], step["source_ids"],
                            source_texts=source_texts(case, q), source_state=step.get("source_state"))
        result = evaluate("new", case, q, q["query_meta"], q["expect_common"], view, ledger or {})
        # This also exposes full statements, plan speech on records, dates and
        # qualifiers for reviewers. No guessed semantic matching of slots.
        rows.append({"question":q["id"], "label_status":case["label_status"], "absolute":
                     "draft" if case["label_status"] == "draft" else absolute_state(result), "result":result, "view":view})
    return {"version":"m15.common.v1@1", "label_status":case["label_status"], "questions":rows,
            "execution":execution(document), "real_calls":document.get("real_calls",0),
            "previous_sends":document.get("previous_sends",0),"total_sends":document.get("total_sends",0),
            "writes":[s for s in run["steps"] if s["step"] in ("prepare", "message")]}


if __name__ == "__main__":
    path = pathlib.Path(sys.argv[1])
    ledger = load_ledger(pathlib.Path(sys.argv[2])) if len(sys.argv) > 2 else {}
    result = report(json.loads(path.read_text()), ledger)
    output = path.with_name(path.stem + "-report.json")
    output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
    print(f"MVP common.v1 报告：{output}；draft/待复核不计通过；真实发送 {result['real_calls']}")
