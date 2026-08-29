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

#[cfg(test)]
mod workspace_guards {
    //! Guards over the whole workspace's source, not just this crate's.
    //!
    //! These exist because the sweep that produced them found the same shape
    //! four separate times, in four separate places.

    use std::path::{Path, PathBuf};

    /// Every `.rs` file under a directory.
    fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    /// Source with any trailing `#[cfg(test)]` module removed.
    ///
    /// Truncating at the FIRST occurrence is crude, and deliberately so: this
    /// crate's convention is that test modules come last, so everything after
    /// the first `#[cfg(test)]` is test code. If a file ever puts production
    /// code after a test module, this guard reads that code as tests and could
    /// report a false positive -- which is the safe direction to be wrong in,
    /// and the assertion message says to move the test module to the end.
    fn production_source(path: &Path) -> String {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        text.split("#[cfg(test)]")
            .next()
            .unwrap_or_default()
            .to_owned()
    }

    /// A `pub fn` that nothing outside its own definition ever calls.
    ///
    /// # Why this is a test and not a lint
    ///
    /// `dead_code` does not fire on a `pub` item: it is public, so the compiler
    /// assumes an external consumer. Nothing in this workspace is published, so
    /// that assumption is always wrong here, and it hid the same defect shape
    /// four times over:
    ///
    /// | function | what it hid |
    /// | --- | --- |
    /// | `replay::should_retry` | `flush` re-implemented the retry rule inline; nine tests exercised a parallel copy |
    /// | `replay::would_discard` | likewise for the discard rule -- the historical data-loss bug could have been restored with every test green |
    /// | `ServerVersion::max_entry_limit` | both branches returned 1000; the cap that reached the wire was elsewhere |
    /// | `Settings::media_policy_for` | the Images setting was rendered, persisted, read back, and reached nothing (§9.3) |
    ///
    /// In each case a test asserted on the helper as though it were the rule,
    /// so the test passed while the production path did something else. The
    /// helper is not the bug; the bug is a test pointed at it. Catching the
    /// orphan catches the whole shape.
    ///
    /// Fixing a hit means one of two things, and both are fine: make the
    /// production path call the helper, or delete the helper and point its
    /// test at whatever the production path actually does.
    #[test]
    fn every_pub_fn_has_a_caller_outside_its_own_definition() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root")
            .join("crates");

        let mut files = Vec::new();
        for crate_dir in ["vuo-core", "vuo-shim", "harbour-vuo"] {
            collect_rs(&workspace.join(crate_dir).join("src"), &mut files);
        }
        assert!(
            files.len() > 15,
            "found only {} sources; the scan is not reaching the workspace",
            files.len()
        );

        // Every file's production half, concatenated: the haystack a call has
        // to appear in somewhere.
        let all: String = files.iter().map(|f| production_source(f)).collect();

        // Exempt, each for a stated reason. This list is the guard's only
        // escape hatch, and it is deliberately a nuisance: adding a name means
        // writing down who the caller is, which is the question the guard
        // exists to force. An entry with no reason is a bug in this list.
        const EXEMPT: &[(&str, &str)] = &[
            (
                "vuo_register_qml_types",
                "C ABI entry point; a Sailfish main() written in C++ calls it",
            ),
            (
                "register_qml_types",
                "called by main(), which is the process entry point",
            ),
            (
                "conn_mut",
                "tests/concurrent_writers.rs needs &mut Connection for \
                 transaction_with_behavior; an integration test is a separate \
                 crate, so this cannot be #[cfg(test)]",
            ),
        ];

        let mut orphans = Vec::new();
        for file in &files {
            let source = production_source(file);
            for line in source.lines() {
                let line = line.trim_start();
                let Some(rest) = line
                    .strip_prefix("pub fn ")
                    .or_else(|| line.strip_prefix("pub(crate) fn "))
                    .or_else(|| line.strip_prefix("pub async fn "))
                    .or_else(|| line.strip_prefix("pub(crate) async fn "))
                else {
                    continue;
                };
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if name.is_empty() || name == "main" || EXEMPT.iter().any(|(n, _)| *n == name) {
                    continue;
                }
                // The definition itself is one occurrence. A second anywhere in
                // the workspace's production source is a caller (or a re-export,
                // or a trait impl -- all of them mean it is reachable).
                if all.matches(name.as_str()).count() < 2 {
                    orphans.push(format!("{}: {name}", file.display()));
                }
            }
        }

        assert!(
            orphans.is_empty(),
            "these `pub fn`s have no caller in the workspace's production code:\n  {}\n\n\
             `dead_code` cannot see this -- they are `pub`, so the compiler assumes an \
             external consumer, and nothing here is published. Either make the production \
             path call it, or delete it AND the test that asserts on it: a test pointed at \
             a function the app does not use passes while the app does something else. \
             If one really is reachable from outside this scan -- C, QML, or an \
             integration test, which is a separate crate -- add it to EXEMPT with \
             a note naming the caller.\n\
             (A production item declared AFTER a `#[cfg(test)]` module in its file also \
             lands here; move the test module to the end of the file.)",
            orphans.join("\n  ")
        );
    }
}
