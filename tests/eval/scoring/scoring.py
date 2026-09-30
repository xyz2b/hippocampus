"""common.v1 scorer: expect_common + adapted view -> scoring items.

Only three things are judged automatically (common-scoring-view.md 3):
structural fields, whole-string equality of an old plain answer to an
annotated value, and forbidden entries decidable from those two. Everything
else is `review`. `review` and `missing` are never counted as passes.
"""
from __future__ import annotations

import datetime as _dt

from adapters import strip_edges

SCORING_VERSION = "common.v1"
PASS, FAIL, REVIEW, MISSING, NOT_MEASURABLE = "auto_pass", "auto_fail", "review", "missing", "not_measurable"
CANNOT = "format_cannot_express"
# Forbidden kinds whose hit is an acceptance blocker (common-scoring-view.md 5).
BLOCKER_KINDS = {"value_as_current": "错误替换", "condition_broadened": "条件扩大",
                 "source_out_of_scope": "来源越权"}


def _norm_src(s):
    return s


def _srcset(xs):
    return {_norm_src(x) for x in xs}


def sources_ok(spec: dict | None, srcs) -> bool:
    if spec is None:
        return True
    have = _srcset(srcs)
    all_of, any_of = _srcset(spec.get("all_of", [])), _srcset(spec.get("any_of", []))
    return all_of <= have and (not any_of or bool(any_of & have))


def _vals(value_any):
    return {strip_edges(v) for v in value_any or []}


def annotated_values(expect: dict) -> set:
    out = set()
    for f in expect.get("facts") or []:
        out |= _vals(f.get("value_any"))
    for c in expect.get("conflicts") or []:
        for m in c.get("members", []):
            out |= _vals(m.get("value_any"))
    for fb in expect.get("forbidden") or []:
        out |= _vals(fb.get("value_any"))
    return out


class _Ctx:
    def __init__(self, expect, meta, view):
        self.expect, self.meta, self.view = expect, meta, view
        self.values = annotated_values(expect)

    def plain_exact(self, a, value_any=None) -> bool:
        if not a.get("plain") or a.get("value") is None:
            return False
        pool = _vals(value_any) if value_any is not None else self.values
        return a["value"] in pool

    def polarity(self, a):
        if a["polarity"] == "affirm_if_plain":
            return "affirm" if self.plain_exact(a) else None
        return a["polarity"]

    def current(self, a):
        if a["current"] == "yes_if_plain":
            return "yes" if self.plain_exact(a) else None
        if a["current"] is None and a.get("plain"):
            return CANNOT
        return a["current"]


def _item(iid, result, basis, content=None, sources=(), flag=None):
    return {"id": iid, "result": result, "basis": basis, "flag": flag,
            "content": content, "sources": sorted(set(sources))}


def _assertion_content(a):
    return {k: a.get(k) for k in ("id", "text", "value", "sources", "unmapped_sources", "quotes", "polarity",
                                  "current", "applies", "conditions", "uncertainty", "qualifiers", "grade",
                                  "update", "record_ids", "supersedes", "plain", "until")}


def match_facts(ctx, facts, assertions):
    used, match = set(), {}
    order = sorted(range(len(facts)), key=lambda i: -len((facts[i].get("sources") or {}).get("all_of", [])))
    for i in order:
        f = facts[i]
        free = [a for a in assertions if a["id"] not in used]
        cands = [a for a in free if "sources" in f and sources_ok(f["sources"], a["sources"])]
        if not cands and "value_any" in f:
            cands = [a for a in free if ctx.plain_exact(a, f["value_any"])]
        if not cands and "sources" not in f and "value_any" not in f:
            cands = free
        if cands:
            exact = [a for a in cands if ctx.plain_exact(a, f.get("value_any"))] if "value_any" in f else []
            a = (exact or cands)[0]
            used.add(a["id"])
            match[i] = a
    # One fact and one assertion left over: they answer the same question
    # slot, so score the assertion against that fact (wrong source/value
    # become auto_fail/review instead of "missing" plus an extra).
    left_f = [i for i in range(len(facts)) if i not in match]
    left_a = [a for a in assertions if a["id"] not in used]
    if len(left_f) == 1 and len(left_a) == 1:
        match[left_f[0]] = left_a[0]
    return match


