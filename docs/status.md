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
| Adversarial review | **A negative `total` from `/v1/entries/ids` disabled the torn-listing guard.** The check read `total >= 0 && collected != total`, so a server answering `{"total": -1, "entry_ids": []}` skipped it entirely and the reconcile deleted the user's whole mirror, pending outbox rows included. |
| Adversarial review | **A policy-refused redirect discarded queued user actions.** Dropping was gated on "not transient", which is true of a refused redirect — so a mistyped server URL silently destroyed every queued mark and star. |
| Adversarial review | **`BEGIN DEFERRED` lost a user's mark to a concurrent sync.** Read-then-write took its WAL snapshot at the read; a commit from the timer in between made the write fail with `SQLITE_BUSY_SNAPSHOT`, which `busy_timeout` cannot rescue. |
| Adversarial review | **The deletion reconcile never paged**: `PAGE` was 10 000 but the query clamps to the server's 1 000 cap, so "a short page means done" fired on page one and every corpus over 1 000 entries aborted on every run. |
| Adversarial review | **The pull advanced its cursor after stopping at the page cap**, marking as seen a window it never finished reading — every entry beyond the stopping point skipped forever. |
| Adversarial review | **An empty `/v1/feeds` response wiped the mirror.** A proxy serving a stale cached `[]` read as "the user unsubscribed from everything". |
| Adversarial review | **A reachable panic**: hostile feed counters overflowed the divergence check's accumulator. §9.5 forbids this outright — unwinding into Qt's C++ frames is undefined behaviour. |
| Adversarial review | **Tables were unbounded.** A table is one block, so `max_blocks` did not constrain it; half a million `<td>`s built an 80 MB structure reporting one block and no truncation. |
| Adversarial review | **Undecodable icons starved every feed behind them**, forever: 40 requests over five passes, zero icons stored, two good icons never reached. |
| Adversarial review | **`removed` was never translated into a local delete**, despite a schema comment promising it — so on a pre-2.3 server, soft-deleted entries stayed in the mirror permanently. |
| Adversarial review | Concurrent first open failed: `journal_mode = WAL` needs a lock and `busy_timeout` does **not** apply to it, so the UI and the timer starting together raced. |
| Adversarial review | Three comments claimed more than the code did, including one asserting `rusqlite::pragma_update` binds its value as a parameter. It does not — it renders it into the statement text. |
| `cargo-deny` | Vuo's own crates were rejected: the workspace is `GPL-3.0-or-later`, a different SPDX id from the `GPL-3.0` the allow-list named. |
| A guard test | `reqwest::Certificate::from_pem_bundle` answers `Ok(vec![])` rather than `Err` for input containing no PEM blocks, so a truncated CA file was silently becoming "no extra CA". |

## Known limitations in the UI layer

Found by review and **not** fixed, so that they are stated rather than
discovered:

- **The app must be restarted after first-run account setup.** The shared
  context (database handle plus sync worker) is installed once at start-up
  from the stored account, so a device with no account configured yet starts
  without one and does not pick it up when Settings writes the file. Fixing it
  properly means tearing down and re-installing the worker at runtime, which
  is exactly the kind of lifetime work that should not be written without a
  device to test it on.
- **Model updates are polled, not pushed.** QML owns the list models, so Rust
  holds no handle on a live one; a 1.5-second timer polls a generation counter
  the worker bumps. Correct and simple, but not instant.
- **Feed unread badges are computed per reload**, not incrementally. Fine at
  the feed counts a phone has; not fine at thousands.

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
