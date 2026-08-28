//! `vuo-core` — the Qt-free heart of Vuo, a SailfishOS client for Miniflux.
//!
//! Everything in this crate builds and tests on a plain host toolchain: no Qt,
//! no SailfishOS SDK, no phone, no server. That is the point. §5 of the scope:
//! *if a bug can only be reproduced on a phone, the layering is wrong.* The
//! sync engine is where this project's real invariants live — cursors,
//! conflict resolution, an outbox that must not lose or double-apply
//! mutations — and they are all testable on a laptop.
//!
//! # Layout
//!
//! - [`api`] — the Miniflux REST client, and the boundary where foreign
//!   bytes become validated domain values.
//! - [`db`] — the local SQLite mirror, and the outbox that makes the write
//!   path offline-tolerant.
//! - [`sync`] — the incremental pull and the idempotent outbox replay.
//! - [`model`] — the strict domain types.
//! - [`content`] — the HTML → render-block transform (§9.2, §9.3).
//! - [`redact`] — keeping the API token and credentials out of error paths.
//! - [`error`] — the error taxonomy, and the retry classification the outbox
//!   depends on.
//!
//! # Safety posture
//!
//! Vuo is structurally a program that renders bytes written by strangers.
//! Entry content, titles, author names, feed and category names, link targets
//! and image URLs all originate at arbitrary websites and reach this crate
//! verbatim. A feed operator who wants to attack Vuo does not need to
//! compromise the user's server — they only need the user to subscribe.
//!
//! So: `#![forbid(unsafe_code)]` here, `unwrap`/`expect`/`panic`/indexing
//! denied by lint, every loop bounded, and allowlists rather than blocklists
//! wherever foreign input is shaped into something the UI will draw.

#![forbid(unsafe_code)]
// The `unwrap`/`expect`/`panic`/indexing denials exist because foreign input
// reaches production paths and unwinding out of Rust into Qt's C++ frames is
// undefined behaviour (§9.5). Test code has neither property: a test that
// cannot unwrap is a test that spends more lines on error handling than on the
// thing it is checking, and a panicking assertion is the entire point.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::integer_division,
    )
)]

pub mod api;
pub mod content;
pub mod db;
pub mod error;
pub mod model;
pub mod redact;
pub mod sync;

pub use error::{Error, Result};
