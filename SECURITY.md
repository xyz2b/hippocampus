# Security policy

## Reporting a vulnerability

Use GitHub's private vulnerability reporting entry under this repository's
Security tab when enabled:
https://github.com/xyz2b/hippocampus/security/advisories/new

The repository owner must enable private vulnerability reporting before this
channel can receive reports. If it is unavailable, open an issue requesting a
private contact channel **without disclosing the vulnerability, credentials,
conversation text, database, trace, or authorization link**. Do not post sensitive
reproduction material in a public issue. There is no guaranteed response or fix
schedule; this project is under active development.

## Data boundaries

The memory database is stored locally. Enabling online model operations sends
required original text, context and/or memory content to the configured provider.
The chat client and model provider have their own retention policies. A local
model is also a separately configured service, not an implicit privacy guarantee.

Automatic enrichment can be disabled with `enrichment_mode=off`; set
`HIPPOCAMPUS_MODEL_NETWORK=deny` to deny model HTTP calls throughout the service.
A single request's `offline=true` does not stop previously authorized background work.
The program does not automatically load `.env` files.

Diagnostics, stderr output, backups and exports can contain sensitive content.
`HIPPOCAMPUS_OPENAI_TRACE_CONTENT` is enabled by the variable's presence, even if its value
is `0`. Full trace snapshots are separately opt-in and can include extensive state.
Keep database, configuration, trace and backup directories private. Do not share
Inspector authorization links. Inspector listens on loopback and reads existing
data; forwarding its port changes that access boundary.

Deleting memory or a source does not erase previously exported files, backups,
all copies of trace/log files, or provider-side records. Runtime deletion and
Inspector filtering cannot revoke an already copied file. No application-level
encryption-at-rest guarantee is made.

Historical test recordings are shipped as evidence, including an embedded
Inspector example. They are not configuration templates or a safe destination for
new personal data. Never attach live personal databases or logs to public reports.
