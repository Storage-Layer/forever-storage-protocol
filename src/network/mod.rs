//! Phase 4 Authenticated Network Transport & RPC Protocol (`network`).
//!
//! Implements framed binary RPC over TLS 1.3 with mutual certificate authentication (`mTLS`),
//! explicit protocol versioning, bounded request/response sizes, timeouts, fault injection,
//! and role-based authorization for all 7 node operations (`PutShard`, `GetShard`, `DeleteShard`,
//! `VerifyShard`, `ListShards`, `Heartbeat`, `GetNodeStatus`).

pub mod client;
pub mod connection_pool;
pub mod protocol;
pub mod retry;
pub mod server;
pub mod transport;
