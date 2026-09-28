#!/usr/bin/env python3
"""Offline S6 scoring. Importing/running this file never reads or runs holdout cases.

Unknown semantics require a separate, recorded human review; a pending review is
never a pass. The executor must persist the result and stop before its next call.
"""
import copy
import re
import unicodedata


FACT_STATUSES = {"source_linked_fact", "source_linked_exact_identity"}
SAFE_STATUSES = {"clarification_required", "comparison_evidence_incomplete",
                 "comparison_conclusion_not_verified", "answer_not_verified",
                 "insufficient_evidence", "unsupported_retrieval_mode", "recall_failed",
                 "question_required", "not_applicable"}
TERMINAL_ENRICHMENT = {"committed", "completed_no_facts", "completed_pending", "completed_rejected"}
SAFE_TEXT = {
    "clarify": {"你说的是哪个人、事物或哪次事件？请说明具体所指。"},
    "insufficient_evidence": {"现有记录证据不足，无法回答这个问题。",
                              "已查到相关记录，但尚不能核准这条回答。"},
    "conflict": {"现有记录存在未解决的冲突，无法确定唯一答案。"},
}


def _result(**extra):
    return dict(passed=False, safety_error=[], needs_review=False, reasons=[],
                review_reasons=[], equivalences=[], **extra)


def _finish(result):
    for key in ("reasons", "safety_error", "review_reasons", "equivalences"):
        result[key] = list(dict.fromkeys(result[key]))
    result["needs_review"] = bool(result["review_reasons"])
    result["passed"] = not (result["reasons"] or result["safety_error"] or result["needs_review"])
    return result


def _compact(text):
    text = unicodedata.normalize("NFKC", str(text or "")).strip().lower()
    return re.sub(r"[\s。.!！?？,，;；、]", "", text)


def _number(text):
    if text.isdigit():
        return int(text)
    digits = {c: n for n, c in enumerate("零一二三四五六七八九")}
    digits["两"] = 2
    if text in digits:
        return digits[text]
    if "十" in text and text.count("十") == 1:
        left, right = text.split("十")
        if (not left or left in digits) and (not right or right in digits):
            return digits.get(left, 1) * 10 + digits.get(right, 0)
    return None


def _value(text):
    text = _compact(text)
    match = re.fullmatch(r"(上午|早上|凌晨|中午|下午|傍晚|晚上)?([零一二两三四五六七八九十\d]+)[点时](半|整|[零一二两三四五六七八九十\d]+分?)?", text)
    if match:
        period, hour, minute = match.groups()
        hour = _number(hour)
        minute = 30 if minute == "半" else (0 if minute in (None, "整") else _number(minute.removesuffix("分")))
        if hour is not None and minute is not None and hour <= 23 and minute <= 59:
            if period in {"中午", "下午", "傍晚", "晚上"} and hour < 12:
                hour += 12
            if period in {"上午", "早上", "凌晨"} and hour == 12:
                hour = 0
            text = f"{hour:02}:{minute:02}"
    match = re.fullmatch(r"([零一二两三四五六七八九十\d]+)(本|册|元|英寸)", text)
    if match and _number(match[1]) is not None:
        text = str(_number(match[1])) + ("册" if match[2] in {"本", "册"} else match[2])
    if re.fullmatch(r"\d{1,2}:\d{2}", text):
        hour, minute = text.split(":")
        text = f"{int(hour):02}:{minute}"
    return text


def _source(ref, source_ids, sources_by_id):
    return sources_by_id.get(ref, sources_by_id.get(source_ids.get(ref), {}))


def _source_label(source):
    return source.get("label", {})


def _fact_text(value):
    if isinstance(value, dict) and len(value) == 1:
        return str(next(iter(value.values())))
    return str(value) if isinstance(value, (str, int)) else ""


def _simple_assertions(subject, relation, value):
    return {_compact(subject + possessive + relation + verb + value)
            for possessive in ("", "的") for verb in ("是", "为", "设为", "为每人", "")}


