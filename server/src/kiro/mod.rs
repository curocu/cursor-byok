//! Implements the Kiro client integration.
//!
//! Kiro is reached through `codewhisperer.config` endpoint overrides written
//! into Kiro's own settings file ([`host_settings`]), so no certificate
//! authority and no proxy interception are involved. Kiro then sends its
//! inference and control-plane traffic to two loopback listeners:
//!
//! ```text
//! Kiro ──► runtime listener   /generateAssistantResponse  ──► provider router
//!      └─► control listener   /ListAvailableModels        ──► model library
//! ```
//!
//! [`wire`] encodes the AWS event-stream frames the runtime listener answers
//! with.

mod catalog;
mod host_settings;
mod wire;

pub use catalog::build as build_catalog;
pub use host_settings::{
    apply as apply_endpoints, clear as clear_endpoints, matches as endpoints_match,
};
pub use wire::CONTENT_TYPE;
