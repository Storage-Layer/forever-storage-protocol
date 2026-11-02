//!Failure domain representation and concentration constraints.

/// Strongly-typed identifier for an independent failure domain (rack, region, AS, provider).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FailureDomainId(pub usize);

/// Metadata and constraints for a failure domain.
#[derive(Debug, Clone, PartialEq)]
pub struct FailureDomain {
    /// Unique domain identifier.
    pub id: FailureDomainId,
    /// Maximum number of distinct shards of a single object allowed in this domain.
    pub max_shards_allowed: usize,
}

impl FailureDomain {
    /// Creates a new failure domain with the given concentration cap.
    pub const fn new(id: FailureDomainId, max_shards_allowed: usize) -> Self {
        Self {
            id,
            max_shards_allowed,
        }
    }
}
