#!/usr/bin/env python3
"""Isolated N05 comparison probe. Build fixtures locally; --dry-run never sends.

--send requires a separate JSON authorization file with these exact fields:
  authorized_scope: "S5-N05 comparison good/unsafe/negated"
                    (or "S5-N05 comparison good" with --good-only,
                     or "S5-N05 comparison unsafe/negated" with --negative-only)
  output_dir: absolute path passed to --output (single-use)
  provider_base_url: "https://ark.cn-beijing.volces.com/api/v3"
  max_calls: 3 (or 1 with --good-only, or 2 with --negative-only)
  max_operation_calls: 1
  query_output_tokens: 32768
  max_input_bytes: 65536
"""
import argparse
import contextlib
import http.server
import json
import os
import sqlite3
import tempfile
import threading
import uuid
from pathlib import Path

from mcp_smoke import Client
from s5_comparison_process import Fixture, PREDICATES, QUESTION, STATEMENTS, ingest
from s5_normal_intake import settings


ROOT = Path(__file__).resolve().parents[1]
ARK_BASE = "https://ark.cn-beijing.volces.com/api/v3"
CASES = {"good": ("choice", "limit"),
         "unsafe": ("choice", "current"),
         "negated": ("negated", "limit")}
LIMITS = {"max_operation_calls": 1, "query_output_tokens": 32768,
          "max_input_bytes": 65536}


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def authorize(path, output, cases):
    if not path or not path.is_file():
        raise ValueError("--send requires a separate authorization JSON file")
    auth = json.loads(path.read_text())
    required = dict(LIMITS, max_calls=len(cases),
                    authorized_scope="S5-N05 comparison " + "/".join(cases),
                    output_dir=str(output), provider_base_url=ARK_BASE)
    if any(auth.get(key) != value or type(auth.get(key)) is not type(value)
           for key, value in required.items()):
        raise ValueError("authorization scope, output directory, provider, or limits mismatch")
    if os.environ.get("DEEPSEEK_BASE_URL") != ARK_BASE:
        raise ValueError("DEEPSEEK_BASE_URL must be the exact Ark HTTPS base")
    if not os.environ.get("DEEPSEEK_API_KEY") or os.environ["DEEPSEEK_API_KEY"] == "local-fixture-key":
        raise ValueError("a real DEEPSEEK_API_KEY is required")


def check_rejected_send_preflight(output, cases):
    """Exercise --send's authorization gate without entering the send branch."""
    with tempfile.TemporaryDirectory(prefix="s5-n05-send-guard-") as directory:
        wrong = Path(directory) / "wrong-authorization.json"
        wrong.write_text(json.dumps(dict(LIMITS, max_calls=len(cases),
            authorized_scope="S5-N05 comparison " + "/".join(cases),
            output_dir=str(output) + "-different", provider_base_url=ARK_BASE)))
        wrong_scope = Path(directory) / "wrong-scope.json"
        other_cases = CASES if len(cases) == 1 else {"good": CASES["good"]}
        wrong_scope.write_text(json.dumps(dict(LIMITS, max_calls=len(other_cases),
            authorized_scope="S5-N05 comparison " + "/".join(other_cases),
            output_dir=str(output), provider_base_url=ARK_BASE)))
        for auth in (None, wrong, wrong_scope):
            try:
                authorize(auth, output, cases)
            except ValueError:
                pass
            else:
                raise AssertionError("unauthorized send preflight was accepted")
            assert not output.exists(), "rejected send preflight created its output"


def ledger_rows(db, group):
    with sqlite3.connect(db) as connection:
        return [dict(zip(("stage", "outcome", "input_bytes", "output_token_limit",
                          "input_tokens", "output_tokens"), row))
                for row in connection.execute(
                    "SELECT stage,outcome,input_bytes,output_token_limit,input_tokens,output_tokens "
                    "FROM model_calls WHERE group_id=? ORDER BY created_at,call_id", (group,))]


