"""AF1–AF4 targeted capacity plan; reads AC2 only, never sends or changes recordings."""
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
