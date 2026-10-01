"""Dump an old-binary SQLite database as a replayable SQL script (schema + rows + user_version).

FTS5 shadow tables are skipped: CREATE VIRTUAL TABLE recreates them and the triggers refill the
index while source_segments rows are inserted.

How the fixtures here were made: check out the commit named in the file name, append a test to its
tests/w5_acceptance.rs that runs the w4_01 scenario (and, for *-rejudge, the needs_rejudge
scenario of a3_needs_rejudge_receipt_from_an_intermediate_version_stays_needs_rejudge; for v5, only
the seeded record and a claimed source), copy its database, then run:
    python3 dump.py <db> <name>.sql <name>
"""
import sqlite3, sys
src, out, label = sys.argv[1], sys.argv[2], sys.argv[3]
c = sqlite3.connect(src)
version = c.execute("PRAGMA user_version").fetchone()[0]
objs = c.execute("SELECT type, name, tbl_name, sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY rowid").fetchall()
virtual = [n for t, n, _, s in objs if t == 'table' and s.upper().startswith('CREATE VIRTUAL TABLE')]
shadow = lambda n: any(n.startswith(v + '_') for v in virtual)
lines = [f"-- {label}: written by that commit's own binary (tests/w5_acceptance.rs scenario w4_01).",
         "-- Regenerate only from the commit itself; do not edit by hand.", "BEGIN;"]
tables, rest = [], []
for t, n, tbl, s in objs:
    if shadow(n):
        continue
    (tables if t == 'table' else rest).append((t, n, s))
for t, n, s in tables:
    lines.append(s + ";")
for t, n, s in tables:
    if n in virtual:
        continue
    cols = [r[1] for r in c.execute(f"PRAGMA table_info('{n}')")]
    for row in c.execute(f"SELECT * FROM '{n}' ORDER BY rowid" if 'WITHOUT ROWID' not in s.upper() else f"SELECT * FROM '{n}'"):
        vals = []
        for v in row:
            if v is None: vals.append("NULL")
            elif isinstance(v, (int, float)): vals.append(repr(v))
            elif isinstance(v, bytes): vals.append("X'" + v.hex() + "'")
            else: vals.append("'" + v.replace("'", "''") + "'")
        lines.append(f"INSERT INTO \"{n}\" ({', '.join(cols)}) VALUES ({', '.join(vals)});")
for t, n, s in rest:
    lines.append(s + ";")
lines.append("COMMIT;")
lines.append(f"PRAGMA user_version = {version};")
open(out, "w").write("\n".join(lines) + "\n")