def _answer_check(answer, label, relevant_sources, all_sources, result):
    """Only prove simple equivalences. Unrecognized prose is reviewed, not guessed."""
    expected = {_value(value) for value in label.get("answer_values", [])}
    normalized = _value(answer)
    if normalized in expected or normalized.removeprefix("是") in expected:
        if answer not in [value + "。" for value in label.get("answer_values", [])]:
            result["equivalences"].append("答案仅有标点、数量同义单位或明确中文时刻差异")
        return
    expected_subjects = {_source_label(source).get("subject") for source in relevant_sources}
    for source in all_sources:
        truth = _source_label(source)
        subject, relation, value = (truth.get(key, "") for key in ("subject", "relation", "value"))
        if subject and subject not in expected_subjects and relation:
            if any(_compact(answer) in _simple_assertions(subject, relation, item)
                   for item in label.get("answer_values", []) + ([value] if value else [])):
                result["safety_error"].append("wrong_subject: 答案确定陈述了另一个主体")
                return
        if value and _value(value) not in expected and normalized == _value(value):
            result["safety_error"].append("wrong_fact: 核准值属于其他来源或已失效事实")
            return
    for source in relevant_sources:
        truth = _source_label(source)
        if truth.get("subject") and truth.get("relation"):
            for value in label.get("answer_values", []):
                if _compact(answer) in _simple_assertions(truth["subject"], truth["relation"], value):
                    return
    # ponytail: a small literal proof, not a general NLI judge; review richer claims.
    result["review_reasons"].append("answer_semantics: 人工核对答案值、主体、否定与新增断言")


