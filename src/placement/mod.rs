//! Physical Failure-Domain Placement Subsystem (`src/placement/`).
//!
//! Provides mathematical durability verification across host, rack, zone, and region
//! failure domains, enforcing single-host and multi-host recovery constraints.

pub mod failure_domains;

pub use failure_domains::{
    plan_physical_placement, validate_placement_durability, FailureDomainConstraints,
    PhysicalFailureDomain, PlacementCandidate, PlacementValidationReport,
};
