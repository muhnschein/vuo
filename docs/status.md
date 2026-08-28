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

## Bugs found by the project's own tooling

Recorded because they justify the tooling's cost, and because each one passed
every other check in the build.

| Found by | Bug |
| --- | --- |
| The QML load test | `harbour-vuo.qml` never imported `pages/`, so the app would have failed at launch. `qmllint` passes this file. |
| A QML/Rust API contract test | Nine methods the QML called — `setRead`, `setStarred`, `markAllRead`, `markFeedRead`, `subscribe`, `unsubscribe`, `load`, `fetchOriginal`, `allowImagesFrom` — had never been implemented. QML resolves calls at runtime, so nothing else caught it. |
| Fuzzing the transform | A **quadratic blow-up**: a skipped subtree (`<svg>`, `<script>`) grew the open-element stack without applying the depth cap, and each unmatched end tag scanned all of it. 320 KB of markup took 23 s, well inside the 2 MiB input cap — a frozen UI, which is exactly what §9.2's caps exist to prevent. Now linear (245 ms). |
| A probe over void elements | `<meta>`, `<link>`, `<input>`, `<embed>`, `<source>`, `<track>`, `<param>`, `<area>` and self-closing `<svg/>` **silently truncated the entire article**: each is on the skip list and is void, so skip mode was entered with no end tag able to clear it. `<input>` is common in real feed HTML. |
| The same probe | `<script>` and `<style>` bodies **leaked into the article as text** when opened past the depth cap, because the cap's early return preempted skip handling. An allowlist breach (§9.2). |
| Fuzzing | One bug in a *test*: the fuzz target asserted on substrings like `onerror=`, which correctly-escaped text can legitimately contain. Replaced with the real structural invariant — every tag in rendered output must come from the closed set. |
| `cargo-deny` | Vuo's own crates were rejected: the workspace is `GPL-3.0-or-later`, a different SPDX id from the `GPL-3.0` the allow-list named. |
| A guard test | `reqwest::Certificate::from_pem_bundle` answers `Ok(vec![])` rather than `Err` for input containing no PEM blocks, so a truncated CA file was silently becoming "no extra CA". |

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