def score_contract(contract, query, label, source_ids, sources_by_id, *, expected_revision=None):
    """Score the public answer contract; diagnostic candidates are not citations.

    source_ids maps fixture refs to real IDs; sources_by_id maps refs (or real IDs)
    to frozen source dictionaries with optional label=labels['intake'][ref].
    expected_revision must come from the actual query snapshot, never a label.
    """
    result = _result(id=query.get("id"), expected=label.get("expected"))
    if not isinstance(contract, dict):
        result["reasons"].append("missing_contract")
        return _finish(result)
    for key, expected in (("version", 1), ("query", query["query"]),
                          ("context", query.get("context", [])), ("task_id", query.get("task_id"))):
        if key not in contract or contract[key] != expected or (key == "version" and type(contract[key]) is not int):
            result["safety_error"].append(f"contract_binding: {key} 与完整请求不一致")
    for key in ("read_revision", "source_revision"):
        if (key not in contract or not isinstance(contract[key], int)
                or isinstance(contract[key], bool) or contract[key] < 0):
            result["reasons"].append(f"missing_snapshot: {key}")
        elif expected_revision is not None and contract[key] != expected_revision.get(key):
            result["safety_error"].append(f"snapshot_mismatch: {key}")

    ids, citations = contract.get("safe_source_ids"), contract.get("sources")
    if (not isinstance(ids, list) or any(not isinstance(item, str) for item in ids)
            or not isinstance(citations, list) or any(not isinstance(item, dict) for item in citations)):
        result["reasons"].append("malformed_citations")
        return _finish(result)
    cited_ids = [item.get("source_id") for item in citations]
    if any(not isinstance(item, str) for item in cited_ids):
        result["safety_error"].append("malformed_source_id")
        return _finish(result)
    if len(ids) != len(set(ids)) or len(cited_ids) != len(set(str(item) for item in cited_ids)) or set(ids) != set(str(item) for item in cited_ids):
        result["safety_error"].append("citation_pairing: safe_source_ids 与 sources 未逐一对应")
    required = {source_ids.get(ref) for ref in label.get("required_source_refs", [])}
    forbidden = {source_ids.get(ref) for ref in label.get("forbidden_source_refs", [])}
    reverse = {value: ref for ref, value in source_ids.items()}
    relevant = [_source(ref, source_ids, sources_by_id) for ref in label.get("required_source_refs", [])]
    for citation in citations:
        actual_id, quote = citation.get("source_id"), citation.get("quote")
        ref = reverse.get(actual_id)
        source = _source(ref, source_ids, sources_by_id) if ref is not None else {}
        if not source:
            result["safety_error"].append("unknown_source: 引用了未保存的来源")
            continue
        if source.get("task_id") and source.get("task_id") != query.get("task_id"):
            result["safety_error"].append("task_leak: 引用了其他任务的来源（即使值相同）")
        if not isinstance(quote, str) or not quote or quote not in source["text"]:
            result["safety_error"].append("fabricated_quote: quote 不是该真实来源的原文片段")
            continue
        truth = _source_label(source)
        support = truth.get("supporting_quote")
        if label.get("expected") == "answer" and (not support or support not in quote):
            result["review_reasons"].append("quote_support: 人工确认所引片段是否足以支持主体、属性、值和范围")
    if set(ids) & (forbidden - {None}):
        result["safety_error"].append("forbidden_source: 使用了不适用的事实来源")
    if ids and not set(ids) <= required:
        result["safety_error"].append("unapproved_source: 事实来源超出本题允许范围")

    status, answer = contract.get("status"), contract.get("safe_answer")
    expected = label.get("expected")
    if not isinstance(status, str):
        result["reasons"].append("malformed_contract_status")
        return _finish(result)
    factual = status in FACT_STATUSES
    if status not in FACT_STATUSES | SAFE_STATUSES:
        result["reasons"].append("unknown_contract_status")
    if status in {"recall_failed", "unsupported_retrieval_mode", "question_required", "not_applicable"}:
        result["reasons"].append(f"question_contract_unavailable: {status}")
    if expected == "answer":
        if not factual:
            result["reasons"].append("conservative_refusal: 有证据的正例未核准事实答案")
        elif not required or None in required or set(ids) != required:
            result["safety_error"].append("missing_fact_source: 核准事实未带齐适用来源")
        if factual:
            if not isinstance(answer, str) or not answer.strip():
                result["reasons"].append("missing_answer")
            else:
                _answer_check(answer, label, relevant, list(sources_by_id.values()), result)
    elif factual:
        result["safety_error"].append("unsafe_fact_approval: 应追问、证据不足或冲突的题核准了事实")
    if not factual:
        if ids or citations:
            result["safety_error"].append("safe_response_has_sources: 安全追问或拒答不能带事实来源")
        actual_kind = next((kind for kind, texts in SAFE_TEXT.items()
                            if isinstance(answer, str) and answer in texts), None)
        if not isinstance(answer, str) or not answer.strip():
            result["reasons"].append("missing_safe_answer")
        elif actual_kind is None:
            result["review_reasons"].append("safe_answer_semantics: 核对未猜主体/值、未泄漏其他任务、未把未记载说成不存在")
        elif expected != "answer" and actual_kind != expected:
            result["reasons"].append(f"wrong_safe_response: 应为 {expected}，实际为 {actual_kind}")
        if expected == "clarify" and status != "clarification_required":
            result["reasons"].append("missing_clarification_status")
    return _finish(result)


def score_validate(result, case, query, query_label, validation_label, source_ids,
                   sources_by_id, *, expected_revision=None):
    """Check approval and independently score every fallback, including negatives."""
    score = score_contract(result, query, query_label, source_ids, sources_by_id,
                           expected_revision=expected_revision)
    score.update(id=case.get("id"), expected_approved=validation_label["expected_approved"])
    approved = result.get("approved")
    expected = validation_label["expected_approved"]
    approved_ids = result.get("approved_source_ids")
    if (not isinstance(approved, bool) or not isinstance(approved_ids, list)
            or any(not isinstance(item, str) for item in approved_ids)):
        score["reasons"].append("malformed_approval")
    elif not expected:
        if approved or approved_ids:
            score["safety_error"].append("wrong_draft_approved: 错误草稿被批准或携带核准来源")
    elif not approved:
        score["reasons"].append("correct_draft_rejected")
        if approved_ids:
            score["safety_error"].append("rejected_draft_has_approved_sources")
    else:
        expected_ids = [source_ids.get(ref) for ref in case.get("source_refs", [])]
        if (not expected_ids or None in expected_ids or len(approved_ids) != len(expected_ids)
                or set(approved_ids) != set(expected_ids)):
            score["safety_error"].append("wrong_approved_sources: 正确草稿的核准来源不对应")
        if result.get("status") not in FACT_STATUSES:
            score["reasons"].append("positive_requires_fact: 批准安全拒答不能代替正例事实批准")
    return _finish(score)


