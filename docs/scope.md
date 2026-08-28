# Vuo — Project Scope

*A native SailfishOS client for Miniflux*

**Status:** Draft scope / pre-implementation
**Working name:** Vuo (Finnish *vuo*, "flux/flow" — as in *magneettivuo*, magnetic flux)
**Document type:** Scope & non-goals

---

## 1. Summary

Vuo is a native SailfishOS feed reader that syncs against a self-hosted
Miniflux instance. No native Sailfish client speaks any Miniflux-compatible
protocol today: the Chum repository ships Tidings (standalone, no server
sync), a Tiny Tiny RSS client, and an ownCloud/Nextcloud News client, and
Miniflux implements none of those APIs. Sailfish users on Miniflux currently
fall back to the web UI in the built-in browser or an Android client under
AppSupport.

The project's thesis is narrow: **do not build a feed reader, build a
SailfishOS UI over an existing feed-reading server.** Fetching, parsing,
sanitisation, deduplication, full-text extraction, and scheduling all stay on
the server. Vuo contributes a Silica presentation layer, a local mirror for
offline reading, an offline-tolerant write path, and packaging.

The application core is written in **Rust** from the first commit. This is a
deliberate constraint, not an optimisation: the sync engine is the part of
the app with real invariants (cursors, conflict resolution, an outbox that
must not lose or double-apply mutations), and it should be testable on a
laptop without a phone, a server, or a running Qt event loop.

---

## 2. Goals

- A Silica-native (Qt/QML) Miniflux client that feels like a first-class
  Sailfish app rather than a port.
- Integrate through Miniflux's **own REST API**, authenticated with an API
  key, not through the Fever or Google Reader compatibility layers.
- Support the daily reading workflow: unread list, article view, mark
  read/unread, star, per-feed and per-category browsing, refresh.
- **Read offline.** Articles synced while online are readable with no
  network, and state changes made offline reconcile when connectivity
  returns.
- Keep all logic that is not literally UI in Rust, in a crate that builds and
  tests on a plain host toolchain with no Qt and no Sailfish SDK.
- Ship as an installable RPM through community channels (Chum / OpenRepos),
  built reproducibly via the Sailfish SDK and, ideally, OBS.

---

## 3. Non-goals (explicitly out of scope)

Called out deliberately to prevent scope creep. Several are tempting
precisely because the code exists nearby or the feature is one API call away.

- **Local feed fetching or parsing.** No RSS/Atom/JSON Feed parser, no
  HTTP polling of feed URLs, no standalone mode for users without a server.
  If a feed-format parser is being written, the server is being misused.
  This is the single most important boundary in the project.
- **Owning an HTML sanitiser.** Miniflux sanitises server-side and strips
  trackers before delivery; Vuo does not ship a second sanitiser chasing the
  same threats. Note this is not the same as trusting the bytes: the
  content-to-block transform (§5) is allowlist-by-construction, since only
  recognised elements map to render blocks and everything else is dropped or
  flattened to text. Safety there is a property of the parser's shape, not a
  filter bolted on after it.
- **Other sync protocols.** No Fever, no Google Reader, no TT-RSS, no
  Nextcloud News. Single-backend by design. Multi-backend abstraction is the
  fastest route to a client that serves every backend badly.
- **Client-side readability extraction.** Miniflux exposes an endpoint to
  fetch original article content; use it. No local Readability port.
- **Feed administration beyond the basics.** Adding and removing a
  subscription is in scope. Rewrite rules, scraper rules, blocklists,
  integrations, and user management belong in the web UI.
- **A podcast player.** Enclosures may be listed and handed to the system
  media player. Building a player, a queue, or download management is a
  separate application.
- **Push notifications.** Miniflux has no push infrastructure for third-party
  clients. Background refresh is a periodic pull under Sailfish's power
  rules, not a push relay.
- **A desktop, Android, or web build.** Sailfish only.
- **Hosting advice.** Vuo is a client. It does not install, configure, or
  recommend how to run Miniflux.
- **Broad backwards compatibility with old Sailfish releases** at launch.
  Pick one baseline (§7) and expand only when someone asks.

