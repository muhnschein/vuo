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
| `test` | 263 tests across 17 binaries, the shim's under offscreen Qt |
| `qmllint` | QML syntax |
| `qml-load` | **every QML file compiled in a real engine** against the Silica stubs |
| `packaging` | spec/Cargo version drift, missing installed files, desktop entry validity, the Harbour intake rules that need no device (`scripts/check-harbour.sh`), and the libraries the host build actually links (`scripts/check-linked-libs.sh`) |
| `deny` | advisories, licences, banned and duplicated crates |

`make vendor-check` is a gate of its own, run by CI and not by `check`: it
needs crates.io to prove `third_party/qmetaobject` is upstream plus its one
patch. See `docs/packaging.md`.

## SonarQube Cloud

A second opinion, published by `.github/workflows/sonar.yml`, and deliberately
**not** part of the gate. `make check` and `ci.yml` decide what is allowed in;
Sonar's quality gate is advice. Making a hosted service part of intake would
break the one rule at the top of this file — that a laptop with no network is
enough to know whether a change is good.

Two things it reports on are generated locally by `make sonar-reports`, which
writes `target/sonar/clippy.json` and `target/sonar/lcov.info`:

- **Clippy.** The Sonar Rust analyser can run clippy itself, and does not here.
  It would invoke cargo at the workspace root, where `vuo-shim` and
  `harbour-vuo` do not compile without Qt headers, and it knows nothing about
  the `--exclude` lists that keep the Qt-free crates buildable on a bare
  runner. `make sonar-reports` runs the same three invocations `make clippy`
  runs and hands over the JSON.
- **Coverage,** from `cargo llvm-cov` over `vuo-core` and `vuo-shim`.

`make sonar-reports` runs `cargo clean -p` on the three workspace crates first.
That is not tidiness: cargo prints each diagnostic once and caches it
afterwards, so on a warm `target/` the report comes out empty — and an empty
report is imported without complaint, as *clippy found nothing*.

Expect the clippy report to contain nothing of Vuo's own, and treat that as
correct rather than broken: `make check` runs clippy with `-D warnings`, so a
green build has no warnings left to report. It is carried anyway as insurance
— if that denial is ever relaxed, the findings surface here instead of
disappearing. (What the report *does* contain today is
`third_party/qmetaobject`, which is excluded from the analysis, so those are
dropped on import.)

The results do not stay on the dashboard. `scripts/sonar-report.sh` asks the
server, from the runner that just fed it, and prints the quality gate, the
measures and the open issues into the job log and the step summary — so the
numbers sit beside the commit that earned them, readable without an account.
It waits for the server to finish processing first: asking too early returns
the PREVIOUS run's numbers, which is worse than none, because they look right.

Two things worth knowing before reading a report. Imported clippy findings
arrive as **external issues**: they do count toward the quality gate, but the
rules raising them cannot be switched off in a Sonar quality profile — clippy's
own configuration is the only place to silence one. And the analyser has no
idea what QML is: the `qml/` tree is covered by `qmllint` and the QML load
test, and by nothing here.

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

### The cover test, and the texture

The load test instantiates every file with its properties at their defaults,
which for the app cover shows the heading -- the name, the sync line, the
count. What it cannot see is what that heading says as sync and the count
move underneath it, which is what `crates/vuo-shim/tests/qml_cover.rs` drives
in an engine of its own: the count survives a refresh, a failure puts Vuo's
own translated line on the cover rather than the server's words, and a count
too wide for the corner is capped rather than pushed into the app's name.

The texture under it is **not computed at runtime**. It is painted ahead of
time by `tools/textart/` and shipped as a coverage mask in `qml/art/`; see
the packaging notes. Two things about it are still checked:

- `qml_loads.rs` walks every `source:` in the QML and asserts the file is
  there and is an 8-bit **grayscale** PNG. The shader reads coverage from the
  red channel, so a mask re-exported as RGBA -- which any image editor does
  by default, and which looks identical in a viewer -- would tint the whole
  surface solid.
- The cover test asserts the texture fades in *below* the heading. It is
  drawn under the whole cover, so that fade is the only thing keeping the
  app's name off a field of text.

What the pattern *looks like* is a manual check: render it, look at it. That
is worth doing before touching `tools/textart/`, and the harness that does it
is `make textart` itself -- it writes the masks, and the masks are what ships.

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

### A crash that only happens on a device

`make check` runs the QML in a real engine, and the shim's tests drive the
settings screen's save-and-test path against a temporary home directory. Both
are worth having, and neither reproduces a fault that needs a phone: Sailfish's
Qt, libhybris, Sailjail, and an `aarch64` binary built at `opt-level = "z"` with
fat LTO are all outside what a laptop can stand in for.

When the process dies on a **signal** rather than an error, there is nothing
else to read. The device package is stripped, so a backtrace names no
functions; `panic = "abort"` means a Rust panic would at least print its
message first, so a silent death (`echo $?` → 139, `SIGSEGV`) is *not* a panic
and no Rust diagnostic is coming. `/proc/sys/kernel/core_pattern` is
`|/bin/false` on a stock device, so there is no core either. The only evidence
is the last line the process managed to log.

That is why the shim logs at `info` **by default**, not only under `VUO_LOG`,
and why the account path is narrated step by step. It is how the one fault this
found was found: creating a thread from the Qt thread, once Wayland and the GPU
stack were up, killed the process outright — the line before
`thread::Builder::spawn` was the last thing printed, and neither the parent's
next line nor the new thread's first one ever arrived. That is why the sync
worker's thread is now created in `main`, before the UI, and handed its account
by a channel send afterwards. **A thread created after start-up is a hazard on
this platform**; if another one is ever needed, start it there too.

Run the app from a terminal so the lines are on screen:

```sh
rm -rf ~/.local/share/harbour-vuo ~/.cache/harbour-vuo   # a genuine first run
sailjail /usr/bin/harbour-vuo ; echo $?                  # 139 SIGSEGV, 134 abort, 137 OOM
```

Every launch starts the worker, whether or not there is an account:

```
the sync worker thread is spawned    # the Qt thread, as `spawn` returns
the sync worker thread is running    # the worker thread's own first statement
the sync runtime is up
```

Those two are a deliberate pair. Once the process is gone, nothing else says
which thread it died on, and they interleave by a few microseconds — so read
them as a pair rather than as an order. Saving an account then reads:

```
saving the account
the account file is written
opening the mirror
handing the account to the sync worker
the application context is built
the application context is installed
the settings screen has published the saved account
the test is queued for the worker            # only from the Test button
the settings screen has finished the test
the worker has opened the mirror             # the worker thread, in parallel
the sync worker is ready
asking the server who we are
```

Whichever line is missing bounds the fault to the statements between it and the
one before it. One test costs nothing and separates the two halves: fill in the
server and the key and **swipe back instead of tapping Test connection**. The
page saves on destruction, so that runs everything up to and including handing
the account over, and none of the network round trip.
