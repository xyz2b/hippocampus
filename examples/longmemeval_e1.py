#!/usr/bin/env python3
"""Prepare pinned LongMemEval E1 inputs without model calls or history truncation.

python3 examples/longmemeval_e1.py --self-check
python3 examples/longmemeval_e1.py --output /tmp/pmr-longmemeval/e1
The runner reads ingestion.jsonl and queries.jsonl only; labels.json is evaluator-only.
"""
import argparse
from collections import Counter
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re

REVISION = "98d7416c24c778c2fee6e6f3006e7a073259d48f"
DATASET_SHA256 = "d6f21ea9d60a0d56f34a05b609c79c88a451d2ae03597821ea3d5a9678c3a442"
URL = f"https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned/resolve/{REVISION}/longmemeval_s_cleaned.json"


def digest(value):
    return hashlib.sha256(value).hexdigest()


def clock(value):
    match = re.fullmatch(r"(\d{4}/\d{2}/\d{2}) \(([A-Za-z]{3})\) (\d{2}:\d{2})", value)
    if not match:
        raise ValueError(f"unsupported dataset date: {value}")
    stamp = datetime.strptime(match[1] + " " + match[3], "%Y/%m/%d %H:%M").replace(tzinfo=timezone.utc)
    if ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")[stamp.weekday()] != match[2]:
        raise ValueError(f"dataset weekday disagrees with date: {value}")
    return int(stamp.timestamp())


def segments(text):
    """Byte ranges matching lexical.rs's 768-scalar windows and 128-scalar overlap."""
    offsets = [0]
    for char in text:
        offsets.append(offsets[-1] + len(char.encode()))
    result, start = [], 0
    while start < len(text):
        end = min(start + 768, len(text))
        result.append([offsets[start], offsets[end]])
        if end == len(text):
            break
        start = end - 128
    return result


def sessions(case):
    ids, dates, contents = (case[k] for k in ("haystack_session_ids", "haystack_dates", "haystack_sessions"))
    if not len(ids) == len(dates) == len(contents):
        raise ValueError("session IDs, dates and contents must have identical lengths")
    return sorted(((clock(date), index, sid, date, turns)
                   for index, (sid, date, turns) in enumerate(zip(ids, dates, contents))), key=lambda row: row[:2])


def describe(case, index):
    ordered = sessions(case)
    turns = [turn for *_, session in ordered for turn in session]
    if any(t["role"] not in ("user", "assistant") or not isinstance(t["content"], str) for t in turns):
        raise ValueError("unknown role or non-text content")
    user = [t for t in turns if t["role"] == "user"]
    marked = [t for t in turns if t.get("has_answer") is True]
    marked_sessions = {sid for _, _, sid, _, ts in ordered if any(t.get("has_answer") is True for t in ts)}
    missing = sorted(set(case["answer_session_ids"]) - marked_sessions)
    abstention = "_abs" in case["question_id"]
    roles = Counter(t["role"] for t in marked)
    reasons = []
    if abstention:
        reasons.append("abstention_separate")
    else:
        if roles["assistant"]:
            reasons.append("assistant_evidence_required")
        if not marked or missing:
            reasons.append("required_session_missing_turn_labels")
    if ordered and ordered[-1][0] > clock(case["question_date"]):
        reasons.append("history_after_question")
    if len(case["haystack_session_ids"]) != len(set(case["haystack_session_ids"])):
        reasons.append("duplicate_session_ids")
    over_bytes = sum(len(t["content"].encode()) > 24000 for t in user)
    return dict(index=index, question_id=case["question_id"], question_type=case["question_type"],
                sessions=len(ordered), user_turns=len(user), assistant_turns=len(turns) - len(user),
                user_text_bytes=sum(len(t["content"].encode()) for t in user),
                user_text_chars=sum(len(t["content"]) for t in user),
                max_user_turn_bytes=max((len(t["content"].encode()) for t in user), default=0),
                over_24000_bytes=over_bytes, over_24000_chars=sum(len(t["content"]) > 24000 for t in user),
                user_turns_over_768_chars=sum(len(t["content"]) > 768 for t in user),
                expected_source_segments=sum(len(segments(t["content"])) for t in user),
                required_user_turns=roles["user"], required_assistant_turns=roles["assistant"],
                required_turns_with_single_fragment_full_text=sum(len(t["content"]) <= 768 for t in marked),
                missing_turn_label_sessions=missing, exclusion_reasons=reasons,
                user_evidence_role_eligible=bool(not abstention and marked and not roles["assistant"] and not missing),
                development_eligible=not reasons, runnable_with_current_input_limit=not reasons and not over_bytes,
                extraction_physical_request_upper_bound=2 * len(user))