---

## 4. What can be reused (do not start from scratch)

| Component | Reuse as | Notes |
| --- | --- | --- |
| Miniflux REST API | **Interface contract** | Documented, stable, JSON. Authenticate with an API key created in Settings → API Keys. Prefer key auth over username/password so credentials can be revoked per-device. |
| Whisperfish | **Architectural template** | The closest prior art for Rust + QML on Sailfish: cross-compilation, `sfdk`/`mb2` packaging, cargo vendoring for network-less OBS builds, driving async Rust from Qt's event loop. Study the build tooling and crate layout, not the Signal code. |
| `qmetaobject-rs` | **Direct dependency** | Compile-time `QMetaObject` generation; exposes Rust structs to QML as `QObject`s and list models. Has Qt 5.6 support contributed for Sailfish. Note it is only passively maintained upstream — budget for carrying a patch or two. |
| `sailo-rs` | **Reference / possible dependency** | Sailfish-specific Rust glue factored out of Whisperfish. Check current state before depending on it. |
| Postivene | **Repo-layout and CI template** | Another Rust-core Sailfish app: workspace split into a Qt-free core crate, a `qmetaobject` shim, and a thin app harness. Its test and CI tooling is the most directly transplantable thing in this table — see §8. |
| Existing open-source Silica apps | **UI convention reference** | For idiomatic page flow, pulley menus, remorse actions, and empty states — so the result feels native. Check each app's licence before copying anything verbatim; most are GPL. |

---

## 5. Architecture (intended)

```
+---------------------------------------------------------------+
|  Vuo (SailfishOS RPM)                                         |
|                                                               |
|  +-----------------------------+   +----------------------+   |
|  |  QML / Silica UI            |   |  Background sync     |   |
|  |  - unread / feed / category |   |  - systemd user timer|   |
|  |  - article view (blocks)    |   |  - suspend-aware     |   |
|  |  - settings, account setup  |   |  - shares the core   |   |
|  +--------------+--------------+   +----------+-----------+   |
|                 |  models / signals           |               |
|  +--------------v-----------------------------v-----------+   |
|  |  vuo-shim  (Rust, qmetaobject-rs)                      |   |
|  |  - QAbstractListModel adapters (entries, feeds, cats)  |   |
|  |  - QObject facade: sync(), setRead(), star()           |   |
|  |  - async runtime driven from the Qt event loop         |   |
|  +--------------------------+-----------------------------+   |
|                             |  plain Rust API, no Qt types    |
|  +--------------------------v-----------------------------+   |
|  |  vuo-core  (Rust, no Qt, no Sailfish)                  |   |
|  |  - Miniflux REST client (reqwest + rustls + serde)     |   |
|  |  - SQLite mirror (entries, feeds, categories, icons)   |   |
|  |  - sync engine: incremental pull + outbox replay       |   |
|  |  - content -> render-block transform                   |   |
|  +--------------------------+-----------------------------+   |
|                             |  HTTPS                          |
+-----------------------------|---------------------------------+
                              v
                     Miniflux instance
```

Key decisions:

- **`vuo-core` has no Qt dependency and no Sailfish dependency.** It is a
  library with an ordinary Rust API, unit-tested against a mock HTTP server
  on the host. If a bug can only be reproduced on a phone, the layering is
  wrong. This is what buys the Rust core its keep.
- **The local SQLite mirror is the single source of truth for the UI.** The
  UI never waits on the network. Sync writes to SQLite; models observe
  SQLite. This makes offline reading a consequence of the architecture rather
  than a feature bolted on later.
- **Local mutations go through an outbox.** Marking read, starring, and
  marking a whole feed read are written locally and enqueued, then replayed
  against the server in batches. Replay must be idempotent and survive being
  killed mid-flight. This is the part most worth writing tests for first.
- **Article HTML is transformed into a block list in Rust**, not rendered as
  one rich-text blob. Sailfish's Qt vintage supports only a subset of HTML in
  `Text`, and a `WebView` is heavy and awkward inside a list. Parsing to
  paragraphs, headings, code blocks, images, and quotes in Rust gives control
  over rendering, lazy image loading, and font scaling — and keeps the QML
  dumb.
