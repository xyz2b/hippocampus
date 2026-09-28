"""Convert LongMemEval questions into resident eval cases (tests/eval/cases/lme_*.json).

Usage: python3 tests/eval/convert_longmemeval.py <longmemeval_oracle.json> <question_id>...

Only USER turns become messages (sessions in date order); assistant turns are not memory
sources. Intake has no per-turn labels, so its expectation is only that the source is stored.
Abstention questions (*_abs) expect insufficient_evidence; others expect the dataset answer
as a substring of the safe answer. Existing case files are left untouched so recordings survive.
"""

import json
import sys
from datetime import datetime
from pathlib import Path

CASES = Path(__file__).parent / "cases"


def convert(item):
    sessions = sorted(
        zip(item["haystack_dates"], item["haystack_session_ids"], item["haystack_sessions"]),
        key=lambda session: datetime.strptime(session[0][:10], "%Y/%m/%d"),
    )
    messages = [
        {
            "id": f"s{number}_t{turn}",
            "text": entry["content"],
            "expect": {"store": [], "forbid": []},
        }
        for number, (_, _, session) in enumerate(sessions, 1)
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
        if path.exists():
            print(f"skip existing {path.name}")
            continue
        case = convert(items[question_id])
        path.write_text(json.dumps(case, ensure_ascii=False, indent=2) + "\n")
        print(f"wrote {path.name}: {len(case['messages'])} user turns")


if __name__ == "__main__":
    main()
