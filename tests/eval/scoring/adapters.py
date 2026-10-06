"""Adapters from raw recall outputs to the common.v1 scoring view.

Adapters read only: the question input (query, context, at, task), query_meta,
the raw output, and the run's id mapping (runtime source/memory ids -> stable
message ids). They never see `expect` or `expect_common`: `QuestionInput`
has no field for them, and callers build it with `question_input()`.

No natural-language parsing. Free text (old safe_answer, recall.v3 statement)
is carried as text for review; only whole-string comparison happens later, in
the scorer.
"""
from __future__ import annotations

import datetime as _dt
import unicodedata

OLD_ADAPTER = "old_answer_contract@1"
NEW_ADAPTER = "recall_v3@1"
V4_ADAPTER = "recall_v4@1"
V5_ADAPTER = "recall_v5@1"
TZ = _dt.timezone(_dt.timedelta(seconds=28800))  # eval time zone, UTC+8
RECALL_VERSION = "recall.v3"

# Old answer_contract status -> common status. Unknown statuses map to "unknown".
OLD_STATUS = {
    "source_linked_model_answer": "answer",
    "source_linked_fact": "answer",
    "source_linked_computed_elapsed": "answer",
    "source_linked_quote": "answer",
    "source_linked_exact_identity": "answer",
    "insufficient_evidence": "insufficient",
    "answer_not_verified": "insufficient",
    "comparison_evidence_incomplete": "insufficient",
    "comparison_conclusion_not_verified": "insufficient",
    "clarification_required": "clarify",
    "conflicting_records": "conflict",
}
NEW_STATUS = {
    "answered": "answer",
    "insufficient": "insufficient",
    "conflict": "conflict",
    "needs_clarification": "clarify",
    "context_only": "context_only",
    "degraded": "degraded",
}


# Only these may be removed before whole-string comparison: surrounding
# whitespace and sentence-final full stops (the old contract always appends
# "。"). Marks that carry meaning (？ ! quotes, brackets, ellipses) stay, so
# "钢笔？" never equals "钢笔".
_FINAL_STOPS = "。.．"


def strip_plain(text: str) -> str:
    """Drop surrounding whitespace and at most one final full stop. A run of
    stops ("钢笔..." / "钢笔。。") is an ellipsis and is kept."""
    text = text.strip()
    if len(text) >= 2 and text[-1] in _FINAL_STOPS and text[-2] not in _FINAL_STOPS + "…":
        text = text[:-1].rstrip()
    return text


strip_edges = strip_plain  # used by the scorer for annotated values


def question_input(case: dict, question: dict) -> dict:
    """The only view of a question an adapter receives (no labels)."""
    return {
        "case": case["id"],
        "id": question["id"],
        "query": question["query"],
        "context": list(question.get("context", [])),
        "at": question.get("at"),
        "task_id": question.get("task_id"),
    }


def _sorted_unique(xs):
    return sorted(set(xs))


# --------------------------------------------------------------------------
# Old answer_contract v1 (baseline 0517000)
# --------------------------------------------------------------------------

def old_store_context(baseline: dict, question_id: str) -> dict:
    """Semantic store state at the time of `question_id` in a baseline export."""
    store, enrichment = {}, {}
    for step in baseline["steps"]:
        if step["step"] == "question" and step["question"] == question_id:
            break
        if step["step"] == "intake":
            store = step["memories_after"]
            remember = step["result"].get("remember", {})
            res = remember.get("result", remember)  # both export shapes occur
            enrichment[step["message"]] = (res.get("enrichment") or {}).get("status")
    else:
        raise KeyError(question_id)
    return {"memories": store, "enrichment": enrichment}


def _old_memory_stable(mem: dict) -> str:
    turns = _sorted_unique(e.get("turn_id") for e in mem.get("evidence", []) if e.get("turn_id"))
    value = mem.get("value")
    return "rec[" + ",".join(turns) + "]" + _value_text(value)


def _value_text(value) -> str:
    if isinstance(value, dict):
        for k in ("Text", "text", "Integer", "integer", "Boolean", "boolean", "Entity", "entity_id"):
            if k in value and value[k] is not None:
                return str(value[k])
        return str(value)
    return "" if value is None else str(value)


