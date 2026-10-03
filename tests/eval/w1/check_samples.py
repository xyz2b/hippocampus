"""W1 / W3 样本结构检查：片段逐字在原文中、枚举合法、索引有效。用法：python3 tests/eval/w1/check_samples.py"""
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
SPEECH = {"user_statement", "question", "hypothetical", "reported", "plan"}
MEMORY_TYPES = {"fact", "preference", "constraint", "episode"}
ROLES = {"event", "validity", "retention"}
TIME_KINDS = {"date", "range", "relative", "month", "anchored", "recurring", "unknown"}
VERDICTS = {"supported", "contradicted", "insufficient"}
ASPECTS = {"subject", "relation", "memory_type", "value", "polarity", "strength",
           "uncertainty", "condition", "time", "speech"}
EPISODE = {"useful", "low_value", "unknown", "not_applicable"}
REQUESTS = {"remember", "retention", "do_not_store", "forget"}


def options(v):
    return v if isinstance(v, list) else [v]


def check_candidate(c, text, where, relations, entities, errors):
    def quote(q, field):
        if q is not None and q not in text:
            errors.append(f"{where}.{field}: {q!r} not in source text")

    for sentence in options(c.get("sentence")):
        quote(sentence, "sentence")
    # contracts 2.1：fact 的值与完整原句相同。可接受集合含 fact 时，每个 fact 组合都必须合法。
    if "fact" in options(c["relation"]) and c.get("kind", "assertion") == "assertion":
        if not set(options(c.get("value"))) & set(options(c.get("sentence"))):
            errors.append(f"{where}: relation fact requires value == sentence")
    for v in options(c.get("value")):
        # 值可以是规范化值（英文、实体名），只有含中文且看起来是原文片段时才要求逐字。
        if isinstance(v, str) and len(v) > 4:
            quote(v, "value")
    quote(c.get("uncertainty"), "uncertainty")
    for q in c.get("conditions", []):
        quote(q["quote"] if isinstance(q, dict) else q, "conditions")
    for tm in c.get("times", []):
        quote(tm["quote"], "times")
        if not set(options(tm["role"])) <= ROLES or tm["interp"]["kind"] not in TIME_KINDS:
            errors.append(f"{where}.times: bad role/kind")
    if not set(options(c.get("speech", "user_statement"))) <= SPEECH:
        errors.append(f"{where}.speech")
    if not set(options(c["memory_type"])) <= MEMORY_TYPES:
        errors.append(f"{where}.memory_type")
    if relations and not set(options(c["relation"])) <= set(relations):
        errors.append(f"{where}.relation not in catalog")
    s = c.get("subject", "self")
    if isinstance(s, dict):
        quote(s["unresolved"], "subject.unresolved")
    elif entities and s not in entities:
        errors.append(f"{where}.subject not in catalog")


def check_w1(s, errors):
    i, text = s["input"], s["input"]["text"]
    entities = [e["id"] for e in i["entities"]]
    e = s["expect"]
    if e["candidate_count"] != len(e["candidates"]):
        errors.append("candidate_count mismatch")
    for n, c in enumerate(e["candidates"]):
        check_candidate(c, text, f"candidates[{n}]", i["relations"], entities, errors)
    for n, r in enumerate(e["memory_requests"]):
        if r["kind"] not in REQUESTS or r["quote"] not in text:
            errors.append(f"memory_requests[{n}] kind/quote")
        if r.get("target_quote") and r["target_quote"] not in text:
            errors.append(f"memory_requests[{n}].target_quote")
        if any(k >= e["candidate_count"] for k in r["candidate_indices"]):
            errors.append(f"memory_requests[{n}] index out of range")
        d = r.get("deadline")
        if d is not None and (d["role"] != "retention" or d["quote"] not in text):
            errors.append(f"memory_requests[{n}].deadline")
        if r["kind"] != "retention" and d is not None:
            errors.append(f"memory_requests[{n}] non-retention with deadline")


