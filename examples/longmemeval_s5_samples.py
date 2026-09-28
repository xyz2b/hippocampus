#!/usr/bin/env python3
"""Freeze six LongMemEval S development questions as data only; never ingest or call models."""

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path

import longmemeval_e1 as e1


QUESTIONS = (
    "545bd2b5",      # single-session extraction
    "06878be2",      # preference
    "28dc39ac",      # cross-session reasoning
    "5e1b23de",      # temporal reasoning
    "f685340e",      # knowledge update
    "f685340e_abs",  # refusal, paired with the knowledge-update topic
)
ABSTENTION = "f685340e_abs"
REPO = Path(__file__).resolve().parents[1]
DEFAULT_OUTPUT = REPO / "docs/refactor/s5/development-samples-20260925"
RUNTIME_KEYS = {"case_id", "owner_id", "session_id", "turn_id", "text", "observed_at", "utc_offset_seconds", "history"}
LABEL_KEYS = {"answer", "answer_session_ids", "has_answer", "required_turns", "source_mapping"}


def file_sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def write_jsonl(path, rows):
    path.write_text("".join(json.dumps(row, ensure_ascii=False) + "\n" for row in rows))


def check_dataset(dataset):
    if len(dataset) != 500 or dataset[203]["question_id"] != "ba358f49":
        raise ValueError("pinned E1 question c0203 is not at its expected location")
    by_id = {row["question_id"]: (index, row) for index, row in enumerate(dataset)}
    if len(by_id) != len(dataset) or len(set(QUESTIONS)) != len(QUESTIONS) or "ba358f49" in QUESTIONS:
        raise ValueError("duplicate or E1 question selected")
    groups = e1.history_groups(dataset)
    if len(set(groups.values())) != 1:
        raise ValueError("shared-history grouping changed; review holdout statement")
    selected = []
    for question_id in QUESTIONS:
        index, case = by_id[question_id]
        description = e1.describe(case, index)
        marked = [turn for session in case["haystack_sessions"] for turn in session if turn.get("has_answer") is True]
        if description["max_user_turn_bytes"] > 24000 or description["over_24000_bytes"]:
            raise ValueError(f"question exceeds the current input limit: {question_id}")
        if question_id == ABSTENTION:
            if description["exclusion_reasons"] != ["abstention_separate"] or marked:
                raise ValueError("refusal case must have no marked answer turns or other exclusions")
        elif (not description["runnable_with_current_input_limit"] or not marked
              or any(turn["role"] != "user" for turn in marked)):
            raise ValueError(f"positive case needs complete USER-only evidence: {question_id}")
        selected.append((index, case, description, groups[index]))
    return selected