def adapt_old(q: dict, query_meta: dict, raw: dict, source_ids: dict, store: dict, *,
              source_texts: dict, source_state: dict | None = None) -> dict:
    """raw = memory_recall result (only answer_contract is authoritative).

    source_ids: stable message id -> runtime source id for this run.
    store: old_store_context() of the same run.
    """
    contract = raw["answer_contract"]
    rev = {v: k for k, v in source_ids.items()}
    # Binding: the contract must be the old v1 format answering exactly this
    # question. A mismatch fails the common `binding` item and disables every
    # whole-string inference below.
    binding = []
    if contract.get("version") != 1:
        binding.append(f"version={contract.get('version')!r}，期望 1")
    for k, want in (("query", q["query"]), ("context", q["context"]), ("task_id", q["task_id"])):
        if contract.get(k) != want:
            binding.append(f"{k}={contract.get(k)!r}，问题为 {want!r}")
    status_raw = contract.get("status")
    status = OLD_STATUS.get(status_raw, "unknown")
    cited = contract.get("safe_source_ids") or []
    sources = _sorted_unique(rev[s] for s in cited if s in rev)
    deleted = {k for k, v in (source_state or {}).items() if v.get("deleted") or v.get("linkable") is False}
    unmapped = [s for s in cited if s not in rev] + [s for s in sources if s in deleted]
    # Every cited source must carry a quote that occurs verbatim in that
    # source's original text; otherwise the citation is unverified.
    quoted = {}
    for src in contract.get("sources", []):
        quoted.setdefault(src.get("source_id"), []).append(src.get("quote") or "")
    for rid in cited:
        sid = rev.get(rid)
        text = source_texts.get(sid) if sid else None
        qs = quoted.get(rid, [])
        if sid and (not qs or text is None or any(not qq or qq not in text for qq in qs)):
            unmapped.append(f"quote_unverified:{sid}")
    quotes = [{"source": rev.get(s["source_id"], s["source_id"]), "text": s.get("quote", "")}
              for s in contract.get("sources", [])]
    answer = contract.get("safe_answer") or ""

    view = {
        "adapter": OLD_ADAPTER,
        "status": status,
        "status_raw": status_raw,
        "binding": binding,
        "assertions": [],
        "conflicts": [],
        "computed": None,
        "history": [],
        "clarification": answer if status == "clarify" else None,
        "cannot_express": ["polarity", "current", "conditions", "uncertainty", "subject",
                           "conflict_members", "computed_structure", "history"],
    }
    if status == "answer":
        # A plain answer is "value only": if the scorer finds it whole-string
        # equal to an annotated value, polarity=affirm and (for
        # time_focus=current) current=yes follow; otherwise both go to review.
        # Only a correctly bound answer whose citations all map to valid
        # sources qualifies as plain.
        plain = not binding and bool(sources) and not unmapped
        plain_current = "yes_if_plain" if query_meta.get("time_focus") == "current" else None
        view["assertions"].append({
            "id": "safe_answer",
            "text": answer,
            "value": strip_edges(answer),
            "sources": sources,
            "unmapped_sources": unmapped,
            "quotes": quotes,
            "polarity": "affirm_if_plain",
            "current": plain_current,
            "conditions": None,
            "uncertainty": "none_if_plain",
            "plain": plain,
        })
        if status_raw == "source_linked_computed_elapsed":
            view["computed"] = {"parsed": False, "text": answer, "sources": sources,
                                "unmapped_sources": unmapped}
    elif status == "conflict":
        # Old format reports that a conflict exists but not its members.
        view["conflicts"] = [{"id": "conflicting_records", "status": "open", "members": None}]

    # Semantic state (for review-ledger keys), mapped to stable ids.
    records = []
    for mem in store["memories"].values():
        turns = _sorted_unique(e.get("turn_id") for e in mem.get("evidence", []) if e.get("turn_id"))
        sup = mem.get("superseded_by")
        sup_stable = None
        if sup is not None and str(sup) in store["memories"]:
            sup_stable = _old_memory_stable(store["memories"][str(sup)])
        records.append({
            "id": _old_memory_stable(mem),
            "sources": turns,
            "subject": mem.get("subject_id"),
            "state": {
                "status": mem.get("status"),
                "supersede_reason": mem.get("supersede_reason"),
                "superseded_by": sup_stable,
                "predicate": mem.get("predicate"),
                "value": _value_text(mem.get("value")),
                "polarity": mem.get("polarity"),
                "strength": mem.get("strength"),
                "scope": mem.get("scope"),
                "memory_type": mem.get("memory_type"),
                "reinforcement_count": mem.get("reinforcement_count"),
                "temporal": mem.get("temporal"),
                "certainty": mem.get("certainty"),
                "evidence": sorted([e.get("turn_id"), e.get("quote")] for e in mem.get("evidence", [])),
            },
        })
    grades = {rev.get(s["source_id"], s["source_id"]): s.get("grade") for s in contract.get("sources", [])}
    view["semantic_state"] = {
        "records": sorted(records, key=lambda r: r["id"]),
        "relations": [],
        "sources": _source_state(source_ids, source_state, lambda sid: {
            "enrichment": store["enrichment"].get(sid), "grade": grades.get(sid)}),
        "counter_evidence": _map_ids(raw.get("historical_or_conflicting", []), rev),
        "completeness": {
            "degraded_reasons": sorted(raw.get("degraded_reasons", [])),
            "evidence_status": raw.get("evidence_status"),
            "coverage_complete": (raw.get("budget") or {}).get("coverage_complete"),
        },
    }
    return view


