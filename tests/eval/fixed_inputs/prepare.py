#!/usr/bin/env python3
"""从首轮 live 结果冻结失败阶段输入；不访问网络，不修改原录制。"""
import argparse
import hashlib
import json
import statistics
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LABEL = "ac1-pro-medium-flash-medium-pro-minimal"


def fingerprint(value):
    text = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return "sha256:" + hashlib.sha256(text.encode()).hexdigest()


def extract():
    source = ROOT / "end_to_end" / LABEL
    run = json.loads((source / "run.json").read_text())
    assert run["real_calls"] == 552
    cases = {c["id"]: c for c in (json.loads(p.read_text()) for p in (ROOT / "cases").glob("*.json"))}
    samples = []
    for case in run["cases"]:
        cid = case["case"]
        doc = json.loads((source / f"{cid}.json").read_text())
        for step in case["steps"]:
            if step["step"] == "message":
                op = "write:" + step["message"]
                stage = "w3"
                failed = step["result"].get("Ok", {}).get("enrichment", {}).get("status") == "failed"
            else:
                op = "recall:" + step["question"]
                stage = "r4"
                failed = step["result"].get("semantic_judgment") == "failed"
            rows = doc["operations"][op]
            row = rows[-1]
            if not failed or row["schema"] != {"w3": "w3_v1", "r4": "r4_v3"}[stage]:
                continue
            assert fingerprint(row["request_content"]) == row["request"]
            input_ = row["request_content"]["input"]
            sample = {"id": f"{stage}_{cid}_{op.split(':')[1]}", "stage": stage,
                      "fixed_input": True, "label_status": "contract_only", "split": "failure_regression",
                      "input": input_, "original_usage": row["usage"],
                      "origin": {"label": LABEL, "case": cid, "operation": op,
                                 "row": len(rows) - 1, "request": row["request"]}}
            if stage == "r4":
                # 核验/旧方补入用，永不进入模型 payload；源正文来自相同用例原话。
                evidence = input_["evidence"]
                ids = {r["source_id"] for r in evidence["raw"]}
                ids.update(e["source"]["source_id"] for r in evidence["records"]
                           for e in r["evidence"] if e["source"]["kind"] == "stored")
                sample["source_texts"] = {m["id"]: m["text"] for m in cases[cid]["messages"] if m["id"] in ids}
            samples.append(sample)
    assert sum(s["stage"] == "w3" for s in samples) == 23
    assert sum(s["stage"] == "r4" for s in samples) == 10
    return {"version": "eval.fixed_inputs@1", "samples": samples}


