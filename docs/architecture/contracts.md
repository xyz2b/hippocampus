# Runtime contracts

Current model schemas: W1 `w1.v5`, W3 `w3.v6`, W4 `w4.v5`, R1 `r1.v2`,
R4 `r4.v7`. Host protocol: `recall.v11`. DB format: v11. Pipeline names
`w4_runtime_v1` / `recall_v3` are configuration identifiers, not schema versions.
The generated MCP `tools/list` schemas and Rust types define accepted fields;
this guide describes the invariants rather than duplicating every JSON schema.

## Shared evidence and state

Quotes must identify authorized sources and exact contiguous text. Subjects,
polarity, uncertainty, strength, conditions and time retain separate meanings.
An extracted value does not replace its complete sentence or evidence. Unknown
time is not inferred from receipt time, and a plan is not a completed event.
Legacy records retain their original data and explicit gaps; missing provenance
cannot be repaired by inventing source IDs or quotes.

Stored grade and update state describe how a statement was processed. Neither
`Active` nor a committed record by itself proves a currently applicable fact.
All operations are scoped to the authorized owner and task context.

## Write contracts

- W0 stores the current USER text and necessary authorized USER history. Retention
  requires `capture_source=true`, stable session/turn IDs and permission for the
  entire retained payload. Source retention and enrichment are independent results.
- W1 proposes personal assertions, preferences, constraints, plans and experiences
  with quotes and qualifiers. Pure external proposals are not personal commitments.
- W2 checks types, references, quotes and projection eligibility mechanically.
  Semantic support remains a model judgment, not a string-match guarantee.
- W3 reviews semantic support, qualifiers and in-scope coverage. Gaps need exact
  evidence. A failed review cannot be relabeled as a committed fact.
- W4 judges updates against the bounded evidence basis. Conflict, supersession,
  reinforcement and unresolved decisions preserve their distinct meaning.
- W5 validates the decision and frozen bindings, then commits atomically with
  receipts and relations. Stale decisions require explicit rejudgment; replay
  cannot apply the same mutation twice or resurrect deleted sources.

Ordinary negative facts may be projected when qualified; unknown polarity and
unsupported negative episodes remain unresolved. Plans retain their status as
plans. Source confirmation, reextraction and history import use explicit management
operations and generations; entity registration does not silently reprocess old data.
See the [client contract](../memory-client-contract.md).

## Recall contracts

R1 builds a question plan. R2/R3 retrieve authorized records and raw sources within
explicit limits and report evidence gaps. R4 judges relevance, current applicability,
conflicts and computation endpoints. Code verifies references, supported arithmetic
and output validity before delivering `recall.v11`.

The host must preserve `facts`, `computed`, `history`, `conflicts`, qualifiers,
`grade`/`update`, evidence and completeness. An empty result is not proof of absence.
When `semantic_judgment` is skipped or failed, raw/record context is not a verified
current answer. Retrieval index status is separate from semantic completeness.

`memory_prepare` exposes `source`, `enrichment` and `recall` independently, plus
local global context. Pending enrichment means the original text may already be
stored. A missing or failed global-context projection does not undo a completed
primary operation. Inspect MCP transport errors and `isError` before interpreting
payloads. Only the current recall version is supported; unsupported versions fail
explicitly rather than silently upgrading.

## Replay, visibility and budget

Stable IDs and identical parameters reuse accepted operations. Changed parameters
with the same ID fail. A retained recall result binds its original read snapshot;
ordinary background progress does not refresh it. Deletion, authorization and
relevant content/control changes invalidate unsafe results and late publications.

Foreground recall uses at most two model stages, background enrichment at most
three per message, sharing finite persistent authorization. Failed attempts consume
budget; no automatic retry expands authorization. Bootstrap and request-level
offline recall skip recall models but do not cancel already authorized background
work. Full-service denial uses `HIPPOCAMPUS_MODEL_NETWORK=deny`.

`memory_validate_answer` makes no model call. It checks a narrow mechanically
verifiable unchanged answer against an available same-process result and source
set; it does not approve arbitrary paraphrases, complex qualifiers or calculations.

## Storage and compatibility

SQLite stores original sources, records/versions, relations, receipts, checkpoints
and budget state. Current DB format is v11. Readers preserve legacy gaps; opening
an old database for normal operation may migrate it. Never roll back by writing an
upgraded database with an older binary. Use a verified pre-upgrade backup instead.

See [architecture](../architecture.md), [recall behavior](recall-runtime.md),
[backup and upgrade](m1-cutover.md) and the source definitions in
[contract.rs](../../src/contract.rs).