def _fact_items(ctx, i, f, a):
    p = f"facts[{i}]"
    keys = [k for k in ("value_any", "sources", "polarity", "current", "conditions", "uncertainty") if k in f]
    if a is None:
        return [_item(f"{p}.{'value' if k == 'value_any' else k}", MISSING, "没有与该事实对应的输出")
                for k in keys] + [_item(f"{p}.semantic", MISSING, "没有与该事实对应的输出")]
    c, srcs = _assertion_content(a), a["sources"]
    exact = ctx.plain_exact(a, f.get("value_any")) if "value_any" in f else False
    out = []
    other = a.get("plain") and not exact and ctx.plain_exact(a)
    if "value_any" in f:
        if exact:
            out.append(_item(f"{p}.value", PASS, "旧答案去首尾空白标点后整串等于 value_any", c, srcs))
        elif other:
            out.append(_item(f"{p}.value", FAIL, f"旧答案整串等于另一个标注值 {a['value']!r}", c, srcs))
        else:
            out.append(_item(f"{p}.value", REVIEW, "值不能整串等值判定（自由文本/statement）", c, srcs))
    if "sources" in f:
        bad = a.get("unmapped_sources") or []
        ok = sources_ok(f["sources"], srcs) and not bad
        out.append(_item(f"{p}.sources", PASS if ok else FAIL,
                         f"输出来源 {srcs}，要求 {f['sources']}" + (f"；无效引用 {bad}" if bad else ""), c, srcs))
    if "polarity" in f:
        pol = ctx.polarity(a)
        if pol is None:
            out.append(_item(f"{p}.polarity", REVIEW, "旧答案非整串值，方向需复核", c, srcs))
        else:
            out.append(_item(f"{p}.polarity", PASS if pol == f["polarity"] else FAIL,
                             f"输出 {pol}（{'整串值推定' if a['polarity'] == 'affirm_if_plain' else '结构字段'}），要求 {f['polarity']}", c, srcs))
    if "current" in f:
        cur = ctx.current(a)
        if cur == CANNOT:
            out.append(_item(f"{p}.current", MISSING, "旧格式无法表达当前资格", c, srcs, flag=CANNOT))
        elif cur is None:
            out.append(_item(f"{p}.current", REVIEW, "旧答案非整串值，当前资格需复核", c, srcs))
        else:
            out.append(_item(f"{p}.current", PASS if cur == f["current"] else FAIL,
                             f"输出 {cur}，要求 {f['current']}", c, srcs))
    if "conditions" in f:
        exp = f["conditions"]
        texts = {x["text"] for x in exp}
        required = {x["text"] for x in exp if not x.get("bound_by_query")}
        if a["conditions"] is None:
            if not exact:
                out.append(_item(f"{p}.conditions", REVIEW, "旧答案非整串值，条件需复核", c, srcs))
            elif required:
                out.append(_item(f"{p}.conditions", MISSING, f"旧格式无法表达未被问题限定的条件 {sorted(required)}",
                                 c, srcs, flag=CANNOT))
            else:
                out.append(_item(f"{p}.conditions", PASS, "整串值；所有条件已被问题限定", c, srcs))
        else:
            have = set(a["conditions"])
            if required - have:
                out.append(_item(f"{p}.conditions", FAIL, f"缺少未被问题限定的条件 {sorted(required - have)}（条件扩大）", c, srcs))
            elif not exp and have:
                out.append(_item(f"{p}.conditions", FAIL, f"标注要求无条件，输出 {sorted(have)}", c, srcs))
            elif have - texts:
                out.append(_item(f"{p}.conditions", REVIEW, f"输出了未标注的条件 {sorted(have - texts)}", c, srcs))
            else:
                out.append(_item(f"{p}.conditions", PASS, f"输出条件 {sorted(have)}", c, srcs))
    if "uncertainty" in f:
        e, o = f["uncertainty"], a["uncertainty"]
        if o == "none_if_plain":
            if not exact:
                out.append(_item(f"{p}.uncertainty", REVIEW, "旧答案非整串值，不确定表达需复核", c, srcs))
            elif e is None:
                out.append(_item(f"{p}.uncertainty", PASS, "整串值，不含不确定表达", c, srcs))
            else:
                out.append(_item(f"{p}.uncertainty", MISSING, "旧格式无法表达不确定性", c, srcs, flag=CANNOT))
        elif e is None:
            out.append(_item(f"{p}.uncertainty", PASS if o is None else FAIL, f"输出 {o!r}，要求无", c, srcs))
        elif o is None:
            out.append(_item(f"{p}.uncertainty", FAIL, "丢失不确定表达", c, srcs))
        else:
            out.append(_item(f"{p}.uncertainty", PASS if o == e else REVIEW, f"输出 {o!r}，标注 {e!r}", c, srcs))
    if exact and sources_ok(f.get("sources"), srcs):
        out.append(_item(f"{p}.semantic", PASS, "整串值且来源正确：答案只含该值，主体与关系由问题确定", c, srcs))
    elif other and "value_any" in f:
        out.append(_item(f"{p}.semantic", FAIL, f"旧答案整串等于另一个标注值 {a['value']!r}", c, srcs))
    else:
        sem = dict(c, expected_subject=f.get("subject"), relation_note=f.get("relation_note"))
        out.append(_item(f"{p}.semantic", REVIEW, "主体、关系与表述需人工复核", sem, srcs))
    return out


