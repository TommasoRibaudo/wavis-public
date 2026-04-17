//! Shared types and protocol definitions used by both the backend and clients.
//!
//! **Owns:** the canonical signaling protocol types (`SignalingMessage` enum
//! and all payload structs), serialization format (JSON with `tag = "type"`),
//! and protocol-level validation helpers. These types define the wire format
//! between the Wavis backend and any client (GUI, CLI, test harness).
//!
//! **Does not own:** transport (WebSocket framing, HTTP), business logic
//! (room policy, auth), or any server/client-specific implementation. This
//! crate is intentionally framework-free so it can be used in backend,
//! Tauri, and test contexts without pulling in Axum or UI dependencies.
//!
//! **Key invariants:**
//! - Wire format changes to `SignalingMessage` are breaking changes — both
//!   backend and all clients must agree on the schema.
//! - New message variants must be added to the enum; ad-hoc JSON is not
//!   allowed on the signaling channel.
//!
//! **Layering:** leaf crate with no internal Wavis dependencies. Depended
//! on by `wavis-backend` and client crates.

pub mod signaling;
