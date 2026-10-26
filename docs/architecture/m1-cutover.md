# Backup and upgrade / 备份与升级

The current database format is v11. Normal service startup may migrate older data;
Inspector is read-only and does not migrate it. Legacy statements remain readable
with explicit gaps and are not automatically reinterpreted by models.

## Before upgrading

1. Stop every MCP process, CLI and Inspector connected to the database, including
   client auto-restart. Confirm that background work has stopped.
2. Create a consistent SQLite backup in a private directory. Replace the paths
   below; the destination must be a new file, not an existing backup:

   ```sh
   umask 077
   mkdir -p /absolute/path/private-backup
   sqlite3 /absolute/path/memory.sqlite3 ".backup /absolute/path/private-backup/memory-before-upgrade.sqlite3"
   sqlite3 /absolute/path/private-backup/memory-before-upgrade.sqlite3 "PRAGMA integrity_check; PRAGMA user_version;"
   ```

   Require `integrity_check` to return `ok` and record the version. Do not copy only
   the main database while WAL is active. The backup includes original text and
   the budget ledger; retain it privately with an explicit retention policy.
3. Preserve the old configuration and the actual old binary, including its version
   or checksum. Rebuilding a binary later is not preserving the old executable.
4. Inspect `--print-default-config`. Supported pipeline names are `w4_runtime_v1`
   and `recall_v3`. Retired fields (`retrieval_mode`, `local_recall_enabled`,
   `local_recall_budget`, `is_a`) and explicit legacy selectors are rejected,
   even when their values are false, null or empty. Do not silently rewrite them.
   Clients must use `recall.v11` and stable request IDs, including bootstrap.
5. Verify the upgrade against a separate backup copy first, with automatic enrichment
   off and `HIPPOCAMPUS_MODEL_NETWORK=deny`. Confirm original text, legacy records, owner
   isolation, source status and configuration before enabling online capability.
   Do not test destructive operations against the only production copy.

## Rollback

Stop all processes before restoring. Preserve the newer database separately if it
contains changes worth retaining. Run the old binary only against a separate restored
pre-upgrade backup, using the matching old configuration. Do not write an upgraded
database with an older binary or claim new writes can be safely reverse-migrated.

A restored backup can contain previously deleted data and an older budget ledger.
Review deletion obligations and expenditure since the backup; do not resume online
calls against a rolled-back budget as though its allowance were new authorization.
Backups, exports and provider-side records are outside automatic runtime erasure.

See [configuration](../configuration.md) and [security](../../SECURITY.md).
