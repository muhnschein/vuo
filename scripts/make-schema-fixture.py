#!/usr/bin/env python3
"""Regenerate a shipped-schema fixture for crates/vuo-core/tests/fixtures/schema/.

§8.3: "Test each migration against a fixture database from the previous
version." A fixture is an on-disk database as some released version of Vuo left
it, populated with data an upgrade must not lose. `cargo test` never writes
these -- they are committed and stay frozen at the shape that release produced.

Freezing them is the point. It is what lets
tests/migrations.rs::fixtures_upgrade_without_data_loss notice a SHIPPED
MIGRATION BEING EDITED IN PLACE: a database upgraded from the frozen v1 skips
migration 1 entirely, so if migration 1's text has changed, the upgraded schema
no longer matches a freshly created one. No in-memory test can see that,
because an in-memory fixture is built from the edited migration too.

The DDL is read from the version's own entry in MIGRATIONS, so regenerating a
fixture for a version whose migration has since been edited reproduces the edit
rather than the release. That is why regeneration is a deliberate act with this
script rather than something the test suite does.

Usage: scripts/make-schema-fixture.py <version>
"""
import os
import re
import sqlite3
import sys

FIXTURE_DATA = """
-- Data an upgrade must carry across. The outbox is the part that cannot be
-- re-fetched (§9.4), so it is what this fixture exists to protect.
INSERT INTO categories (id, title) VALUES (1, 'News');
INSERT INTO feeds (id, category_id, title) VALUES (10, 1, 'Fixture Feed');
INSERT INTO entries (id, feed_id, status, title) VALUES (100, 10, 'unread', 'Fixture entry');
INSERT INTO entries (id, feed_id, status, title) VALUES (101, 10, 'read', 'Another');
INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (100, 'status', 'read', 1700000000);
INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (101, 'starred', '1', 1700000001);
"""


def migrations_up_to(root, want):
    src = open(f"{root}/crates/vuo-core/src/db/migrations.rs").read()
    src = src.split("#[cfg(test)]")[0]
    steps = re.findall(
        r'version:\s*(\d+),\s*name:\s*"[^"]*",\s*sql:\s*r#"(.*?)"#', src, re.S
    )
    if not steps:
        sys.exit("no migrations parsed out of migrations.rs")
    sql = [s for v, s in steps if int(v) <= want]
    if not sql:
        sys.exit(f"no migration at or below version {want}")
    return sql


def main():
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} <schema version>")
    version = int(sys.argv[1])
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    out = f"{root}/crates/vuo-core/tests/fixtures/schema/v{version}.sqlite"
    os.makedirs(os.path.dirname(out), exist_ok=True)
    if os.path.exists(out):
        os.remove(out)

    conn = sqlite3.connect(out)
    try:
        for sql in migrations_up_to(root, version):
            conn.executescript(sql)
        conn.executescript(f"PRAGMA user_version = {version};")
        conn.executescript(FIXTURE_DATA)
        conn.commit()
        rows = conn.execute("SELECT COUNT(*) FROM outbox").fetchone()[0]
        got = conn.execute("PRAGMA user_version").fetchone()[0]
    finally:
        conn.close()

    assert got == version, f"user_version is {got}, expected {version}"
    print(f"wrote {out} ({os.path.getsize(out)} bytes, user_version={got}, {rows} outbox rows)")


if __name__ == "__main__":
    main()