def build(dataset_path, output):
    if file_sha(dataset_path) != e1.DATASET_SHA256:
        raise ValueError("LongMemEval S cleaned dataset hash differs from the pinned revision")
    dataset = json.loads(dataset_path.read_text())
    selected = check_dataset(dataset)
    ingestion, queries, labels, cases = [], [], [], []
    for index, case, description, group in selected:
        rows, query, label = e1.project(case, index)
        if any(set(row) != RUNTIME_KEYS for row in rows) or LABEL_KEYS & query.keys():
            raise ValueError("runtime input contains a label or an unexpected field")
        if len(rows) != description["user_turns"] or len(label["required_turns"]) != description["required_user_turns"]:
            raise ValueError("USER history or required-turn projection differs from the inventory")
        ingestion.extend(rows)
        queries.append(query)
        labels.append(label)
        cases.append(dict(case_id=query["case_id"], official_question_id=case["question_id"],
                          question_type=description["question_type"], abstention=case["question_id"] == ABSTENTION,
                          history_group=group, sessions=description["sessions"],
                          user_bearing_sessions=len({row["session_id"] for row in rows}),
                          user_turns=description["user_turns"],
                          assistant_turns_omitted=description["assistant_turns"],
                          required_user_turns=description["required_user_turns"],
                          required_assistant_turns=description["required_assistant_turns"],
                          user_text_bytes=description["user_text_bytes"],
                          max_user_turn_bytes=description["max_user_turn_bytes"]))
    output.mkdir(parents=True, exist_ok=False)
    write_jsonl(output / "ingestion.jsonl", ingestion)
    write_jsonl(output / "queries.jsonl", queries)
    write_json(output / "labels.json", labels)
    readme = """# S5 LongMemEval 开发样本（数据冻结）

固定官方 S cleaned revision `98d7416c24c778c2fee6e6f3006e7a073259d48f`，原文件 SHA-256 `d6f21ea9d60a0d56f34a05b609c79c88a451d2ae03597821ea3d5a9678c3a442`。此目录只导出数据，未摄入数据库、未发送模型请求、未运行检索／阅读／回答评分。原 E1 c0203、`final-pairs.jsonl` 及其归档保持原样。

| 官方题号 | 能力／题型 | USER 来源 | 必要 USER 轮次 | 用途 |
|---|---|---:|---:|---|
"""
    for row in cases:
        readme += (f"| `{row['official_question_id']}` | `{row['question_type']}` | {row['user_turns']} | "
                   f"{row['required_user_turns']} | {'拒答负例' if row['abstention'] else '有答案开发题'} |\n")
    empty_user_sessions = sum(row["sessions"] - row["user_bearing_sessions"] for row in cases)
    readme += f"\n原始会话中有 {empty_user_sessions} 个仅含 ASSISTANT 轮次，因无 USER 来源而没有摄入行。\n"
    readme += """
`ingestion.jsonl` 只含获准 USER 原话及同会话先前 USER history；原 ASSISTANT 轮次不改写为用户记忆。`queries.jsonl` 只含问题与查询时钟；答案、证据位置和原始会话映射仅在评测侧 `labels.json`。全部题目按数据集原时间及原顺序投影，每条 USER 消息不超过 24,000 UTF-8 字节。`_abs` 题没有 `has_answer` 标记，应单列拒答与错误确定回答，不能把零召回或标签外来源直接当作拒答正确。

六题以及原 E1 题都在同一个共享历史连通组（全量 500/500），这些是 **S5 已见开发样本**，`holdout_frozen=false`。S6 仍需独立历史，不能从这份 S cleaned 中把未选择题称作独立留出。S／M／Oracle 变体不跨开发和未来留出集合。此英文子集不代替中文自然问句、删除纠错或网页宿主验收。

复核：`python3 examples/longmemeval_s5_samples.py --verify`。生成需要本机已固定的 `/tmp/pmr-longmemeval/longmemeval_s_cleaned.json`；脚本拒绝覆盖已有输出目录。
"""
    (output / "README.md").write_text(readme)
    artifacts = {name: dict(bytes=(output / name).stat().st_size, sha256=file_sha(output / name))
                 for name in ("ingestion.jsonl", "queries.jsonl", "labels.json", "README.md")}
    manifest = dict(dataset=dict(url=e1.URL, revision=e1.REVISION, sha256=e1.DATASET_SHA256,
                                 bytes=dataset_path.stat().st_size),
                    tool_sha256=file_sha(Path(__file__)), helper_sha256=file_sha(Path(e1.__file__)),
                    selected=cases, statistics=dict(questions=len(cases), positive_questions=len(cases) - 1,
                                                    abstention_questions=1, user_turns=len(ingestion),
                                                    question_types=dict(Counter(row["question_type"] for row in cases))),
                    source_policy="all USER turns retained verbatim; ASSISTANT turns omitted; assistant-only sessions have no ingestion rows",
                    timestamp_policy="dataset wall times with fixed UTC+00 convention; query uses original question_date",
                    label_policy="answer, answer_session_ids and has_answer only in labels.json; runtime IDs carry no answer mark",
                    split_policy="all 500 S cleaned questions share one exact-history connected group; selected cases are development only",
                    holdout_frozen=False, db_ingestion=False, provider_requests=0,
                    artifacts=artifacts)
    write_json(output / "manifest.json", manifest)
    return manifest


def verify(dataset_path, output):
    manifest = json.loads((output / "manifest.json").read_text())
    if (file_sha(dataset_path) != manifest["dataset"]["sha256"]
            or file_sha(Path(__file__)) != manifest["tool_sha256"]
            or file_sha(Path(e1.__file__)) != manifest["helper_sha256"]):
        raise ValueError("dataset, adapter or helper hash changed")
    for name, expected in manifest["artifacts"].items():
        path = output / name
        if path.stat().st_size != expected["bytes"] or file_sha(path) != expected["sha256"]:
            raise ValueError(f"frozen artifact changed: {name}")
    ingestion = [json.loads(line) for line in (output / "ingestion.jsonl").read_text().splitlines()]
    queries = [json.loads(line) for line in (output / "queries.jsonl").read_text().splitlines()]
    labels = json.loads((output / "labels.json").read_text())
    if (len(queries) != len(QUESTIONS) or len(labels) != len(QUESTIONS)
            or len(ingestion) != manifest["statistics"]["user_turns"]
            or any(set(row) != RUNTIME_KEYS for row in ingestion)
            or any(LABEL_KEYS & row.keys() for row in queries)
            or [row["official_question_id"] for row in manifest["selected"]] != list(QUESTIONS)
            or [row["official_question_id"] for row in labels] != list(QUESTIONS)
            or any(row["required_turns"] for row in labels if row["official_question_id"] == ABSTENTION)):
        raise ValueError("frozen inputs or label separation failed")
    print(json.dumps(dict(status="passed", questions=len(queries), user_turns=len(ingestion),
                          provider_requests=0, holdout_frozen=False), ensure_ascii=False))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dataset", type=Path, default=Path("/tmp/pmr-longmemeval/longmemeval_s_cleaned.json"))
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--verify", action="store_true", help="verify frozen files without writing")
    args = parser.parse_args()
    if args.verify:
        verify(args.dataset, args.output)
    else:
        build(args.dataset, args.output)
        verify(args.dataset, args.output)


if __name__ == "__main__":
    main()
