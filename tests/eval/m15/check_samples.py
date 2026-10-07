"""Offline input scan. Never opens private files or sends a model request."""
import json
import pathlib

ROOT = pathlib.Path(__file__).resolve().parent
EVAL = ROOT.parent
# AF36 explicitly names these production topics; do not inspect/copy private text.
PRIVATE_TOPICS = ("青甘", "泸沽湖", "张掖")


def inputs(value):
    """Only original USER text/history/query fields, not labels or prompts."""
    if isinstance(value, dict):
        for key, item in value.items():
            if key in ("text", "query") and isinstance(item, str):
                yield item
            elif key in ("history", "context") and isinstance(item, list):
                yield from (s for s in item if isinstance(s, str))
            elif key not in ("expect", "expect_common", "review_requirements", "recorded"):
                yield from inputs(item)
    elif isinstance(value, list):
        for item in value:
            yield from inputs(item)


def check():
    mvp = json.loads((ROOT / "mvp.json").read_text())
    w1 = json.loads((ROOT / "w1.json").read_text())
    w3 = json.loads((ROOT / "w3.json").read_text())
    w3_effort = json.loads((ROOT / "w3-effort.json").read_text())
    assert mvp["label_status"] in ("draft", "reviewed") and mvp["fictional"]
    assert len(mvp["messages"]) == 4 and len(mvp["questions"]) == 2
    assert len(w1) == 6 and all(s["label_status"] == "draft" and s["fictional"] for s in w1)
    assert len(w3) == 4 and all(s["label_status"] in ("draft", "reviewed") and s["fictional"] for s in w3)
    assert len({s["id"] for s in w3}) == 4
    assert len(w3_effort) == 4 and all(
        s["label_status"] in ("draft", "reviewed") and s["fictional"] for s in w3_effort
    )
    assert len({s["id"] for s in w3 + w3_effort}) == 8
    for prior, revised in zip(w3, w3_effort):
        assert revised["input"]["text"] == prior["input"]["text"].replace("应为63", "是63")
        assert revised["expect"] == prior["expect"]
        assert revised["input"]["history"] == prior["input"]["history"]
    assert "编号所属的人" in w3_effort[3]["focus"]
    for sample in w3 + w3_effort:
        entity_ids = {e["id"] for e in sample["input"]["entities"]}
        for candidate in sample["input"]["candidates"]:
            assert candidate["subject"] in entity_ids, "W3 negative control must pass mechanical subject lookup"
            assert candidate["value"] == candidate["sentence"]
            assert candidate["sentence"] in sample["input"]["text"]
        assert len(sample["expect"]["reviews"]) == len(sample["input"]["candidates"])
    old = (ROOT / "w3-old-91a068a.txt").read_bytes()
    digest = 0xcbf29ce484222325
    for byte in old:
        digest = ((digest ^ byte) * 0x100000001b3) & ((1 << 64) - 1)
    assert f"fnv1a64:{digest:016x}" == "fnv1a64:e20f41189ca2159c", "old W3 prompt snapshot changed"
    new = list(inputs(mvp)) + list(inputs(w1)) + list(inputs(w3)) + list(inputs(w3_effort))
    for text in new:
        assert not any(word in text for word in PRIVATE_TOPICS), "private production topic in fictional input"
    # Deliberately exclude recordings and the new directory. Compare authored
    # corpus originals (including fixed input adapters), not every generic
    # scalar in a nested response. Short strings are not a useful leak signal.
    old = []
    for directory in ("cases", "w1/samples", "w3/samples", "w4/samples", "recall_stage", "fixed_inputs"):
        for path in (EVAL / directory).rglob("*.json"):
            old.extend((s, path) for s in inputs(json.loads(path.read_text())) if len(s) >= 8)
    for text in new:
        for prior, path in old:
            assert not (text in prior or prior in text), f"existing original overlaps: {path}"
    return {"new_inputs": len(new), "prior_inputs": len(old), "private_topics": list(PRIVATE_TOPICS)}


if __name__ == "__main__":
    print("M1.5 虚构输入扫描通过：", check())
