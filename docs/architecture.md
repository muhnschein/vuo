# Architecture

```
+---------------------------------------------------------------+
|  Vuo (SailfishOS RPM)                                         |
|                                                               |
|  +-----------------------------+   +----------------------+   |
|  |  QML / Silica UI            |   |  Background sync     |   |
|  |  qml/                       |   |  systemd user timer  |   |
|  +--------------+--------------+   +----------+-----------+   |
|                 |  models / signals           |               |
|  +--------------v-----------------------------v-----------+   |
|  |  vuo-shim  (Rust, qmetaobject-rs)                      |   |
|  |  QAbstractListModel adapters; QObject facade;          |   |
|  |  a worker thread owning the async runtime              |   |
|  +--------------------------+-----------------------------+   |
|                             |  plain Rust API, no Qt types    |
|  +--------------------------v-----------------------------+   |
|  |  vuo-core  (Rust, no Qt, no SailfishOS)                |   |
|  |  api/ db/ sync/ content/                               |   |
|  +--------------------------+-----------------------------+   |
|                             |  HTTPS                          |
+-----------------------------|---------------------------------+
                              v
                     Miniflux instance
```

## The layering rule

`vuo-core` has no Qt dependency and no SailfishOS dependency. It is an ordinary
Rust library, unit-tested against a mock HTTP server on the host.

> If a bug can only be reproduced on a phone, the layering is wrong.

That is what buys the Rust core its keep, and it is why the sync engine — the
part with real invariants — has 155 tests that run in under a second on a
laptop with no network.

## The data flow that makes offline work

```
    server ──pull──▶ SQLite ──observe──▶ models ──▶ QML
                       ▲                    │
                       └──── outbox ◀───────┘
                              │
                              └──replay──▶ server
```

The **local SQLite mirror is the single source of truth for the UI**. The UI
never waits on the network: a tap writes to SQLite and returns, and the models
re-read from SQLite. Offline reading is a consequence of that shape rather than
a feature bolted on later.

Local mutations go through the **outbox**, which is a keyed desired-state map
rather than an operation log. See [`api-contract.md`](api-contract.md) §3 for
why the server forces that choice, and `db/outbox.rs` for the implementation.

## Boundaries, and what each one narrows

```
socket ──▶ transport ──▶ wire ──▶ convert ──▶ model ──▶ content ──▶ QML
           bounded,      permissive per-item  strict    allowlisted  explicit
           redirect-     serde      validation          blocks       textFormat
           policed
```

Each arrow reduces what the next stage has to worry about:

- **transport** — size-capped, timeout-bounded, redirect-policed, token never
  leaving the configured origin.
- **wire** — `serde` succeeds on any plausible response, so version skew is
  uninteresting rather than fatal.
- **convert** — validates *per item*, so one absurd entry costs one row rather
  than the whole sync.
- **model** — strict types whose invariants the rest of the crate can rely on.
- **content** — an allowlist that maps recognised elements to a closed set of
  render blocks; there is no passthrough.

## Threading

Everything Qt touches runs on the Qt thread. Network and database work runs on
a dedicated worker thread with its own current-thread Tokio runtime, and
results return via `queued_callback`.

This is *not* `qmetaobject::execute_async`, which polls futures from Qt's event
loop, and the reason is worth recording: `reqwest` needs a Tokio **reactor** to
wake its futures on socket readiness. Polling a `reqwest` future from Qt's
event loop with no runtime entered panics at the first socket registration.

Local mutations are the exception — they are applied inline on the Qt thread,
because the write is a fast local transaction and doing it inline is what lets
the UI update in the same frame as the tap.

No database transaction is ever held across an `await`. Holding one open for
the duration of a request would block the UI's readers for however long the
phone's signal takes.

## Crate boundaries and `unsafe`

| Crate | Qt? | `unsafe` |
| --- | --- | --- |
| `vuo-core` | no | `#![forbid(unsafe_code)]` |
| `vuo-shim` | yes | allowed, but all of it is macro-generated; the crate writes no `unsafe` block of its own |
| `harbour-vuo` | yes | one `cpp!` block for the SailfishOS entry point |

`vuo-shim` and `harbour-vuo` are deliberately **not** in the workspace's
`default-members`, so `cargo build` on a runner without Qt headers does the
right thing rather than failing for a reason unrelated to the change.
