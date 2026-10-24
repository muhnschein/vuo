//! Fuzz JSON response deserialiser and validation.
//!
//! Two properties (§9.2):
//!
//! 1. Permissive wire types accept *any* JSON object: server versions skew,
//!    unknown fields must be uninteresting, not fatal.
//! 2. Validation into strict domain types rejects cleanly, per item. Panic
//!    would kill sync over one absurd entry.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vuo_core::api::{convert, wire};

fuzz_target!(|data: &[u8]| {
    // Whole response: envelope plus page of entries.
    if let Ok(response) = serde_json::from_slice::<wire::EntriesResponse>(data) {
        let page = convert::entries(response.entries);
        // Rejection always item-local, never sync abort.
        for error in &page.rejected {
            assert!(error.is_item_local(), "a bad entry escalated to a sync failure: {error}");
        }
        // Nothing both usable and deleted.
        for id in &page.removed {
            assert!(
                !page.valid.iter().any(|e| e.id == *id),
                "an entry was reported as both valid and removed"
            );
        }
        for entry in &page.valid {
            // Domain type invariants hold: no non-http(s) URL survived.
            for url in [entry.url.as_ref(), entry.comments_url.as_ref()].into_iter().flatten() {
                let scheme = url.as_url().scheme();
                assert!(scheme == "http" || scheme == "https", "a {scheme}: URL survived");
            }
            assert!(entry.reading_time >= 0, "a negative reading time survived");
        }
    }

    // Single objects: fuzzer needn't go through envelope.
    if let Ok(entry) = serde_json::from_slice::<wire::Entry>(data) {
        let _ = convert::entry(entry);
    }
    if let Ok(feed) = serde_json::from_slice::<wire::Feed>(data) {
        let _ = convert::feed(feed);
    }
    if let Ok(icon) = serde_json::from_slice::<wire::Icon>(data) {
        // Icons = most attacker-controlled bytes: fetched automatically, never
        // chosen by user, fed to image decoder on phone.
        let _ = vuo_core::api::decode_icon(&icon, vuo_core::api::IconLimits::default());
    }
});
