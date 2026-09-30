"""Structural checks for the W4 sample set (contracts.md 2.4, w4.v1).

Checks only what code can check: every quote occurs verbatim in its
source, ids and references are consistent, and each expected decision
obeys the w4.v1 output rules. Whether the expected action is the right
judgement is for human review (label_status).

    python3 tests/eval/w4/check_samples.py
"""
from __future__ import annotations

import datetime as dt
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
TZ = dt.timezone(dt.timedelta(hours=8))
ACTIONS = {"new", "reinforce", "state_change", "correction", "refine", "retract", "coexist", "conflict"}
PENDING = {"target_unknown", "scope_incomplete", "unclear_relation"}
NEED_TARGETS = {"state_change", "correction", "refine", "retract", "conflict"}


def load_samples():
    return [json.loads(p.read_text(encoding="utf-8")) for p in sorted((HERE / "samples").glob("*.json"))]


def _quotes(c):
    yield "sentence", c["sentence"]["text"]
    if c.get("value") and c["value"].get("quote"):
        yield "value", c["value"]["quote"]["text"]
    for q in c.get("conditions", []):
        yield "condition", q["quote"]["text"]
    for t in c.get("times", []):
        yield "time", t["quote"]["text"]
    if c.get("uncertainty"):
        yield "uncertainty", c["uncertainty"]["quote"]["text"]


def _month_bounds(said_on: str, offset: int):
    d = dt.date.fromisoformat(said_on)
    y, m = divmod(d.month - 1 + offset, 12)
    start = dt.date(d.year + y, m + 1, 1)
    y2, m2 = divmod(start.month, 12)
    end = dt.date(start.year + y2, m2 + 1, 1)
    ts = lambda x: int(dt.datetime(x.year, x.month, x.day, tzinfo=TZ).timestamp())
    return ts(start), ts(end)


def _check_times(tag, times, resolutions, said_on, errs):
    """One TimeResolution per TimeInterp, consistent with its kind (contracts 1.2)."""
    if len(resolutions) != len(times):
        errs.append(f"{tag}：时间 {len(times)} 个、换算结果 {len(resolutions)} 个，数量不符")
        return
    for t, res in zip(times, resolutions):
        it = t["interp"]
        kind = it["kind"]
        if kind in ("recurring", "unknown"):
            if res != {"status": "unknown", "start": None, "end_exclusive": None, "precision": "unknown"}:
                errs.append(f"{tag}：{kind} 时间 {t['quote']['text']!r} 须保留原文且换算状态为 unknown，不生成日期区间")
        elif kind == "relative" and it["unit"] == "month":
            if res.get("status") != "resolved" or res.get("precision") != "month" or \
                    (res.get("start"), res.get("end_exclusive")) != _month_bounds(said_on, it["offset"]):
                errs.append(f"{tag}：相对月份 {t['quote']['text']!r} 的换算与接收日不符或精度不是月")
        elif kind == "relative" and it["unit"] == "day":
            if res.get("status") != "resolved" or res.get("precision") != "day":
                errs.append(f"{tag}：相对日期须换算到日精度")


def _uses_generic_fact(inp):
    return any(c["candidate"]["relation"] == "fact" for c in inp["candidates"]) or \
        any(r["candidate"]["relation"] == "fact" for r in inp["records"] if r.get("format") == "current")


