//! Forever Storage Protocol (FSP) — Phase 0 Research Simulation Foundation.
//!
//! This library provides a deterministic, discrete-event simulation model for
//! evaluating erasure-coded and replicated storage preservation strategies under
//! transient node churn, permanent device failures, correlated failure-domain
//! disasters, and constrained repair bandwidth.

pub mod cli;
pub mod cluster;
pub mod coding;
pub mod config;
pub mod coordinator;
pub mod detection;
pub mod model;
pub mod network;
pub mod node;
pub mod observability;
pub mod placement;
pub mod recovery;
pub mod reliability;
pub mod security;
pub mod simulation;
pub mod storage;
pub mod strategies;
pub mod testing;
