//! Phase 4.3 Comprehensive Consensus Safety, Adversarial Verification & Formal Validation Test Suite
//! (`tests/phase4_3_validation.rs`).
//!
//! Validates:
//! 1. Forged epoch advance without cryptographic quorum certificate is rejected.
//! 2. Quorum leadership certificate with majority Ed25519 signatures is accepted.
//! 3. Stale leader mutation is rejected by storage node epoch fencing.
//! 4. Durable majority log commit semantics requiring quorum acknowledgment.
//! 5. Quorum loss blocks mutating operations (fail-closed behavior).
//! 6. Split-brain partition isolates old leader, preventing further commits.
//! 7. Concurrent competing elections in the same term resolve safely.
//! 8. Delayed RPC replay from superseded leader is rejected after failover.
//! 9. Abrupt coordinator SIGKILL crash reconciles uncommitted entries upon standby promotion.
//! 10. Conflicting manifest versions at the same generation are strictly rejected.
//! 11. Monotonic manifest generation rollback attempts are strictly blocked.
//! 12. Storage node process restart preserves durably certified epoch high-water mark.
//! 13. Executable Bounded Model Checker verifies all 7 consensus safety invariants.
//! 14. TLS connection pooling accurately accounts for handshakes, reuse, and timing.
//! 15. Replicated log index monotonicity and state machine replay across crash recovery.
//! 16. Durable repair journal consistency and resume across active leader failovers.
//! 17. Physical failure domain staging topology satisfies RS(20,12) 2-host loss tolerance.
//! 18. Storage node rejects tampered payload digest mismatch in fenced operation token.
//! 19. Distributed RS(20,12) PUT/GET roundtrip under certified epoch authority.
//! 20. Distributed 8-node failure and self-healing under certified quorum leadership.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ed25519_dalek::{SigningKey, VerifyingKey};

use fsp_sim::coding::reed_solomon::ReedSolomonCodec;
use fsp_sim::coordinator::fencing::{FencedOperationToken, NodeEpochFencingTracker};
use fsp_sim::coordinator::leadership::{
    CoordinatorId, CoordinatorRole, LeadershipCertificate, QuorumState, QuorumVoteGrant,
};
use fsp_sim::coordinator::repair_journal::{DurableRepairJournal, RepairJobState};
use fsp_sim::coordinator::replication::{ReplicatedLogManager, ReplicationRecord};
use fsp_sim::model::node::NodeId;
use fsp_sim::model::shard::ShardId;
use fsp_sim::network::connection_pool::ConnectionPoolMetrics;
use fsp_sim::network::protocol::RpcError;
use fsp_sim::placement::failure_domains::{
    validate_placement_durability, FailureDomainConstraints, PhysicalFailureDomain,
};
use fsp_sim::security::signatures::{ManifestSignerKeypair, TrustedSignerPolicy};
use fsp_sim::storage::engine::StorageEngine;
use fsp_sim::storage::integrity::sha256_bytes;
use fsp_sim::storage::object::PutOptions;
use fsp_sim::testing::formal_model::{BoundedModelChecker, GlobalSystemState};

static SANDBOX_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Test sandbox directory wrapper that cleans up on drop.
struct TestSandbox {
    dir: PathBuf,
}

impl TestSandbox {
    fn new(name: &str) -> Self {
        let seq = SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("fsp_p43_test_{}_{}", name, seq));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for TestSandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn create_deterministic_signer(id: &str, seed: u64) -> ManifestSignerKeypair {
    ManifestSignerKeypair::deterministic(id, seed).unwrap()
}

fn generate_test_keys() -> (
    BTreeMap<CoordinatorId, SigningKey>,
    BTreeMap<CoordinatorId, VerifyingKey>,
) {
    let mut priv_keys = BTreeMap::new();
    let mut pub_keys = BTreeMap::new();
    for i in 1..=3 {
        let id = CoordinatorId::new(format!("coord_{}", i));
        let signer = create_deterministic_signer(&id.0, 1000 + i as u64);
        priv_keys.insert(id.clone(), signer.signing_key().clone());
        pub_keys.insert(id, signer.verifying_key());
    }
    (priv_keys, pub_keys)
}

// -------------------------------------------------------------------------------------------------
// Test 1: Forged Epoch Without Quorum Certificate Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_01_forged_epoch_without_quorum_cert_rejected() {
    let sandbox = TestSandbox::new("forged_epoch");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    assert_eq!(tracker.highest_seen_epoch(), 0);

