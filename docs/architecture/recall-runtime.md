# Recall runtime

The current host contract is `recall.v11`, with R1 v2 and R4 v7. The pipeline
configuration name remains `recall_v3`. Requests for older host versions fail
explicitly. See the [client contract](../memory-client-contract.md).

## Question and evidence

R1 interprets the question; code retrieves and assembles authorized evidence; R4
judges it; code validates the result and supported calculations. Foreground recall
uses at most two model calls, without implicit retries. Raw sources and records
remain distinct from the final semantic judgment.

Retrieval combines actual bounded enumeration and lexical search. Optional local
embeddings add ranking when enabled and appropriate; enumeration/list computations
must not infer completeness from ranking. Vector failures can fall back to lexical
retrieval without claiming semantic completeness. Subject names and mentions are
candidate hints, not proof of identity. R4 can request clarification.

`retrieval.status` is one of disabled, not_needed, ready, pending or unavailable.
It describes retrieval/index readiness, not whether an answer is true or complete.
A pending index does not mean the original text was not retained.

## Snapshot and concurrency

Model calls run outside service locks and SQLite transactions. Results bind the
R3 read snapshot. Ordinary background progress preserves that snapshot; deletion,
authorization and relevant content/control changes invalidate unsafe results.
Same-ID/same-argument replay reuses the retained result, not a fresh model call.
New requests need new IDs. Cancellation does not undo an already sent HTTP request
or the budget it consumed; it does not authorize retries.

Prepare stores authorized text first and wakes background enrichment after commit.
It does not wait for enrichment. If the response is `in_progress`, continue with
exactly the same parameters rather than starting another save operation.
`memory_source_status` provides current progress without model calls.

Bootstrap and `offline=true` skip recall models. Neither stops an already authorized
background worker. Use `enrichment_mode=off` and `HIPPOCAMPUS_MODEL_NETWORK=deny` for a
source-only service. `memory_validate_answer` is a zero-call mechanical check of a
narrow unchanged answer against a still-valid same-process result; it cannot
validate arbitrary generated prose.

## Stored-statement snapshot

This opt-in bootstrap feature is disabled by default:

```json
{"statement_snapshot":{"enabled":true,"max_records":8,"max_bytes":16384}}
```

Limits are 1–32 records and 512–65536 UTF-8 bytes of the complete snapshot JSON.
Only bootstrap returns this snapshot; normal Recall, Prepare and per-tool global
context do not attach it. Records are included whole, never stripped of qualifiers.

The fixed relation allowlist is `response_language`, `response_style`,
`code_example_language`, `likes`, `avoids` (these five restricted to
preference/constraint), `job_title`, `works_at`, `lives_in` and `allergic_to`.
Eligible records must belong to the authorized owner/task, refer to self, be Active,
committed and judged, have no open relation, and have authorized exact source evidence.
Legacy, pending and unknown states are excluded; existing expiration checks apply.
Selection orders by observed time descending and ID ascending, with truncation
reported explicitly. Multiple values are not merged into a single truth.

Treat these as **stored statements, not a judgment for the current question**.
Preserve their sources, plans, negation, qualifiers, uncertainty and time. Health
and residence information can enter the host context when enabled. For current
status, applicable conditions, complete lists or new relevant statements, perform
ordinary recall (save newly authorized text with Prepare first).

`unfinished_sources` counts all unfinished authorized sources, not question-specific
ones. `coverage_unknown=true` always; an empty snapshot or zero unfinished sources
does not establish complete coverage. Same-ID replay keeps the original `read_at`
and snapshot; a new bootstrap ID requests a refresh. Safety changes still invalidate
results. Full snapshots are retained only in the bounded process result cache,
not as new database content.

## Limitations

Models can miss candidates, attach time to the wrong statement or propose unsupported
answers. Raw evidence is a fallback context, not a substitute for passed semantic
checks. Full-load latency targets remain unvalidated; historical scores and expired
recordings do not establish current performance. See [evaluation](../../tests/eval/README-v3.md)
and [backup instructions](m1-cutover.md).