def _source_state(source_ids, override, extra):
    """Validity of each stable source: linkable (has a runtime id) and not deleted.
    `override` comes from the run harness (e.g. {"s2_t3": {"deleted": true}})."""
    out = {}
    for sid in source_ids:
        st = {"linkable": source_ids[sid] is not None, "deleted": False, **extra(sid)}
        st.update((override or {}).get(sid, {}))
        out[sid] = st
    for sid, st in (override or {}).items():
        out.setdefault(sid, {"linkable": False, "deleted": False, **st})
    return out


def _map_ids(obj, rev):
    if isinstance(obj, dict):
        return {k: _map_ids(v, rev) for k, v in obj.items()}
    if isinstance(obj, list):
        return [_map_ids(v, rev) for v in obj]
    if isinstance(obj, str) and obj in rev:
        return rev[obj]
    return obj


# --------------------------------------------------------------------------
# recall.v3/v4 historical / recall.v5 current (contracts.md 3.4)
# --------------------------------------------------------------------------

def _date(s: str) -> _dt.date:
    return _dt.date.fromisoformat(s)


def _shift(d: _dt.date, unit: str, offset: int):
    if unit == "day":
        return d + _dt.timedelta(days=offset)
    if unit == "week":
        return d + _dt.timedelta(weeks=offset)
    if unit in ("month", "year"):
        months = offset * (12 if unit == "year" else 1)
        y, m = divmod(d.month - 1 + months, 12)
        try:
            return d.replace(year=d.year + y, month=m + 1)
        except ValueError:
            return None  # day does not exist in target month: not resolvable here
    return None