def check(s) -> list:
    errs = []
    src = s["sources"]
    inp = s["input"]
    rec_ids = [r["id"] for r in inp["records"]]
    if len(set(rec_ids)) != len(rec_ids):
        errs.append("记录 ID 重复")
    rel_ids = {r["id"] for r in inp["relations"]}
    for rel in inp["relations"]:
        if not set(rel["members"]) <= set(rec_ids) or len(rel["members"]) < 2:
            errs.append(f"关系 {rel['id']} 的成员须至少两个且都在输入记录中")
        for e in rel.get("basis", []):
            sid = e["source"]["source_id"]
            if sid not in src or e["quote"]["text"] not in src[sid]["text"]:
                errs.append(f"关系 {rel['id']} 的依据不在来源 {sid} 中")
    for r in inp["records"]:
        for e in r["evidence"]:
            sid = e["source"]["source_id"]
            if sid not in src or e["quote"]["text"] not in src[sid]["text"]:
                errs.append(f"记录 {r['id']} 的依据不在来源 {sid} 中")
        if r.get("format") == "legacy":
            # contracts 3.2: legacy records carry no Candidate; gaps are listed
            # and the update state is unknown (null), never assumed judged.
            if not r.get("gaps"):
                errs.append(f"legacy 记录 {r['id']} 须列出 gaps")
            if r.get("update") is not None:
                errs.append(f"legacy 记录 {r['id']} 的 update 应为 null（未知）")
        else:
            for where, q in _quotes(r["candidate"]):
                sids = [e["source"]["source_id"] for e in r["evidence"]]
                if not any(q in src[x]["text"] for x in sids if x in src):
                    errs.append(f"记录 {r['id']} 的 {where} 原文 {q!r} 不在其来源中")
            _check_times(f"记录 {r['id']}", r["candidate"].get("times", []), r.get("time_resolutions", []),
                         r["said_on"], errs)
        for rel in r.get("open_relations", []):
            if rel not in rel_ids:
                errs.append(f"记录 {r['id']} 引用了不存在的关系 {rel}")
    for c in inp["candidates"]:
        sid = c["source"]["source_id"]
        if sid not in src:
            errs.append(f"候选 {c['index']} 的来源 {sid} 不存在")
            continue
        if c["said_on"] != src[sid]["said_on"]:
            errs.append(f"候选 {c['index']} 的 said_on 与来源不符")
        for where, q in _quotes(c["candidate"]):
            if q not in src[sid]["text"]:
                errs.append(f"候选 {c['index']} 的 {where} 原文 {q!r} 不在 {sid} 中")
        cd = c["candidate"]
        if cd["kind"] == "assertion" and cd["value"] is None:
            errs.append("assertion 必须有值")
        if cd["kind"] == "retraction" and cd["value"] is not None:
            errs.append("retraction 不应有值")
        _check_times(f"候选 {c['index']}", cd.get("times", []), c.get("time_resolutions", []), c["said_on"], errs)
    exp = s["expect"]
    if s["call_w4"]:
        decs = exp.get("decisions", [])
        if sorted(d["candidate"] for d in decs) != sorted(c["index"] for c in inp["candidates"]):
            errs.append("每个候选必须恰好一个决定")
        for d in decs:
            tag = f"决定 {d['candidate']}"
            if len(set(d["targets"])) != len(d["targets"]) or not set(d["targets"]) <= set(rec_ids):
                errs.append(f"{tag}：targets 重复或不在输入记录中")
            if d["status"] == "decided":
                if d["action"] not in ACTIONS or d["pending_reason"] is not None:
                    errs.append(f"{tag}：decided 须有合法 action 且 pending_reason 为 null")
                if d["action"] in ("new", "coexist") and d["targets"]:
                    errs.append(f"{tag}：{d['action']} 的 targets 必须为空")
                if d["action"] == "reinforce" and len(d["targets"]) != 1:
                    errs.append(f"{tag}：reinforce 只能有一个目标")
                if d["action"] in NEED_TARGETS and not d["targets"]:
                    errs.append(f"{tag}：{d['action']} 至少一个目标")
                if d["resolves_relations"] and d["action"] in ("new", "coexist", "conflict"):
                    errs.append(f"{tag}：{d['action']} 不能解决关系")
            elif d["status"] == "pending":
                if d["action"] is not None or d["pending_reason"] not in PENDING:
                    errs.append(f"{tag}：pending 的 action 须为 null 且有合法 pending_reason")
                if d["resolves_relations"]:
                    errs.append(f"{tag}：pending 不能解决关系")
            else:
                errs.append(f"{tag}：status 非法")
            if not set(d["resolves_relations"]) <= {r["id"] for r in inp["relations"] if r["status"] == "open"}:
                errs.append(f"{tag}：只能解决输入中 open 的关系")
            cand_src = next(c["source"]["source_id"] for c in inp["candidates"] if c["index"] == d["candidate"])
            need = {cand_src} | {e["source"]["source_id"] for r in inp["records"] if r["id"] in d["targets"]
                                 for e in r["evidence"]}
            if not need <= set(d["basis_sources"]) or not set(d["basis_sources"]) <= set(src):
                errs.append(f"{tag}：依据须覆盖新候选及全部目标的来源")
            eff = d.get("effective")
            if eff is not None:
                cin = next(c for c in inp["candidates"] if c["index"] == d["candidate"])
                times = cin["candidate"].get("times", [])
                if eff["role"] not in ("event", "validity"):
                    errs.append(f"{tag}：effective 只能是事件或有效期角色")
                if eff not in times:
                    errs.append(f"{tag}：effective 必须是新候选已核验的时间之一（逐字段相同）")
                # A verified time may still be unresolvable ("那阵子"): the
                # resolution is then `unknown` and the old record ends with
                # end_unknown; only a date-precise resolution fixes the end.
    else:
        cp = exp.get("code_path")
        if not cp or cp.get("w4_called") is not False:
            errs.append("不调用 W4 的样本须写明 code_path")
        if "decisions" in exp:
            errs.append("不调用 W4 的样本不应有 decisions")
    # Generic `fact` retrieval never guarantees full coverage (contracts 2.4);
    # with incomplete coverage no update may be recorded as fully judged.
    if _uses_generic_fact(inp) and inp["coverage"]["complete"]:
        errs.append("通用 fact 只按本次全文检索取候选，coverage.complete 不能为 true")
    if not inp["coverage"]["complete"]:
        # Only what this run commits or re-judges carries the scope gap: the
        # new record(s), the candidate's receipt, and re-judged committed ids.
        # Existing records left untouched keep their own update state.
        after = exp.get("after", {})
        rejudged = {str(c["committed_id"]) for c in inp["candidates"] if c.get("committed_id") is not None}
        states = [after.get("candidate_update")] + [v.get("update") for k, v in after.get("records", {}).items()
                                                   if (k == "new" or k.startswith("new") or k in rejudged)
                                                   and isinstance(v, dict) and "update" in v]
        states += [exp.get("code_path", {}).get("update")]
        states = [x for x in states if x]
        if not states:
            errs.append("范围不完整的样本须写明提交后的更新状态")
        for u in states:
            if u.get("status") != "pending" or "update_scope_incomplete" not in u.get("reasons", []):
                errs.append("范围不完整时更新状态须为 pending 且含 update_scope_incomplete")
    for k in ("must_not", "after"):
        if k not in exp:
            errs.append(f"缺少 expect.{k}")
    return errs


def main():
    bad = 0
    for s in load_samples():
        errs = check(s)
        bad += bool(errs)
        print(("FAIL " if errs else "ok   ") + s["id"] + ("：" + "；".join(errs) if errs else ""))
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
