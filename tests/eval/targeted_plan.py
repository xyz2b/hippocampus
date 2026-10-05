"""Prepare targeted AC2 copies and capacity reports; never sends or changes original recordings."""
import argparse
import collections
import hashlib
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent
GROUPS = {
    "C": ["lme_603deb26/q", "device_default_paper/device_q3", "device_default_paper/device_q4",
          "device_default_paper/device_q5", "event_registration_fee/event_q3", "event_registration_fee/event_q4",
          "explicit_correction/correction_q1", "organization_borrow_limit/organization_q3",
          "organization_borrow_limit/organization_q5", "unresolved_conflict/conflict_q1"],
    "R1": ["lme_5831f84d/q", "task_isolation/task_q2"],
    "account": ["lme_gpt4_b5700ca9/q", "lme_gpt4_e072b769/q",
                "unresolved_conflict/conflict_q2", "unresolved_conflict/conflict_q3"],
}
STAGES = ("W1", "W3", "W4", "R1", "R4")
SCHEMAS = {"w1_v1": "W1", "w3_v1": "W3", "w4_v2": "W4", "w4_v3": "W4",
           "r1_v1": "R1", "r4_v3": "R4"}
SOURCE = ROOT / "end_to_end/ac2-pro-medium-pro-minimal-pro-minimal-r4-compute-low"


def model_output(row):
    body = json.loads(row["body"])
    text = "".join(part["text"] for message in body.get("output", [])
                   for part in message.get("content", []) if part.get("type") == "output_text")
    return json.loads(text)


