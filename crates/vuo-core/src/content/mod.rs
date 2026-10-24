//! Article HTML → something Silica can draw.
//!
//! Entry point: [`transform()`](transform::transform). [`transform`](mod@transform) says why
//! tokenizer not DOM walk; [`block`] why output list is flat.

pub mod block;
pub mod transform;
pub mod url;

pub use block::{
    BlockKind, Document, MediaFetch, RenderBlock, Span, SpanStyle, TableCell, Truncation,
};
pub use transform::{transform, Limits, TransformContext};
pub use url::{MediaDecision, MediaPolicy, MediaUrl, UnproxiedMedia};