def adapt_recall(q: dict, query_meta: dict, raw: dict, source_ids: dict, *,
                 source_texts: dict, source_state: dict | None = None) -> dict:
    """raw = recall.v3/v4/v5 JSON (contracts.md 3.4). source_ids: stable message id
    -> runtime source id. source_texts: stable message id -> original text,
    used only to verify that quotes occur verbatim.

    Every field a reviewer's verdict can depend on (qualifiers, times,
    strength, update state, grade, evidence quotes, full record candidates)
    is carried into the view with runtime ids replaced by stable refs, so it
    enters review-ledger keys.
    """
    rev = {v: k for k, v in source_ids.items()}
    deleted = {k for k, v in (source_state or {}).items() if v.get("deleted") or v.get("linkable") is False}
    req = raw.get("request") or {}
    binding = []
    if raw.get("version") not in (RECALL_VERSION, "recall.v4", "recall.v5"):
        binding.append(f"version={raw.get('version')!r}，期望 recall.v3、recall.v4 或 recall.v5")
    for k, want in (("query", q["query"]), ("context", q["context"]), ("task_id", q["task_id"]), ("at", q["at"])):
        if req.get(k) != want:
            binding.append(f"request.{k}={req.get(k)!r}，问题为 {want!r}")

    def stable(ref: dict):
        if ref.get("kind") == "current":
            return "current"
        return rev.get(ref.get("source_id"), "unmapped:" + str(ref.get("source_id")))

    def ev(e):
        return {"source": stable(e["source"]), "quote": (e.get("quote") or {}).get("text")}

    records = {r["id"]: r for r in raw.get("records", [])}

    def record_sources(rid):
        r = records.get(rid)
        if r is None:
            return ["unmapped-record:" + str(rid)]
        return [stable(e["source"]) for e in r.get("evidence", [])]

    def record_stable(rid):
        r = records.get(rid)
        if r is None:
            return "unmapped-record:" + str(rid)
        if r.get("format") == "current":
            v = (r.get("candidate") or {}).get("value") or {}
            val = _value_text(v.get("text") if v.get("text") is not None else v.get("integer"))
        else:
            val = _value_text((r.get("data") or {}).get("value"))
        return "rec[" + ",".join(_sorted_unique(record_sources(rid))) + "]" + val

    def record_values(rid):
        r = records.get(rid)
        if not r or r.get("format") != "current":
            return []
        v = (r.get("candidate") or {}).get("value") or {}
        # Structured value only; the quote is evidence for it, not a value.
        return [str(v[k]) for k in ("text", "integer", "boolean", "entity_id") if v.get(k) is not None]

    cm_text = (raw.get("current_message") or {}).get("text")

    def text_of(sid):
        return cm_text if sid == "current" else source_texts.get(sid)

    def invalid(srcs, evidence=()):
        bad = [s for s in srcs if s.startswith("unmapped") or s in deleted]
        for e in evidence:
            sid, qt = stable(e["source"]), (e.get("quote") or {}).get("text")
            t = text_of(sid)
            if not qt or t is None or qt not in t:
                bad.append(f"quote_unverified:{sid}")
        return bad

    said_on = {}
    cm = raw.get("current_message")
    if cm and cm.get("said_on"):
        said_on["current"] = cm["said_on"]
    for r in raw.get("raw", []):
        said_on.setdefault(rev.get(r["source_id"], r["source_id"]), r.get("said_on"))
    for r in raw.get("records", []):
        for e in r.get("evidence", []):
            if r.get("said_on"):
                said_on.setdefault(stable(e["source"]), r["said_on"])

    def qualifiers(f):
        return {
            "polarity": f.get("polarity"),
            "strength": f.get("strength"),
            "uncertainty": f.get("uncertainty"),
            "conditions": f.get("conditions", []),
            "times": f.get("times", []),
        }

    assertions = []
    for i, f in enumerate(raw.get("facts", [])):
        srcs = [stable(e["source"]) for e in f.get("evidence", [])]
        for rid in f.get("record_ids", []):
            srcs += record_sources(rid)
        assertions.append({
            "id": f"facts[{i}]",
            "text": f.get("statement", ""),
            "value": None,  # recall.v3 has no value field; statement goes to review
            "sources": _sorted_unique(srcs),
            "unmapped_sources": invalid(srcs, f.get("evidence", []) + [
                e for rid in f.get("record_ids", []) for e in (records.get(rid) or {}).get("evidence", [])]),
            "quotes": [{"source": stable(e["source"]), "text": e["quote"]["text"]} for e in f.get("evidence", [])],
            "polarity": f.get("polarity"),
            "current": f.get("current"),
            "applies": f.get("applies"),
            "conditions": [c["quote"]["text"] for c in f.get("conditions", [])],
            "uncertainty": (f.get("uncertainty") or {}).get("quote", {}).get("text") if f.get("uncertainty") else None,
            "qualifiers": qualifiers(f),
            "grade": f.get("grade"),
            "update": f.get("update"),
            "record_ids": [record_stable(r) for r in f.get("record_ids", [])],
            "supersedes": [record_stable(r) for r in f.get("supersedes_record_ids", [])],
            "plain": False,
        })

    history = []
    for i, h in enumerate(raw.get("history", [])):
        srcs = [stable(e["source"]) for e in h.get("evidence", [])]
        history.append({"id": f"history[{i}]", "text": h.get("statement", ""), "until": h.get("until"),
                        "sources": _sorted_unique(srcs), "unmapped_sources": invalid(srcs, h.get("evidence", [])),
                        "quotes": [ev(e) for e in h.get("evidence", [])]})

    conflicts = []
    for link in raw.get("conflicts", []):
        conflicts.append({
            "id": "rel[" + ",".join(sorted(record_stable(m) for m in link.get("members", []))) + "]",
            "status": link.get("status"),
            "conditions": link.get("conditions"),
            "times": link.get("times"),
            "basis": [ev(e) for e in link.get("basis", [])],
            "members": [{"id": record_stable(m), "values": _sorted_unique(record_values(m)),
                         "quotes": [e["quote"]["text"] for e in (records.get(m) or {}).get("evidence", [])],
                         "sources": _sorted_unique(record_sources(m)),
                         "unmapped_sources": invalid(record_sources(m), (records.get(m) or {}).get("evidence", []))}
                        for m in link.get("members", [])],
        })

    computed = None
    c = raw.get("computed")
    if c is not None:
        # Structural checks before any lookup table is built: item ids must be
        # unique and must not use the reserved "@" prefix; "@request.at" may
        # only be the end operand of elapsed. A dict built first would let a
        # duplicate or forged item silently replace the real one.
        structure_errors = []
        ids = [it.get("id") for it in c.get("items", [])]
        for iid in sorted({i for i in ids if ids.count(i) > 1}, key=str):
            structure_errors.append(f"条目 ID 重复：{iid!r}")
        for iid in ids:
            if not isinstance(iid, str) or iid.startswith("@"):
                structure_errors.append(f"条目 ID 使用保留前缀或非法：{iid!r}")
        ops = list(c.get("operands", []))
        for k, op in enumerate(ops):
            if isinstance(op, str) and op.startswith("@") and not (
                    op == "@request.at" and k == 0 and c.get("op") == "elapsed"):
                structure_errors.append(f"保留操作数用法非法：{op!r}（位置 {k}）")
        if "@request.at" not in ops and c.get("request_time") is not None:
            structure_errors.append("未使用 @request.at 却带 request_time")
        items = {}
        for it in c.get("items", []):
            if structure_errors:
                break
            src = stable(it["date_source"]) if it.get("date_source") else None
            date = it.get("date") or {}
            interp = date.get("interp") or {}
            resolved, basis = None, "no_date"
            kind = interp.get("kind")
            # Day precision only for a calendar date or a day/week offset from
            # the day it was said; month/year offsets, ranges and unknowns are
            # coarser and must not be turned into a day.
            precision = "day" if kind == "date" or (kind == "relative" and interp.get("unit") in ("day", "week")) \
                else f"coarse:{kind}" + (f"/{interp.get('unit')}" if interp.get("unit") else "")
            if interp.get("kind") == "date":
                resolved, basis = interp.get("date"), "date"
            elif precision != "day":
                basis = "coarse_precision"
            elif interp.get("kind") == "relative" and src and said_on.get(src):
                d = _shift(_date(said_on[src]), interp.get("unit"), int(interp.get("offset", 0)))
                resolved, basis = (d.isoformat() if d else None), "relative_to_said_on"
            elif interp:
                basis = "unresolved:" + str(interp.get("kind"))
            items[it["id"]] = {
                "invalid": invalid([stable(e["source"]) for e in it.get("evidence", [])] + ([src] if src else []),
                                   it.get("evidence", [])),
                "evidence": [ev(e) for e in it.get("evidence", [])],
                "evidence_sources": _sorted_unique(stable(e["source"]) for e in it.get("evidence", [])),
                "sources": _sorted_unique([stable(e["source"]) for e in it.get("evidence", [])] + ([src] if src else [])),
                "date_source": src,
                "role": date.get("role"),
                "quote": (date.get("quote") or {}).get("text"),
                "interp": interp or None,
                "quantity": it.get("quantity"),
                "event_group": it.get("event_group"),
                "date": resolved,
                "date_basis": basis,
                "precision": precision if interp else None,
            }
        # r4.v2+ / recall.v2+: the reserved operand "@request.at" is the trusted
        # question time; code supplies it as computed.request_time.
        if "@request.at" in c.get("operands", []):
            rt = c.get("request_time") or {}
            at, off = rt.get("at"), rt.get("tz_offset_seconds")
            ok = (isinstance(at, int) and at == req.get("at") and isinstance(off, int)
                  and rt.get("precision") == "day"
                  and rt.get("local_date") == _dt.datetime.fromtimestamp(
                      at, _dt.timezone(_dt.timedelta(seconds=off))).date().isoformat())
            items["@request.at"] = {
                "invalid": [], "evidence": [], "evidence_sources": [], "sources": [], "date_source": "request.at",
                "role": "request_time", "quote": None, "interp": None, "quantity": None, "event_group": None,
                "request_time": rt,
                "precision": "day",
                "date": rt.get("local_date") if ok else None,
                "date_basis": "trusted_request_at" if ok else "request_time_invalid",
            }
        computed = {"parsed": True, "structure_errors": structure_errors,
                    "raw_items": [{"id": it.get("id"), "evidence": [ev(e) for e in it.get("evidence", [])],
                                   "date": it.get("date"), "quantity": it.get("quantity"),
                                   "event_group": it.get("event_group"),
                                   "date_source": stable(it["date_source"]) if it.get("date_source") else None}
                                  for it in c.get("items", [])],
                    # raw_items: every item, in order, with stable ids only
                    "op": c.get("op"), "value": c.get("value"), "unit": c.get("unit"),
                    # recall.v3: code-computed rounding basis for elapsed
                    "rounding": c.get("rounding"), "exact_days": c.get("exact_days"),
                    "approximate": c.get("approximate"),
                    "bound": c.get("bound"), "items": items, "operands": list(c.get("operands", [])),
                    "sources": _sorted_unique(s for it in items.values() for s in it["sources"])}

    view = {
        "adapter": {"recall.v4": V4_ADAPTER, "recall.v5": V5_ADAPTER}.get(raw.get("version"), NEW_ADAPTER),
        "status": NEW_STATUS.get(raw.get("status"), "unknown"),
        "status_raw": raw.get("status"),
        "binding": binding,
        "assertions": assertions,
        "conflicts": conflicts,
        "computed": computed,
        "history": history,
        "clarification": (raw.get("clarification") or {}).get("question"),
        "cannot_express": ["subject"],
    }

    def record_state(r):
        st = {
            "format": r.get("format"),
            "stored_status": r.get("stored_status"),
            "termination_reason": r.get("termination_reason"),
            "ended": r.get("ended"),
            "update": r.get("update"),
            "gaps": r.get("gaps"),
            "grade": r.get("grade"),
            "said_on": r.get("said_on"),
            "time_resolutions": r.get("time_resolutions"),
            "evidence": [ev(e) for e in r.get("evidence", [])],
            "history": sorted(record_stable(h) for h in r.get("history", [])),
            "open_relations": len(r.get("open_relations", [])),
        }
        if r.get("format") == "current":
            st["candidate"] = r.get("candidate")  # contains only quotes/entity ids, no runtime ids
        else:
            st["data"] = {k: v for k, v in (r.get("data") or {}).items()
                          if k not in ("id", "last_retrieved_at", "retrieval_count", "superseded_by", "supersedes")}
        return st

    view["semantic_state"] = {
        "records": sorted(({
            "id": record_stable(r["id"]),
            "sources": _sorted_unique(record_sources(r["id"])),
            "subject": ((r.get("candidate") or {}).get("subject") or {}).get("entity_id")
                       if r.get("format") == "current" else (r.get("data") or {}).get("subject_id"),
            "state": record_state(r),
        } for r in raw.get("records", [])), key=lambda r: r["id"]),
        "relations": sorted(({"members": sorted(record_stable(m) for m in l.get("members", [])),
                              "status": l.get("status"),
                              "conditions": l.get("conditions"), "times": l.get("times"),
                              "basis": [ev(e) for e in l.get("basis", [])],
                              "resolution": ev(l["resolution"]) if l.get("resolution") else None}
                             for l in raw.get("conflicts", [])), key=lambda x: x["members"]),
        "sources": _source_state(source_ids, source_state, lambda sid: {}),
        "raw": sorted(({"source": rev.get(r["source_id"], r["source_id"]), "quote": (r.get("quote") or {}).get("text"),
                        "said_on": r.get("said_on"), "enrichment": r.get("enrichment"), "grade": r.get("grade"),
                        "memory_requests": r.get("memory_requests")}
                       for r in raw.get("raw", [])), key=lambda x: (x["source"], x["quote"] or "")),
        "current_message": ({k: v for k, v in cm.items() if k not in ("source_id", "sequence")} if cm else None),
        "counter_evidence": {"history": history, "conflicts": [x["id"] for x in conflicts]},
        "completeness": {
            **(raw.get("completeness") or {}),
            "degraded_reasons": sorted(raw.get("degraded_reasons", [])),
            "semantic_judgment": raw.get("semantic_judgment"),
        },
    }
    return view