def prepare(source, destination, targets=None):
    """Copy successful prefixes only; retained hashes are never rewritten or certified reusable."""
    source, destination = pathlib.Path(source).resolve(), pathlib.Path(destination).resolve()
    if destination == source or source in destination.parents or destination.exists():
        raise ValueError("目标必须为原录制目录之外的新目录；不覆盖任何已有文件")
    targets = sorted(targets or {q for qs in GROUPS.values() for q in qs})
    cases = selected_cases(targets)
    run = json.loads((source / "run.json").read_text())
    # Reuse the report's local timeout/decode/validation diagnosis, including late completed responses.
    sys.path.insert(0, str(ROOT / "scoring"))
    import compare
    old_cases = {c["case"]: c for c in run["cases"]}
    prepared, manifest_cases = {}, {}
    retained, removed, reserved = (dict.fromkeys(STAGES, 0) for _ in range(3))
    reasons = collections.Counter()
    for case in cases:
        cid = case["id"]
        original = json.loads((source / (cid + ".json")).read_text())
        outcomes = compare.local_calls(old_cases[cid])
        prefixes, slots, ops = {}, {}, {}
        wanted = [("write:" + m["id"], i) for i, m in enumerate(case["messages"])]
        wanted += [("recall:" + q["id"], None) for q in case["questions"]]
        for op, index in wanted:
            rows = original["operations"].get(op, [])
            keep = []
            cut = None
            for row in rows:
                stage = SCHEMAS[row["schema"]]
                local = outcomes.get((op, stage), [{}])[0].get("local_outcome", "unknown")
                try:
                    body = json.loads(row["body"]) if row.get("body") else {}
                except ValueError:
                    body = {}
                if not isinstance(body, dict):
                    body = {}
                if cut is None:
                    if (row.get("transport_error") or not 200 <= row["http_status"] < 300
                            or body.get("status") != "completed" or body.get("error")):
                        cut = "调用失败"
                    elif local not in ("completed", "unknown"):
                        cut = "本地" + local
                    elif stage == "W4":
                        cut = "W4契约/输入已改变"
                    elif stage == "R4":
                        cut = "R4证据包须重建"
                if cut is None:
                    keep.append(row)
                    retained[stage] += 1
                else:
                    removed[stage] += 1
                    reasons[cut] += 1
            ops[op] = keep
            prefixes[op] = [r["request"] for r in keep]
            possible = ["W1", "W3"] + (["W4"] if index and index > 0 else []) if index is not None else ["R1", "R4"]
            frozen = {SCHEMAS[r["schema"]]: r for r in keep}
            if "W1" in frozen and not model_output(frozen["W1"]).get("candidates"):
                possible = ["W1"]
            elif "W3" in frozen and not any(r["verdict"] == "supported"
                                           for r in model_output(frozen["W3"])["reviews"]):
                possible = [s for s in possible if s != "W4"]
            if "R1" in frozen and model_output(frozen["R1"])["need"] != "judge":
                possible = ["R1"]
            slots[op] = [s for s in possible if s not in frozen]
            for stage in slots[op]:
                reserved[stage] += 1
        prepared[cid] = dict(original, operations=ops)
        manifest_cases[cid] = {"prefixes": prefixes, "slots": slots}
    manifest = {"version": "eval.e2e.targeted@1", "source": str(source),
                "source_run_sha256": hashlib.sha256((source / "run.json").read_bytes()).hexdigest(),
                "questions": targets, "cases": manifest_cases,
                "retained": retained, "removed": removed, "reserved": reserved,
                "call_ceiling": sum(reserved.values()), "removal_reasons": dict(reasons),
                "count_semantics": "已知录制数精确；reserved是未决模型输出下的调用上限，非最终真实发送数"}
    # Validate everything before creating the new directory; never read credentials or send.
    destination.mkdir(parents=True)
    for cid, doc in prepared.items():
        (destination / (cid + ".json")).write_text(json.dumps(doc, ensure_ascii=False, indent=2) + "\n")
    (destination / "target-plan.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
    print(f"已生成 {destination}：{len(cases)}例/{len(targets)}题；原AC2未修改；真实调用0。")
    print("阶段 | 保留成功前缀 | 移除录制 | 未决调用上限")
    for stage in STAGES:
        print(f"{stage} | {retained[stage]} | {removed[stage]} | {reserved[stage]}")
    print(f"保留{sum(retained.values())}；移除{sum(removed.values())}；未决调用上限{manifest['call_ceiling']}。")
    print(manifest["count_semantics"])
    return manifest


def dry_run(directory, limit=None):
    directory = pathlib.Path(directory).resolve()
    manifest = json.loads((directory / "target-plan.json").read_text())
    if manifest["version"] != "eval.e2e.targeted@1":
        raise ValueError("未知定点计划版本")
    actual = dict.fromkeys(STAGES, 0)
    for cid, case in manifest["cases"].items():
        doc = json.loads((directory / (cid + ".json")).read_text())
        for op, prefix in case["prefixes"].items():
            if [r["request"] for r in doc["operations"][op][:len(prefix)]] != prefix:
                raise ValueError("整批拒绝：保留前缀已改变")
            for stage in case["slots"][op]:
                actual[stage] += 1
    if actual != manifest["reserved"] or sum(actual.values()) != manifest["call_ceiling"]:
        raise ValueError("整批拒绝：计划调用上限不符")
    if limit is not None and limit < manifest["call_ceiling"]:
        raise ValueError(f"整批拒绝：EVAL_RECORD={limit} < 预留上限{manifest['call_ceiling']}；未读取凭据、未发送")
    source = pathlib.Path(manifest["source"])
    if hashlib.sha256((source / "run.json").read_bytes()).hexdigest() != manifest["source_run_sha256"]:
        raise ValueError("整批拒绝：原AC2运行与准备时绑定不符")
    means = {s: [] for s in STAGES}
    routes = {r: [] for r in ("low", "minimal")}
    for case in json.loads((source / "run.json").read_text())["cases"]:
        for call in case.get("calls", []):
            usage = call.get("usage") or {}
            if all(isinstance(usage.get(k), int) for k in ("input_tokens", "output_tokens")):
                means[call["stage"]].append(usage)
        doc = json.loads((source / (case["case"] + ".json")).read_text())
        for rows in doc["operations"].values():
            for row in rows:
                usage = row.get("usage") or {}
                if row["schema"] == "r4_v3" and all(isinstance(usage.get(k), int) for k in ("input_tokens", "output_tokens")):
                    routes[row["request_content"]["reasoning"]["effort"]].append(usage)
    tokens = {s: [round(actual[s] * sum(u[k] for u in means[s]) / len(means[s]))
                  for k in ("input_tokens", "output_tokens")] for s in STAGES}
    routing = collections.Counter()
    for case in selected_cases(manifest["questions"]):
        doc = json.loads((directory / (case["id"] + ".json")).read_text())
        for q in case["questions"]:
            op = "recall:" + q["id"]
            if "R4" not in manifest["cases"][case["id"]]["slots"][op]:
                continue
            r1 = next((r for r in doc["operations"][op] if r["schema"] == "r1_v1"), None)
            operation = model_output(r1)["plan"]["op"] if r1 else q["query_meta"]["operation"]
            routing["low" if operation in ("count", "sum", "elapsed", "compare") else "minimal"] += 1
    tokens["R4"] = [round(sum(n * sum(u[k] for u in routes[r]) / len(routes[r]) for r,n in routing.items()))
                    for k in ("input_tokens", "output_tokens")]
    print(f"定点目录 {directory}：{len(manifest['questions'])}题/{len(manifest['cases'])}例；EVAL_RECORD={limit}。")
    print("阶段 | 保留成功前缀 | 移除录制 | 未决调用上限 | 估算输入/输出token | AC2已知usage样本")
    for stage in STAGES:
        print(f"{stage} | {manifest['retained'][stage]} | {manifest['removed'][stage]} | {actual[stage]} | {tokens[stage][0]}/{tokens[stage][1]} | {len(means[stage])}")
    inputs, outputs = (sum(v[i] for v in tokens.values()) for i in range(2))
    print(f"保留{sum(manifest['retained'].values())}/移除{sum(manifest['removed'].values())}为实际目录计数；预留上限{sum(actual.values())}；输入约{inputs}，输出约{outputs}；AC2约50元比例粗估约{50*(inputs+outputs)/(2070551+1773736):.2f}元，非报价或费用上界。")
    print(f"R4路由估算{dict(routing)}；已保留R1按输出，缺R1按query_meta容量假设，实际仍由已核验R1决定。")
    print("未来模型输出未知，不能精确预测最终发送数；仅已知录制数与预留上限精确。保留前缀指纹变化禁止自动补发，旧指纹不改写。真实调用0。")
    return manifest


def selected_cases(targets):
    selected = []
    found = set()
    for path in sorted((ROOT / "cases").glob("*.json")):
        case = json.loads(path.read_text())
        qs = [q for q in case["questions"] if f"{case['id']}/{q['id']}" in targets]
        if not qs:
            continue
        found.update(f"{case['id']}/{q['id']}" for q in qs)
        selected.append(dict(case, questions=qs, messages=case["messages"][:max(q['after'] for q in qs)]))
    if found != set(targets):
        raise ValueError("unavailable targeted question")
    return selected


def plan(run, targets, recording_dir):
    cases = selected_cases(targets)
    observed, usage, count = dict.fromkeys(STAGES, 0), {s: [0, 0] for s in STAGES}, dict.fromkeys(STAGES, 0)
    failed = dict.fromkeys(STAGES, 0)
    ceiling = dict.fromkeys(STAGES, 0)
    for c in cases:
        m, q = len(c["messages"]), len(c["questions"])
        for s, n in zip(STAGES, (m, m, max(m - 1, 0), q, q)):
            ceiling[s] += n
        ops = {f"write:{x['id']}" for x in c['messages']} | {f"recall:{x['id']}" for x in c['questions']}
        old = next(x for x in run["cases"] if x["case"] == c["id"])
        for call in old.get("calls", []):
            if call["operation"] not in ops:
                continue
            s = call["stage"]
            observed[s] += 1
            failed[s] += (call.get("http_status") or 0) >= 400 or bool(call.get("transport_error"))
            u = call.get("usage") or {}
            if all(isinstance(u.get(k), int) for k in ("input_tokens", "output_tokens")):
                count[s] += 1
                for i, k in enumerate(("input_tokens", "output_tokens")):
                    usage[s][i] += u[k]
    # W1/W3/R1 unchanged requests may replay. Failed upstream calls need explicit reruns;
    # a repaired W1 may newly call W3. All W4 are v3; selected R4 packages must be regenerated.
    rerecord = {"W1": failed["W1"], "W3": failed["W3"] + failed["W1"],
                "W4": ceiling["W4"], "R1": failed["R1"], "R4": ceiling["R4"]}
    global_means = {}
    for s in STAGES:
        known = [c["usage"] for case in run["cases"] for c in case.get("calls", [])
                 if c["stage"] == s and all(isinstance((c.get("usage") or {}).get(k), int)
                                           for k in ("input_tokens", "output_tokens"))]
        global_means[s] = [sum(u[k] for u in known) / len(known) for k in ("input_tokens", "output_tokens")]
    estimate = {s: [round(rerecord[s] * (usage[s][i] / count[s] if count[s] else global_means[s][i]))
                    for i in range(2)] for s in STAGES}
    # Account failures have no usable R1 output. For these only, query_meta is a routing
    # capacity proxy; do not estimate a repaired elapsed request with minimal's usage.
    routes = {r: [] for r in ("low", "minimal")}
    recordings = {}
    for old in run["cases"]:
        trace = json.loads((recording_dir / (old['case'] + '.json')).read_text())
        recordings[old['case']] = trace['operations']
        for rows in trace['operations'].values():
            for row in rows:
                u = row.get('usage') or {}
                if row['schema'] == 'r4_v3' and all(isinstance(u.get(k), int) for k in ('input_tokens', 'output_tokens')):
                    routes[row['request_content']['reasoning']['effort']].append(u)
    r4_tokens = [0, 0]
    for case in cases:
        for q in case['questions']:
            rows = recordings[case['id']].get('recall:' + q['id'], [])
            old_r4 = next((r for r in rows if r['schema'] == 'r4_v3'), None)
            u = (old_r4 or {}).get('usage') or {}
            if not all(isinstance(u.get(k), int) for k in ('input_tokens', 'output_tokens')):
                op = q.get('query_meta', {}).get('operation')
                route = 'low' if op in ('count', 'sum', 'elapsed', 'compare') else 'minimal'
                u = {k: sum(v[k] for v in routes[route]) / len(routes[route]) for k in ('input_tokens', 'output_tokens')}
            for i, k in enumerate(('input_tokens', 'output_tokens')):
                r4_tokens[i] += u[k]
    estimate['R4'] = [round(n) for n in r4_tokens]
    return {"questions": len(targets), "cases": len(cases), "ceiling": ceiling,
            "observed": observed, "failed": failed, "rerecord_capacity": rerecord,
            "estimate": estimate, "known_usage": count}


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "dry-run":
        parser = argparse.ArgumentParser(description="独立定点目录预检及AC2均值token估算；零调用")
        parser.add_argument("--directory", type=pathlib.Path, required=True)
        parser.add_argument("--eval-record", type=int)
        args = parser.parse_args(sys.argv[2:])
        try:
            dry_run(args.directory, args.eval_record)
        except (ValueError, OSError, KeyError) as error:
            parser.error(str(error))
        return
    if len(sys.argv) > 1 and sys.argv[1] == "prepare":
        parser = argparse.ArgumentParser(description="生成独立定点目录，保留原AC2及严格请求指纹")
        parser.add_argument("--source", type=pathlib.Path, default=SOURCE)
        parser.add_argument("--output", type=pathlib.Path, required=True)
        args = parser.parse_args(sys.argv[2:])
        try:
            prepare(args.source, args.output)
        except (ValueError, OSError, KeyError) as error:
            parser.error(str(error))
        return
    run_path = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "end_to_end/ac2-pro-medium-pro-minimal-pro-minimal-r4-compute-low/run.json"
    run = json.loads(run_path.read_text())
    groups = dict(GROUPS, union=sorted({q for qs in GROUPS.values() for q in qs}))
    for name, targets in groups.items():
        p = plan(run, targets, run_path.parent)
        print(f"{name}: {p['questions']}题/{p['cases']}例；零真实调用")
        print("阶段 | 旧实际/HTTP失败 | 不复用上限 | 定点重录容量 | 估算输入/输出token | 已知usage样本")
        for s in STAGES:
            print(f"{s} | {p['observed'][s]}/{p['failed'][s]} | {p['ceiling'][s]} | {p['rerecord_capacity'][s]} | {p['estimate'][s][0]}/{p['estimate'][s][1]} | {p['known_usage'][s]}")
        inputs, outputs = (sum(v[i] for v in p['estimate'].values()) for i in range(2))
        print(f"重录容量合计 {sum(p['rerecord_capacity'].values())}；输入约{inputs}，输出约{outputs}；按AC2约50元、输入输出token合计等比例粗估约{50*(inputs+outputs)/(2070551+1773736):.2f}元。")
    print("EVAL_QUESTIONS=" + ",".join(groups['union']))
    print("容量不是精确缺录制数：W4输入变化会改变后续分支；请求须严格核对指纹。未记录或未知usage不计零，估算不是费用/额度上限。")
    print("R4缺usage时按query_meta预计路由采用AC2 low/minimal各自均值；实际仍由获批重录后的R1决定，费用可能变化。")
    print("失败响应不会自动重试；余额确认和真实调用另行授权，保留旧审计，显式重录使用独立目录。")


if __name__ == "__main__":
    main()