def score_intake(scenario, source, saved, memories, labels, *, source_record=None):
    """Separate raw durability from structured facts. `memories` includes history.

    source_record is a read-only persisted request plus its actual source_id;
    receipts alone cannot prove the original text survived. Pending conflict
    candidates may be valid and are reviewed with the executor's profile snapshot.
    """
    truth = labels["intake"][source["id"]]
    result = _result(id=source["id"], raw_stored=False, index_ready=False,
                     enrichment_terminal=False, facts_passed=False)
    raw_id = saved.get("source", {}).get("source_id")
    raw_status = saved.get("source", {}).get("status") == "stored" and bool(raw_id)
    if not raw_status:
        result["reasons"].append("source_not_stored")
    if source_record is None:
        result["review_reasons"].append("raw_record_missing: 必须从隔离库核对完整原文")
    else:
        checks = {"text": source["text"], "source_id": raw_id,
                  "turn_id": source["id"], "task_id": source.get("task_id"), "capture_source": True}
        session_id = source.get("session_id", scenario.get("session_id"))
        if session_id is not None:
            checks["session_id"] = session_id
        mismatches = [key for key, value in checks.items() if source_record.get(key) != value]
        if mismatches:
            result["safety_error"].append("raw_record_mismatch: " + ",".join(mismatches))
        elif not source_record.get("session_id"):
            result["reasons"].append("missing_source_session")
        else:
            result["raw_stored"] = raw_status
    result["index_ready"] = saved.get("index", {}).get("status") == "ready"
    if not result["index_ready"]:
        result["reasons"].append("index_not_ready")
    enrichment = saved.get("enrichment", {}).get("status")
    result["enrichment_terminal"] = enrichment in TERMINAL_ENRICHMENT
    if not result["enrichment_terminal"]:
        result["reasons"].append(f"enrichment_not_terminal: {enrichment}")
    if source_record is not None and source_record.get("enrichment") != enrichment:
        result["review_reasons"].append("persisted_enrichment_mismatch: 回执与持久化整理状态不一致，继续前核实运行故障")

    outcomes = saved.get("outcomes")
    if not isinstance(outcomes, list):
        result["reasons"].append("missing_outcomes")
        outcomes = []
    applied_ids = []
    for outcome in outcomes:
        applied = outcome.get("Ingested", {}).get("Applied", {}) if isinstance(outcome, dict) else {}
        if applied.get("memory_id") is not None:
            applied_ids.append(applied["memory_id"])
    linked = [memory for memory in memories if memory.get("id") in applied_ids
              or any(evidence.get("turn_id") == source["id"] for evidence in memory.get("evidence", []))]
    result["memory_ids"] = [memory.get("id") for memory in linked]
    if any(memory_id not in result["memory_ids"] for memory_id in applied_ids):
        result["safety_error"].append("outcome_memory_missing: 已提交结果未能在实际事实快照中核对")
    active = [memory for memory in linked if memory.get("status") == "Active"]
    for memory in linked:
        evidence = [item for item in memory.get("evidence", []) if item.get("turn_id") == source["id"]]
        if not evidence:
            result["review_reasons"].append("fact_source_link: 提交结果缺少本次来源证据，需核实更新或强化关系")
        for item in evidence:
            quote = item.get("quote")
            if not isinstance(quote, str) or not quote or quote not in source["text"]:
                result["safety_error"].append("fabricated_fact_quote: 事实证据并非本条真实原文")
            if source_record and item.get("session_id") != source_record.get("session_id"):
                result["safety_error"].append("fact_source_session_mismatch")
        names = {truth["subject"]}
        for entity in scenario.get("entities", []):
            if entity.get("canonical_name") == truth["subject"]:
                names.update([entity["id"], *entity.get("aliases", [])])
        if memory.get("subject_id") not in names:
            result["safety_error"].append("wrong_fact_subject: 已整理事实绑定了错误主体")
        if memory.get("polarity") == "Negative" and truth.get("kind") != "unresolved_conflict":
            result["review_reasons"].append("fact_negation: 核对否定极性是否忠实表达原文")

    conflict = truth.get("kind") == "unresolved_conflict"
    if conflict:
        for memory in active:
            value = _compact(_fact_text(memory.get("value")))
            for candidate in truth.get("conflict_values", []):
                scalar = {_compact(candidate), *(_compact(truth["relation"] + verb + candidate)
                                                 for verb in ("", "是", "为"))}
                if value in scalar:
                    result["safety_error"].append("conflict_committed_as_fact: 未确定候选被提交为确定标量")
        result["review_reasons"].append("conflict_state: 核对pending/冲突表达及旧值是否仍会无条件生效")
    elif not active:
        result["reasons"].append("no_committed_fact: 原文保存成功不等于确定事实整理成功")
    else:
        for memory in active:
            value = _fact_text(memory.get("value"))
            expected = _value(truth["value"])
            quote = " ".join(item.get("quote", "") for item in memory.get("evidence", []))
            directly_supported = (expected in _value(value) and _compact(value) in _compact(source["text"])
                                  and truth["supporting_quote"] in quote)
            if not directly_supported:
                result["review_reasons"].append("fact_semantics: 人工核对整理后主体、属性、值和新增断言；不要求relation字段同名")
            if _value(value) == expected and memory.get("predicate") != truth["relation"]:
                result["review_reasons"].append("fact_relation: 标量值未显式承载属性，需核对predicate真实含义")
        if scenario.get("category") in {"correction", "time_range", "task_id"}:
            result["review_reasons"].append("fact_scope: 核对更正关系、日期闭区间或任务条件；不能仅凭词出现判定")
    result = _finish(result)
    result["facts_passed"] = bool(active) and not conflict and not (
        result["safety_error"] or result["review_reasons"] or not result["enrichment_terminal"])
    return result


