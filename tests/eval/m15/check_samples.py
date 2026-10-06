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
    assert mvp["label_status"] == "draft" and mvp["fictional"]
    assert len(mvp["messages"]) == 4 and len(mvp["questions"]) == 2
    assert len(w1) == 6 and all(s["label_status"] == "draft" and s["fictional"] for s in w1)
    new = list(inputs(mvp)) + list(inputs(w1))
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
