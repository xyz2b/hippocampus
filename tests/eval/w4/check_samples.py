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
# Field set of the serialized pre-phase-1 src/model.rs Memory (LegacyMemory,
# contracts 3.2). tests/w4_legacy_fixture.rs additionally decodes it in Rust.
LEGACY_MEMORY_FIELDS = {
    "id", "owner_id", "evidence", "subject_id", "memory_type", "predicate", "value", "polarity", "strength",
    "scope", "retention_tier", "status", "temporal", "extraction_confidence", "certainty", "supersedes",
    "superseded_by", "supersede_reason", "explicit_memory_request", "reinforcement_count", "retrieval_count",
    "last_reinforced_at", "last_retrieved_at", "tags"}
LEGACY_EVIDENCE_FIELDS = {"id", "session_id", "turn_id", "quote"}
# Actions that end their targets, with the allowed termination reasons (contracts 2.4 table).
ENDING = {"state_change": {"StateChanged", "PreferenceChanged", "ConstraintChanged"},
          "correction": {"Correction"}, "refine": {"Refinement", "ScopeRefined"}, "retract": {"UserRetracted"}}


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


def _whole_sentences(sentence, source):
    """True if `sentence` is a run of consecutive reviewer-marked sentences of the source.

    `sources.<id>.sentences` lists the source's complete sentences in order;
    without it the whole text is one sentence. The span runs from the start of
    the first to the end of the last, so whitespace or newlines between
    sentences are kept as written and no punctuation rule is guessed.
    """
    text = source["text"]
    spans, pos = [], 0
    for sent in source.get("sentences", [text]):
        i = text.find(sent, pos)
        if i < 0:
            return False
        spans.append((i, i + len(sent)))
        pos = i + len(sent)
    return any(text[spans[a][0]:spans[b][1]] == sentence for a in range(len(spans)) for b in range(a, len(spans)))


def _check_qualifiers(tag, c, marks, source_text, errs):
    """The assertion keeps exactly the qualifiers marked for it (qualifier_marks[key])."""
    for kind in marks:
        for q in marks[kind]:
            if q not in source_text:
                errs.append(f"{tag}：标注的 {kind} {q!r} 不在原文中")
    kept = {"times": [t["quote"]["text"] for t in c.get("times", [])],
            "conditions": [q["quote"]["text"] for q in c.get("conditions", [])],
            "uncertainty": [c["uncertainty"]["quote"]["text"]] if c.get("uncertainty") else []}
    for kind, got in kept.items():
        want = marks.get(kind, [])
        if sorted(got) != sorted(want):
            errs.append(f"{tag}：{kind} 保留为 {got}，本断言标注为 {want}（无法换算的时间也须以 unknown 保留）")
    if marks.get("strength") and c.get("strength") is None:
        errs.append(f"{tag}：本断言标注有强度表达，strength 不能为 null")
    if not marks.get("strength") and c.get("strength") is not None:
        errs.append(f"{tag}：本断言没有标注强度表达，strength 应为 null")


def _check_resolutions(inp, decs, after, errs):
    """A resolved conflict must leave at most the named survivors current.

    Closing the relation while every member stays active would present both
    sides as current conclusions; each non-survivor must be ended by a
    decision in the same sample (contracts 2.4: members fully covered).
    """
    rels = {r["id"]: r for r in inp["relations"]}
    claimed = {x for d in decs for x in d["resolves_relations"]}
    ended_by = {t: d["action"] for d in decs if d["status"] == "decided" and d["action"] in ENDING for t in d["targets"]}
    listed = {r["id"]: r for r in after.get("relations", []) if r.get("status") == "resolved" and "id" in r}
    for rid in claimed - set(listed):
        errs.append(f"关系 {rid} 被决定解决，但执行后状态未写明 resolved")
    for rid, r in listed.items():
        if rid not in claimed:
            errs.append(f"关系 {rid} 标为 resolved，但没有决定给出 resolves_relations")
            continue
        members = set(rels[rid]["members"])
        surv = r.get("survivors")
        if surv is None or not set(surv) <= members or len(set(surv)) >= len(members):
            errs.append(f"关系 {rid} 解决后须写明 survivors，且只能是部分成员")
            continue
        for m in members - set(surv):
            st = after.get("records", {}).get(str(m), {})
            if m not in ended_by or st.get("stored_status") != "superseded" or \
                    st.get("termination_reason") not in ENDING[ended_by[m]]:
                errs.append(f"关系 {rid} 的成员 {m} 不是保留方，须由本次的终止动作结束（强化等不算）")


