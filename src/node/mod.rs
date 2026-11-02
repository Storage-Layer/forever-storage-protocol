//! Phase 4 Independent Storage Node Daemon Subsystem (`node`).
//!
//! Provides persistent node identity and failure-domain descriptors (`identity`), node health and
//! telemetry tracking (`health`), and the local shard container store, transaction journal, and
//! startup crash recovery engine (`daemon`).

pub mod daemon;
pub mod health;
pub mod idempotency;
pub mod identity;
