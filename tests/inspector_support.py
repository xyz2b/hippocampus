"""Test drivers for the Rust Inspector; no Python server or data conversion.

Build with: cargo build --release -p pmr-inspector
PMR_INSPECTOR_BINARY may select another explicitly built Inspector binary.
"""
import json
import os
import re
import selectors
import subprocess
from contextlib import contextmanager
from pathlib import Path
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[1]
UI = ROOT / "inspector/web/index.html"


def event(identifier, ok, observed):
    print(json.dumps({"acf_assertion": dict(id=identifier, outcome="pass" if ok else "fail", observed=observed)}, ensure_ascii=False))
    return ok


def schema_check(bundle):
    # Reuse the actual importer and check its schema has not drifted from the file.
    html = UI.read_text()
    schema_text = html.split("const BUNDLE_SCHEMA=", 1)[1].split(";\nconst STATES=", 1)[0]
    assert json.loads(schema_text) == json.loads((ROOT / "docs/memory-inspector-mvp/trace-bundle.schema.json").read_text())
    validator = html[html.index("function validate(data){"):html.index("function load(data){")]
    js = ("const BUNDLE_SCHEMA=" + schema_text + ";\n"
          "const ref=m=>`${m.id}@${m.version}`;const present=v=>v!==null&&v!==undefined&&v!=='';\n"
          + validator + "\nconst fs=require('fs');const data=JSON.parse(fs.readFileSync(0,'utf8'));"
          "console.log(JSON.stringify(validate(data)));")
    return subprocess.run(["node", "-e", js], input=json.dumps(bundle), text=True, capture_output=True)


def inspector_binary():
    binary = Path(os.environ.get("PMR_INSPECTOR_BINARY", ROOT / "target/release/pmr-inspector")).resolve()
    if not binary.is_file():
        raise RuntimeError("Build Inspector first: cargo build --release -p pmr-inspector")
    return binary


def export_runtime(path):
    """Run the shipped exporter; its live contract permits unfinished tail lines."""
    result = subprocess.run([str(inspector_binary()), "--export-trace", str(Path(path).resolve())],
                            capture_output=True, text=True, timeout=30)
    if result.returncode:
        raise ValueError(result.stderr.strip())
    return json.loads(result.stdout)


@contextmanager
def start_inspector(db, owner, logs, trace_dir=None):
    # Require explicit logs so tests cannot accidentally read the user's defaults.
    if not logs:
        raise ValueError("Inspector tests require an explicit fixture log")
    args = [str(inspector_binary()), "--port", "0", "--db", str(Path(db).resolve()), "--owner", owner]
    for log in logs:
        args.extend(["--log", str(Path(log).resolve())])
    if trace_dir is not None:
        args.extend(["--trace-dir", str(Path(trace_dir).resolve())])
    process = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            if not selector.select(10):
                raise RuntimeError("Rust Inspector did not print a startup URL")
            line = process.stdout.readline()
        match = re.search(r"http://127\.0\.0\.1:(\d+)/\?token=([^\s]+)", line)
        if not match:
            raise RuntimeError("Rust Inspector startup failed (credentials withheld)")
        port, token = int(match[1]), match[2]
        yield SimpleNamespace(server_port=port, token=token, cookie_name=f"pmr_inspector_{port}",
                              origin=f"http://127.0.0.1:{port}")
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        process.stdout.close()
        process.stderr.close()
