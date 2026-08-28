# The Miniflux contract Vuo relies on

*Answers to §11's open questions, established by reading Miniflux's own source
rather than its documentation, and recorded here so they do not have to be
rediscovered.*

Everything below was traced through `miniflux/v2` on `main`. Where a claim
matters enough that being wrong would corrupt user data, the test that would
catch a regression is named.

---

## 1. Incremental sync cursor (§11, question 1)

**Question.** Which query parameters give a reliable "changed since" pull
without gaps or unbounded re-fetching, and how are server-side deletions
detected?

**Answer.** `changed_after` for the window; a **keyset on `id`** for the
paging. Never `offset`.

### Why not offset

`GET /v1/entries` validates `order` against a whitelist of nine column names,
but the generated `ORDER BY` contains exactly **one** expression with **no id
tiebreaker**. Every order except `id` is therefore unstable: rows that compare
equal may come back in a different order on each query.

This is not theoretical. A single mark-all-as-read stamps thousands of rows
with an identical `changed_at`. Paging through them with `offset` silently
skips some and duplicates others, and the user sees articles that never appear
in their unread list.

`order=id&direction=asc&after_entry_id=N` compiles to `e.id > N` over the
primary key. No ties, so no skipping.

### The loop

```
GET /v1/entries
    ?changed_after=<cursor>        # omitted entirely on first sync
    &order=id&direction=asc
    &limit=250
    &after_entry_id=<last id>      # omitted on the first page
```

Stop when a page comes back shorter than `limit`. Do **not** use `total` as a
loop bound: with `after_entry_id` set it counts only the rows still ahead of
the cursor, so it shrinks every page.

### Why there are no gaps

An entry mutated *during* a pass either has `id > last_id`, in which case this
pass sees it, or `id <= last_id`, in which case it does not — but then its
`changed_at` is at least the pass start, which is later than the cursor this
pass will persist. The next pass catches it.

The re-fetch is bounded by the skew constant (60 s of churn), not by the
corpus.

### Why the cursor comes from the server's clock

The comparison happens server-side, and a phone's clock is not trustworthy: it
can be hours out, or jump when the user travels. The pass reads the `Date`
header of its **first** response and persists that minus 60 s. Later pages
happen after mutations this pass will not see, so anchoring to a later reading
would skip them.

The cursor is written only after the whole pass commits. A crash mid-pass
replays from the old cursor, which is safe because upserts are idempotent, and
is the only choice that cannot lose an entry.

### Request-builder traps

| Parameter | Trap |
| --- | --- |
| `limit` | `limit=0` means **unlimited** on 2.2.x — a full-corpus dump into a phone. 2.3.x caps at 1000 with a hard 400. `EntriesQuery` clamps to `1..=1000` and cannot express zero. |
| `direction` | Lowercase only. `"ASC"` is a 400. |
| `order` | Whitelisted server-side; an enum here so an invalid value cannot be constructed. |
| `changed_after` | Epoch **seconds**, and `0` means "unset", not "the epoch". An unset cursor omits the parameter rather than sending `0`, which would mean *everything since 1970*. |
| `status` | Repeat the key for multiple values; they are OR'ed. Do not send `removed` — it is gone in 2.3.x and 400s. |
| `starred` | Exactly `"true"`/`"false"`. The list endpoints ignore garbage; `/v1/entries/ids` 400s on it. |

Covered by `api::client::tests` and `tests/sync_pull.rs`.

---

## 2. Deletions (§11, question 1, second half)

**Deletions are invisible to any cursor.** There are two regimes and the
cutover is recent:

- **Through 2.2.x**, deletion was a *soft* delete: the retention job flipped
  `status` to a third value, `removed`, which the API exposed.
- **From 2.3.0**, `removed` was deleted from the model, a migration
  hard-deleted every such row, and both retention and flush-history now issue
  real `DELETE FROM entries`. Tombstones exist only in a server-internal table
  that the API never exposes.

