# hippocampus Memory Inspector

`pmr-inspector` is a separate Rust binary in this workspace. It reads existing
SQLite data, logs and traces without starting MCP, calling models or modifying
memory. Stopping Inspector does not stop the memory service.

## Start

```sh
./scripts/start-memory-inspector.sh --trace-dir "$HOME/.local/share/personal-memory-runtime/traces"
```

The script incrementally builds release assets before starting. For explicit paths:

```sh
cargo build --locked -p pmr-inspector --release
./target/release/pmr-inspector --port 8765 --owner local-user \
  --db /absolute/path/to/memory.sqlite3 \
  --log /absolute/path/to/service.log \
  --trace-dir /absolute/path/to/traces
```

Open the authorization link printed in the terminal. A restart generates a new
credential. Keep the link private; it is not a model API key. The HTTP service binds
only to `127.0.0.1`. Use `--help` for the complete CLI.

| Option | Default / meaning |
| --- | --- |
| `--port` | 8765; use 0 to allocate a free port |
| `--db` | `~/.local/share/personal-memory-runtime/memory.sqlite3` |
| `--owner` | `local-user` |
| `--log` | Repeatable; otherwise reads `personal-memory.log` and `launchd.stderr.log` under `~/Library/Application Support/tunnel-client/logs/` |
| `--trace-dir` | Disabled by default; reads runtime `trace-*.jsonl` |
| `--dump` | Outputs the current filtered bundle to stdout, without HTTP |
| `--export-trace PATH` | Converts a runtime JSONL trace using current DB/owner privacy checks |

Set paths explicitly when the defaults do not match your installation. Pre-create
trace directories with private permissions (0700 on Unix); runtime logging uses
`PMR_TRACE_DIR`. Binaries embed the HTML and historical example, so rebuild/restart
after changing assets. Installed binaries do not require Python, Node or source files.

## What the views mean

原文保存、候选核验、更新判定和事务提交分别显示。来源 pending 不等于原话未收到；
缺字段表示未记录，不能解释为成功。记录保留候选、限定语、时间、来源与关系；旧记录的
缺口保持未知。模型 raw 输出不是通过核验的 final，也不是已提交结果。

The SQLite reader uses read-only/query-only access and a consistent transaction;
it does not migrate or maintain the database. Logs are read from a bounded tail.
Trace parsing isolates malformed events and reports missing fields. Trace files
larger than 16 MiB are reported as over the size limit, not silently treated as empty.
Events are associated through explicit IDs and bindings, not timestamp proximity.

## Privacy and deletion

Current database content is filtered by owner, source availability and evidence
dependencies. Unlinked history and unverifiable blocks are hidden. Deleted sources
only expose a minimal receipt (internal source ID, deleted status and received time);
that timestamp can still reveal activity timing.

Runtime traces use deletion epochs. Source deletion attempts to clear runtime trace
files; Inspector checks the current database and epoch before exposing content.
Events from before an applicable deletion epoch expose only allowed metadata, and
unverifiable bodies remain hidden. Unreadable databases prevent verified export.
Ordinary logs and unlinked old receipts remain conservatively hidden.

`--dump` and the live API use the same current-data filtering. Standalone
`--export-trace` also needs the matching `--db` and `--owner`. Already downloaded
or copied files cannot be revoked. Keep dumps, traces, logs and authorization links
private; see [security boundaries](../SECURITY.md).

```sh
./target/release/pmr-inspector --db /absolute/path/to/memory.sqlite3 \
  --owner local-user --log /absolute/path/to/service.log --dump
./target/release/pmr-inspector --db /absolute/path/to/memory.sqlite3 \
  --owner local-user --export-trace /absolute/path/to/trace-ID.jsonl
```

The embedded historical failure example is isolated test evidence, not the current
user's memory. Its original model labels and conversation references remain part
of that archive. Loading it does not make a model call.

## Development

- [HTTP entry point](src/main.rs)
- [Embedded UI](web/index.html)
- [Embedded historical example](web/recorded-4096-failure.json)
- [Bundle schema](../docs/memory-inspector-mvp/trace-bundle.schema.json)
- [Python test driver](../tests/inspector_support.py)

```sh
cargo build --locked --workspace --bins
PMR_MODEL_NETWORK=offline cargo test --locked -p pmr-inspector
node tests/s1_inspector.js
PMR_INSPECTOR_BINARY="$PWD/target/debug/pmr-inspector" python3 -B tests/s0_offline.py
```

Run these from the repository root. Tests use synthetic fixtures or the existing
historical package. Schema/UI drivers require Node.js; the application does not.
Test the root package separately, since workspace-wide serde_json feature unification
changes its canonical receipt test behavior. Historical scripts requiring private
archives are not part of the public CI entry point.
