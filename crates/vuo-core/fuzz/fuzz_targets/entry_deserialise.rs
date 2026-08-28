//! Fuzzing the JSON response deserialiser and its validation.
//!
//! Two properties, both from §9.2:
//!
//! 1. The permissive wire types must accept *any* JSON object — server
//!    versions skew, fields get added and deprecated, and a field this client
//!    has never heard of must be uninteresting rather than fatal.
//! 2. Validation into strict domain types must reject cleanly, per item. A
//!    panic here would take down a sync over one absurd entry, which is
//!    exactly the failure §9.2 forbids.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vuo_core::api::{convert, wire};

fuzz_target!(|data: &[u8]| {
    // Whole-response shape: the envelope plus a page of entries.
    if let Ok(response) = serde_json::from_slice::<wire::EntriesResponse>(data) {
        let (valid, rejected) = convert::entries(response.entries);
        // A rejection must always be item-local, never something that would
        // abort the whole sync.
        for error in &rejected {
            assert!(error.is_item_local(), "a bad entry escalated to a sync failure: {error}");
        }
        for entry in &valid {
            // Anything that reached the domain type must satisfy its
            // invariants: no non-http(s) URL can have survived.
            for url in [entry.url.as_ref(), entry.comments_url.as_ref()].into_iter().flatten() {
                let scheme = url.as_url().scheme();
                assert!(scheme == "http" || scheme == "https", "a {scheme}: URL survived");
            }
            assert!(entry.reading_time >= 0, "a negative reading time survived");
        }
    }

    // Single objects, so the fuzzer is not forced through the envelope to
    // reach the interesting code.
    if let Ok(entry) = serde_json::from_slice::<wire::Entry>(data) {
        let _ = convert::entry(entry);
    }
    if let Ok(feed) = serde_json::from_slice::<wire::Feed>(data) {
        let _ = convert::feed(feed);
    }
    if let Ok(icon) = serde_json::from_slice::<wire::Icon>(data) {
        // Icons are the most attacker-controlled bytes in the app: fetched
        // automatically, never chosen by the user, and handed to an image
        // decoder on the phone.
        let _ = vuo_core::api::decode_icon(&icon, vuo_core::api::IconLimits::default());
    }
});
