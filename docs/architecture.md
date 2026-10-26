# Current architecture / 当前架构

hippocampus is a local, owner-scoped Rust memory runtime backed by SQLite, exposed
through a stdio MCP process. The separate Inspector reads existing data and traces.
Online semantic operations require a configured provider and a finite persistent
budget. Source text is data, never an instruction for the runtime to execute.

## Write path

1. **W0 — retain original words.** The host supplies the current USER message,
   stable session/turn IDs, optional necessary USER history and explicit permission
   to retain the whole payload. Original-text storage and enrichment report separately.
2. **W1 — extract.** A model proposes candidates with original quotes, subjects,
   temporal expressions and qualifiers. A proposal is not a committed fact.
3. **W2 — check mechanically.** Code validates structure, references, exact quote
   support and supported projections. It does not decide natural-language truth.
4. **W3 — review meaning.** A model checks support, qualifiers and in-scope coverage.
5. **W4 — judge updates.** A model compares candidates with a bounded evidence set
   and proposes changes, conflict relationships or unresolved decisions.
6. **W5 — commit.** Code validates bindings and decisions, then commits atomically.
   Stale or deleted inputs cannot be made valid by a late model response.

With `inline` or `deferred`, MCP wakes a separate worker after original-text
storage; Prepare does not wait for enrichment. `off` does not enqueue automatic
enrichment. Pending sources remain inspectable and can be recalled as raw context.
The worker uses durable checkpoints, generations, leases and ordering barriers.
A failed operation does not authorize new model attempts or a larger budget.

## Recall path

R1 interprets a question; R2/R3 retrieve and assemble a bounded evidence snapshot;
R4 judges evidence; R5 validates the result before delivery. Ordinary recall uses
at most two model calls. Code performs supported date arithmetic and source checks.
Raw evidence, structured statements, semantic judgments and computed results remain
distinct. `Active` does not prove a statement is currently true.

Bootstrap loads local global requirements and, if explicitly enabled, a bounded
stored-statement snapshot. It is not ordinary question answering. Offline recall
returns local context without a model judgment. Optional local embeddings augment
retrieval; they do not establish factual correctness or complete coverage.
See [recall behavior](architecture/recall-runtime.md).

## Storage, concurrency and deletion

SQLite stores retained sources, structured records and versions, relations,
management receipts, enrichment checkpoints and persistent budget state. Current
DB format is v11. Legacy records are readable with explicit gaps; upgrades do not
automatically reinterpret them with models. Derived lexical/vector indexes are
separate from the authority of original sources and reviewed records.

Model waits run outside global service locks and SQLite transactions. Recall binds
a read snapshot: ordinary background progress does not silently refresh it, while
deletion, authorization and relevant content/control changes invalidate unsafe
results. Stable request IDs support replay; changed arguments are rejected.
Source deletion propagates to dependent state and blocks late results. Separate
backup/export copies remain the user's responsibility.

The Inspector applies owner and deletion checks to content, preserves unknown or
incomplete states, and does not migrate the database or invoke the memory pipeline.
See the [Inspector guide](../inspector/README.md).

## Implemented and unverified boundaries

Source retention, background enrichment, recall, source confirmation/reprocessing,
history import, restricted entity merge and optional embeddings are implemented.
Model outputs can omit or misinterpret information; the host must preserve conflicts,
uncertainty and evidence limits. Full-load latency targets and general model quality
are not established by offline regression success. A lighter local-model pipeline
is exploratory, not an implemented alternative here.

See [contracts](architecture/contracts.md), [client integration](memory-client-contract.md)
and [backup instructions](architecture/m1-cutover.md).