def history_groups(dataset):
    parent, seen = list(range(len(dataset))), {}

    def find(index):
        while parent[index] != index:
            parent[index] = parent[parent[index]]
            index = parent[index]
        return index

    for index, case in enumerate(dataset):
        for sid, turns in zip(case["haystack_session_ids"], case["haystack_sessions"]):
            content = [[t["role"], t["content"]] for t in turns]
            # Exact history overlap is conservative; semantic topics still need separate review.
            for key in [("id", sid), ("content", digest(json.dumps(content, ensure_ascii=False).encode()))]:
                if key in seen:
                    parent[find(index)] = find(seen[key])
                else:
                    seen[key] = index
    members = {}
    for index in range(len(dataset)):
        members.setdefault(find(index), []).append(index)
    return {index: f"history-group-{min(group):04d}" for group in members.values() for index in group}


def project(case, index):
    case_id = f"c{index:04d}"
    owner = f"lme-e1-{REVISION[:8]}-{case_id}"
    inputs, mapping, required = [], [], []
    for observed, session_index, sid, date, turns in sessions(case):
        session_id = f"{case_id}-s{session_index:04d}"
        history = []
        for turn_index, turn in enumerate(turns):
            if turn["role"] != "user":
                continue
            text, turn_id = turn["content"], f"t{turn_index:04d}"
            inputs.append(dict(case_id=case_id, owner_id=owner, session_id=session_id, turn_id=turn_id,
                               text=text, observed_at=observed, utc_offset_seconds=0, history=history.copy()))
            mapping.append(dict(session_id=session_id, turn_id=turn_id, original_session_id=sid,
                                original_session_index=session_index, original_turn_index=turn_index,
                                original_date=date, role="user", text=text, text_sha256=digest(text.encode())))
            if turn.get("has_answer") is True:
                required.append(dict(session_id=session_id, turn_id=turn_id, text=text,
                                     full_turn_byte_range=[0, len(text.encode())], expected_segments=segments(text)))
            history.append(text)
    query = dict(case_id=case_id, question_id=case_id, owner_id=owner,
                 query=case["question"], at=clock(case["question_date"]), utc_offset_seconds=0)
    labels = dict(case_id=case_id, owner_id=owner, official_question_id=case["question_id"], question_type=case["question_type"],
                  answer=case["answer"], question_date=case["question_date"],
                  answer_session_ids=case["answer_session_ids"], source_mapping=mapping, required_turns=required,
                  evidence_granularity="official has_answer marks whole turns, not atomic information points",
                  full_turn_coverage="union returned byte ranges per runtime session/turn; verify bytes against text",
                  session_recall="unique matched required sessions / required sessions; report unrelated sessions separately")
    return inputs, query, labels


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def score_pair(pair, labels):
    if pair["owner_id"] != labels["owner_id"] or not pair["shared_snapshot_verified"]:
        raise ValueError("pair owner or snapshot does not match the evaluation contract")
    sources = {(s["session_id"], s["turn_id"]): s["text"].encode() for s in labels["source_mapping"]}
    required = {(s["session_id"], s["turn_id"]): s["text"].encode() for s in labels["required_turns"]}
    required_sessions = {key[0] for key in required}
    scores = []
    for variant in pair["paired"]:
        result = variant["result"]
        historical = [entry["source"] for entry in result["historical_or_conflicting"] if "source" in entry]
        returned, ranges = set(), {}
        for source in result["source_evidence"] + historical:
            key = (source["session_id"], source["turn_id"])
            start, end = source["byte_start"], source["byte_end"]
            if key not in sources or not 0 <= start < end <= len(sources[key]) or sources[key][start:end] != source["text"].encode():
                raise ValueError("returned source text or byte range differs from the frozen USER history")
            returned.add((*key, start, end))
            if key in required:
                ranges.setdefault(key, []).append((start, end))
        coverage = []
        for key, text in required.items():
            covered, previous_end = 0, 0
            for start, end in sorted(ranges.get(key, [])):
                covered += max(0, end - max(start, previous_end))
                previous_end = max(previous_end, end)
            coverage.append(dict(session_id=key[0], turn_id=key[1], returned_bytes=covered,
                                 required_bytes=len(text), whole_turn_covered=covered == len(text)))
        found_sessions = {key[0] for key in returned}
        scores.append(dict(fact_index_enabled=variant["fact_index_enabled"],
                           required_sessions_found=len(found_sessions & required_sessions), required_sessions_total=len(required_sessions),
                           session_recall=len(found_sessions & required_sessions) / len(required_sessions) if required_sessions else None,
                           required_turn_byte_coverage=coverage,
                           whole_turns_covered=sum(c["whole_turn_covered"] for c in coverage), required_turns_total=len(required),
                           non_answer_sessions=sorted(found_sessions - required_sessions),
                           non_answer_session_count=len(found_sessions - required_sessions),
                           unlabeled_segment_count=sum(key[:2] not in required for key in returned),
                           returned_segment_count=len(returned), historical_segment_count=len(historical),
                           evidence_status=result["evidence_status"], degraded_reasons=result["degraded_reasons"],
                           content_bytes=result["budget"]["content_bytes"]))
    return dict(case_id=labels["case_id"], official_question_id=labels["official_question_id"], paired=scores,
                scoring_scope="final returned source segments, including explicitly marked historical evidence",
                noise_proxy_definition="outside official answer session / has_answer turn labels; not a semantic irrelevance judgment",
                limitations="whole-turn byte coverage is not atomic information-point completeness; candidate recall, state correctness and final answers are not evaluated")