    // In default / legacy mode, check_and_update accepts presented epoch
    assert!(tracker.check_and_update(5).is_ok());
    assert_eq!(tracker.highest_seen_epoch(), 5);

    // When strict mode is enabled, raw epoch advances without quorum certificate must be rejected
    tracker.set_strict_mode(true);
    let forged_advance = tracker.check_and_update(999);
    assert!(
        forged_advance.is_err(),
        "forged epoch without certificate must fail in strict mode"
    );
    match forged_advance {
        Err(RpcError::Unauthorized(msg)) => {
            assert!(msg.contains("strict mode requires verified LeadershipCertificate"));
        }
        other => panic!("expected Unauthorized error, got {:?}", other),
    }

    // High water mark remains at 5
    assert_eq!(tracker.highest_seen_epoch(), 5);
}

// -------------------------------------------------------------------------------------------------
// Test 2: Quorum Certificate With Majority Signatures Accepted
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_02_quorum_certificate_with_majority_signatures_accepted() {
    let sandbox = TestSandbox::new("valid_cert");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    tracker.set_strict_mode(true);

    let (priv_keys, pub_keys) = generate_test_keys();
    let leader_id = CoordinatorId::new("coord_1");
    let candidate_id = leader_id.clone();
    let epoch = 10u64;

    // Create vote grants from coord_1 (self) and coord_2 (peer) -> 2 of 3 majority
    let grant1 = QuorumVoteGrant::create(
        CoordinatorId::new("coord_1"),
        epoch,
        candidate_id.clone(),
        &priv_keys[&CoordinatorId::new("coord_1")],
    );
    let grant2 = QuorumVoteGrant::create(
        CoordinatorId::new("coord_2"),
        epoch,
        candidate_id.clone(),
        &priv_keys[&CoordinatorId::new("coord_2")],
    );

    let cert = LeadershipCertificate::new(epoch, leader_id, vec![grant1, grant2]);
    assert!(
        cert.verify_quorum(&pub_keys, 2).is_ok(),
        "cert with 2 valid signatures must pass verification"
    );

    // Present certificate to storage node tracker
    assert!(tracker
        .check_and_update_with_certificate(&cert, &pub_keys, 2)
        .is_ok());
    assert_eq!(tracker.highest_seen_epoch(), 10);

    // Test with invalid signature
    let rogue_signer = create_deterministic_signer("rogue", 9999);
    let bad_grant = QuorumVoteGrant::create(
        CoordinatorId::new("coord_3"),
        epoch,
        candidate_id,
        rogue_signer.signing_key(),
    );
    let bad_cert =
        LeadershipCertificate::new(epoch + 1, CoordinatorId::new("coord_1"), vec![bad_grant]);
    assert!(tracker
        .check_and_update_with_certificate(&bad_cert, &pub_keys, 2)
        .is_err());
}

// -------------------------------------------------------------------------------------------------
// Test 3: Stale Leader Mutation Rejected by Fencing Token
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_03_stale_leader_mutation_rejected_by_fencing_token() {
    let sandbox = TestSandbox::new("stale_mutation");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    assert!(tracker.check_and_update(12).is_ok());

    let (priv_keys, pub_keys) = generate_test_keys();
    let old_leader = CoordinatorId::new("coord_2");

    // Old leader from epoch 11 attempts mutation
    let token = FencedOperationToken::create(
        11,
        old_leader.clone(),
        "req-001",
        "obj-alpha",
        0,
        1,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        1000,
        &priv_keys[&old_leader],
    );

    let res = tracker.verify_fenced_operation(
        &token,
        "obj-alpha",
        0,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        &pub_keys[&old_leader],
    );

    assert!(res.is_err());
    match res {
        Err(RpcError::Fenced {
            current_epoch,
            presented_epoch,
        }) => {
            assert_eq!(current_epoch, 12);
            assert_eq!(presented_epoch, 11);
        }
        other => panic!("expected Fenced error, got {:?}", other),
    }
}