- **Background refresh shares `vuo-core`** rather than reimplementing sync in
  a script. Whether it runs as a separate systemd user unit or in-process
  under Sailfish's keepalive rules is an open question (§11).

---

## 6. Milestones

1. **Headless core.** `vuo-core` authenticates with an API key, pulls
   categories, feeds, and entries into SQLite, and replays an outbox.
   Tested entirely on the host against a mock server, plus one opt-in
   integration test against a real instance behind an env var. The host CI
   gate (§8) is stood up here, before there is any code to retrofit it onto.
2. **On-device bring-up.** Cross-compile for `aarch64` and `armv7hl`, build
   an RPM via `sfdk`, install on a device, and confirm a round trip against a
   real server from a minimal harness. Resolve the toolchain questions here,
   before any UI exists.
3. **Read-only UI.** Unread list, article view, pull-to-refresh. No writes,
   no offline reconciliation, no polish.
4. **Write path.** Mark read/unread, star, mark-all-read, with the outbox
   reconciling correctly across airplane-mode transitions. Feed and category
   browsing.
5. **Background sync and notifications.** Periodic refresh under the
   platform's power rules; a cover showing unread count; notification on new
   entries. Expected to be the hardest platform work.
6. **Packaging and release.** OBS build, Chum submission, translations
   scaffolding, README and screenshots.

Subscription management (add/remove feed, share-to-subscribe from the
browser) lands after milestone 4 and before release.

---

## 7. Platform baseline & constraints

- **Qt 5.6.** Silica links against the system Qt, and Jolla's reference
  documentation still names 5.6. The Qt 5.15 packages in Chum live under
  `/opt` and cannot be combined with Silica. Assume 5.6-era QML: no
  `required` properties, no Qt Quick Controls 2. Verify against the current
  SDK before writing much QML.
- **Rust toolchain floor.** Sailfish ships an older Rust than current stable
  (reported as 1.75 in comparable projects). Pin `rust-version` in the
  workspace, keep `Cargo.lock` in the v3 format if targeting < 1.78, and run
  an MSRV check in CI so a dependency bump does not silently break the device
  build. Confirm the exact floor for the chosen target.
- **Build engine:** Rust compilation requires the Sailfish SDK's **Docker**
  build engine; the VirtualBox engine cannot build it. Expect slow first
  builds.
- **Architectures:** `aarch64` and `armv7hl` for devices, `x86_64` optionally
  for the emulator.
- **OBS builds have no network.** Vendor crate sources and gate the offline
  path behind a spec flag from the start; retrofitting this is tedious.
- **Distribution:** target Chum/OpenRepos. Harbour's library restrictions and
  the background-service requirement make it a stretch goal at best; do not
  design around it.
- **Credentials:** the API key is stored under the app's data directory with
  restrictive permissions, relying on Sailfish's home encryption. No custom
  keyring, no SQLCipher, unless a concrete threat model justifies it.

---

## 8. Testing & CI

The governing rule, taken from Postivene: **`make check` runs exactly what CI
runs, from a clean checkout, with no phone, no server account, and no
network.** Anything that cannot be verified under those conditions is either
badly layered or belongs behind an explicit opt-in gate.

### 8.1 What transplants from Postivene

