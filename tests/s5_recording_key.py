#!/usr/bin/env python3
"""S5-07: offline Planner recording key checks against the frozen S5-04 traces."""

import copy
import hashlib
import json
import pathlib
import socket


ROOT = pathlib.Path(__file__).resolve().parents[1]
ARCHIVE = ROOT / "docs/refactor/s5/assisted"


def encoded(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()


def key(record):
    return hashlib.sha256(encoded(record)).hexdigest()


def request_fingerprint(request):
    value = 0xCBF29CE484222325
    for byte in encoded(request):
        value = ((value ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"fnv1a64:{value:016x}"


def archived(path, manifest):
    relative = path.relative_to(ROOT).as_posix()
    expected = manifest[relative]
    body = path.read_bytes()
    assert len(body) == expected["bytes"]
    assert hashlib.sha256(body).hexdigest() == expected["sha256"]
    return body


def load_case(directory, name, manifest):
    root = ARCHIVE / directory
    case = json.loads(archived(root / f"{name}.json", manifest))
    report = json.loads(archived(root / "report.json", manifest))
    traces = list((root / "trace").glob("*.jsonl"))
    assert len(traces) == 1
    events = [json.loads(line) for line in archived(traces[0], manifest).splitlines()]
    starts = [event["data"] for event in events if event["kind"] == "model_start"]
    ends = [event["data"] for event in events if event["kind"] == "model_end"]
    rows = [row for row in report["ledger_rows"] if row["operation_id"] == f"s5-assisted-eval-{name}"]
    assert len(starts) == len(ends) == len(rows) == 1
    start, end, ledger = starts[0], ends[0], rows[0]
    assert start["call_id"] == end["call_id"]
    assert request_fingerprint(start["request"]) == ledger["request_fingerprint"]
    assert len(encoded(start["request"])) == ledger["input_bytes"]
    assert end["status"] == ledger["outcome"] == "completed"
    assert end["parsed_result"]["queries"] == case["assisted"]["assistance"]["queries"]
    local = {field: value for field, value in case["local"].items() if field != "duration_ms"}
    record = {
        "input": {field: case[field] for field in ("id", "owner", "query", "context", "trusted_at")},
        "upstream": local,
        "request": start["request"],
        "output_contract_version": "personal_memory_assistance_queries_v1",
    }
    return record, end["parsed_result"]


def main():
    raw_manifest = json.loads((ARCHIVE / "manifest-live-20260924.json").read_bytes())
    manifest = {item["path"]: item for item in raw_manifest["files"]}
    n04, output = load_case("live-corrected-n04-20260924", "N04", manifest)
    ambiguous, _ = load_case(
        "live-corrected-ambiguous-20260924", "N04-ambiguous", manifest
    )
    recordings = {key(n04): output}
    assert key(n04) != key(ambiguous), "different upstream case reused a Planner recording"
    reordered = json.loads(json.dumps(n04, ensure_ascii=False))
    reordered["request"] = dict(reversed(list(reordered["request"].items())))
    assert key(reordered) == key(n04), "JSON object order changed the recording key"

    changes = {
        "input": lambda r: r["input"].update(query="另一道问题"),
        "upstream_status": lambda r: r["upstream"].update(recall_status="failed"),
        "upstream_output": lambda r: r["upstream"].update(source_evidence=[{"source_id": "new"}]),
        "upstream_revision": lambda r: r["upstream"].update(source_revision=999),
        "model": lambda r: r["request"].update(model="different-model"),
        "prompt": lambda r: r["request"].update(instructions="changed prompt"),
        "parameters": lambda r: r["request"].update(max_output_tokens=4096),
        "output_schema": lambda r: r["request"]["text"]["format"].update(name="v2"),
        "output_contract": lambda r: r.update(output_contract_version="v2"),
    }
    for label, change in changes.items():
        modified = copy.deepcopy(n04)
        change(modified)
        assert key(modified) not in recordings, f"{label} reused an unrelated recording"

    original_socket = socket.socket
    try:
        socket.socket = lambda *args, **kwargs: (_ for _ in ()).throw(AssertionError("network used"))
        assert recordings[key(n04)] == output
    finally:
        socket.socket = original_socket
    print(json.dumps({"status": "passed", "archive_cases": 2,
                      "key_mutations_rejected": len(changes), "network_requests": 0}))


if __name__ == "__main__":
    main()