def _conflict_items(ctx, exp, out_conflicts):
    items, used_conflicts, extra_members = [], set(), []
    if exp == []:
        ok = out_conflicts == []
        return [_item("conflicts", PASS if ok else FAIL, "标注要求无冲突" + ("" if ok else f"，输出 {len(out_conflicts)} 个"),
                      out_conflicts)], set(range(len(out_conflicts))), []
    for j, ec in enumerate(exp):
        best, best_n = None, -1
        for k, oc in enumerate(out_conflicts):
            if k in used_conflicts:
                continue
            n = 0 if oc["members"] is None else sum(
                1 for m in ec["members"] for om in oc["members"] if sources_ok(m.get("sources"), om["sources"]))
            if n > best_n:
                best, best_n = k, n
        if best is not None:
            used_conflicts.add(best)
        oc = out_conflicts[best] if best is not None else None
        used = set()
        order = sorted(range(len(ec["members"])), key=lambda m: -len((ec["members"][m].get("sources") or {}).get("all_of", [])))
        for m in order:
            em, p = ec["members"][m], f"conflicts[{j}].members[{m}]"
            if oc is None:
                items += [_item(f"{p}.sources", MISSING, "输出没有冲突"), _item(f"{p}.value", MISSING, "输出没有冲突")]
                continue
            if oc["members"] is None:
                items += [_item(f"{p}.sources", MISSING, "旧格式只报告存在冲突，不给成员", oc, flag=CANNOT),
                          _item(f"{p}.value", MISSING, "旧格式只报告存在冲突，不给成员", oc, flag=CANNOT)]
                continue
            free = [om for om in oc["members"] if om["id"] not in used]
            cands = [om for om in free if sources_ok(em.get("sources"), om["sources"])]
            by_value = [om for om in free if _vals(em.get("value_any")) & {strip_edges(v) for v in om["values"]}]
            pick = ([x for x in cands if x in by_value] or cands or by_value or [None])[0]
            if pick is None:
                items += [_item(f"{p}.sources", MISSING, "没有对应的冲突成员", oc),
                          _item(f"{p}.value", MISSING, "没有对应的冲突成员", oc)]
                continue
            used.add(pick["id"])
            bad = pick.get("unmapped_sources") or []
            items.append(_item(f"{p}.sources", PASS if pick in cands and not bad else FAIL,
                               f"成员来源 {pick['sources']}，要求 {em.get('sources')}" + (f"；无效引用 {bad}" if bad else ""),
                               pick, pick["sources"]))
            exact = pick in by_value
            items.append(_item(f"{p}.value", PASS if exact else REVIEW,
                               "成员值整串等值" if exact else "成员值不能整串等值（完整原句），需复核", pick, pick["sources"]))
        if oc is not None and oc["members"] is not None:
            for om in oc["members"]:
                if om["id"] not in used:
                    # A member beyond the annotated ones is an extra conclusion.
                    bad = om.get("unmapped_sources") or []
                    extra_members.append(_item(f"extra:conflict[{j}].member:{om['id']}", FAIL if bad else REVIEW,
                                               f"未被标注覆盖的冲突成员" + (f"；无效引用 {bad}" if bad else ""),
                                               om, om["sources"]))
    return items, used_conflicts, extra_members


