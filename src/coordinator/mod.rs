//! Distributed Coordinator subsystem (`src/coordinator/`):
//! - Failure-domain-aware distributed shard placement (`placement.rs`)
//! - Distributed PUT/GET transaction coordinator & crash recovery (`transactions.rs`)
//! - Distributed Adaptive Repair v2 scheduler & background data scrubber (`scheduler.rs`)

pub mod failover;
pub mod fencing;
pub mod incarnation;
pub mod leadership;
pub mod placement;
pub mod repair_journal;
pub mod replication;
pub mod scheduler;
pub mod transactions;
