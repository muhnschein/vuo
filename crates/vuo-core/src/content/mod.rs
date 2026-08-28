//! Turning article HTML into something Silica can draw.
//!
//! The entry point is [`transform`]. See [`transform`](transform::transform)
//! for why this is a tokenizer rather than a DOM walk, and [`block`] for why
//! the output list is flat.

pub mod block;
pub mod transform;
pub mod url;

pub use block::{BlockKind, Document, RenderBlock, Span, SpanStyle, TableCell, Truncation};
pub use transform::{transform, Limits, TransformContext};
pub use url::{MediaDecision, MediaPolicy, MediaUrl, UnproxiedMedia};
