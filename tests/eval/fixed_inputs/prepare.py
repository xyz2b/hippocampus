#!/usr/bin/env python3
"""从首轮 live 结果冻结失败阶段输入；不访问网络，不修改原录制。"""
import argparse
import hashlib
import json
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
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, help="显式写出固定集；缺省只打印 token 估算")
    args = parser.parse_args()
    data = extract()
    if args.output:
        args.output.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(estimate(data), ensure_ascii=False, indent=2))