def _produced(inp, decs, after, errs):
    """Bind each committed or reused record in `after` to the candidate that produced it.

    Any after.records key that is not an input record is a produced record and
    must name `from_candidate`; a re-judged candidate (committed_id) produces
    exactly its committed id. Replacements from state_change/correction/refine
    must keep the complete predecessor chain (history ⊇ targets).
    """
    input_ids = {str(r["id"]) for r in inp["records"]}
    cands = {c["index"]: c for c in inp["candidates"]}
    by_cand = {}
    for k, v in after.get("records", {}).items():
        if k in input_ids:
            continue
        fc = v.get("from_candidate") if isinstance(v, dict) else None
        if fc not in cands:
            errs.append(f"执行后记录 {k} 须用 from_candidate 绑定到产生它的候选")
            continue
        cid = cands[fc].get("committed_id")
        if cid is not None and k != str(cid):
            errs.append(f"候选 {fc} 是重判（committed_id={cid}），只能复用记录 {cid}，不能产生 {k}")
        if fc in by_cand:
            errs.append(f"候选 {fc} 绑定了多条执行后记录")
        by_cand[fc] = (k, v)
    for d in decs:
        if d["status"] == "decided" and d["action"] in ("state_change", "correction", "refine"):
            got = by_cand.get(d["candidate"])
            if not got:
                errs.append(f"决定 {d['candidate']}：{d['action']} 须有绑定的替代记录")
            elif not set(d["targets"]) <= set(got[1].get("history") or []):
                errs.append(f"决定 {d['candidate']}：替代记录 {got[0]} 的 history 须包含全部目标 {d['targets']}")
    return by_cand


def _check_unresolved_replacement(inp, decs, after, by_cand, errs):
    """contracts 2.5: ending a member of an open relation without resolving it keeps
    the relation and its members as they were; the record that replaces it
    (bound by candidate, new or re-judged) is update pending with
    update_relation_unclear. Records of unrelated candidates are unaffected."""
    claimed = {x for d in decs for x in d["resolves_relations"]}
    listed = {r.get("id"): r for r in after.get("relations", [])}
    for rel in inp["relations"]:
        if rel["status"] != "open" or rel["id"] in claimed:
            continue
        hits = [d for d in decs if d["status"] == "decided" and d["action"] in ENDING
                and set(d["targets"]) & set(rel["members"])]
        if not hits:
            continue
        out = listed.get(rel["id"])
        if not out or out.get("status") != "open" or sorted(out.get("members", [])) != sorted(rel["members"]):
            errs.append(f"关系 {rel['id']} 未被解决：执行后须仍为 open，成员保持 {sorted(rel['members'])}")
        for d in hits:
            if d["action"] == "retract":
                continue
            k, v = by_cand.get(d["candidate"], (None, {}))
            u = v.get("update") or {}
            if u.get("status") != "pending" or "update_relation_unclear" not in u.get("reasons", []):
                errs.append(f"替代 {rel['id']} 成员的记录 {k} 须为 pending 且原因含 update_relation_unclear")


def _check_target_states(decs, after, errs):
    """Ending actions end their targets with a matching reason; other outcomes leave them current."""
    recs = after.get("records", {})
    for d in decs:
        for t in d["targets"]:
            st = recs.get(str(t))
            if st and "stored_status" not in st and st.get("format") == "legacy" and "status" in st:
                st = {**st, "stored_status": st["status"].lower()}
            if not st or "stored_status" not in st:
                errs.append(f"决定 {d['candidate']}：目标 {t} 缺执行后的状态（after.records 须写明 stored_status）")
                continue
            if d["status"] == "decided" and d["action"] in ENDING:
                if st["stored_status"] != "superseded" or st.get("termination_reason") not in ENDING[d["action"]]:
                    errs.append(f"决定 {d['candidate']}：{d['action']} 的目标 {t} 须结束，原因为 {sorted(ENDING[d['action']])}")
            elif st["stored_status"] == "superseded":
                errs.append(f"决定 {d['candidate']}：{d['action'] or d['status']} 不结束目标 {t}")


