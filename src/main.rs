//! Recommended V2 Hub entrypoint.
//!
//! The historical single-process V1 gateway remains available as the
//! `v1_gateway` binary for regression/reference use. The enrolled desktop runs
//! the separate `v2_agent` binary.

include!("bin/v2_hub.rs");
