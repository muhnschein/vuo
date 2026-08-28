//! The Miniflux REST client, and the boundary where foreign bytes become
//! domain values.
//!
//! The layering is deliberate and is what §9.2 asks for:
//!
//! ```text
//!   socket ──▶ transport ──▶ wire ──▶ convert ──▶ model
//!              bounded,      permissive  per-item   strict,
//!              redirect-     serde       validation trusted
//!              policed
//! ```
//!
//! Each arrow narrows what the next layer has to worry about. By the time a
//! value is a [`crate::model`] type it has been size-bounded, deserialised
//! without assumptions, and validated individually — so a single absurd entry
//! costs one row rather than the whole sync.

pub mod client;
pub mod convert;
pub mod icon;
pub mod transport;
pub mod wire;

pub use client::{EntriesQuery, EntryMutation, EntryOrder, MinifluxClient, SortDirection};
pub use icon::{decode_icon, IconLimits};
pub use transport::{Transport, TransportConfig};