def self_check():
    case = dict(question_id="official-answer-id", question_type="single-session-user", question="probe",
                answer="secret label", question_date="2023/01/03 (Tue) 00:00", answer_session_ids=["answer_session"],
                haystack_session_ids=["later", "answer_session"],
                haystack_dates=["2023/01/02 (Mon) 00:00", "2023/01/01 (Sun) 00:00"],
                haystack_sessions=[[dict(role="user", content="later")],
                                   [dict(role="assistant", content="not user evidence"),
                                    dict(role="user", content="汉" * 769, has_answer=True),
                                    dict(role="user", content="follow-up")]])
    rows, query, labels = project(case, 7)
    assert len(rows) == 3 and rows[0]["text"] == "汉" * 769 and rows[1]["history"] == ["汉" * 769]
    assert all(set(row) == {"case_id", "owner_id", "session_id", "turn_id", "text", "observed_at", "utc_offset_seconds", "history"} for row in rows)
    assert "official-answer-id" not in json.dumps(rows) and "answer_session" not in json.dumps(rows)
    assert query["question_id"] == "c0007" and labels["answer"] == "secret label"
    assert segments("汉" * 769) == [[0, 2304], [1920, 2307]]
    assert describe(case, 7)["development_eligible"]
    other = json.loads(json.dumps(case))
    other["haystack_session_ids"] = ["different-1", "different-2"]
    assert len(set(history_groups([case, other]).values())) == 1
    case["haystack_sessions"][1][0]["has_answer"] = True
    assert "assistant_evidence_required" in describe(case, 7)["exclusion_reasons"]
    source = labels["required_turns"][0]
    pieces = [dict(session_id=source["session_id"], turn_id=source["turn_id"], byte_start=a, byte_end=b,
                   text=source["text"].encode()[a:b].decode()) for a, b in source["expected_segments"]]
    pair = dict(owner_id=labels["owner_id"], shared_snapshot_verified=True,
                paired=[dict(fact_index_enabled=False, result=dict(source_evidence=pieces + pieces,
                        historical_or_conflicting=[], evidence_status="sufficiency_not_verified",
                        degraded_reasons=[], budget=dict(content_bytes=2307)))])
    score = score_pair(pair, labels)["paired"][0]
    assert score["whole_turns_covered"] == 1 and score["returned_segment_count"] == 2
    assert score["required_turn_byte_coverage"][0]["returned_bytes"] == 2307
    pieces[0]["text"] = "fabricated"
    try:
        score_pair(pair, labels)
    except ValueError:
        pass
    else:
        raise AssertionError("mismatched returned evidence must fail scoring")
    try:
        clock("2023/01/01 (Mon) 00:00")
    except ValueError:
        pass
    else:
        raise AssertionError("weekday mismatch must reject")
    print("self-check passed: chronology, roles, label isolation, shared-history grouping, verified byte-union scoring")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dataset", type=Path, default=Path("/tmp/pmr-longmemeval/longmemeval_s_cleaned.json"))
    parser.add_argument("--output", type=Path, default=Path("/tmp/pmr-longmemeval/e1"))
    parser.add_argument("--question-id", action="append", help="explicit development question; repeat to select more")
    parser.add_argument("--pairs", type=Path, help="Score existing pair JSONL; does not load or ingest the dataset")
    parser.add_argument("--labels", type=Path, help="Evaluator-only labels accompanying --pairs")
    parser.add_argument("--self-check", action="store_true")
    args = parser.parse_args()
    if args.self_check:
        self_check()
        return
    if args.pairs:
        if not args.labels:
            parser.error("--pairs requires --labels")
        labels = {row["case_id"]: row for row in json.loads(args.labels.read_text())}
        pairs = [json.loads(line) for line in args.pairs.read_text().splitlines() if line.strip()]
        if not pairs:
            raise ValueError("no pair results to score")
        scores = [score_pair(pair, labels[pair["case_id"]]) for pair in pairs]
        args.output.mkdir(parents=True, exist_ok=True)
        write_json(args.output / "scores.json", scores)
        print(json.dumps(scores, ensure_ascii=False))
        return
    with args.dataset.open("rb") as stream:
        actual_sha = hashlib.file_digest(stream, "sha256").hexdigest()
    if actual_sha != DATASET_SHA256:
        raise ValueError(f"dataset SHA256 differs from pinned revision: {actual_sha}")
    with args.dataset.open() as stream:
        dataset = json.load(stream)
    descriptions = [describe(case, i) for i, case in enumerate(dataset)]
    groups = history_groups(dataset)
    selected_ids = args.question_id or ["ba358f49"]
    selected = [row for row in descriptions if row["question_id"] in selected_ids]
    if len(selected) != len(set(selected_ids)) or any(not row["runnable_with_current_input_limit"] for row in selected):
        raise ValueError("selected questions must exist and have complete USER evidence within current input limits")
    dev_groups = {groups[row["index"]] for row in selected}
    inputs, queries, labels = [], [], []
    for row in selected:
        case_inputs, query, case_labels = project(dataset[row["index"]], row["index"])
        inputs.extend(case_inputs)
        queries.append(query)
        labels.append(case_labels)
    args.output.mkdir(parents=True, exist_ok=True)
    for name, rows in [("ingestion.jsonl", inputs), ("queries.jsonl", queries)]:
        (args.output / name).write_text("".join(json.dumps(row, ensure_ascii=False) + "\n" for row in rows))
    write_json(args.output / "labels.json", labels)
    for row in descriptions:
        row["history_group"] = groups[row["index"]]
        row["split"] = ("development_selected" if row["question_id"] in selected_ids else
                        "holdout_unfrozen_due_to_shared_history" if groups[row["index"]] in dev_groups else
                        "holdout_candidate_pending_topic_review")
        if row["over_24000_bytes"]:
            row["exclusion_reasons"].append("user_turn_exceeds_24000_utf8_bytes")
    manifest = dict(dataset=dict(url=URL, revision=REVISION, sha256=actual_sha, bytes=args.dataset.stat().st_size),
                    tool_sha256=digest(Path(__file__).read_bytes()),
                    timestamp_policy="dataset timestamps omit timezone; preserve wall times with fixed UTC+00 convention, not observed user timezone",
                    source_policy="all USER turns retained verbatim; ASSISTANT turns omitted; no history sessions removed",
                    history_policy="adapter supplies all preceding USER turns in each original session; runner must report any explicit history selection",
                    split_policy="exact session ID or role/content match joins histories; transitive groups cannot be called independent holdout; topics unreviewed",
                    holdout_frozen=False, provider_requests=0, paid_execution_authorized=False,
                    statistics=dict(questions=len(dataset), question_types=dict(Counter(d["question_type"] for d in dataset)),
                                    evidence_role_user_eligible=sum(r["user_evidence_role_eligible"] for r in descriptions),
                                    development_eligible=sum(r["development_eligible"] for r in descriptions),
                                    input_compatible_user_eligible=sum(r["runnable_with_current_input_limit"] for r in descriptions),
                                    abstention_questions=sum("_abs" in d["question_id"] for d in dataset),
                                    exclusion_reasons=dict(Counter(reason for r in descriptions for reason in r["exclusion_reasons"])),
                                    user_turns=sum(r["user_turns"] for r in descriptions),
                                    assistant_turns=sum(r["assistant_turns"] for r in descriptions),
                                    history_group_sizes=dict(Counter(groups.values())),
                                    minimum_complete_user_history_turns=min(r["user_turns"] for r in descriptions)),
                    selected_development=selected, candidate_inventory=descriptions,
                    artifacts={name: dict(bytes=(args.output / name).stat().st_size,
                                          sha256=digest((args.output / name).read_bytes()))
                               for name in ("ingestion.jsonl", "queries.jsonl", "labels.json")})
    write_json(args.output / "manifest.json", manifest)
    print(json.dumps(dict(output=str(args.output), statistics=manifest["statistics"], selected=selected,
                          ingestion_jsonl_bytes=manifest["artifacts"]["ingestion.jsonl"]["bytes"]), ensure_ascii=False))


if __name__ == "__main__":
    main()
