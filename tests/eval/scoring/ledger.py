"""Review ledger (common-scoring-view.md 6) and final results.

A review verdict is cached under a key hashed from everything the verdict
depends on. Any change invalidates it; revisions, durations, request ids and
runtime ids do not enter the key (runtime ids are mapped to stable refs by
the adapters first).

Ledger entry: {"verdict": "pass" | "fail", "status": "confirmed" | "proposed",
"reviewer": str, "note": str, "question": str, "side": str, "item": str}.
Only confirmed entries count. Proposed entries are drafts awaiting a human.
"""
from __future__ import annotations

import hashlib
import json

from scoring import FAIL, MISSING, NOT_MEASURABLE, PASS, REVIEW, SCORING_VERSION


def canonical(obj) -> str:
    return json.dumps(obj, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def expect_sources(expect: dict) -> set:
    out = set()
    def add(spec):
        if spec:
            out.update(spec.get("all_of", []))
            out.update(spec.get("any_of", []))
    for f in expect.get("facts") or []:
        add(f.get("sources"))
    for requirement in expect.get("answer_requirements") or []:
        add(requirement.get("sources"))
    for c in expect.get("conflicts") or []:
        for m in c.get("members", []):
            add(m.get("sources"))
    for fb in expect.get("forbidden") or []:
        out.update(fb.get("sources", []))
    comp = expect.get("computed") or {}
    for end in ("start", "end"):
        if isinstance(comp.get(end), dict):
            out.add(comp[end].get("source"))
    out.discard(None)
    return out


def select_state(state: dict, relevant: set) -> dict:
    """Semantic state the verdict depends on: records touching the relevant
    sources (and records of the same subject), their relations, validity of
    the relevant sources, counter-evidence and completeness."""
    recs = [r for r in state["records"] if set(r["sources"]) & relevant]
    subjects = {r["subject"] for r in recs if r["subject"]}
    recs = [r for r in state["records"] if set(r["sources"]) & relevant or r["subject"] in subjects]
    ids = {r["id"] for r in recs}
    rels = [x for x in state.get("relations", []) if set(x["members"]) & ids]
    return {
        "records": recs,
        "relations": rels,
        "sources": {s: v for s, v in state.get("sources", {}).items() if s in relevant},
        "raw": state.get("raw", []),
        "current_message": state.get("current_message"),
        "counter_evidence": state.get("counter_evidence"),
        "completeness": state.get("completeness"),
    }


def review_key(*, view: dict, question: dict, query_meta: dict, expect: dict, item: dict,
               extras: list, source_texts: dict) -> str:
    cm = view["semantic_state"].get("current_message")
    if cm and cm.get("text") is not None:
        source_texts = dict(source_texts, current=cm["text"])  # quotes may cite the current message
    relevant = set(item["sources"]) | expect_sources(expect)
    for e in extras:
        relevant |= set(e["sources"])
    material = {
        "scoring_version": SCORING_VERSION,
        "adapter": view["adapter"],
        "question": {k: question.get(k) for k in ("case", "id", "query", "context", "at", "task_id")},
        "query_meta": query_meta,
        "expect_common": expect,
        "item": {"id": item["id"], "content": item["content"]},
        "quoted_sources": {s: source_texts.get(s) for s in sorted(item["sources"])},
        "extras": [{"id": e["id"], "content": e["content"],
                    "quoted_sources": {s: source_texts.get(s) for s in sorted(e["sources"])}} for e in extras],
        "semantic_state": select_state(view["semantic_state"], relevant),
    }
    return "sha256:" + hashlib.sha256(canonical(material).encode("utf-8")).hexdigest()


def finalize(*, scored: dict, view: dict, question: dict, query_meta: dict, expect: dict,
             source_texts: dict, ledger: dict, side: str) -> dict:
    """Attach ledger verdicts. Returns items/extras with `final` and `key`."""
    entries = ledger.get("entries", {})
    qname = f"{question['case']}/{question['id']}"

    def resolve(it):
        it = dict(it)
        if it["result"] == PASS:
            it["final"] = "pass"
        elif it["result"] == REVIEW:
            key = review_key(view=view, question=question, query_meta=query_meta, expect=expect,
                             item=it, extras=scored["extras"], source_texts=source_texts)
            it["key"] = key
            e = entries.get(key)
            if e and e.get("status") == "confirmed":
                it["final"] = e["verdict"]
                it["review"] = {"status": "confirmed", "verdict": e["verdict"], "reviewer": e.get("reviewer"),
                                "note": e.get("note")}
            else:
                it["final"] = "pending_review"
                if e:
                    it["review"] = {"status": e.get("status"), "verdict": e.get("verdict"),
                                    "reviewer": e.get("reviewer"), "note": e.get("note")}
        elif it["result"] == NOT_MEASURABLE:
            it["final"] = "not_measurable"
        else:
            it["final"] = "fail" if it["result"] == FAIL else "missing"
        it["question"], it["side"] = qname, side
        return it

    items = [resolve(i) for i in scored["items"]]
    extras = [resolve(i) for i in scored["extras"]]
    gated = [i for i in items if i["final"] != "not_measurable"]
    return {
        "question": qname,
        "side": side,
        "items": items,
        "extras": extras,
        "denominator": len(gated),
        "auto_pass": sum(1 for i in gated if i["result"] == PASS),
        "final_pass": sum(1 for i in gated if i["final"] == "pass"),
        "question_final_pass": all(i["final"] == "pass" for i in gated) and all(e["final"] == "pass" for e in extras),
        "question_auto_only_pass": all(i["result"] == PASS for i in gated) and not extras,
    }


def load_ledger(path) -> dict:
    try:
        with open(path, encoding="utf-8") as fh:
            return json.load(fh)
    except FileNotFoundError:
        return {"version": 1, "entries": {}}