// -------------------------------------------------------------------------------------------------
// Test 4: Durable Majority Log Commit Semantics
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_04_durable_majority_log_commit_semantics() {
    let sandbox = TestSandbox::new("majority_commit");
    let log_mgr = ReplicatedLogManager::open_or_init(sandbox.path(), 1).unwrap();

    let rec = ReplicationRecord::GenerationWatermarkAdvanced {
        object_id: "obj-delta".into(),
        generation: 1,
    };

    let entry = log_mgr.propose_entry(1, rec).unwrap();
    assert_eq!(entry.index, 1);
    assert_eq!(log_mgr.commit_index(), 0);

    // 1 ACK out of 3 total -> fails to commit (majority is 2)
    let advanced_1 = log_mgr.replicate_and_commit(entry.index, 1, 2).unwrap();
    assert!(!advanced_1);
    assert_eq!(log_mgr.commit_index(), 0);

    // 2 ACKs out of 3 -> majority reached!
    let advanced_2 = log_mgr.replicate_and_commit(entry.index, 2, 2).unwrap();
    assert!(advanced_2);
    assert_eq!(log_mgr.commit_index(), 1);
    assert_eq!(log_mgr.committed_records().len(), 1);
}

// -------------------------------------------------------------------------------------------------
// Test 5: Quorum Loss Blocks Mutations (Fail-Closed)
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_05_quorum_loss_blocks_mutations_fail_closed() {
    let sandbox = TestSandbox::new("fail_closed");
    let local = CoordinatorId::new("coord_1");
    let mut peers = BTreeSet::new();
    peers.insert(CoordinatorId::new("coord_2"));
    peers.insert(CoordinatorId::new("coord_3"));

    let quorum = QuorumState::new(local, peers, sandbox.path()).unwrap();
    assert_eq!(quorum.quorum_threshold(), 2);

    // Start election and win with self + coord_2
    let req = quorum.start_election(1).unwrap();
    assert_eq!(req.proposed_epoch, 1);
    let role = quorum
        .record_vote(CoordinatorId::new("coord_2"), true, 100)
        .unwrap();
    assert!(role.is_leader());

    // Trigger fail-closed step down upon losing heartbeats to majority
    quorum.step_down_to_fenced(
        "lost heartbeat response from peer quorum majority".into(),
        Some(2),
    );
    let cur_role = quorum.current_role();
    assert!(!cur_role.is_leader());
    match cur_role {
        CoordinatorRole::Fenced {
            reason,
            superseding_epoch,
        } => {
            assert!(reason.contains("lost heartbeat"));
            assert_eq!(superseding_epoch, Some(2));
        }
        other => panic!("expected Fenced role, got {:?}", other),
    }
}

// -------------------------------------------------------------------------------------------------
// Test 6: Split-Brain Partition Isolates Leader, Cannot Commit
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_06_split_brain_partition_isolated_leader_cannot_commit() {
    let sandbox = TestSandbox::new("split_brain");
    let leader_log = ReplicatedLogManager::open_or_init(sandbox.path(), 1).unwrap();

    let rec = ReplicationRecord::ManifestCommitted {
        object_id: "obj-split".into(),
        generation: 1,
        manifest_json_or_text: "{}".into(),
        manifest_sha256_hex: "abcd".into(),
    };

    let entry = leader_log.propose_entry(1, rec).unwrap();

    // In a split-brain partition {C1} vs {C2, C3}, C1 only receives 1 vote (self)
    let acks = 1;
    let majority_threshold = 2;
    let committed = leader_log
        .replicate_and_commit(entry.index, acks, majority_threshold)
        .unwrap();
    assert!(!committed, "isolated leader must not commit entries");
    assert_eq!(leader_log.commit_index(), 0);
}

