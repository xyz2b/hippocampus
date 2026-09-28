"""Adapt libtest JSON events; never infer a business pass from cargo's exit code."""
import json
import os
import subprocess
import sys

target, *names = sys.argv[1:]
build = subprocess.run(["cargo", "test", "--offline", "--no-run", "--message-format=json",
                        *(["--lib"] if target == "lib" else ["--test", target])],
                       stdout=subprocess.PIPE, text=True, check=True)
executables = [event["executable"] for line in build.stdout.splitlines()
               if (event := json.loads(line)).get("executable") and event.get("profile", {}).get("test")]
if len(executables) != 1:
    sys.exit("expected exactly one test executable")
command = [executables[0], "-Z", "unstable-options", "--format", "json", "--show-output",
            "--test-threads=1", "--exact", *names]
if target == "openai_live_scenarios":
    command.append("--ignored")
env = dict(os.environ, RUSTC_BOOTSTRAP="1")  # libtest's structured event format only.
process = subprocess.Popen(command, stdout=subprocess.PIPE, text=True, env=env)
started, completed = set(), set()
bad = False
for line in process.stdout:
    print(line, end="", flush=True)
    event = json.loads(line)
    if event.get("type") != "test":
        continue
    name, outcome = event["name"], event["event"]
    if outcome == "started":
        if name in started or name not in names:
            bad = True
        started.add(name)
    elif outcome in ("ok", "failed", "ignored"):
        if name not in started or name in completed or outcome == "ignored":
            bad = True
        completed.add(name)
        print(json.dumps({"acf_assertion": {
            "id": name.replace("::", "-"), "outcome": "pass" if outcome == "ok" else "fail",
            "observed": f"libtest {name}: {outcome}; original assertions executed"
        }}), flush=True)
code = process.wait()
if code or bad or started != set(names) or completed != set(names):
    sys.exit(1)