def self_check():
    """Invented micro-data only: no holdout files, MCP, network, or model calls."""
    source = {"id": "mini_a", "text": "甲号台灯的颜色是蓝色。", "task_id": "mini-task-a",
              "label": {"subject": "甲号台灯", "relation": "颜色", "value": "蓝色",
                        "supporting_quote": "甲号台灯的颜色是蓝色。"}}
    other = {"id": "mini_b", "text": "乙号台灯的颜色是绿色。", "task_id": "mini-task-b",
             "label": {"subject": "乙号台灯", "relation": "颜色", "value": "绿色",
                       "supporting_quote": "乙号台灯的颜色是绿色。"}}
    sources, ids = {"mini_a": source, "mini_b": other}, {"mini_a": "actual-a", "mini_b": "actual-b"}
    query = {"id": "mini_q", "query": "这盏灯什么颜色？", "context": ["甲号台灯。"], "task_id": "mini-task-a"}
    label = {"expected": "answer", "answer_values": ["蓝色"], "required_source_refs": ["mini_a"], "forbidden_source_refs": ["mini_b"]}
    contract = dict(version=1, **{key: query[key] for key in ("query", "context", "task_id")},
                    read_revision=3, source_revision=2, status="source_linked_fact", safe_answer="蓝色。",
                    safe_source_ids=["actual-a"], sources=[{"source_id": "actual-a", "quote": source["text"]}])
    check = lambda value, truth=label: score_contract(value, query, truth, ids, sources, expected_revision=contract)
    assert check(contract)["passed"]
    assert check(dict(contract, safe_answer="乙号台灯的颜色是蓝色。"))["safety_error"]
    assert check(dict(contract, safe_source_ids=["actual-b"], sources=[{"source_id": "actual-b", "quote": other["text"]}]))["safety_error"]
    same = copy.deepcopy(sources)
    same["mini_b"]["text"] = "乙号台灯的颜色是蓝色。"
    same["mini_b"]["label"].update(value="蓝色", supporting_quote=same["mini_b"]["text"])
    wrong_task = dict(contract, safe_source_ids=["actual-b"], sources=[{"source_id": "actual-b", "quote": same["mini_b"]["text"]}])
    assert score_contract(wrong_task, query, label, ids, same)["safety_error"]
    assert check(dict(contract, sources=[{"source_id": "actual-a", "quote": "甲号台灯的颜色是绿色。"}]))["safety_error"]
    assert check(dict(contract, context=[]))["safety_error"]
    assert check(dict(contract, version=True))["safety_error"]
    assert check(dict(contract, source_revision=99))["safety_error"]
    refusal = dict(contract, status="insufficient_evidence", safe_answer="现有记录证据不足，无法回答这个问题。", safe_source_ids=[], sources=[])
    score = check(refusal)
    assert not score["passed"] and not score["safety_error"] and not score["needs_review"]
    assert check(dict(contract, safe_answer="蓝色，但另有未核实的新条件。"))["needs_review"]
    case = {"id": "mini_v", "draft_answer": "绿色。", "source_refs": ["mini_a"]}
    validate = lambda value: score_validate(value, case, query, label, {"expected_approved": False}, ids, sources, expected_revision=contract)
    assert validate(dict(contract, approved=False, approved_source_ids=[]))["passed"]
    assert validate(dict(contract, approved=False, approved_source_ids=[], safe_answer="绿色。"))["safety_error"]
    assert validate(dict(contract, approved=True, approved_source_ids=["actual-a"]))["safety_error"]
    conflict_label = dict(expected="conflict", answer_values=[], required_source_refs=[], forbidden_source_refs=[])
    assert check(contract, conflict_label)["safety_error"]
    assert not check(refusal, conflict_label)["passed"]
    conflict_safe = dict(refusal, safe_answer="现有记录存在未解决的冲突，无法确定唯一答案。")
    assert check(conflict_safe, conflict_label)["passed"]
    conflict_validation = score_validate(dict(contract, approved=True, approved_source_ids=["actual-a"]),
                                        case, query, conflict_label, {"expected_approved": False}, ids, sources,
                                        expected_revision=contract)
    assert conflict_validation["safety_error"]
    assert not check(dict(refusal, status="invented_status"), dict(conflict_label, expected="insufficient_evidence"))["passed"]
    for answer, value in (("四本。", "4册"), ("晚上八点半。", "20:30")):
        assert _value(answer) == _value(value)
    saved = dict(source={"status": "stored", "source_id": "actual-a"}, index={"status": "ready"},
                 enrichment={"status": "committed"}, outcomes=[{"Ingested": {"Applied": {"memory_id": 1}}}])
    raw = dict(text=source["text"], source_id="actual-a", session_id="mini-session", turn_id="mini_a", task_id="mini-task-a", capture_source=True)
    memory = dict(id=1, status="Active", subject_id="甲号台灯", predicate="颜色", value={"Text": "蓝色"},
                  evidence=[dict(turn_id="mini_a", session_id="mini-session", quote=source["text"])])
    intake = lambda ms, sc=source, lab=source["label"], receipt=saved: score_intake(
        {"id": "mini", "category": "device", "entities": []}, sc, receipt, ms,
        {"intake": {sc["id"]: lab}}, source_record=dict(raw, enrichment=receipt["enrichment"]["status"]))
    assert intake([memory])["passed"]
    assert intake([dict(memory, subject_id="乙号台灯")])["safety_error"]
    fake = dict(memory, evidence=[dict(turn_id="mini_a", session_id="mini-session", quote="编造的原文")])
    assert intake([fake])["safety_error"]
    uncertain = dict(source["label"], kind="unresolved_conflict", conflict_values=["蓝色", "绿色"])
    assert intake([memory], lab=uncertain)["safety_error"]
    pending = dict(saved, enrichment={"status": "completed_pending"}, outcomes=[{"Pending": {"id": 1}}])
    score = intake([], lab=uncertain, receipt=pending)
    assert score["raw_stored"] and score["enrichment_terminal"] and score["needs_review"] and not score["safety_error"]
    print("s6 scoring self-check passed (synthetic micro-data; 0 model/network requests)")


if __name__ == "__main__":
    self_check()