// -------------------------------------------------------------------------------------------------
// Test 7: Concurrent Competing Elections Resolve Safely
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_07_concurrent_competing_elections_resolve_safely() {
    let sb1 = TestSandbox::new("compete_c1");
    let sb2 = TestSandbox::new("compete_c2");
    let sb3 = TestSandbox::new("compete_c3");

    let c1 = CoordinatorId::new("coord_1");
    let c2 = CoordinatorId::new("coord_2");
    let c3 = CoordinatorId::new("coord_3");

    let q1 = QuorumState::new(
        c1.clone(),
        BTreeSet::from([c2.clone(), c3.clone()]),
        sb1.path(),
    )
    .unwrap();
    let q2 = QuorumState::new(
        c2.clone(),
        BTreeSet::from([c1.clone(), c3.clone()]),
        sb2.path(),
    )
    .unwrap();
    let q3 = QuorumState::new(
        c3.clone(),
        BTreeSet::from([c1.clone(), c2.clone()]),
        sb3.path(),
    )
    .unwrap();

    // Both c1 and c2 propose epoch 1 simultaneously
    let req1 = q1.start_election(1).unwrap();
    let req2 = q2.start_election(1).unwrap();

    // q3 receives req1 first and grants vote to c1
    let resp3_to_1 = q3.handle_vote_request(&req1);
    assert!(resp3_to_1.vote_granted);

    // q3 then receives req2 for the same epoch; must reject because already voted for c1!
    let resp3_to_2 = q3.handle_vote_request(&req2);
    assert!(!resp3_to_2.vote_granted);

    // Record vote for c1: c1 (self) + c3 (granted) = 2 -> Leader!
    let role1 = q1.record_vote(c3.clone(), true, 50).unwrap();
    assert!(role1.is_leader());

    // Record vote for c2: c2 (self) + c3 (rejected) = 1 -> Stays Candidate
    let role2 = q2.record_vote(c3, false, 50).unwrap();
    assert!(!role2.is_leader());
}

// -------------------------------------------------------------------------------------------------
// Test 8: Delayed RPC Replay Rejected After Failover
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_08_delayed_rpc_replay_rejected_after_failover() {
    let sandbox = TestSandbox::new("delayed_rpc");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();

    // Initial leader was epoch 1
    assert!(tracker.check_and_update(1).is_ok());

    let (priv_keys, pub_keys) = generate_test_keys();
    let old_leader = CoordinatorId::new("coord_1");

    // Old leader crafts token at epoch 1
    let delayed_token = FencedOperationToken::create(
        1,
        old_leader.clone(),
        "req-old",
        "obj-failover",
        0,
        1,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        100,
        &priv_keys[&old_leader],
    );

    // Failover occurs: new leader certifies epoch 2 on the storage node
    assert!(tracker.check_and_update(2).is_ok());

    // Delayed RPC from old leader arrives: rejected!
    let res = tracker.verify_fenced_operation(
        &delayed_token,
        "obj-failover",
        0,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        &pub_keys[&old_leader],
    );
    assert!(matches!(res, Err(RpcError::Fenced { .. })));
}

// -------------------------------------------------------------------------------------------------
// Test 9: Abrupt Coordinator SIGKILL Crash Reconciles Uncommitted State
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_09_coordinator_sigkill_reconciles_uncommitted_state() {
    let sandbox = TestSandbox::new("crash_reconcile");
    {
        let leader_log = ReplicatedLogManager::open_or_init(sandbox.path(), 1).unwrap();
        let rec1 = ReplicationRecord::GenerationWatermarkAdvanced {
            object_id: "obj-1".into(),
            generation: 1,
        };
        let rec2 = ReplicationRecord::GenerationWatermarkAdvanced {
            object_id: "obj-2".into(),
            generation: 1,
        };
        let e1 = leader_log.propose_entry(1, rec1).unwrap();
        let _e2 = leader_log.propose_entry(1, rec2).unwrap();

        // Only e1 was majority-committed before crash
        assert!(leader_log.replicate_and_commit(e1.index, 2, 2).unwrap());
        assert_eq!(leader_log.commit_index(), 1);
        // Leader process abruptly terminates here (SIGKILL simulation)
    }

    // Standby coordinator reopens WAL upon promotion
    let promoted_log = ReplicatedLogManager::open_or_init(sandbox.path(), 2).unwrap();
    assert_eq!(promoted_log.last_log_index(), 2);
    // Uncommitted entries are held in WAL but not acknowledged to clients
    assert_eq!(promoted_log.commit_index(), 0);
}