| Component | Reuse | Notes |
| --- | --- | --- |
| `make check` / `make msrv` targets | **Near-verbatim** | fmt, clippy, tests, `qmllint`, packaging checks in one target; a second target compiling against the Sailfish Rust floor so a dependency bump cannot silently break the device build. |
| `rust-toolchain.toml`, pinned MSRV, v3 `Cargo.lock` | **Verbatim** | Same platform floor, same constraint, same failure mode if ignored. |
| Silica QML stubs for off-device `qmllint` | **Mechanism verbatim, stubs extended** | `Sailfish.Silica` only ships with the SDK, so linting QML on a CI runner needs a stub module. Vuo needs more of the surface than a chat app does — `SilicaListView`, `PullDownMenu`, `RemorsePopup`, `SectionHeader`, `ViewPlaceholder`, `SilicaFlickable`. Extend rather than reinvent. |
| Offscreen Qt event-loop smoke test | **Verbatim pattern** | Exercises the shim under `QT_QPA_PLATFORM=offscreen` on a headless runner. Catches model-registration and threading mistakes without a device. |
| `build-rpm.sh` (`sfdk` wrapper) | **Near-verbatim** | Rename and retarget; the `sfdk -c target=<arch> build` mechanics are identical. |
| `vendor-crates.sh` + `--with vendor` spec flag | **Verbatim** | Same OBS no-network constraint, same solution. |
| Env-var-gated live integration test | **Pattern only** | Their gate points at a bundled binary; ours points at a Miniflux base URL and token (§8.3). |
| GitHub Actions layout | **Structure** | Host job (fmt/clippy/test/qmllint) plus an MSRV job. Device builds need the SDK and do not run on stock runners; the RPM build stays local or on OBS. |
| Host-build dependency list | **Verbatim** | `qtbase5-dev`, `qtdeclarative5-dev`, `qtdeclarative5-dev-tools` (for `qmllint`), `qml-module-qtquick2`. The `-dev` packages omit the QtQuick runtime plugin, which is a confusing failure if missed. |

### 8.2 What does not transplant

- **`fetch-rpc-server.sh`, `vendor/`, checksum provenance.** Vuo bundles no
  third-party binary. That whole apparatus — and the licensing analysis that
  comes with bundling — simply does not exist here.
- **Their integration-test shape.** Postivene spawns a subprocess it ships.
  Vuo talks to a service it does not control, which is a harder dependency to
  make reproducible (§8.3).

### 8.3 What Vuo needs that Postivene does not

- **A mock HTTP server for `vuo-core`.** `wiremock` or equivalent, with
  recorded fixtures for the endpoints in use. This is the main new
  infrastructure and it is what makes milestone 1 possible without a phone or
  a server. Postivene had no need for it: it mocks a subprocess instead.
- **An ephemeral Miniflux in CI.** A `docker compose` of
  `miniflux/miniflux:<pinned>-distroless` plus Postgres, seeded with a fixed
  OPML and a generated API key, run as a separate scheduled or opt-in job.
  The distroless variant is built on `gcr.io/distroless/static` and is the
  right default here: no shell, no package manager, nothing in the image but
  the static binary. Two consequences to plan for rather than discover.
  First, there is no shell inside the container, so readiness cannot be a
  `curl` healthcheck in the service definition — poll the health endpoint
  from the runner or a sidecar instead. Second, distroless builds cover fewer
  architectures than the Alpine ones; fine on x86_64 runners, worth checking
  before anyone tries this on an ARM box. Pin by digest, not by tag, and bump
  deliberately — an unpinned `latest` turns an upstream release into a
  mystery CI failure.

  This job is worth its setup cost specifically because the §11 open
  questions — cursor semantics and bulk-mutation idempotency — are contract
  questions about someone else's server. Verifying them once by hand and
  writing the answer into a comment means finding out by regression when the
  server updates. Fixtures for the mock server should be captured from this
  instance rather than hand-written.
- **Outbox reconciliation tests.** Deterministic, not incidental: replay is
  idempotent; a process killed mid-flight resumes without losing or
  double-applying; an offline burst of marks reconciles correctly on
  reconnect; a server-side change to an entry mutated locally resolves by a
  stated rule. These are the app's real invariants and they are all testable
  on the host.
- **Schema migration tests.** The SQLite mirror is the source of truth, so an
  upgrade that drops a pending outbox loses user actions. Test each migration
  against a fixture database from the previous version.
- **Snapshot tests for the HTML-to-block transform.** `insta` over a corpus
  of real article HTML pulled from actual feeds — malformed markup, deep
  nesting, tables, `<pre>`, figures. Cheap to write, and the failures are
  legible diffs rather than a rendering bug reported months later.
