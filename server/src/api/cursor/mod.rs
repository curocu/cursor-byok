//! Wires the Cursor-facing API routes.

pub mod bidi;
mod handlers;
pub mod inference;
pub mod proxy;
mod run_sse;

pub use handlers::router;