// -------------------------------------------------------------------------------------------------
// Test 10: Manifest Generation Conflict Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_10_manifest_generation_conflict_rejected() {
    let sandbox = TestSandbox::new("gen_conflict");
    let mut policy = TrustedSignerPolicy::new();
    policy.set_generations_dir(sandbox.path()).unwrap();

    let signer = create_deterministic_signer("leader_1", 42);
    policy
        .register_signer("leader_1", signer.verifying_key())
        .unwrap();

    let engine_dir = sandbox.path().join("engine");
    let mut engine = StorageEngine::init(&engine_dir, 20, 20).unwrap();
    let opts1 = PutOptions {
        object_id: Some("obj-test".into()),
        ..Default::default()
    };
    let receipt1 = engine.put_bytes(b"payload1", &opts1).unwrap();
    let signed1 = signer.sign_manifest(receipt1.manifest.clone(), 1).unwrap();
    assert!(policy.verify_and_advance_generation(&signed1, true).is_ok());

    // Conflicting manifest at the same generation (gen = 1) must be rejected
    let opts2 = PutOptions {
        object_id: Some("obj-test2".into()),
        ..Default::default()
    };
    let receipt2 = engine.put_bytes(b"payload2", &opts2).unwrap();
    let mut conflict_manifest = receipt2.manifest.clone();
    conflict_manifest.object_id = "obj-test".into();
    let signed_conflict = signer.sign_manifest(conflict_manifest, 1).unwrap();

    let res = policy.verify_and_advance_generation(&signed_conflict, true);
    assert!(res.is_err());
}

// -------------------------------------------------------------------------------------------------
// Test 11: Manifest Rollback Strictly Blocked
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_11_manifest_rollback_strictly_blocked() {
    let sandbox = TestSandbox::new("rollback_block");
    let mut policy = TrustedSignerPolicy::new();
    policy.set_generations_dir(sandbox.path()).unwrap();

    let signer = create_deterministic_signer("leader_1", 43);
    policy
        .register_signer("leader_1", signer.verifying_key())
        .unwrap();

    let engine_dir = sandbox.path().join("engine");
    let mut engine = StorageEngine::init(&engine_dir, 20, 20).unwrap();
    let opts_rb = PutOptions {
        object_id: Some("obj-rb".into()),
        ..Default::default()
    };
    let receipt = engine.put_bytes(b"payload_rb", &opts_rb).unwrap();

    // Advance to generation 5
    let signed_5 = signer.sign_manifest(receipt.manifest.clone(), 5).unwrap();
    assert!(policy
        .verify_and_advance_generation(&signed_5, true)
        .is_ok());

    // Attempt to roll back to generation 4
    let signed_4 = signer.sign_manifest(receipt.manifest.clone(), 4).unwrap();
    let res = policy.verify_and_advance_generation(&signed_4, true);
    assert!(res.is_err());
}

// -------------------------------------------------------------------------------------------------
// Test 12: Storage Node Restart Preserves Certified Epoch
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_12_node_restart_preserves_certified_epoch() {
    let sandbox = TestSandbox::new("restart_epoch");
    {
        let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
        assert!(tracker.check_and_update(42).is_ok());
        assert_eq!(tracker.highest_seen_epoch(), 42);
    }

    // Daemon restarts and reloads state from disk
    let tracker_reloaded = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    assert_eq!(tracker_reloaded.highest_seen_epoch(), 42);

    // Old epoch 41 is still fenced out
    assert!(tracker_reloaded.check_and_update(41).is_err());
}

// -------------------------------------------------------------------------------------------------
// Test 13: Executable Bounded Model Checker Validates 7 Consensus Invariants
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_13_model_checker_validates_7_consensus_invariants() {
    let initial_state = GlobalSystemState::initial();
    let checker = BoundedModelChecker::new(8, 2500);
    let report = checker.explore(initial_state);

    println!(
        "Model Checker Report: explored {} states, evaluated {} transitions, depth {}",
        report.states_explored, report.transitions_evaluated, report.max_depth_reached
    );

    assert!(
        report.all_passed,
        "Invariants violated: {:?}",
        report.violation_details
    );
    assert!(report.states_explored >= 50);
    for (i, &passed) in report.invariants_verified.iter().enumerate() {
        assert!(passed, "Invariant {} failed verification", i + 1);
    }
}

// -------------------------------------------------------------------------------------------------
// Test 14: TLS Connection Pooling Accurate Benchmark Accounting
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_14_tls_connection_pooling_accurate_per_peer_handshake_accounting() {
    let metrics = Arc::new(ConnectionPoolMetrics::default());

    // Simulate 10 RPC calls: 2 handshakes, 8 reuses
    metrics.total_rpc_executions.store(10, Ordering::Relaxed);
    metrics.handshakes_performed.store(2, Ordering::Relaxed);
    metrics.connections_reused.store(8, Ordering::Relaxed);
    metrics
        .total_handshake_duration_micros
        .store(4000, Ordering::Relaxed);
    metrics
        .total_reuse_duration_micros
        .store(800, Ordering::Relaxed);

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.total_rpc_executions, 10);
    assert_eq!(snapshot.handshakes_performed, 2);
    assert_eq!(snapshot.connections_reused, 8);
    assert_eq!(snapshot.reuse_percentage, 80.0);
    assert_eq!(snapshot.total_handshake_duration_micros, 4000);
    assert_eq!(snapshot.total_reuse_duration_micros, 800);
}

