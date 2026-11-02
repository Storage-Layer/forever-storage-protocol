//! Phase 3 Persistent Storage Engine (`storage`).
//!
//! Provides byte-accurate erasure-coded object ingestion, cryptographic SHA-256 verification,
//! canonical manifests, crash-safe 6-state atomic transactions, and sandboxed filesystem storage.

pub mod backend;
pub mod container;
pub mod crash_point;
pub mod engine;
pub mod filesystem;
pub mod integrity;
pub mod manifest;
pub mod object;
pub mod transaction;

pub use backend::DurabilityLevel;
pub use crash_point::{
    arm_crash_point, disarm_crash_point, get_crash_point_hits, is_crash_point_armed,
    reset_crash_point_hits, trigger_crash_point, CrashAction, CrashPoint,
};
