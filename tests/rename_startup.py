"""Isolated startup regression: no provider credentials or model calls."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

MCP = Path(sys.argv[1]).resolve()
INSPECTOR = Path(os.environ.get("HIPPOCAMPUS_INSPECTOR_BINARY", MCP.parent / "hippo-inspector")).resolve()
SUFFIXES = "MODEL_NETWORK TRACE_DIR INSPECTOR_BINARY TRACE_FULL_SNAPSHOTS OPENAI_TRACE OPENAI_TRACE_CONTENT MODEL_OPERATION_ID MODEL_BUDGET_DB MODEL_BUDGET_OWNER MODEL_BUDGET_CONFIG MODEL_TIMEOUT_SECS BUDGET_TEST_DB BACKGROUND_TRACE_TEST".split()
ENV = {k: v for k, v in os.environ.items() if not k.startswith(("PMR_", "HIPPOCAMPUS_", "OPENAI_", "DEEPSEEK_", "EVAL_"))}
ENV["HIPPOCAMPUS_MODEL_NETWORK"] = "deny"
INIT = "\n".join(json.dumps(v) for v in [
    {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "startup-test", "version": "1"}}},
    {"jsonrpc": "2.0", "method": "notifications/initialized"},
]) + "\n"


def run(binary, home, args=(), extra=None):
    config = home / "config.json"
    config.write_text('{"enrichment_mode":"off"}')
    command = [str(binary)] + (["--config", str(config)] if binary == MCP else ["--dump"])
    return subprocess.run(command + list(args), env={**ENV, "HOME": str(home), **(extra or {})},
                          input=INIT if binary == MCP else "", text=True, capture_output=True, timeout=20)


def ok(result):
    assert result.returncode == 0, (result.stdout, result.stderr)


for binary in (MCP, INSPECTOR):
    assert binary.is_file(), f"Build workspace binaries first: {binary}"
    with tempfile.TemporaryDirectory(prefix="hippo-startup-") as temporary:
        home = Path(temporary)
        old = home / ".local/share/personal-memory-runtime/memory.sqlite3"
        new = home / ".local/share/hippocampus/memory.sqlite3"
        # A fresh installation must start without touching the legacy location.
        ok(run(binary, home))
        assert not old.parent.exists()
        if binary == MCP:
            assert new.is_file()
            # Remove only our disposable newly created test database directory.
            import shutil
            shutil.rmtree(new.parent)
        old.parent.mkdir(parents=True)
        old.write_bytes(b"legacy sentinel")
        for args in ((), ("--export-trace", str(home / "trace.jsonl"))) if binary == INSPECTOR else ((),):
            result = run(binary, home, args)
            assert result.returncode != 0 and "Legacy default database" in result.stderr, result
            assert 'mv ' in result.stderr and not new.parent.exists(), result.stderr
        new.parent.mkdir(parents=True)
        result = run(binary, home)
        assert result.returncode != 0 and "Legacy default database" in result.stderr, result
        assert not new.exists()
        # Explicit --db bypasses even when explicitly selecting the new default path.
        ok(run(binary, home, ["--db", str(new)]))
        new.touch(exist_ok=True)
        ok(run(binary, home))
        assert old.read_bytes() == b"legacy sentinel"
        for suffix in SUFFIXES:
            legacy = "PMR_" + suffix
            for value in ("", "secret-sentinel-do-not-print"):
                result = run(binary, home, extra={legacy: value})
                assert result.returncode != 0, legacy
                assert legacy in result.stderr and "HIPPOCAMPUS_" + suffix in result.stderr
                assert "secret-sentinel-do-not-print" not in result.stdout + result.stderr
            ok(run(binary, home, ["--help"], {legacy: "secret-sentinel-do-not-print"}))
        # Informational commands also bypass the old-database guard.
        new.unlink()
        ok(run(binary, home, ["--help"]))
        if binary == MCP:
            ok(run(binary, home, ["--print-default-config"], {"PMR_MODEL_NETWORK": "deny"}))
print("startup guards passed for both binaries (13 legacy variables, including empty values)")
