//! `vuo-core`: Qt-free heart of Vuo, `SailfishOS` client for Miniflux.
//!
//! Builds and tests on plain host toolchain: no Qt, no `SailfishOS` SDK, no
//! phone, no server. That's the point. Scope §5: *if a bug can only be
//! reproduced on a phone, the layering is wrong.* Real invariants live in sync
//! engine (cursors, conflict resolution, outbox that must not lose or
//! double-apply mutations), all testable on laptop.
//!
//! # Layout
//!
//! - [`api`]: Miniflux REST client; boundary where foreign bytes become
//!   validated domain values.
//! - [`db`]: local SQLite mirror, and outbox making write path offline-tolerant.
//! - [`sync`]: incremental pull, idempotent outbox replay.
//! - [`model`]: strict domain types.
//! - [`content`]: HTML → render-block transform (§9.2, §9.3).
//! - [`notify`]: what new-articles notification says; feed titles fit for
//!   surface Vuo doesn't control.
//! - [`redact`]: API token and credentials out of error paths.
//! - [`error`]: error taxonomy, retry classification outbox depends on.
//!
//! # Safety posture
//!
//! Vuo renders bytes written by strangers. Content, titles, authors, feed and
//! category names, link targets, image URLs: from arbitrary websites, verbatim.
//! Attacker needn't compromise user's server, only get subscribed.
//!
//! So: `#![forbid(unsafe_code)]`, `unwrap`/`expect`/`panic`/indexing denied by
//! lint, every loop bounded, allowlists not blocklists wherever foreign input
//! becomes something UI draws.

#![forbid(unsafe_code)]
// More of same, for what phone runs and tests don't. Release profile: no
// overflow checks, so `+` on server-chosen length wraps silently; slicing off
// char boundary panics; `let _ =` on `Result` drops only diagnosis. Here, not
// workspace lints table: table can't exclude tests, and test indexing what it
// just built is fine. Integration tests, examples, build scripts = other
// crates, or this one under `cfg(test)`.
#![cfg_attr(
    not(test),
    deny(
        clippy::string_slice,
        clippy::arithmetic_side_effects,
        clippy::let_underscore_must_use,
        clippy::panic_in_result_fn,
        clippy::unwrap_in_result,
        clippy::get_unwrap
    )
)]
// `unwrap`/`expect`/`panic`/indexing denied because foreign input reaches
// production and unwinding into Qt's C++ frames = UB (§9.5). Tests have
// neither: no-unwrap test spends more lines on error handling than check;
// panicking assert is the point.
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
pub mod net;
pub mod notify;
pub mod redact;
pub mod search;
pub mod sync;

pub use error::{Error, Result};

#[cfg(test)]
mod workspace_guards {
    //! Guards over whole workspace's source, not just this crate.

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

    /// Source with trailing `#[cfg(test)]` module removed.
    ///
    /// Truncates at FIRST occurrence, crude on purpose: convention = test
    /// modules last. Production code after test module reads as tests →
    /// possible false positive (safe direction); assertion message says move
    /// test module to end.
    fn production_source(path: &Path) -> String {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        text.split("#[cfg(test)]")
            .next()
            .unwrap_or_default()
            .to_owned()
    }

    /// `pub fn` nothing outside own definition calls.
    ///
    /// # Why test, not lint
    ///
    /// `dead_code` doesn't fire on `pub` items (assumes external consumer). Nothing here published:
    /// assumption always wrong. Shape this catches:
    ///
    /// | function | what it hid |
    /// | --- | --- |
    /// | `replay::should_retry` | `flush` re-implemented the retry rule inline; nine tests exercised a parallel copy |
    /// | `replay::would_discard` | likewise for the discard rule -- the historical data-loss bug could have been restored with every test green |
    /// | `ServerVersion::max_entry_limit` | both branches returned 1000; the cap that reached the wire was elsewhere |
    /// | `Settings::media_policy_for` | the Images setting was rendered, persisted, read back, and reached nothing (§9.3) |
    ///
    /// Test asserts on helper as though it were rule; passes while production does something else.
    /// Helper not bug; test pointed at it is. Orphan catches whole shape.
    ///
    /// Fix: production path calls helper, or delete helper and point test at what production does.
    #[test]
    fn every_pub_fn_has_a_caller_outside_its_own_definition() {
        // Exempt, each with stated reason. Only escape hatch, nuisance on
        // purpose: adding name means writing down caller (question guard
        // forces). Entry without reason = bug in list.
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

        // Every file's production half, concatenated: haystack a call must
        // appear in.
        let all: String = files.iter().map(|f| production_source(f)).collect();

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
                // Definition = one occurrence. Second anywhere in production
                // source = caller (or re-export, or trait impl: all reachable).
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
