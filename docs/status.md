# Status: what is verified, and what is not

Written down because the difference matters and is not visible from a green CI
badge. Everything in the first table is checked by `make check` on every
change; nothing in the second has ever run on a phone.

## Verified on every change

| Area | How |
| --- | --- |
| Miniflux REST client | 9 wiremock-backed tests, plus unit tests on every request-builder invariant |
| Outbox reconciliation | 9 tests covering each property §8.3 names |
| Incremental pull and deletion reconcile | 8 tests, including the torn-listing abort |
| HTML → block transform | 43 unit tests, 6 snapshots, 2 fuzz targets |
| Foreign-input handling (§9) | scheme validation, escaping, caps, icon sniffing — all tested |
| Schema migrations | machinery tested against a synthetic second migration |
| Qt shim | compiled and tested against real Qt under `QT_QPA_PLATFORM=offscreen` |
| QML | every file compiled in a real QML engine against the Silica stubs |
| Packaging | spec/version consistency, installed-file existence, desktop entry validity |

## NOT verified — milestone 2 is outstanding

**No part of this has run on a SailfishOS device, or been built with the
SailfishOS SDK.** The environment this was developed in has neither. Concretely:

- **The RPM has never been built.** `rpm/harbour-vuo.spec` is written from
  Whisperfish's working spec and is internally consistent
  (`scripts/check-packaging.sh` checks what it can without an SDK), but a spec
  that has never been run is a hypothesis.
- **The cross-compile has never run.** The `SB2_RUST_TARGET_TRIPLE` and
  per-target linker exports are taken from a project that ships this way; they
  are not confirmed for this dependency set. `rusqlite`'s bundled SQLite and
  `rustls`'s crypto backend are the two most likely to need attention on
  `armv7hl`.
- **The shim is compiled against Qt 5.15, not Qt 5.6.** The Silica target is
  5.6. `qmetaobject` sets no `qt_5_*` cfg at all on 5.6, so
  `qml_register_enum` and friends do not exist there — the code already avoids
  them, but "avoids them" is not the same as "was compiled there".
- **The Silica stubs are an approximation.** They declare the surface Vuo uses,
  which is enough for the load test to be meaningful, but real Silica has
  geometry, theming and behaviours the stubs do not. Pages that compile here
  can still lay out wrongly on a device.
- **SailfishApp integration is unrun.** The `cpp!` block in
  `crates/harbour-vuo/src/main.rs` is behind the `sailfishapp` feature and has
  never been compiled, because the headers only exist inside the SDK.
- **Background sync is unproven against suspend.** §11 flags this as carrying
  the most schedule risk after the toolchain, and it remains open: a systemd
  user timer is the design, but whether it survives device suspend reliably is
  exactly the thing that needs a device.
- **The ephemeral-Miniflux CI job has never run**, and its container images are
  pinned by version tag rather than by digest — see the note at the top of that
  workflow.

## Open questions from §11 that are now answered

Resolved from Miniflux's source and written up in
[`api-contract.md`](api-contract.md):

- incremental sync cursor and deletion detection;
- batch mutation semantics and idempotency;
- media proxy addressing, authentication and coverage;
- icon delivery and the thundering-herd question.

## Still open

- **Async runtime under Qt 5.6** — resolved in design (worker thread plus
  `queued_callback`, because `reqwest` needs a Tokio reactor), unresolved in
  practice until it runs on 5.6.
- **Background execution** — see above.
- **Sailfish 5.x targets** — the current SDK's target availability and whether
  the Qt and Rust assumptions still hold on the newest release.
