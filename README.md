# Vuo

*A native SailfishOS client for [Miniflux](https://miniflux.app/).*

Vuo (Finnish *vuo*, "flux/flow" — as in *magneettivuo*, magnetic flux) is a
Silica-native feed reader that syncs against a self-hosted Miniflux instance
over Miniflux's own REST API.

No native Sailfish client speaks a Miniflux-compatible protocol today. Sailfish
users on Miniflux fall back to the web UI in the built-in browser, or to an
Android client under AppSupport. Vuo exists to close that gap.

## Thesis

**Do not build a feed reader — build a SailfishOS UI over an existing
feed-reading server.**

Fetching, parsing, sanitisation, deduplication, full-text extraction and
scheduling all stay on the server. Vuo contributes a Silica presentation layer,
a local mirror for offline reading, an offline-tolerant write path, and
packaging. If a feed-format parser is being written here, the server is being
misused.

## Architecture

The application core is Rust from the first commit. This is a deliberate
constraint, not an optimisation: the sync engine is the part of the app with
real invariants — cursors, conflict resolution, an outbox that must not lose or
double-apply mutations — and it must be testable on a laptop without a phone, a
server, or a running Qt event loop.

| Crate | Depends on Qt? | What it is |
| --- | --- | --- |
| `vuo-core` | no | Miniflux REST client, SQLite mirror, sync engine, HTML→block transform |
| `vuo-shim` | yes | `qmetaobject-rs` adapters exposing the core to QML as `QObject`s and list models |
| `qml/` | yes | Silica UI |

Two properties fall out of the layering and are worth stating explicitly:

- **The local SQLite mirror is the single source of truth for the UI.** The UI
  never waits on the network. Sync writes to SQLite; models observe SQLite.
  Offline reading is a consequence of the architecture, not a feature bolted on
  later.
- **Local mutations go through an outbox.** Marking read, starring and
  mark-all-read are written locally and enqueued, then replayed against the
  server in batches. Replay is idempotent and survives being killed mid-flight.

See [`docs/scope.md`](docs/scope.md) for the full scope and non-goals.

## Building

`vuo-core` builds and tests on a plain host toolchain — no Qt, no Sailfish SDK:

```sh
make check      # fmt, clippy, tests, qmllint, packaging checks — exactly what CI runs
make msrv       # re-check against the Sailfish Rust floor
```

The governing rule: **`make check` runs exactly what CI runs, from a clean
checkout, with no phone, no server account, and no network.** Anything that
cannot be verified under those conditions is either badly layered or belongs
behind an explicit opt-in gate.

Device RPMs are built with the Sailfish SDK (Docker build engine — the
VirtualBox engine cannot build Rust):

```sh
scripts/build-rpm.sh aarch64
```

## Status

Pre-1.0, under active development. Targets Harbour: one process, sandboxed,
syncing on its own while it runs.

## Licence

GPL-3.0-or-later. See [`LICENSE`](LICENSE).