So on any server ≥ 2.3.0 an entry simply vanishes, and `changed_after` cannot
report it. Deletion detection is a **separate mechanism**, and conflating the
two is the mistake to avoid.

Vuo's approach, cheapest first:

1. **Feed removal** — a feed absent from `GET /v1/feeds` was unsubscribed. Its
   entries go with it. Costs no extra request.
2. **`GET /v1/feeds/counters`** — one small request per sync. Compare
   `reads[f] + unreads[f]` against the local per-feed count. Feeds that agree
   are skipped entirely, which keeps the common case at near-zero bandwidth.
3. **`GET /v1/entries/ids`** — only when something diverges, or daily. Present
   from **2.3.2 only**, so it is feature-gated.

### The reconcile's correctness guard

`/v1/entries/ids` pages by `offset` over an `id DESC` ordering, which is *not*
stable under concurrent writes: a concurrent insert shifts the window and an id
can fall through the crack. Acting on that directly deletes a live entry.

So the accumulated id count is checked against the `total` the first page
declared, and **a mismatch aborts the reconcile** rather than deleting
anything. A reconcile that does not run leaves a slightly stale cache; one that
runs on a torn listing destroys the user's data.

Covered by `tests/sync_pull.rs::a_torn_id_listing_aborts_the_reconcile_instead_of_deleting`.

### The second blind spot

A feed refresh that rewrites an entry's title or body does **not** bump
`changed_at`. A `changed_after` pull therefore never re-fetches an edited body
for an entry already held. Vuo refreshes content on user open rather than
pretending the cursor covers it.

---

## 3. Batch mutation semantics (§11, question 2)

**Question.** Confirm the exact endpoints and payload shapes for bulk status
changes and bookmark toggling, and whether they are idempotent under replay.

**Answer.** Exactly one endpoint is safe for an outbox.

| Endpoint | Body | Idempotent? |
| --- | --- | --- |
| `PUT /v1/entries` | `{entry_ids, status?, starred?}` | **Yes — absolute set. Use this for everything.** |
| `PUT /v1/entries/{id}/star` | none | **No.** The SQL is `SET starred = NOT starred`. |
| `PUT /v1/entries/{id}/bookmark` | none | **No** — the same handler as `/star`. |
| `PUT /v1/feeds/{id}/mark-all-as-read` | none | Monotonic, but see below. |
| `PUT /v1/users/{id}/mark-all-as-read` | none | Yes; 403 unless it is your own id. |
| `POST /v1/entries/{id}/save` | none | **No** — duplicates third-party bookmarks. Out of scope (§3). |

### The toggle problem, and the shape it forces

`/star` being `SET starred = NOT starred` is the single most important fact in
this document. It means an outbox modelled as an **operation log** — "user
starred 7", replayed later — flips the value back on any retry. There is no way
to make a log of toggles safe.

The escape is that `PUT /v1/entries` takes `starred` as an absolute boolean.
To use it, the queue has to hold **desired states**, not operations. So Vuo's
outbox is keyed `(entry_id, field)` and queueing *upserts*: star, unstar, star
again while offline collapses to one row holding `true`.

That single decision buys idempotent replay, safety after an ambiguous
timeout, and a queue that cannot grow past one row per entry per field however
long the device stays offline. No request ids, no dedup tokens, no server-side
idempotency keys.

### Mark-all-as-read is not queueable

The feed and category variants apply a server-side `published_at < now()`
cut-off captured **at request time**. Queued offline at noon and replayed at
six, one also marks everything that arrived in between — entries the user never
saw. So an offline mark-all is expanded locally into the concrete set of entry
ids currently unread, which preserves the actual intent.

### Other behaviours worth knowing

- Unknown entry ids are **silently ignored** (`WHERE user_id=$2 AND id=ANY($3)`,
  no rows-affected check). A 204 does not mean every id existed.
- An empty `entry_ids` is a hard 400. Vuo refuses locally so the outbox's retry
  classifier never sees a self-inflicted client error.
