"""Generates tests/eval/w4/fixtures/<id>.output.json: a w4.v2 model output for each
sample that calls W4, derived from the expected decisions.

The samples only say which sources a decision must cite (`basis_sources`); a real
output cites concrete quotes. Each fixture keeps every decision field as expected
and replaces `basis_sources` with EvidenceRefs taken verbatim from the sample:
the candidate's own sentence for its source, otherwise the evidence quote of an
input record (targets first) or the basis of an input relation from that source.

The fixtures are offline stand-ins for the model. Their quotes are checked to be
verbatim by the code under test; they are not human labels, and the sample files
are not modified.
"""
import json
import pathlib

HERE = pathlib.Path(__file__).resolve().parent
OUT = HERE / "fixtures"


def quote_for(sample, decision, sid):
    inp = sample["input"]
    cand = next(c for c in inp["candidates"] if c["index"] == decision["candidate"])
    if cand["source"]["source_id"] == sid:
        return cand["candidate"]["sentence"]["text"]
    records = sorted(inp["records"], key=lambda r: r["id"] not in decision["targets"])
    for record in records:
        for evidence in record["evidence"]:
            if evidence["source"]["source_id"] == sid:
                return evidence["quote"]["text"]
    for relation in inp["relations"]:
        for evidence in relation["basis"]:
            if evidence["source"]["source_id"] == sid:
                return evidence["quote"]["text"]
    return sample["sources"][sid]["text"]


def fixture(sample):
    decisions = []
    for expected in sample["expect"]["decisions"]:
        decision = {}
        for key, value in expected.items():
            if key == "basis_sources":
                decision["basis"] = [
                    {"source": {"kind": "stored", "source_id": sid},
                     "quote": {"text": quote_for(sample, expected, sid)}}
                    for sid in value]
            else:
                decision[key] = value
        decisions.append(decision)
    return {"decisions": decisions}


def main():
    OUT.mkdir(exist_ok=True)
    count = 0
    for path in sorted((HERE / "samples").glob("*.json")):
        sample = json.loads(path.read_text(encoding="utf-8"))
        if not sample["call_w4"]:
            continue
        (OUT / f"{sample['id']}.output.json").write_text(
            json.dumps(fixture(sample), ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        count += 1
    print(f"{count} fixtures")


if __name__ == "__main__":
    main()
