# Architecture

```
+---------------------------------------------------------------+
|  Vuo (SailfishOS RPM)                                         |
|                                                               |
|  +-----------------------------+   +----------------------+   |
|  |  QML / Silica UI            |   |  Background sync     |   |
|  |  qml/                       |   |  the worker's cadence|   |
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

## The words on screen

Every `qsTr()` in `qml/` is translated into 39 languages, so the English is
not only what a reader sees -- it is the source text a translator works from.
English that leans on a figure, an idiom or a verbless fragment does not
survive that trip, and the damage is invisible from here. Four that shipped and
had to be undone:

- **A figurative verb read literally.** "Marking an article read or unread
  yourself always *wins*" became *voittaa aina* in Finnish and *az mindig
  erősebb* ("is always stronger") in Hungarian.
- **A precise verb flattened.** "The server *scrapes* each article's own page"
  became plain *reads* in Finnish, Dutch, Russian and Swedish, which loses the
  one thing the switch does.
- **A short label resolved the wrong way.** "Hide from unread" became *hide
  among the unread* in Polish and Swedish.
- **A loose verb given its other sense.** "Star an article to *keep* it here"
  became *store* in Dutch and *save* in Swedish.

So a string says what the thing does, once, in a finished sentence:

- One clause with a subject and a verb. No second fragment as a coda, no
  epigram.
- Literal verbs. "Downloads", not "scrapes"; "contains", not "carries".
- Miniflux's label where the control is a Miniflux setting: "Fetch original
  content", "Do not refresh this feed", "Add feed", "Mark all as read". A
  string that names a place in Miniflux's web UI quotes Miniflux's own label in
  each catalog's language -- and English, in a language Miniflux is not
  translated into, because that is what its reader sees there.
- "Article", not Miniflux's "entry": it is the reader's word, and the one every
  catalog already uses.
- Sentence case, and `…` rather than three dots.

A subtitle under a control earns its place only by saying what the label
cannot: a consequence that is not obvious (what "Keep read articles" never
deletes, what loading an unproxied image tells its website) or a thing to do
(where the CA certificate goes). One that restates its label goes.

Changing a source string orphans its translation in every catalog. `lupdate`
leaves the new string empty, Qt falls back to English on those phones, and
nothing fails: `make check` counts messages, not translations (see
`scripts/check-packaging.sh`). So the cost of getting the English wrong is paid
39 times, silently. Write it once.
