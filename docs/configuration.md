# Configuration / 配置

Run `pmr-mcp --print-default-config` for the complete configuration supported by
your binary. Pass its JSON file with `--config`, and explicitly select `--db` and
`--owner`. The service reads process environment variables; it does **not** load
`.env`. A desktop MCP client may not inherit your terminal environment, so configure
its `env` explicitly. Do not store credentials in tracked JSON files.

## Start without model calls

```json
{"enrichment_mode":"off"}
```

Combine this with `PMR_MODEL_NETWORK=deny`. Original-text retention still requires
explicit permission for the whole message/history and `capture_source=true`.
`source.status` and `enrichment.status` describe different outcomes.

| Network mode | Behavior |
| --- | --- |
| `deny` | Denies all model HTTP, including loopback |
| `offline` (default) | Allows only the supported literal-loopback test path; not a guarantee of zero HTTP |
| `live` | Allows configured providers, subject to authorization and persistent budgets |

Explicit providers and real local embedding/model services require `live` as well;
a localhost URL does not bypass authorization. Request-level `offline=true` and
bootstrap skip recall models but do not stop an already authorized background worker.

## Online enrichment and recall

Use your own `DEEPSEEK_BASE_URL`, `DEEPSEEK_API_KEY` and `DEEPSEEK_PRO_MODEL` through
the process or MCP client environment. For example, the client's `env` object can
contain these placeholders (replace them before enabling online operation):

```json
{
  "PMR_MODEL_NETWORK": "live",
  "DEEPSEEK_BASE_URL": "https://your-provider.example/v1",
  "DEEPSEEK_API_KEY": "YOUR_PRIVATE_KEY",
  "DEEPSEEK_PRO_MODEL": "YOUR_PRO_MODEL_OR_ENDPOINT_ID"
}
```

The [joint budget example](../examples/background-recall.json) enables deferred
enrichment, with up to three background calls and two foreground calls sharing a
finite total authorization. Its five-call budget is a demonstration, not a
recommendation to spend or an automatically renewable grant. Choose a unique
`group_id` and explicit limits you authorize. Reusing a group does not reset its
ledger; failures and uncertain usage do not create free retries. Call ceilings
alone are not a currency-price guarantee.

Default roles are W1/W3 Pro low, W4 Pro medium, R1/R4 Pro minimal, and R4 computation
low. Explicit `write_models` and `recall_models` settings can override roles.
Provider mappings use `base_url_env` and `api_key_env` names, not literal secrets.
Supported adapters include `openai_responses`, `anthropic_messages` and
`openai_chat`, with model-specific validation; an arbitrary model supporting a
similar HTTP interface is not automatically supported. Production rejects the
experimental `prompt_schema` evaluation mode. Local models require verified
context-window and tokenizer/template bounds; do not invent this metadata.

Historical `ep-*` IDs in recordings identify a developer account's old routes.
They are not public endpoints or configuration examples.

## Environment reference

### User configuration

| Variable | Purpose |
| --- | --- |
| `DEEPSEEK_BASE_URL` | Default provider API base URL |
| `DEEPSEEK_API_KEY` | Default provider secret; supply privately |
| `DEEPSEEK_PRO_MODEL` | Your default Pro model/endpoint ID; not required for source-only mode |
| `PMR_MODEL_NETWORK` | `deny`, `offline`, or `live` |
| `PMR_MODEL_TIMEOUT_SECS` | Adapter timeout setting; stage budgets also bound calls |
| `HOME` | Default local data paths; explicit CLI paths avoid relying on it |
| Names in `base_url_env` / `api_key_env` | User-selected variables for explicit providers; no fixed exhaustive list |

Some direct adapter entry points additionally read `PMR_MODEL_BUDGET_DB`,
`PMR_MODEL_BUDGET_OWNER`, `PMR_MODEL_BUDGET_CONFIG` and `PMR_MODEL_OPERATION_ID`.
These must be supplied together when that path is used. Normal MCP setup uses the
JSON `model_budget`; do not copy these advanced variables into every client.

### Diagnostics (optional, private)

| Variable | Behavior |
| --- | --- |
| `PMR_TRACE_DIR` | Enables runtime JSONL traces in a pre-created private directory |
| `PMR_TRACE_FULL_SNAPSHOTS=1` | With tracing enabled, records full diagnostic snapshots; default off |
| `PMR_OPENAI_TRACE` | Presence enables legacy attempt/response metadata on stderr |
| `PMR_OPENAI_TRACE_CONTENT` | **Presence enables model output on stderr; even `=0` enables it** |

Leave diagnostic variables absent unless needed. Content traces, full snapshots,
stderr, exports and backups may retain personal information. Full snapshots can
include extensive state and unlinked history. Inspector filtering does not erase
independent copies. See [security boundaries](../SECURITY.md).

### Tests and evaluation

`PMR_INSPECTOR_BINARY` selects an explicitly built Inspector for Python drivers.
`EVAL_*`, `RETRIEVAL_EMBED_*` and diagnostic probe variables are test controls,
not normal client configuration. See [evaluation commands](../tests/eval/README-v3.md).
Leave `EVAL_RECORD` **unset** for replay; `0` is not a universal offline switch.
CI has no provider secrets and uses synthetic loopback fixtures when needed.

## Optional retrieval and snapshots

`embedding.enabled` defaults to false. Enabling it requires a separately configured,
verified local embedding service and network authorization. Model/dimension/weight
bindings are checked; a stale or partial index must not be described as complete.

`statement_snapshot.enabled` also defaults to false. It exposes bounded stored
statements during bootstrap, potentially including health and residence details.
Read the [snapshot policy](architecture/recall-runtime.md#stored-statement-snapshot)
before enabling it. It is not a current-fact or complete-list answer.