def _months_between(a: _dt.date, b: _dt.date) -> int:
    m = (b.year - a.year) * 12 + (b.month - a.month)
    if b.day < a.day:
        m -= 1
    return m


def _computed_items(exp, oc, source_texts):
    if exp is None:
        return [_item("computed", PASS if oc is None else FAIL,
                      "标注要求无计算" + ("" if oc is None else "，输出了计算结果"), oc)]
    if oc is None:
        return [_item("computed.value", MISSING, "输出没有计算结果"),
                _item("computed.endpoints", MISSING, "输出没有计算结果")]
    if not oc["parsed"]:
        return [_item("computed.value", REVIEW, "旧答案含计算结果但不能整串等值", oc, oc["sources"]),
                _item("computed.endpoints", MISSING, "旧格式不给结构化端点（角色、依据、终点）", oc, oc["sources"],
                      flag=CANNOT)]
    if oc.get("structure_errors"):
        why = "；".join(oc["structure_errors"])
        return [_item("computed.value", FAIL, f"计算结构非法：{why}", oc, oc["sources"]),
                _item("computed.endpoints", FAIL, f"计算结构非法：{why}", oc, oc["sources"])]
    out = []
    same = all(oc.get(k) == exp.get(k) for k in ("op", "value", "unit", "bound"))
    out.append(_item("computed.value", PASS if same else FAIL,
                     f"输出 {[oc.get(k) for k in ('op','value','unit','bound')]}，要求 {[exp.get(k) for k in ('op','value','unit','bound')]}",
                     oc, oc["sources"]))
    ops = oc["operands"]
    if len(ops) != 2 or any(o not in oc["items"] for o in ops):
        out.append(_item("computed.endpoints", FAIL, "elapsed 需要两个有效操作数", oc, oc["sources"]))
        return out
    end, start = oc["items"][ops[0]], oc["items"][ops[1]]
    problems, unknown = [], []
    for name, it in (("start", start), ("end", end)):
        e = exp[name]
        if it["date_source"] != e["source"]:
            problems.append(f"{name} 来源 {it['date_source']} ≠ {e['source']}")
        if e.get("role") and it["role"] != e["role"]:
            problems.append(f"{name} 时间角色 {it['role']} ≠ {e['role']}")
        if it.get("invalid"):
            problems.append(f"{name} 无效引用 {it['invalid']}")
        if e["source"] == "request.at" and it["date_basis"] != "trusted_request_at":
            problems.append(f"{name} 提问时刻不是代码提供的可信 request.at（{it['date_basis']}）")
        if e["source"] != "request.at":
            # A dated endpoint must rest on evidence from its date source, and
            # the date wording must occur verbatim in that source.
            if it["date_source"] not in it["evidence_sources"]:
                problems.append(f"{name} 缺少来自 {it['date_source']} 的依据")
            text = (source_texts or {}).get(it["date_source"])
            if it["quote"] is None or text is None or it["quote"] not in text:
                problems.append(f"{name} 时间原文 {it['quote']!r} 不在来源 {it['date_source']} 中")
            for evd in it["evidence"]:
                t = (source_texts or {}).get(evd["source"])
                if t is None or evd["quote"] not in t:
                    problems.append(f"{name} 依据片段 {evd['quote']!r} 不在来源 {evd['source']} 中")
        if it["date"] is None:
            unknown.append(f"{name} 日期未能结构化解析（{it['date_basis']}）")
        elif it["date"] != e["date"]:
            problems.append(f"{name} 日期 {it['date']} ≠ {e['date']}")
    if not problems and not unknown and exp.get("unit") == "month":
        m = _months_between(_dt.date.fromisoformat(start["date"]), _dt.date.fromisoformat(end["date"]))
        if str(m) != str(oc.get("value")):
            problems.append(f"输出值 {oc.get('value')} 与端点整日历月数 {m} 不符")
    res = FAIL if problems else REVIEW if unknown else PASS
    out.append(_item("computed.endpoints", res, "；".join(problems + unknown) or "端点、角色、依据与精度一致", oc, oc["sources"]))
    return out