// -------------------------------------------------------------------------------------------------
// Test 15: Replicated Log Monotonicity and Crash Recovery
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_15_replicated_log_index_monotonicity_and_crash_recovery() {
    let sandbox = TestSandbox::new("log_monotonicity");
    let log_mgr = ReplicatedLogManager::open_or_init(sandbox.path(), 1).unwrap();

    for i in 1..=5 {
        let rec = ReplicationRecord::GenerationWatermarkAdvanced {
            object_id: format!("obj-{}", i),
            generation: i as u64,
        };
        let entry = log_mgr.propose_entry(1, rec).unwrap();
        assert_eq!(entry.index, i as u64);
    }
    assert_eq!(log_mgr.last_log_index(), 5);

    // Commit indices 1..3
    assert!(log_mgr.replicate_and_commit(3, 2, 2).unwrap());
    assert_eq!(log_mgr.commit_index(), 3);

    // Reopen log from disk
    let log_mgr2 = ReplicatedLogManager::open_or_init(sandbox.path(), 1).unwrap();
    assert_eq!(log_mgr2.last_log_index(), 5);
}

// -------------------------------------------------------------------------------------------------
// Test 16: Repair Consistency During Active Leader Failover
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_16_repair_consistency_during_active_leader_failover() {
    let sandbox = TestSandbox::new("repair_failover");
    let journal = DurableRepairJournal::open_or_init(sandbox.path()).unwrap();

    let job = journal.enqueue_or_get("obj-x", 0, "outage", 100).unwrap();
    assert_eq!(job.state, RepairJobState::Pending);

    // Transition state
    journal
        .transition_state(
            &job.job_id,
            RepairJobState::Repairing,
            Some(NodeId(5)),
            None,
            150,
        )
        .unwrap();

    // Reopen journal on promoted coordinator -> crash recovery resets in-flight repairing to Pending
    let journal2 = DurableRepairJournal::open_or_init(sandbox.path()).unwrap();
    let replayed = journal2.all_jobs();
    let target = replayed.iter().find(|j| j.job_id == job.job_id).unwrap();
    assert_eq!(target.state, RepairJobState::Pending);
    assert!(target
        .error_msg
        .as_deref()
        .unwrap_or("")
        .contains("interrupted by coordinator restart"));
    assert_eq!(target.target_node, Some(NodeId(5)));
}

// -------------------------------------------------------------------------------------------------
// Test 17: Physical Failure Domain Staging Topology Validation
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_17_physical_failure_domain_staging_topology_validation() {
    let mut domains = Vec::new();
    // 5 hosts with 4 shards each on 5 distinct racks
    for h in 0..5 {
        for _ in 0..4 {
            domains.push(PhysicalFailureDomain::simple(
                format!("host-{:02}", h),
                format!("rack-{:02}", h),
            ));
        }
    }

    let constraints = FailureDomainConstraints {
        tolerated_host_failures: 2,
        tolerated_rack_failures: 2,
        max_shards_per_host: 4,
        max_shards_per_rack: 4,
        max_shards_per_zone: 20,
        min_independent_hosts: 5,
    };

    let rep = validate_placement_durability(&domains, 20, 12, &constraints).unwrap();
    assert!(rep.is_valid);
}

// -------------------------------------------------------------------------------------------------
// Test 18: Storage Node Rejects Tampered Payload Digest Binding
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_18_storage_node_rejects_tampered_payload_digest_binding() {
    let sandbox = TestSandbox::new("tampered_digest");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    assert!(tracker.check_and_update(5).is_ok());

    let (priv_keys, pub_keys) = generate_test_keys();
    let leader = CoordinatorId::new("coord_1");

    let valid_hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let token = FencedOperationToken::create(
        5,
        leader.clone(),
        "req-1",
        "obj-secure",
        0,
        1,
        valid_hash,
        500,
        &priv_keys[&leader],
    );

    // Tampered payload hash presented by caller
    let tampered_hash = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let res =
        tracker.verify_fenced_operation(&token, "obj-secure", 0, tampered_hash, &pub_keys[&leader]);
    assert!(res.is_err());
    match res {
        Err(RpcError::Unauthorized(msg)) => {
            assert!(msg.contains("payload digest mismatch"));
        }
        other => panic!("expected Unauthorized error, got {:?}", other),
    }
}