def estimate(data):
    run = json.loads((ROOT / "end_to_end" / LABEL / "run.json").read_text())
    stages = ["W1", "W3", "W4", "R1", "R4"]
    result = {"basis": "首轮实测（含 incomplete）；W3 是 Flash medium 代理，不是 Pro minimal 预测",
              "full": [], "fixed": []}
    for stage, count in zip(stages, [204, 204, 169, 63, 63]):
        usages = [c["usage"] for case in run["cases"] for c in case["calls"] if c["stage"] == stage]
        assert all(u and "input_tokens" in u and "output_tokens" in u for u in usages)
        means = {k: sum(u[k] for u in usages) / len(usages) for k in ["input_tokens", "output_tokens"]}
        result["full"].append({"stage": stage, "calls": count, "observed_calls": len(usages),
                               "mean": means, "estimate": {k: round(n * count) for k, n in means.items()}})
        subset = [s["original_usage"] for s in data["samples"] if s["stage"].upper() == stage]
        if subset:
            result["fixed"].append({"stage": stage, "calls": len(subset),
                                    "estimate": {k: sum(u[k] for u in subset) for k in means}})
    # AE6 已批准的 Pro minimal W3 样本，仅提供低推理输出量的对照，不替换固定输入实测。
    rows = []
    for p in (ROOT / "write_stage/recordings/DeepSeek-V4-Pro/reasoning-minimal/repeat-1").glob("w3_*.json"):
        records = json.loads(p.read_text())["recorded"]
        rows.append(records[-1].get("usage", records[-1]["response"]["usage"]))
    if rows:
        result["w3_pro_minimal_reference"] = {"samples": len(rows),
            "mean": {k: sum(u[k] for u in rows) / len(rows) for k in ["input_tokens", "output_tokens"]},
            "note": "单阶段标注样本，不同于失败固定输入；不能直接外推复杂长消息"}
    # AE10：仅从已提交录制读取 usage/延迟，不请求模型或改写录制。
    low = [json.loads(p.read_text())["recorded"][-1] for p in
           (ROOT / "write_stage/recordings/DeepSeek-V4-Pro/reasoning-low").glob("repeat-*/r4_*.json")]
    assert len(low) == 40
    output_mean = statistics.mean(row["usage"]["output_tokens"] for row in low)
    low_p50 = statistics.median(row["elapsed_ms"] for row in low) / 1000
    compute_ops = {"count", "sum", "elapsed", "compare"}
    questions = [q for p in (ROOT / "cases").glob("*.json")
                 for q in json.loads(p.read_text())["questions"]]
    assert all(q["query_meta"]["operation"] in compute_ops | {"lookup", "list"} for q in questions)
    full_compute = sum(q["query_meta"]["operation"] in compute_ops for q in questions)
    records = {s["id"]: json.loads((ROOT / "fixed_inputs/recordings/ac2-fixed-pro-minimal"
               / "reasoning-minimal/repeat-1" / (s["id"] + ".json")).read_text())["recorded"][-1]
               for s in data["samples"]}
    fixed_compute = [s for s in data["samples"] if s["stage"] == "r4"
                     and s["input"]["plan"]["plan"]["op"] in compute_ops]
    fixed_other = [s for s in data["samples"] if s["stage"] == "r4" and s not in fixed_compute]
    new_w3 = [records[s["id"]] for s in data["samples"] if s["stage"] == "w3"]
    # 全量输入量沿用同链路首轮，W3 输出量改用 AE9 Pro minimal 失败固定输入的测量。
    full = [dict(row, estimate=dict(row["estimate"]), mean=dict(row["mean"])) for row in result["full"]]
    full[1]["mean"]["output_tokens"] = statistics.mean(r["usage"]["output_tokens"] for r in new_w3)
    full[1]["output_observed_calls"] = len(new_w3)
    full[1]["output_basis"] = "AE9固定输入Pro minimal；input/observed_calls仍为首轮代理"
    full[1]["estimate"]["output_tokens"] = round(statistics.mean(r["usage"]["output_tokens"] for r in new_w3) * 204)
    full[4]["estimate"]["output_tokens"] = round(output_mean * full_compute +
        full[4]["mean"]["output_tokens"] * (len(questions) - full_compute))
    fixed = []
    for stage in ["w3", "r4"]:
        samples = [s for s in data["samples"] if s["stage"] == stage]
        fixed.append({"stage": stage.upper(), "calls": len(samples), "estimate": {
            "input_tokens": sum(records[s["id"]]["usage"]["input_tokens"] for s in samples),
            "output_tokens": round(sum(output_mean if s in fixed_compute else
                records[s["id"]]["usage"]["output_tokens"] for s in samples))}})
    result["ae10"] = {
        "basis": "low: 首轮40次均值/p50；minimal: 首轮全量R4与AE9固定集；非费用/上界/质量预测",
        "r4_low": {"observed_calls": len(low), "mean_input_tokens": statistics.mean(row["usage"]["input_tokens"] for row in low),
            "mean_output_tokens": output_mean, "p50_seconds": low_p50},
        "full": {"op_source": "query_meta.operation 标注；实际分支只取已核验R1计划，R1失败minimal",
            "r4_compute_low": full_compute, "r4_other_minimal": len(questions) - full_compute,
            "stages": full, "total_estimate": {k: sum(row["estimate"][k] for row in full)
                for k in ["input_tokens", "output_tokens"]},
            "r4_serial_seconds": round(full_compute * low_p50 + (len(questions) - full_compute) * 4.5)},
        "fixed": {"op_source": "冻结生产输入中的已核验 input.plan.plan.op",
            "r4_compute_low": len(fixed_compute), "r4_other_minimal": len(fixed_other),
            "stages": fixed, "total_estimate": {k: sum(row["estimate"][k] for row in fixed)
                for k in ["input_tokens", "output_tokens"]},
            "new_low_calls": len(fixed_compute),
            "new_low_estimate": {"input_tokens": sum(records[s["id"]]["usage"]["input_tokens"] for s in fixed_compute),
                "output_tokens": round(len(fixed_compute) * output_mean)},
            "serial_seconds": round(sum(r["elapsed_ms"] for r in new_w3) / 1000 +
                len(fixed_compute) * low_p50 + sum(records[s["id"]]["elapsed_ms"] for s in fixed_other) / 1000),
            "new_low_serial_seconds": round(len(fixed_compute) * low_p50)},
        "limits": "全量输入量/W1/W4仍是首轮代理，W3换模型/提示词且固定集偏长；p50相加只是情景，未计限流/排队/冷启动。固定集input token用AE9同输入，low输出按不同样本均值估算，不是新测量。"}
    # AE15：最新 low 小集8次含一次本地超时；未知 usage 不能按零计入均值。
    latest_low = [json.loads(p.read_text())["recorded"][-1] for p in
        (ROOT / "fixed_inputs/recordings/ac2-fixed-pro-minimal/reasoning-low/repeat-1").glob("r4_*.json")]
    known = [r["usage"] for r in latest_low if r.get("usage")]
    assert len(latest_low) == 8 and len(known) == 7
    latest_output = statistics.mean(u["output_tokens"] for u in known)
    latest_seconds = statistics.median(r["elapsed_ms"] for r in latest_low) / 1000
    latest_full = [dict(row, estimate=dict(row["estimate"]), mean=dict(row["mean"])) for row in full]
    latest_full[4]["estimate"]["output_tokens"] = round(latest_output * full_compute +
        full[4]["mean"]["output_tokens"] * (len(questions) - full_compute))
    latest_full[4]["output_basis"] = "AE14 low已知7次均值 + 首轮minimal均值，按7/56分流"
    result["ae15"] = {
        "basis": "AE14固定小集最新low；超时未知usage不计零；输入/W1/W4及minimal仍是代理，非费用或上界",
        "r4_low": {"observed_calls": len(latest_low), "known_usage_calls": len(known),
            "unknown_usage_calls": len(latest_low) - len(known),
            "mean_input_tokens": statistics.mean(u["input_tokens"] for u in known),
            "mean_output_tokens": latest_output, "median_seconds": latest_seconds,
            "observed_timeout_seconds": 180, "new_timeout_seconds": 240},
        "full": {"op_source": "query_meta.operation；实际只取已核验R1计划，失败走minimal",
            "r4_compute_low": full_compute, "r4_other_minimal": len(questions) - full_compute,
            "stages": latest_full, "total_estimate": {k: sum(row["estimate"][k] for row in latest_full)
                for k in ["input_tokens", "output_tokens"]},
            "r4_serial_seconds": round(full_compute * latest_seconds + (len(questions) - full_compute) * 4.5)},
        "limits": "low仅8个失败固定输入，1个超时使延迟右截断且缺usage；240秒下完成率/用量未经测量，p50相加不是全量墙钟保证。"}
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, help="显式写出固定集；缺省只打印 token 估算")
    args = parser.parse_args()
    data = extract()
    if args.output:
        args.output.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(estimate(data), ensure_ascii=False, indent=2))