def _forbidden_items(ctx, forbidden, fact_match, facts):
    items = []
    view = ctx.view
    for k, fb in enumerate(forbidden):
        p, kind = f"forbidden[{k}]", fb.get("kind")
        hits, undetermined = [], []
        if kind == "value_as_current":
            # Decided only by whole-string equality of a plain answer. Where
            # the value is free text (recall.v2 statement, non-plain old
            # answer), a current or unknown-currency conclusion goes to review;
            # citing an old source does not by itself prove a replacement.
            vals = fb.get("value_any")
            for a in view["assertions"]:
                cur = ctx.current(a)
                if ctx.plain_exact(a):
                    if cur == "yes" and ctx.plain_exact(a, vals):
                        hits.append(a["id"])
                elif cur in ("yes", None, CANNOT):
                    undetermined.append(a["id"])
        elif kind in ("wrong_subject_source", "source_out_of_scope"):
            bad = _srcset(fb.get("sources", []))
            for a in view["assertions"]:
                if _srcset(a["sources"]) & bad:
                    hits.append(a["id"])
            for c in view["conflicts"]:
                for m in c["members"] or []:
                    if _srcset(m["sources"]) & bad:
                        hits.append(m["id"])
            for h in view.get("history", []):
                if _srcset(h["sources"]) & bad:
                    hits.append(h["id"])
            if view["computed"] and _srcset(view["computed"].get("sources", [])) & bad:
                hits.append("computed")
        elif kind == "condition_broadened":
            cond = fb.get("condition")
            owners = [fact_match[i] for i, f in enumerate(facts)
                      if i in fact_match and any(x["text"] == cond for x in f.get("conditions", []))]
            for a in owners or view["assertions"]:
                if a["conditions"] is None:
                    undetermined.append(a["id"])
                elif cond not in a["conditions"]:
                    hits.append(a["id"])
        elif kind == "value_in_negation":
            # A plain answer equal to an annotated value is affirmative by
            # construction; any negated or unresolved free-text assertion may
            # negate the value and goes to review.
            for a in view["assertions"]:
                pol = ctx.polarity(a)
                if ctx.plain_exact(a):
                    continue
                if pol in ("negate", "unknown", None):
                    undetermined.append(a["id"])
        else:
            items.append(_item(p, REVIEW, f"未知禁止项类型 {kind}，不能自动判定", fb))
            continue
        if hits:
            res, basis = FAIL, f"命中 {sorted(set(hits))}"
        elif undetermined:
            res, basis = REVIEW, f"无法结构判定 {sorted(set(undetermined))}"
        else:
            res, basis = PASS, "未命中"
        # The verdict depends on every conclusion it looked at, so all of them
        # (assertions, history, conflict members, computed) enter its content.
        assessed = {
            "assertions": [_assertion_content(a) for a in view["assertions"]],
            "history": view.get("history", []),
            "conflicts": view["conflicts"],
            "computed": view["computed"],
        }
        srcs = {s for a in view["assertions"] for s in a["sources"]} | \
               {s for h in view.get("history", []) for s in h["sources"]} | \
               {s for c in view["conflicts"] for m in (c["members"] or []) for s in m["sources"]}
        it = _item(p, res, f"{kind}：{basis}", {"forbidden": fb, "hits": sorted(set(hits)), "assessed": assessed}, srcs)
        it["kind"] = kind
        items.append(it)
    return items