def check_w3(s, errors):
    i, text = s["input"], s["input"]["text"]
    for n, c in enumerate(i["candidates"]):
        check_candidate(c, text, f"input.candidates[{n}]", None, None, errors)
    e = s["expect"]
    if [r["candidate"] for r in e["reviews"]] != list(range(len(i["candidates"]))):
        errors.append("reviews must map 1:1 to candidates")
    for r in e["reviews"]:
        if r["verdict"] not in VERDICTS or not set(r["aspects"]) <= ASPECTS:
            errors.append(f"review {r['candidate']} enum")
        if r["verdict"] == "supported" and r["aspects"]:
            errors.append(f"review {r['candidate']} supported with problems")
        if r["episode_value"] not in EPISODE:
            errors.append(f"review {r['candidate']} episode_value")
        if (r["episode_value"] != "not_applicable") != (i["candidates"][r["candidate"]]["memory_type"] == "episode"):
            errors.append(f"review {r['candidate']} episode_value vs memory_type")
    if e["coverage"]["status"] not in {"complete", "gaps"}:
        errors.append("coverage.status")
    if (e["coverage"]["status"] == "gaps") != bool(e["coverage"]["missing_contains"]):
        errors.append("coverage gaps need missing quotes")
    for q in e["coverage"]["missing_contains"]:
        if q not in text:
            errors.append(f"coverage.missing {q!r} not in text")
    if len(e["request_reviews"]) != len(i["requests"]):
        errors.append("request_reviews must map 1:1 to requests")
    for r in e["request_reviews"]:
        if r["verdict"] not in VERDICTS:
            errors.append("request verdict")


NEED = {"context_only", "judge"}
CURRENT = {"assertion_present", "query_only", "unknown"}
TYPES = {"single", "aggregate", "comparison", "temporal", "open"}
OPS = {"lookup", "list", "count", "sum", "compare", "elapsed"}


def check_r1(s, errors):
    i, e = s["input"], s["expect"]
    text = "\n".join([i["message"], *i["context"]])
    entities = {x["id"] for x in i["entities"]}
    for field, allowed in (("need", NEED), ("current_evidence", CURRENT), ("type", TYPES), ("plan.op", OPS)):
        if not set(options(e[field])) <= allowed:
            errors.append(f"{field} enum")
    for r in options(e["plan.filter.relation"]):
        if r is not None and r not in i["relations"]:
            errors.append("plan.filter.relation not in catalog")
    for subj in options(e["subject"]) if isinstance(e["subject"], list) else [e["subject"]]:
        if isinstance(subj, dict) and "unresolved" in subj:
            if subj["unresolved"] not in text:
                errors.append("subject.unresolved not in text")
        elif isinstance(subj, dict) and "ambiguous" in subj:
            if len(set(subj["ambiguous"])) < 2 or not set(subj["ambiguous"]) <= entities:
                errors.append("subject.ambiguous")
        elif subj not in entities:
            errors.append("subject not in catalog")
    for c in e["conditions"]:
        if not any(q in i["message"] for q in options(c)):
            errors.append(f"condition {c!r} not in message")
    if e["time"] is not None and e["time"]["quote"] not in i["message"]:
        errors.append("time quote not in message")
    if s.get("split") not in {"dev", "holdout"}:
        errors.append("split")


def main():
    failed = 0
    for contract, check in (("w1", check_w1), ("w3", check_w3), ("r1", check_r1)):
        for path in sorted((ROOT / contract / "samples").glob("*.json")):
            s = json.loads(path.read_text())
            errors = []
            if s["contract"] != f"{contract}.v1" or s["label_status"] not in {"draft", "reviewed"}:
                errors.append("contract/label_status")
            check(s, errors)
            failed += bool(errors)
            print(("ok  " if not errors else "FAIL") + f" {s['id']}" + "".join(f"\n     {x}" for x in errors))
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
