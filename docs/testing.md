# Testing

> `make check` runs exactly what CI runs, from a clean checkout, with no phone,
> no server account, and no network.

Anything that cannot be verified under those conditions is either badly layered
or behind an explicit opt-in gate. There are exactly two gates:
`make live-test` (needs a Miniflux) and `make rpm` (needs the SDK).

## What `make check` does

| Step | What it catches |
| --- | --- |
| `fmt-check` | formatting drift |
| `clippy` | including the `unwrap`/`expect`/`panic`/indexing denials on production code |
| `test` | 155 tests across 8 binaries, plus the shim's under offscreen Qt |
| `qmllint` | QML syntax |
| `qml-load` | **every QML file compiled in a real engine** against the Silica stubs |
| `packaging` | spec/Cargo version drift, missing installed files, desktop entry validity |
| `deny` | advisories, licences, banned and duplicated crates |

## The parts worth explaining

### The QML load test

Qt 5's `qmllint` only checks *syntax*, so on its own it passes a file that
references a type which does not exist or sets a property never declared —
which is most of the mistakes actually made in QML.

`crates/vuo-shim/tests/qml_loads.rs` therefore builds a real `QQmlEngine`,
points it at `qml-stubs/` for `Sailfish.Silica`, registers Vuo's own types, and
compiles every page. It found seven real problems on its first run, including
the root file never importing the `pages/` directory — something `qmllint`
passes happily and that would have failed at launch on a device.

If a Silica property is missing from the stubs, **add it to the stubs**; do not
work around it in the app.

### The cover test

The load test instantiates every file with its properties at their defaults,
which for the app cover is the empty one. What that cannot see is the part
that only exists once there are feeds: the staggered grid of favicons, laid
out by a pass of JavaScript over a parsed JSON list rather than by a view over
model rows, and the rule deciding which cells are drawn bright.

`crates/vuo-shim/tests/qml_cover.rs` loads the cover in an engine of its own,
hands it feeds directly -- no mirror, no worker -- and reads back what was
drawn: how many cells, which of them are lit, that the rows stagger, that a
feed with no icon falls back to its initial, and that a failed refresh puts
Vuo's own translated line on the cover rather than the server's words.

### Outbox reconciliation

These are the app's real invariants, so they are deterministic rather than
incidental. `tests/outbox_reconciliation.rs` covers each property §8.3 names:

- replay is idempotent;
- a process killed mid-flight resumes without losing or double-applying;
- an offline burst of 1203 collapsed intents reconciles in 4 requests;
- a server-side change to a locally mutated entry resolves per field.

Plus one that guards the design itself: the non-idempotent `/star` and
`/bookmark` routes are never called at all.

### Snapshots

`cargo insta review` to inspect a diff. The corpus in `tests/corpus/` covers
malformed markup, deep nesting, tables, `<pre>` and figures.

The hostile sample gets **explicit assertions in addition to** its snapshot. A
snapshot records what the transform *does*, and would happily bless a
regression the moment someone ran `cargo insta accept` without reading the
diff. The assertions record what it must never do.

### Fuzzing

Two targets, over the code paths that eat attacker-influenced bytes: the
content transform and the JSON deserialiser. Both are pure functions, which
makes them unusually cheap to fuzz.

They assert as well as detecting crashes — that every cap holds, that no
non-`http(s)` URL survives, that rendered markup never contains a tag from
outside the closed set. Without those, the fuzzer would only find crashes, and
a cap that silently fails to hold is the quieter and more dangerous bug.

```sh
make fuzz-quick          # 60s per target, as PR CI runs
```

### Migrations

The property that matters is that a migration never drops the outbox: the
mirror can be re-fetched from the server, but pending user actions exist *only*
on the device.

Only one schema version has shipped, so there is no older on-disk format to
restore from yet. The machinery is tested with a synthetic second migration in
`db::migrations`' unit tests, which proves ordering, atomicity, and data
survival for real. When migration 2 lands, commit a database file produced by
the *previous release* into `tests/fixtures/` and
`fixtures_upgrade_without_data_loss` will pick it up automatically.

### The live tests

`tests/live_miniflux.rs` does not re-test Vuo's logic — the mock suite covers
that. It tests **the assumptions Vuo makes about Miniflux**, and each test
names the assumption it protects. That is what makes it worth running against
an ephemeral server weekly: cursor semantics and mutation idempotency are
contract questions about someone else's software, and the alternative to
checking them is finding out by regression.
