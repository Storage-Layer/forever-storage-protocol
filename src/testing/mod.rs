//! Testing and Fault Injection Infrastructure (`src/testing/`).

pub mod fault_injection;
pub mod formal_model;
pub mod formal_model_v2;
pub mod soak_harness;

pub use fault_injection::{FaultInjector, FaultRule};
pub use formal_model::{BoundedModelChecker, GlobalSystemState, ModelCheckerReport};
pub use formal_model_v2::{
    BoundedModelCheckerV2, GlobalSystemStateV2, ModelCheckerV2Report, ModelCoordinatorState,
    ModelLogEntry, ModelMessage, ModelNodeRole, ModelPermit, ModelStorageNodeState,
    QuorumNodeId as ModelQuorumNodeId,
};
pub use soak_harness::{SoakHarnessConfig, SoakMetrics, SoakTestHarness};