def score(label, result, source_ids, sent):
    actual = {(item["source_id"], item["text"])
              for item in result["source_evidence"]}
    expected = ({(source_ids[key], STATEMENTS[key][0]) for key in CASES[label]}
                if label == "good" and sent else set())
    wrong_source = bool(actual - expected)
    exact = actual == expected and len(result["source_evidence"]) == len(expected)
    return {"source_ids": [item["source_id"] for item in result["source_evidence"]],
            "source_quotes": [item["text"] for item in result["source_evidence"]],
            "wrong_source": wrong_source, "passed": exact,
            "assistance": result.get("assistance"),
            "evidence_status": result.get("evidence_status"),
            "model_calls": result.get("model_calls", [])}


def trace_report(directory, output):
    files = sorted(directory.glob("trace-*.jsonl"))
    validation = "missing"
    try:
        for path in files:
            if path.stat().st_mode & 0o777 != 0o600:
                raise PermissionError("trace file must be private")
            for line in path.read_text().splitlines():
                event = json.loads(line)
                if event.get("kind") == "validation" and event.get("data", {}).get("stage") == "comparison":
                    validation = event["data"]["status"]
    except (OSError, ValueError, KeyError):
        validation = "unreadable"
    return {"files": [str(path.relative_to(output)) for path in files],
            "comparison_validation": validation}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--dry-run", action="store_true")
    mode.add_argument("--send", action="store_true")
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/pmr-mcp")
    parser.add_argument("--output", required=True, type=Path,
                        help="fresh output directory; never overwritten")
    parser.add_argument("--authorization", type=Path,
                        help="separate, output-bound JSON authorization; required for --send")
    case_mode = parser.add_mutually_exclusive_group()
    case_mode.add_argument("--good-only", action="store_true",
                           help="run only the positive case with a separate one-call authorization")
    case_mode.add_argument("--negative-only", action="store_true",
                           help="run only the two safety cases with a separate two-call authorization")
    args = parser.parse_args()
    cases = ({"good": CASES["good"]} if args.good_only else
             {key: CASES[key] for key in ("unsafe", "negated")} if args.negative_only else CASES)
    binary = args.binary.resolve()
    output = args.output.resolve()
    if not binary.is_file():
        raise ValueError(f"build the MCP binary first: {binary}")
    if output.exists():
        raise ValueError(f"output directory must be new: {output}")
    if args.send:
        authorize(args.authorization, output, cases)
    elif args.authorization:
        raise ValueError("--dry-run does not use an authorization file")
    else:
        check_rejected_send_preflight(output, cases)

    os.umask(0o077)
    output.mkdir(parents=True, mode=0o700)
    clean = {key: value for key, value in os.environ.items()
             if not key.startswith(("DEEPSEEK_", "OPENAI_", "PMR_"))}
    report = {"mode": "send" if args.send else "dry_run", "provider": ARK_BASE if args.send else None,
              "cases": [], "max_physical_requests": len(cases) if args.send else 0,
              "physical_requests": 0, "status": "preparing"}
    if args.dry_run:
        report["send_preflight_rejections"] = 3
    save(output / "report.json", report)

    Fixture.calls = 0
    Fixture.formats = []
    with contextlib.ExitStack() as stack:
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        stack.callback(server.server_close)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        loopback = dict(clean, PMR_MODEL_NETWORK="offline", DEEPSEEK_API_KEY="local-fixture-key",
                        DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
                        NO_PROXY="127.0.0.1,localhost")
        sources = {}
        try:
            for label, keys in cases.items():
                sources[label] = ingest(binary, output / f"{label}.sqlite3", loopback,
                                        output, label, keys)
        finally:
            server.shutdown()
            thread.join()
    report["synthetic_intake_requests"] = Fixture.calls
    assert Fixture.calls == 2 * len(cases), "two synthetic extractions per database expected"
    report["status"] = "recalling"
    save(output / "report.json", report)

    query_env = dict(clean, PMR_MODEL_NETWORK="live" if args.send else "deny",
                     DEEPSEEK_BASE_URL=ARK_BASE,
                     DEEPSEEK_API_KEY=os.environ["DEEPSEEK_API_KEY"] if args.send else "dry-run-unused")
    run_id = uuid.uuid4().hex
    for label in cases:
        group = f"s5-n05-comparison-{run_id}-{label}"
        budget = dict(group_id=group, max_calls=1, max_operation_calls=1,
                      query_output_tokens=32768, max_input_bytes=65536)
        config = settings(output, f"{label}-assisted", retrieval_mode="assisted",
                          enrichment_mode="off", fact_index_enabled=True, predicates=PREDICATES,
                          local_recall_budget={"candidate_k": 30, "source_limit": 2,
                                               "content_bytes": 8192}, model_budget=budget)
        db = output / f"{label}.sqlite3"
        trace_dir = output / f"{label}-trace"
        trace_dir.mkdir(mode=0o700)
        assert trace_dir.stat().st_mode & 0o777 == 0o700
        try:
            with contextlib.closing(Client(binary, db, dict(query_env, PMR_TRACE_DIR=str(trace_dir)),
                                           "s5-comparison", config=config)) as client:
                result = client.call("memory_recall", {"query": QUESTION})
            save(output / f"{label}-result.json", result)
            scored = score(label, result, sources[label], args.send)
            scored["trace"] = trace_report(trace_dir, output)
            rows = ledger_rows(db, group)
            if len(rows) > 1 or len(scored["model_calls"]) != len(rows):
                raise AssertionError("model call accounting mismatch or per-case budget exceeded")
            if args.dry_run and rows:
                raise AssertionError("dry-run attempted a model send")
            if any(row["stage"] != "assistance" or row["output_token_limit"] != 32768
                   or row["input_bytes"] > 65536 for row in rows):
                raise AssertionError("unexpected model stage or request limit")
            scored["case"] = label
            scored["ledger"] = rows
            if args.send and (
                result.get("diagnostics", {}).get("status") == "evidence_incomplete"
                or scored["trace"]["comparison_validation"] in ("missing", "unreadable")
            ):
                scored["passed"] = False
                scored["trial_status"] = "diagnostic_incomplete"
                report["cases"].append(scored)
                report["physical_requests"] += len(rows)
                report["status"] = "diagnostic_incomplete"
                save(output / "report.json", report)
                break
            if args.send and (len(rows) != 1 or rows[0]["outcome"] != "completed"):
                scored["passed"] = False
                scored["trial_status"] = "no_completed_assistance_send"
                report["cases"].append(scored)
                report["physical_requests"] += len(rows)
                report["status"] = "stopped_incomplete_trial"
                save(output / "report.json", report)
                break
            report["cases"].append(scored)
            report["physical_requests"] += len(rows)
            save(output / "report.json", report)
            if scored["wrong_source"]:
                report["status"] = "stopped_wrong_source"
                break
        except Exception as error:
            rows = ledger_rows(db, group)
            report["physical_requests"] += len(rows)
            report["cases"].append({"case": label, "error_type": type(error).__name__,
                                    "ledger": rows, "trace": trace_report(trace_dir, output),
                                    "passed": False})
            report["status"] = "stopped_error"
            save(output / "report.json", report)
            raise

    if report["status"] == "recalling":
        report["status"] = "passed" if all(row["passed"] for row in report["cases"]) else "failed"
    assert report["physical_requests"] <= report["max_physical_requests"]
    save(output / "report.json", report)
    print(json.dumps({"status": report["status"], "cases": len(report["cases"]),
                      "synthetic_intake_requests": report["synthetic_intake_requests"],
                      "real_provider_requests": report["physical_requests"],
                      "output": str(output / "report.json")}, ensure_ascii=False))
    if report["status"] != "passed":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
