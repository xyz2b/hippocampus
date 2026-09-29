"""Convert LongMemEval questions into resident eval cases (tests/eval/cases/lme_*.json).

Usage: python3 tests/eval/convert_longmemeval.py <longmemeval_oracle.json> <question_id>...

Only USER turns become messages (sessions in date order); assistant turns are not memory
sources. Intake has no per-turn labels, so its expectation is only that the source is stored.
Abstention questions (*_abs) expect insufficient_evidence; others expect the dataset answer
as a substring of the safe answer. Times come from the dataset (UTC+8, the runtime default):
each message's observed_at is its session time plus a minute per turn, and the question's
`at` is the question date. Existing case files only get these times refreshed, so their
recordings survive.
"""

import json
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

CASES = Path(__file__).parent / "cases"
UTC8 = timezone(timedelta(hours=8))


def unix(date):
    """LongMemEval dates look like '2023/05/28 (Sun) 06:23'."""
    day, _, clock = date.split(" ")
    return int(datetime.strptime(f"{day} {clock}", "%Y/%m/%d %H:%M").replace(tzinfo=UTC8).timestamp())


def convert(item):
    sessions = sorted(
        zip(item["haystack_dates"], item["haystack_session_ids"], item["haystack_sessions"]),
        key=lambda session: unix(session[0]),
    )
    messages = [
        {
            "id": f"s{number}_t{turn}",
            "text": entry["content"],
            "observed_at": unix(date) + 60 * (turn - 1),
            "expect": {"store": [], "forbid": []},
        }
        for number, (date, _, session) in enumerate(sessions, 1)
        for turn, entry in enumerate(
            (entry for entry in session if entry["role"] == "user"), 1
        )
    ]
    if item["question_id"].endswith("_abs"):
        expect = {"kind": "insufficient_evidence"}
    else:
        expect = {"kind": "answer", "answer": [str(item["answer"]).strip().rstrip(".")]}
    return {
        "id": f"lme_{item['question_id']}",
        "origin": f"LongMemEval oracle {item['question_id']} ({item['question_type']}); "
        f"session dates {[date for date, _, _ in sessions]}, asked {item['question_date']}",
        "entities": [],
        "messages": messages,
        "questions": [
            {
                "id": "q",
                "after": len(messages),
                "query": item["question"],
                "at": unix(item["question_date"]),
                "context": [],
                "expect": expect,
            }
        ],
    }


def main():
    dataset, *ids = sys.argv[1:]
    items = {item["question_id"]: item for item in json.loads(Path(dataset).read_text())}
    for question_id in ids:
        path = CASES / f"lme_{question_id}.json"
        case = convert(items[question_id])
        if path.exists():
            existing = json.loads(path.read_text())
            for old, new in zip(existing["messages"], case["messages"]):
                assert old["id"] == new["id"] and old["text"] == new["text"], path.name
                old["observed_at"] = new["observed_at"]
            existing["questions"][0]["at"] = case["questions"][0]["at"]
            path.write_text(json.dumps(existing, ensure_ascii=False, indent=2) + "\n")
            print(f"refreshed times in {path.name}")
            continue
        path.write_text(json.dumps(case, ensure_ascii=False, indent=2) + "\n")
        print(f"wrote {path.name}: {len(case['messages'])} user turns")


if __name__ == "__main__":
    main()
