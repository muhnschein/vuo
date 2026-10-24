//! Miniflux REST client; boundary where foreign bytes become domain values.
//!
//! Layering per §9.2:
//!
//! ```text
//!   socket ──▶ transport ──▶ wire ──▶ convert ──▶ model
//!              bounded,      permissive  per-item   strict,
//!              redirect-     serde       validation trusted
//!              policed
//! ```
//!
//! Each arrow narrows next layer's worries. A [`crate::model`] value is
//! size-bounded, deserialised without assumptions, validated individually: one
//! absurd entry costs one row, not the sync.

pub mod client;
pub mod convert;
pub mod icon;
pub mod transport;
pub mod wire;

pub use client::{EntriesQuery, EntryMutation, EntryOrder, MinifluxClient, SortDirection};
pub use icon::{decode_icon, IconLimits};
pub use transport::{Transport, TransportConfig};