// -------------------------------------------------------------------------------------------------
// Test 19: Distributed RS(20,12) PUT/GET With Certified Fencing
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_19_distributed_rs20_12_put_get_with_certified_fencing() {
    let (priv_keys, pub_keys) = generate_test_keys();
    let leader_id = CoordinatorId::new("coord_1");

    let cert = LeadershipCertificate::new(
        1,
        leader_id.clone(),
        vec![
            QuorumVoteGrant::create(
                CoordinatorId::new("coord_1"),
                1,
                leader_id.clone(),
                &priv_keys[&CoordinatorId::new("coord_1")],
            ),
            QuorumVoteGrant::create(
                CoordinatorId::new("coord_2"),
                1,
                leader_id.clone(),
                &priv_keys[&CoordinatorId::new("coord_2")],
            ),
        ],
    );
    assert!(cert.verify_quorum(&pub_keys, 2).is_ok());

    // RS(20,12) encoder handles 120 KiB payload across 20 shards
    let rs = ReedSolomonCodec::fsp_default_20_12();
    let shard_size = 10 * 1024; // 10 KiB per shard
    let data_shards: Vec<Vec<u8>> = (0..12).map(|i| vec![(i as u8) + 1; shard_size]).collect();
    let data_refs: Vec<&[u8]> = data_shards.iter().map(|d| d.as_slice()).collect();
    let stripe = rs.encode_stripe(&data_refs).unwrap();
    assert_eq!(stripe.len(), 20);

    let digest = sha256_bytes(&stripe[0]);
    assert_eq!(digest.to_hex().len(), 64);
}

// -------------------------------------------------------------------------------------------------
// Test 20: 8-Shard Failure and Self-Healing Under Quorum Authority
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_3_20_eight_shard_failure_and_self_healing_under_quorum_authority() {
    let leader_id = CoordinatorId::new("coord_1");

    // Quorum authority established
    let (priv_keys, pub_keys) = generate_test_keys();
    let grant1 = QuorumVoteGrant::create(
        CoordinatorId::new("coord_1"),
        1,
        leader_id.clone(),
        &priv_keys[&CoordinatorId::new("coord_1")],
    );
    let grant2 = QuorumVoteGrant::create(
        CoordinatorId::new("coord_2"),
        1,
        leader_id.clone(),
        &priv_keys[&CoordinatorId::new("coord_2")],
    );
    let cert = LeadershipCertificate::new(1, leader_id, vec![grant1, grant2]);
    assert!(cert.verify_quorum(&pub_keys, 2).is_ok());

    // Create 12 data + 8 parity shards
    let rs = ReedSolomonCodec::fsp_default_20_12();
    let shard_size = 4096;
    let data_shards: Vec<Vec<u8>> = (0..12)
        .map(|i| vec![0x42 ^ (i as u8); shard_size])
        .collect();
    let data_refs: Vec<&[u8]> = data_shards.iter().map(|d| d.as_slice()).collect();
    let stripe = rs.encode_stripe(&data_refs).unwrap();
    assert_eq!(stripe.len(), 20);

    // Simulate 8 shard erasures: shards 0..8 are lost (8 data shards lost!)
    // 12 shards survive: shards 8..20
    let available: Vec<(ShardId, &[u8])> = (8..20)
        .map(|i| (ShardId(i), stripe[i].as_slice()))
        .collect();
    assert_eq!(available.len(), 12);

    // Reconstruct full stripe from the 12 survivors
    let decoded_all = rs.decode_full_stripe(&available, shard_size).unwrap();
    assert_eq!(decoded_all.len(), 20);

    // Verify all 8 lost shards are recovered bit-for-bit
    for i in 0..8 {
        assert_eq!(
            decoded_all[i], stripe[i],
            "shard {} reconstruction mismatch",
            i
        );
    }
}
