#!/usr/bin/env python3
"""AF11 七次独立阶段试验输入；只读录制，不联网、不改旧指纹。

准备：python3 -B tests/eval/fixed_inputs/af11.py --output tests/eval/fixed_inputs/af11.json
干跑：EVAL_FIXED_INPUTS=af11 EVAL_RECORD=7 cargo test --test write_stage_eval dry_run_plan -- --exact --nocapture
获准录制：同环境只运行 write_stage_eval_suite -- --exact --nocapture，模型/端点/密钥沿用评测环境。
"""
import argparse
import copy
import json
from pathlib import Path

from prepare import fingerprint

ROOT = Path(__file__).resolve().parents[1]
AC2 = "ac2-pro-medium-pro-minimal-pro-minimal-r4-compute-low"
TARGET = "ac2-af-targeted"
QUESTIONS = [("unresolved_conflict", "conflict_q3", TARGET),
             ("lme_7a87bd0c", "q", AC2), ("lme_a2f3aa27", "q", AC2),
             ("lme_59524333", "q", AC2), ("event_registration_fee", "event_q5", AC2)]


def recorded(label, case, question, stage):
    operation = "recall:" + question
    rows = json.loads((ROOT / "end_to_end" / label / (case + ".json")).read_text())["operations"][operation]
    schema = {"r1": "r1_v1", "r4": "r4_v3"}[stage]
    index, row = next((i, r) for i, r in enumerate(rows) if r["schema"] == schema)
    if fingerprint(row["request_content"]) != row["request"]:
        raise ValueError("原录制指纹不符，拒绝准备")
    origin = {"label": label, "case": case, "operation": operation,
              "row": index, "request": row["request"]}
    return copy.deepcopy(row["request_content"]["input"]), origin, row.get("usage")


def extract():
    cases = {c["id"]: c for c in (json.loads(p.read_text()) for p in (ROOT / "cases").glob("*.json"))}
    samples = []
    for cid, qid, label in QUESTIONS:
        q = next(q for q in cases[cid]["questions"] if q["id"] == qid)
        for stage in (["r1", "r4"] if cid in ("lme_7a87bd0c", "lme_a2f3aa27") else ["r4"]):
            input_, origin, usage = recorded(label, cid, qid, stage)
            sample = {"id": "af11_" + stage + "_" + cid + "_" + qid, "stage": stage,
                      "fixed_input": True, "label_status": "contract_only", "split": "failure_regression",
                      "input": input_, "original_usage": usage, "origin": origin,
                      "note": "独立阶段试验；R4保留录制中的已核验R1计划，不代表新R1后的端到端结果。"}
            if stage == "r1":
                sample["expect"] = {"plan.op": "lookup"}
                sample["label_status"] = "approved_operation"
            elif cid == "event_registration_fee":
                fresh, evidence_origin, _ = recorded(TARGET, cid, "event_q3", "r4")
                # 同接收范围、主体、任务、时间与硬过滤；不以词法或标签挑选记录。
                evidence_q = next(q for q in cases[cid]["questions"] if q["id"] == "event_q3")
                if q["after"] != evidence_q["after"] or input_["utc_offset_seconds"] != fresh["utc_offset_seconds"]:
                    raise ValueError("event_q5 证据包的来源接收范围/时区不同")
                for key in ("subject", "time", "conditions", "plan"):
                    if input_["plan"][key] != fresh["plan"][key]:
                        raise ValueError("event_q5 证据包的可信范围或计划不同")
                for key in ("task_id", "at"):
                    if input_["request"][key] != fresh["request"][key]:
                        raise ValueError("event_q5 证据包任务/时间不同")
                current = input_["evidence"]["current_message"]
                input_["evidence"] = fresh["evidence"]
                input_["evidence"]["current_message"] = current
                sample["evidence_origin"] = evidence_origin
                sample["note"] = "q5原请求/已核验计划＋定点q3同范围证据包；judged来自真实录制，未手改状态。仍为固定阶段试验。"
            if stage == "r4":
                ids = set()

                def sources(value):
                    if isinstance(value, dict):
                        if value.get("kind") == "stored":
                            ids.add(value["source_id"])
                        for v in value.values():
                            sources(v)
                    elif isinstance(value, list):
                        for v in value:
                            sources(v)

                sources(input_["evidence"])
                ids.update(r["source_id"] for r in input_["evidence"]["raw"])
                sample["source_texts"] = {m["id"]: m["text"] for m in cases[cid]["messages"][:q["after"]] if m["id"] in ids}
                if ids != set(sample["source_texts"]):
                    raise ValueError("证据来源没有获准的对应正文")
            samples.append(sample)
    if len(samples) != 7:
        raise ValueError("AF11 必须为七次独立阶段试验")
    return {"version": "eval.fixed_inputs@1", "samples": samples}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    data = extract()
    if args.output:
        args.output.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n")
    for sample in data["samples"]:
        print(sample["stage"].upper(), sample["id"], "原请求", sample["origin"]["request"])
    print("R1=2；R4=5；合计7；真实调用0。严格新请求指纹由Rust干跑构造；不重写旧指纹。")