- Every mutation bumps `changed_at`, so **Vuo sees its own writes come back**
  through the next `changed_after` pull. The per-field conflict rule makes that
  echo a no-op.

Covered by `tests/outbox_reconciliation.rs`, including a test asserting the
toggle endpoints are never called at all.

---

## 4. Media proxy (§11, question 6)

**Question.** Confirm how the server's media proxy is addressed and
authenticated, and whether it covers every media type Vuo would otherwise fetch
directly.

**Answer.** Vuo **cannot construct proxy URLs**, and un-proxied media is the
normal case rather than the exception.

- Proxy URLs are `GET /proxy/{base64 digest}/{base64 url}`, where the digest is
  `HMAC-SHA256(media_url)` keyed by `MEDIA_PROXY_PRIVATE_KEY` — a server-only
  secret that is **randomly regenerated at every startup when unset**. No API
  endpoint will sign a URL on request. A client can only consume what the
  server already rewrote.
- The good news: the JSON API *does* rewrite. Every read path Vuo uses runs
  `entry.content` and enclosure URLs through the proxy rewriter, producing
  absolute URLs rooted at `BASE_URL`.
- The catch: `MEDIA_PROXY_MODE` defaults to **`http-only`**, which proxies
  plain-`http` images only — and essentially every feed image is `https`. **On
  a stock Miniflux, most images arrive as raw third-party URLs.**
- `/proxy/` is a public, unauthenticated route. Send no token to it.
- `PUT /v1/entries/{id}` returns the entry **without** proxy rewriting. Never
  adopt content from that response.

### What Vuo does

Because un-proxied media is common, silently dropping it would blank most
articles, and silently fetching it is the exact IP/tracking leak the feature
exists to prevent. So there are three states, defaulting to the middle:

| Setting | Behaviour |
| --- | --- |
| Strict | Never fetch. Placeholder only. |
| **Ask (default)** | Placeholder with a tap-to-load affordance; consent remembered **per origin**. |
| Allow | Fetch directly, having been told what that means. |

Classification is by parsed **origin**, never by looking for `/proxy/` in the
path — any third-party host can serve a `/proxy/` path, and matching the string
would trust it. `MEDIA_PROXY_CUSTOM_URL` deployments are handled by a
user-supplied extra trusted origin rather than by guessing.

Vuo's settings screen names the actual fix, which is a server setting:
`MEDIA_PROXY_MODE=all`.

---

## 5. Icons (§11, question 5)

`GET /v1/feeds/{id}/icon` returns JSON, **not** image bytes:

```json
{ "id": 42, "mime_type": "image/png", "data": "image/png;base64,iVBORw0..." }
```

Note the `data` field is *not* a `data:` URI despite looking like one — there
is no `data:` scheme prefix. Icons never go through the media proxy and are
fetched **with** the API token.

Vuo stores them as blobs in SQLite keyed by the server's integer icon id
(§9.4: never derive a filesystem path from server data), and fetches them a few
per sync rather than all at once, which is the answer to §11's thundering-herd
concern.

They are validated by content, not by claimed type: magic-byte sniffing, and
dimension caps read from the header *before* any decoder allocates. A
65535×65535 PNG is a few hundred bytes on the wire and about 17 GB decoded.
SVG is refused outright — exposing an XML parser to feed operators for a
32-pixel favicon is not a trade worth making.

---

## Version gating

`GET /v1/version` is called once per sync. What depends on it:

| Version | Consequence |
| --- | --- |
| ≥ 2.3.2 | `/v1/entries/ids` exists; deletion reconcile is cheap. |
| ≥ 2.3.0 | Hard deletes; `limit > 1000` is a 400; `removed` status is gone. |
| < 2.3.0 | Soft deletes via `removed`; `limit=0` means unlimited. |

An unparseable version (self-built servers report `dev` or a commit hash)
assumes the **oldest** supported behaviour. Guessing high would mean calling
endpoints that 404.