- **Fuzzing on the two parsers.** `cargo-fuzz` targets over the content
  transform and the JSON response deserialiser, seeded from the snapshot
  corpus. These are the code paths that eat attacker-influenced bytes (§9),
  and they are pure functions, which makes them unusually cheap to fuzz. Run
  short in PR CI, long on a schedule.
- **Supply-chain gates.** `cargo-deny` for advisories, licences, and banned
  or duplicated crates, in the same `make check` target as everything else.
  The old toolchain floor means dependency upgrades are already constrained;
  knowing early which advisory applies to a version that cannot be bumped is
  better than knowing at release.

---

## 9. Handling foreign input

Vuo is, structurally, a program that renders bytes written by strangers. The
Miniflux instance is operated by the user and can be treated as
*semi*-trusted; **everything it relays cannot be**. Entry content, titles,
author names, feed names, category names, link targets, image and enclosure
URLs, and icons all originate at arbitrary websites and reach the app
verbatim. A feed operator who wants to attack Vuo does not need to compromise
the server — they only need the user to subscribe.

The design rule follows from that: **the server's sanitisation is a
convenience, never a security boundary.** Vuo must be safe against a
malicious Miniflux instance and against malicious feed content passing
through an honest one.

### 9.1 Transport

- **TLS verification is not optional and gets no toggle.** Self-hosted
  clients routinely grow an "ignore certificate errors" checkbox for people
  with self-signed certs. Refuse it. Offer instead an explicit,
  per-host, user-supplied CA certificate — narrow, auditable, and it does not
  silently disable verification for every other host.
- **Do not follow redirects with the API token attached.** The token travels
  in a custom header, and HTTP clients that strip credentials on cross-origin
  redirect typically only special-case `Authorization`. A hostile or
  compromised instance answering `302` to an attacker's host would otherwise
  hand over the token. Set a restrictive redirect policy, cap the hop count,
  refuse downgrades to plaintext, and re-attach auth only for the configured
  origin.
- **Bound every response.** Cap body size and read incrementally. This is a
  phone with limited memory; an unbounded `Vec<u8>` from an untrusted length
  is an OOM waiting to happen.
- **Set timeouts on everything**, including connect, read, and total request
  duration. A server that accepts a connection and never speaks must not wedge
  a sync forever.
- **Never log the token or a URL containing it.** Redact in error paths too,
  which is where secrets usually escape.

### 9.2 Parsing

- **Treat every JSON field as absent, wrong-typed, or absurd.** Server
  versions skew; fields get added and deprecated. Deserialise into permissive
  types and validate into strict domain types at the boundary, rejecting the
  entry rather than the sync when one item is malformed. One bad entry must
  not stall the outbox.
- **Do not trust server-assigned identifiers to be well-behaved.** Entry and
  feed IDs are numbers chosen by someone else: not necessarily positive, not
  necessarily monotonic, not necessarily stable across a restore. The
  incremental cursor design must survive that.
- **Cap structural depth and size in the content transform.** Deeply nested
  markup against a recursive parser is a stack overflow; a multi-megabyte
  entry is a frozen UI. Parse iteratively or enforce a depth limit, and cap
  input length before parsing rather than after.
- **Allowlist, never blocklist.** Known elements map to known block types;
  everything unrecognised is dropped or flattened. Attributes likewise —
  `href`, `src`, `alt`, and little else. A blocklist is a promise to have
  thought of every tag, which nobody can keep.
- **Validate URL schemes after parsing.** Only `http` and `https` survive
  into a rendered link or image. `javascript:`, `data:`, `file:`, and
  anything else are dropped.

### 9.3 Rendering

- **Titles, author names, and feed names render as plain text.** In QML, a
  `Text` element in rich-text mode interprets markup — which means a crafted
  feed title becomes markup injection into the UI, and can pull remote images
  that leak the device's IP on a list scroll. Set `textFormat` explicitly
  everywhere; never leave it to the default, and never interpolate foreign
  strings into a rich-text context.
- **Never build QML from server data.** No `Qt.createQmlObject`, no dynamic
  component source assembled from a string containing anything foreign. This
  is arbitrary code execution in the app's own process.