def score(expect: dict, query_meta: dict, view: dict, source_texts: dict | None = None) -> dict:
    ctx = _Ctx(expect, query_meta, view)
    items = [_item("binding", FAIL if view["binding"] else PASS,
                   "；".join(view["binding"]) or "输出版本与问题、上下文、任务、提问时间绑定一致"),
             _item("status", PASS if view["status"] == expect["status"] else FAIL,
                   f"输出 {view['status']}（{view['status_raw']}），要求 {expect['status']}")]
    facts = expect.get("facts") or []
    fact_match = match_facts(ctx, facts, view["assertions"])
    for i, f in enumerate(facts):
        items += _fact_items(ctx, i, f, fact_match.get(i))
    used_conflicts, extra_members = set(), []
    if "conflicts" in expect:
        ci, used_conflicts, extra_members = _conflict_items(ctx, expect["conflicts"], view["conflicts"])
        items += sorted(ci, key=lambda i: i["id"])  # stable report order
    if "computed" in expect:
        items += _computed_items(expect["computed"], view["computed"], source_texts)
    items += _forbidden_items(ctx, expect.get("forbidden") or [], fact_match, facts)
    for x in expect.get("not_measurable") or []:
        items.append(_item(f"not_measurable:{x}", NOT_MEASURABLE, "标签不足"))

    matched = {a["id"] for a in fact_match.values()}
    extras = list(extra_members)
    for a in view["assertions"]:
        if a["id"] not in matched:
            bad = a.get("unmapped_sources") or []
            extras.append(_item(f"extra:{a['id']}", FAIL if bad else REVIEW,
                                "未被标注覆盖的断言" + (f"；无效引用 {bad}" if bad else ""), _assertion_content(a), a["sources"]))
    for k, oc in enumerate(view["conflicts"]):
        if k not in used_conflicts and expect.get("conflicts") != []:
            extras.append(_item(f"extra:conflict:{oc['id']}", REVIEW, "未被标注覆盖的冲突", oc))
    for h in view.get("history", []):
        # History statements are assertions too ("以前是……"); none is covered
        # by an annotation, so each goes to review.
        bad = h.get("unmapped_sources") or []
        extras.append(_item(f"extra:{h['id']}", FAIL if bad else REVIEW,
                            "历史断言，未被标注覆盖" + (f"；无效引用 {bad}" if bad else ""), h, h["sources"]))
    return {"items": items, "extras": extras}


def blockers(items: list) -> dict:
    """Acceptance blockers from finalized items: forbidden hits of blocker
    kinds (auto_fail or a confirmed failing review) and broadened conditions.
    Forbidden items of those kinds still awaiting review are listed apart."""
    hit, pending = [], []
    for it in items:
        kind = it.get("kind")
        if kind in BLOCKER_KINDS:
            if it["final"] == "fail":
                hit.append({"item": it["id"], "type": BLOCKER_KINDS[kind], "basis": it["basis"]})
            elif it["final"] == "pending_review":
                pending.append({"item": it["id"], "type": BLOCKER_KINDS[kind], "basis": it["basis"]})
        elif it["result"] == FAIL and it["id"].endswith(".conditions") and "条件扩大" in it["basis"]:
            hit.append({"item": it["id"], "type": "条件扩大", "basis": it["basis"]})
    return {"hit": hit, "pending": pending}