def check(s) -> list:
    errs = []
    src = s["sources"]
    inp = s["input"]
    marks = s.get("qualifier_marks", {})
    item_keys = {f"candidate:{c['index']}" for c in inp["candidates"]} | \
        {f"record:{r['id']}" for r in inp["records"] if r.get("format") == "current"}
    for k in set(marks) - item_keys:
        errs.append(f"限定语标注 {k} 不对应任何候选或完整记录")
    rec_ids = [r["id"] for r in inp["records"]]
    if len(set(rec_ids)) != len(rec_ids):
        errs.append("记录 ID 重复")
    rel_ids = {r["id"] for r in inp["relations"]}
    for rel in inp["relations"]:
        if not set(rel["members"]) <= set(rec_ids) or len(set(rel["members"])) < 2 or \
                len(set(rel["members"])) != len(rel["members"]):
            errs.append(f"关系 {rel['id']} 的成员须至少两个、不重复且都在输入记录中")
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
            data = r.get("data") or {}
            if set(data) != LEGACY_MEMORY_FIELDS:
                errs.append(f"legacy 记录 {r['id']} 的 data 不是完整的旧 Memory："
                            f"缺 {sorted(LEGACY_MEMORY_FIELDS - set(data))}，多 {sorted(set(data) - LEGACY_MEMORY_FIELDS)}")
            elif data["id"] != r["id"]:
                errs.append(f"legacy 记录 {r['id']} 的 data.id 与记录 ID 不符")
            elif not data["evidence"] or any(set(e) != LEGACY_EVIDENCE_FIELDS for e in data["evidence"]):
                errs.append(f"legacy 记录 {r['id']} 的旧 Evidence 须为 id/session_id/turn_id/quote")
            if not r["evidence"] and "source_missing" not in r.get("gaps", []):
                errs.append(f"legacy 记录 {r['id']} 没有可链接依据，gaps 须含 source_missing")
        else:
            for where, q in _quotes(r["candidate"]):
                sids = [e["source"]["source_id"] for e in r["evidence"]]
                if not any(q in src[x]["text"] for x in sids if x in src):
                    errs.append(f"记录 {r['id']} 的 {where} 原文 {q!r} 不在其来源中")
            _check_times(f"记录 {r['id']}", r["candidate"].get("times", []), r.get("time_resolutions", []),
                         r["said_on"], errs)
            rsid = r["evidence"][0]["source"]["source_id"] if r["evidence"] else None
            if rsid in src:
                _check_qualifiers(f"记录 {r['id']}", r["candidate"], marks.get(f"record:{r['id']}", {}),
                                  src[rsid]["text"], errs)
                if not _whole_sentences(r["candidate"]["sentence"]["text"], src[rsid]):
                    errs.append(f"记录 {r['id']}：sentence 须为完整原句")
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
        _check_qualifiers(f"候选 {c['index']}", cd, marks.get(f"candidate:{c['index']}", {}), src[sid]["text"], errs)
        if not _whole_sentences(cd["sentence"]["text"], src[sid]):
            errs.append(f"候选 {c['index']}：sentence 须为完整原句，不能截取片段")
        if cd["relation"] == "fact" and cd["value"] and cd["value"]["quote"]["text"] != cd["sentence"]["text"]:
            errs.append(f"候选 {c['index']}：通用 fact 的 value.quote 须与 sentence 相同")
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
        _check_resolutions(inp, decs, exp.get("after", {}), errs)
        _check_target_states(decs, exp.get("after", {}), errs)
        by_cand = _produced(inp, decs, exp.get("after", {}), errs)
        _check_unresolved_replacement(inp, decs, exp.get("after", {}), by_cand, errs)
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