- **Route remote images through the server's media proxy** rather than
  fetching third-party URLs directly from the phone. Direct fetches leak the
  user's IP and reading times to every host that appears in a feed — which is
  exactly the tracking Miniflux strips server-side, reintroduced by the
  client. Make proxying the default and treat the direct path as an explicit
  opt-out.
- **Validate images by content, not by claimed type.** Check magic bytes,
  cap decoded dimensions, and let decode failures be non-fatal. A feed icon
  is not a reason to kill the process.

### 9.4 Storage

- **Parameterised SQL only.** No query built with `format!`, anywhere, ever
  — including the "obviously safe" ones with an integer ID.
- **Never derive a filesystem path from server data.** Cache files are named
  by a hash of a canonical key, not by feed title or URL path. Path traversal
  through a feed name is a trivially avoidable bug.
- **Migrations must preserve the outbox.** Untrusted input can crash a sync;
  the pending user actions it was carrying must still be there on restart.

### 9.5 Process safety

- **`#![forbid(unsafe_code)]` in `vuo-core`.** Unsafe is confined to the shim,
  where `qmetaobject` requires it, and reviewed line by line.
- **No `unwrap`, `expect`, or panicking indexing on foreign data.** Enforce
  with `clippy::unwrap_used` and `clippy::expect_used` denied in the core
  crate. Panics are not a safe failure mode here: unwinding out of Rust into
  Qt's C++ frames is undefined behaviour. Either abort on panic or catch at
  the boundary, and decide which deliberately.
- **A malformed response is a handled error, not a crash.** The test for
  this is behavioural: the fuzz targets in §8.3 exist to prove it.

### 9.6 What this is not

This is not a claim that Vuo defends the user against a hostile *server
operator* in general — the operator is the user, and a malicious instance can
always lie about article content. The goal is narrower and achievable: no
input reaching the app should be able to execute code, exfiltrate the token,
leak the user's IP to third parties, corrupt the local database, or crash the
process.

---

## 10. Licensing

Miniflux is AGPL, but Vuo only speaks to it over HTTP and links against none
of its code, so the server's licence does not constrain this one. Any licence
is available.

The real constraint is QML: most Sailfish apps worth learning from are GPL,
and copying their QML verbatim makes Vuo GPL too. Decide early whether to
adopt GPLv3 deliberately — which makes borrowing UI code frictionless — or to
keep the tree permissive and treat those apps as read-only reference. Do not
discover the answer after copying a page.

---

## 11. Open questions

- **Incremental sync cursor.** Which query parameters give a reliable
  "changed since" pull without gaps or unbounded re-fetching, and how are
  server-side deletions detected? Resolve against the API docs in
  milestone 1; this determines the mirror's schema.
- **Batch mutation semantics.** Confirm the exact endpoints and payload
  shapes for bulk status changes and bookmark toggling, and whether they are
  idempotent under replay. The outbox design depends on the answer.
- **Async runtime under Qt 5.6.** Whisperfish's approach to driving async
  Rust from Qt's event loop has evolved; establish what the current
  recommended pattern is in `qmetaobject-rs` before writing the shim.
- **Background execution.** Systemd user timer versus in-process keepalive,
  and how reliably either survives device suspend. Prototype early; this
  carries the most schedule risk after the toolchain.
- **Icons.** Feed icons come from a per-feed endpoint as encoded data.
  Decide whether to cache them in SQLite or on disk, and how to avoid a
  thundering herd on first sync.
- **Media proxy behaviour.** Confirm how the server's media proxy is
  addressed and authenticated, and whether it covers every media type Vuo
  would otherwise fetch directly (§9.3). If there are gaps, decide whether
  those media simply do not render rather than fetching them from the phone.
- **Sailfish 5.x targets.** Confirm the current SDK's target availability and
  whether the Qt and Rust assumptions above still hold on the newest release.

---

*Naming note:* alternatives to "Vuo" considered — **Virta** ("current,
stream"), **Uutisvirta** ("news stream"), and **Lokki** ("gull"). Only
verified Finnish vocabulary; no invented compounds.
