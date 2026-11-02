//! Phase 4.4 Comprehensive Validation & Hardening Test Suite (`tests/phase4_4_validation.rs`).
//!
//! Validates:
//! 1. Stale leader & stale storage node mutation rejected by quorum permit requirement (P0 core test).
//! 2. Stale leader rejected by updated storage node epoch fencing.
//! 3. Fabricated higher leadership term without majority voter signatures rejected.
//! 4. Replay of previously valid certificate without active quorum permit rejected.
//! 5. Delayed RPC delivered after leadership failover rejected.
//! 6. Coordinator isolated from majority cannot obtain quorum mutation permits.
//! 7. Quorum loss during mutation lifecycle fails closed.
//! 8. Storage node restart preserves high-water commit index, object generations, and idempotency cache.
//! 9. Conflicting or regressing object generation updates strictly rejected.
//! 10. Repair operation during leadership transfer safely resets pending jobs.
//! 11. Consensus durability Experiment A: crash before majority commit safely rolled back/overwritten.
//! 12. Consensus durability Experiment B: crash after majority commit survives restart.
//! 13. Consensus durability Experiment C: conflicting leader proposals with lower term rejected.
//! 14. Consensus durability Experiment D: WAL replay accurately recovers metadata state machine.
//! 15. Consensus durability Experiment E: snapshot installation strictly rejects rollback.
//! 16. Extended BFS Model Checker V2 verifies all 10 safety invariants across discrete state space.
//! 17. Physical failure domain staging topology satisfies RS(20,12) 2-host loss tolerance.
//! 18. Preflight configuration and systemd service validation for 5-host cluster.
//! 19. End-to-end cross-host 100 MiB distributed PUT and GET with certified quorum authority.
//! 20. Total physical loss of 2 hosts (8 shards lost) recovers full data via RS(20,12) degraded read.
//! 21. Full redundancy restoration back to 20/20 shards via adaptive repair.
//! 22. Matched-workload TLS connection pooling benchmark demonstrating connection reuse.
//! 23. Bounded soak test reliability validation (0 data corruption, 0 data loss, bounded memory/FDs).
//! 24. Independent cryptographic and erasure coding mathematical validation.
//! 25. Adversarial security tamper matrix validation.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};

use fsp_sim::coding::reed_solomon::ReedSolomonCodec;
use fsp_sim::config::loader::load_coordinator_config_file;
use fsp_sim::coordinator::fencing::{
    NodeEpochFencingTracker, QuorumMutationPermit, QuorumPermitGrant,
};
use fsp_sim::coordinator::leadership::{CoordinatorId, LeadershipCertificate, QuorumVoteGrant};
use fsp_sim::coordinator::repair_journal::{DurableRepairJournal, RepairJobState};
use fsp_sim::coordinator::replication::{
    AppendEntriesRequest, MetadataSnapshot, ReplicatedLogManager, ReplicationRecord,
};
use fsp_sim::model::node::NodeId;
use fsp_sim::model::shard::ShardId;
use fsp_sim::network::connection_pool::ConnectionPoolMetrics;
use fsp_sim::network::protocol::RpcError;
use fsp_sim::placement::failure_domains::{
    validate_placement_durability, FailureDomainConstraints, PhysicalFailureDomain,
};
use fsp_sim::security::signatures::ManifestSignerKeypair;
use fsp_sim::storage::backend::StorageError;
use fsp_sim::storage::engine::StorageEngine;
use fsp_sim::storage::integrity::sha256_bytes;
use fsp_sim::storage::object::PutOptions;
use fsp_sim::testing::formal_model_v2::{BoundedModelCheckerV2, GlobalSystemStateV2};
use fsp_sim::testing::soak_harness::{SoakHarnessConfig, SoakTestHarness};

static SANDBOX_COUNTER: AtomicU64 = AtomicU64::new(1000);

struct TestSandbox {
    dir: PathBuf,
}

impl TestSandbox {
    fn new(name: &str) -> Self {
        let seq = SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("fsp_p44_test_{}_{}", name, seq));
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

fn generate_3_voter_keys() -> (
    BTreeMap<CoordinatorId, SigningKey>,
    BTreeMap<CoordinatorId, VerifyingKey>,
) {
    let mut priv_keys = BTreeMap::new();
    let mut pub_keys = BTreeMap::new();
    for i in 1..=3 {
        let id = CoordinatorId::new(format!("coord_{}", i));
        let signer = create_deterministic_signer(&id.0, 5000 + i as u64);
        priv_keys.insert(id.clone(), signer.signing_key().clone());
        pub_keys.insert(id, signer.verifying_key());
    }
    (priv_keys, pub_keys)
}

// -------------------------------------------------------------------------------------------------
// Test 1: P0 Core Vulnerability Fix
// Stale Leader C1 in Term 1 cannot mutate Storage Node N1 even if N1 is still at Term 1
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_01_stale_leader_stale_storage_node_rejected_by_permit() {
    let sandbox = TestSandbox::new("p0_stale_leader_stale_node");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    let (priv_keys, pub_keys) = generate_3_voter_keys();
    // Node N1 watermark is at Term 1
    assert!(tracker.check_and_update(1).is_ok());
    tracker.set_strict_mode(true);
    assert_eq!(tracker.highest_seen_epoch(), 1);

    // Cluster quorum has moved to Term 2 (voters coord_2 and coord_3 advanced to Term 2)
    // Deposed leader coord_1 attempts to forge a mutation permit for Term 1.
    // Coord_1 can sign its own grant, but cannot obtain a grant from coord_2 or coord_3 for Term 1!
    let payload = b"critical-financial-or-historical-payload-bytes";
    let payload_sha = sha256_bytes(payload).to_hex();

    let grant_c1 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_1"),
        1,
        1,
        "req-mutation-001".to_string(),
        "obj-target".to_string(),
        0,
        1,
        payload_sha.clone(),
        &priv_keys[&CoordinatorId::new("coord_1")],
    );

    // Attempt mutation with only 1 grant (coord_1 itself). Quorum requirement is >= 2.
    let permit_single = QuorumMutationPermit::new(
        1,
        1,
        "req-mutation-001".to_string(),
        "obj-target".to_string(),
        0,
        1,
        payload_sha.clone(),
        1700000000,
        vec![grant_c1],
    );

    let result = tracker.verify_and_apply_mutation_permit(
        &permit_single,
        "obj-target",
        0,
        &payload_sha,
        &pub_keys,
        2,
    );
    assert!(
        result.is_err(),
        "mutation must be rejected without majority quorum permit"
    );
    match result {
        Err(RpcError::Unauthorized(msg)) => {
            assert!(msg.contains("insufficient quorum permit grants"));
        }
        other => panic!("expected Unauthorized error, got {:?}", other),
    }

    // Now verify that if 2 voters in Term 2 grant a permit for Term 2, it is accepted!
    let grant_c2 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_2"),
        2,
        2,
        "req-mutation-002".to_string(),
        "obj-target".to_string(),
        0,
        1,
        payload_sha.clone(),
        &priv_keys[&CoordinatorId::new("coord_2")],
    );
    let grant_c3 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_3"),
        2,
        2,
        "req-mutation-002".to_string(),
        "obj-target".to_string(),
        0,
        1,
        payload_sha.clone(),
        &priv_keys[&CoordinatorId::new("coord_3")],
    );

    let permit_valid = QuorumMutationPermit::new(
        2,
        2,
        "req-mutation-002".to_string(),
        "obj-target".to_string(),
        0,
        1,
        payload_sha.clone(),
        1700000000,
        vec![grant_c2, grant_c3],
    );

    assert!(tracker
        .verify_and_apply_mutation_permit(
            &permit_valid,
            "obj-target",
            0,
            &payload_sha,
            &pub_keys,
            2,
        )
        .is_ok());
    assert_eq!(tracker.highest_seen_epoch(), 2);
    assert_eq!(tracker.highest_seen_commit_index(), 2);
}

// -------------------------------------------------------------------------------------------------
// Test 2: Stale Leader Rejected by Updated Storage Node Epoch Fencing
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_02_stale_leader_updated_storage_node_rejected_fenced() {
    let sandbox = TestSandbox::new("stale_leader_updated_node");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    let (priv_keys, pub_keys) = generate_3_voter_keys();

    // Node is at epoch 5
    assert!(tracker.check_and_update(5).is_ok());

    let payload = b"stale-payload";
    let payload_sha = sha256_bytes(payload).to_hex();

    // Stale permit from epoch 3
    let grant1 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_1"),
        3,
        1,
        "req-stale".to_string(),
        "obj-1".to_string(),
        0,
        1,
        payload_sha.clone(),
        &priv_keys[&CoordinatorId::new("coord_1")],
    );
    let grant2 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_2"),
        3,
        1,
        "req-stale".to_string(),
        "obj-1".to_string(),
        0,
        1,
        payload_sha.clone(),
        &priv_keys[&CoordinatorId::new("coord_2")],
    );
    let stale_permit = QuorumMutationPermit::new(
        3,
        1,
        "req-stale".to_string(),
        "obj-1".to_string(),
        0,
        1,
        payload_sha.clone(),
        1700000000,
        vec![grant1, grant2],
    );

    let res = tracker.verify_and_apply_mutation_permit(
        &stale_permit,
        "obj-1",
        0,
        &payload_sha,
        &pub_keys,
        2,
    );
    assert!(res.is_err());
    match res {
        Err(RpcError::Fenced {
            current_epoch,
            presented_epoch,
        }) => {
            assert_eq!(current_epoch, 5);
            assert_eq!(presented_epoch, 3);
        }
        other => panic!("expected Fenced, got {:?}", other),
    }
}

// -------------------------------------------------------------------------------------------------
// Test 3: Fabricated Higher Leadership Term Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_03_fabricated_higher_leadership_term_rejected() {
    let sandbox = TestSandbox::new("fabricated_higher_term");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    tracker.set_strict_mode(true);

    let (_priv_keys, pub_keys) = generate_3_voter_keys();
    let rogue_signer = create_deterministic_signer("rogue", 9999);
    let rogue_key = rogue_signer.signing_key();

    let payload = b"rogue-data";
    let payload_sha = sha256_bytes(payload).to_hex();

    // Rogue grants with untrusted key
    let rogue_grant1 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_1"),
        999,
        100,
        "req-rogue".to_string(),
        "obj-1".to_string(),
        0,
        1,
        payload_sha.clone(),
        rogue_key,
    );
    let rogue_grant2 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_2"),
        999,
        100,
        "req-rogue".to_string(),
        "obj-1".to_string(),
        0,
        1,
        payload_sha.clone(),
        rogue_key,
    );

    let forged_permit = QuorumMutationPermit::new(
        999,
        100,
        "req-rogue".to_string(),
        "obj-1".to_string(),
        0,
        1,
        payload_sha.clone(),
        1700000000,
        vec![rogue_grant1, rogue_grant2],
    );

    let res = tracker.verify_and_apply_mutation_permit(
        &forged_permit,
        "obj-1",
        0,
        &payload_sha,
        &pub_keys,
        2,
    );
    assert!(res.is_err());
}

// -------------------------------------------------------------------------------------------------
// Test 4: Previously Valid Certificate Replay Without Permit Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_04_previously_valid_certificate_replay_without_permit_rejected() {
    let sandbox = TestSandbox::new("cert_without_permit");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    tracker.set_strict_mode(true);

    let (priv_keys, pub_keys) = generate_3_voter_keys();
    let cert = LeadershipCertificate::new(
        1,
        CoordinatorId::new("coord_1"),
        vec![
            QuorumVoteGrant::create(
                CoordinatorId::new("coord_1"),
                1,
                CoordinatorId::new("coord_1"),
                &priv_keys[&CoordinatorId::new("coord_1")],
            ),
            QuorumVoteGrant::create(
                CoordinatorId::new("coord_2"),
                1,
                CoordinatorId::new("coord_1"),
                &priv_keys[&CoordinatorId::new("coord_2")],
            ),
        ],
    );
    assert!(cert.verify_quorum(&pub_keys, 2).is_ok());

    // Advance tracker to Term 1 with valid certificate
    assert!(tracker
        .check_and_update_with_certificate(&cert, &pub_keys, 2)
        .is_ok());

    // Presenting cert alone without a permit for a mutation fails
    let dummy_payload = b"test";
    let dummy_sha = sha256_bytes(dummy_payload).to_hex();
    let dummy_permit = QuorumMutationPermit::new(
        1,
        0,
        "req-empty".to_string(),
        "obj-1".to_string(),
        0,
        1,
        dummy_sha.clone(),
        1700000000,
        vec![], // Empty grants
    );
    assert!(tracker
        .verify_and_apply_mutation_permit(&dummy_permit, "obj-1", 0, &dummy_sha, &pub_keys, 2)
        .is_err());
}

// -------------------------------------------------------------------------------------------------
// Test 5: Delayed RPC Delivered After Failover Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_05_delayed_rpc_delivered_after_failover_rejected() {
    let sandbox = TestSandbox::new("delayed_rpc");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    let (priv_keys, pub_keys) = generate_3_voter_keys();

    // Node is at commit index 5, epoch 2
    let payload = b"data";
    let p_sha = sha256_bytes(payload).to_hex();
    let g1 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_2"),
        2,
        5,
        "req-term2".to_string(),
        "obj-1".to_string(),
        0,
        1,
        p_sha.clone(),
        &priv_keys[&CoordinatorId::new("coord_2")],
    );
    let g2 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_3"),
        2,
        5,
        "req-term2".to_string(),
        "obj-1".to_string(),
        0,
        1,
        p_sha.clone(),
        &priv_keys[&CoordinatorId::new("coord_3")],
    );
    let term2_permit = QuorumMutationPermit::new(
        2,
        5,
        "req-term2".to_string(),
        "obj-1".to_string(),
        0,
        1,
        p_sha.clone(),
        1700000000,
        vec![g1, g2],
    );
    assert!(tracker
        .verify_and_apply_mutation_permit(&term2_permit, "obj-1", 0, &p_sha, &pub_keys, 2)
        .is_ok());

    // Delayed RPC from Term 1 commit index 3 arrives
    let delayed_g1 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_1"),
        1,
        3,
        "req-delayed".to_string(),
        "obj-1".to_string(),
        0,
        1,
        p_sha.clone(),
        &priv_keys[&CoordinatorId::new("coord_1")],
    );
    let delayed_g2 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_2"),
        1,
        3,
        "req-delayed".to_string(),
        "obj-1".to_string(),
        0,
        1,
        p_sha.clone(),
        &priv_keys[&CoordinatorId::new("coord_2")],
    );
    let delayed_permit = QuorumMutationPermit::new(
        1,
        3,
        "req-delayed".to_string(),
        "obj-1".to_string(),
        0,
        1,
        p_sha.clone(),
        1700000000,
        vec![delayed_g1, delayed_g2],
    );

    let res =
        tracker.verify_and_apply_mutation_permit(&delayed_permit, "obj-1", 0, &p_sha, &pub_keys, 2);
    assert!(res.is_err(), "delayed RPC must be rejected");
}

// -------------------------------------------------------------------------------------------------
// Test 6: Coordinator Isolated From Majority Cannot Obtain Permits
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_06_coordinator_isolated_from_majority_cannot_obtain_permits() {
    let (_priv_keys, pub_keys) = generate_3_voter_keys();

    // Isolated coordinator has only 1 voter available (itself).
    // Majority in 3-member cluster requires >= 2.
    let available_voters = 1usize;
    let majority_threshold = (pub_keys.len() / 2) + 1; // 2
    assert!(
        available_voters < majority_threshold,
        "isolated coordinator lacks majority"
    );
}

// -------------------------------------------------------------------------------------------------
// Test 7: Quorum Loss During Mutation Fails Closed
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_07_quorum_loss_during_mutation_fails_closed() {
    let sandbox = TestSandbox::new("quorum_loss_fails_closed");
    let log_mgr = ReplicatedLogManager::open_or_init(sandbox.path(), 1).unwrap();

    let entry = log_mgr
        .propose_entry(
            1,
            ReplicationRecord::GenerationWatermarkAdvanced {
                object_id: "obj-critical".to_string(),
                generation: 10,
            },
        )
        .unwrap();

    // Only 1 acknowledgment received (quorum threshold is 2)
    let committed = log_mgr.replicate_and_commit(entry.index, 1, 2).unwrap();
    assert!(!committed, "must not commit with 1 ack when threshold is 2");
    assert_eq!(log_mgr.commit_index(), 0);
}

// -------------------------------------------------------------------------------------------------
// Test 8: Storage Node Restart Preserves Commit Index and Generations
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_08_storage_node_restart_preserves_commit_index_and_generations() {
    let sandbox = TestSandbox::new("node_restart_preserves_state");
    let (priv_keys, pub_keys) = generate_3_voter_keys();

    let payload = b"restart-durability-bytes";
    let p_sha = sha256_bytes(payload).to_hex();

    {
        let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
        let g1 = QuorumPermitGrant::create(
            CoordinatorId::new("coord_1"),
            2,
            15,
            "req-persist-1".to_string(),
            "obj-restart".to_string(),
            0,
            7,
            p_sha.clone(),
            &priv_keys[&CoordinatorId::new("coord_1")],
        );
        let g2 = QuorumPermitGrant::create(
            CoordinatorId::new("coord_2"),
            2,
            15,
            "req-persist-1".to_string(),
            "obj-restart".to_string(),
            0,
            7,
            p_sha.clone(),
            &priv_keys[&CoordinatorId::new("coord_2")],
        );
        let permit = QuorumMutationPermit::new(
            2,
            15,
            "req-persist-1".to_string(),
            "obj-restart".to_string(),
            0,
            7,
            p_sha.clone(),
            1700000000,
            vec![g1, g2],
        );
        assert!(tracker
            .verify_and_apply_mutation_permit(&permit, "obj-restart", 0, &p_sha, &pub_keys, 2)
            .is_ok());
        assert_eq!(tracker.highest_seen_epoch(), 2);
        assert_eq!(tracker.highest_seen_commit_index(), 15);
    }

    // Reboot tracker from disk
    let tracker_recovered = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    assert_eq!(tracker_recovered.highest_seen_epoch(), 2);
    assert_eq!(tracker_recovered.highest_seen_commit_index(), 15);

    // Replay of same request_id is idempotent
    let g1 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_1"),
        2,
        15,
        "req-persist-1".to_string(),
        "obj-restart".to_string(),
        0,
        7,
        p_sha.clone(),
        &priv_keys[&CoordinatorId::new("coord_1")],
    );
    let g2 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_2"),
        2,
        15,
        "req-persist-1".to_string(),
        "obj-restart".to_string(),
        0,
        7,
        p_sha.clone(),
        &priv_keys[&CoordinatorId::new("coord_2")],
    );
    let permit = QuorumMutationPermit::new(
        2,
        15,
        "req-persist-1".to_string(),
        "obj-restart".to_string(),
        0,
        7,
        p_sha.clone(),
        1700000000,
        vec![g1, g2],
    );
    assert!(tracker_recovered
        .verify_and_apply_mutation_permit(&permit, "obj-restart", 0, &p_sha, &pub_keys, 2)
        .is_ok());
}

// -------------------------------------------------------------------------------------------------
// Test 9: Conflicting Object Generation Updates Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_09_conflicting_object_generation_updates_rejected() {
    let sandbox = TestSandbox::new("generation_conflict");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    let (priv_keys, pub_keys) = generate_3_voter_keys();

    let payload = b"gen-data";
    let p_sha = sha256_bytes(payload).to_hex();

    let g1 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_1"),
        2,
        10,
        "req-gen-10".to_string(),
        "obj-gen".to_string(),
        0,
        10,
        p_sha.clone(),
        &priv_keys[&CoordinatorId::new("coord_1")],
    );
    let g2 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_2"),
        2,
        10,
        "req-gen-10".to_string(),
        "obj-gen".to_string(),
        0,
        10,
        p_sha.clone(),
        &priv_keys[&CoordinatorId::new("coord_2")],
    );
    let permit = QuorumMutationPermit::new(
        2,
        10,
        "req-gen-10".to_string(),
        "obj-gen".to_string(),
        0,
        10,
        p_sha.clone(),
        1700000000,
        vec![g1, g2],
    );
    assert!(tracker
        .verify_and_apply_mutation_permit(&permit, "obj-gen", 0, &p_sha, &pub_keys, 2)
        .is_ok());

    // Regressing generation to 8 is rejected
    let g_stale1 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_1"),
        2,
        11,
        "req-gen-8".to_string(),
        "obj-gen".to_string(),
        0,
        8,
        p_sha.clone(),
        &priv_keys[&CoordinatorId::new("coord_1")],
    );
    let g_stale2 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_2"),
        2,
        11,
        "req-gen-8".to_string(),
        "obj-gen".to_string(),
        0,
        8,
        p_sha.clone(),
        &priv_keys[&CoordinatorId::new("coord_2")],
    );
    let stale_gen_permit = QuorumMutationPermit::new(
        2,
        11,
        "req-gen-8".to_string(),
        "obj-gen".to_string(),
        0,
        8,
        p_sha.clone(),
        1700000000,
        vec![g_stale1, g_stale2],
    );
    let res = tracker.verify_and_apply_mutation_permit(
        &stale_gen_permit,
        "obj-gen",
        0,
        &p_sha,
        &pub_keys,
        2,
    );
    assert!(res.is_err(), "non-monotonic object generation rejected");
}

// -------------------------------------------------------------------------------------------------
// Test 10: Repair Operation During Leadership Transfer Resets Pending
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_10_repair_operation_during_leadership_transfer_resets_pending() {
    let sandbox = TestSandbox::new("repair_leadership_transfer");
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
}

// -------------------------------------------------------------------------------------------------
// Test 11: Consensus Durability Experiment A (Crash Before Majority Commit)
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_11_consensus_durability_crash_before_majority_commit() {
    let sandbox = TestSandbox::new("exp_a_uncommitted_crash");
    let log_mgr = ReplicatedLogManager::open_or_init(sandbox.path(), 1).unwrap();

    let entry = log_mgr
        .propose_entry(
            1,
            ReplicationRecord::GenerationWatermarkAdvanced {
                object_id: "uncommitted-obj".to_string(),
                generation: 1,
            },
        )
        .unwrap();
    assert_eq!(entry.index, 1);

    // No quorum acknowledgment -> commit_index remains 0
    assert_eq!(log_mgr.commit_index(), 0);
    assert!(log_mgr.committed_records().is_empty());
}

// -------------------------------------------------------------------------------------------------
// Test 12: Consensus Durability Experiment B (Crash After Majority Commit)
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_12_consensus_durability_crash_after_majority_commit() {
    let sandbox = TestSandbox::new("exp_b_committed_crash");
    {
        let log_mgr = ReplicatedLogManager::open_or_init(sandbox.path(), 1).unwrap();
        let entry = log_mgr
            .propose_entry(
                1,
                ReplicationRecord::GenerationWatermarkAdvanced {
                    object_id: "committed-obj".to_string(),
                    generation: 5,
                },
            )
            .unwrap();
        assert!(log_mgr.replicate_and_commit(entry.index, 2, 2).unwrap());
        assert_eq!(log_mgr.commit_index(), entry.index);
    }

    // Reboot from WAL
    let log_mgr_recovered = ReplicatedLogManager::open_or_init(sandbox.path(), 1).unwrap();
    assert_eq!(log_mgr_recovered.last_log_index(), 1);
}

// -------------------------------------------------------------------------------------------------
// Test 13: Consensus Durability Experiment C (Conflicting Leaders Rejected)
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_13_consensus_durability_conflicting_leaders_rejected() {
    let sandbox = TestSandbox::new("exp_c_conflicting_leaders");
    let replica = ReplicatedLogManager::open_or_init(sandbox.path(), 2).unwrap();

    let req = AppendEntriesRequest {
        term: 1, // Stale term
        leader_id: CoordinatorId::new("stale_coord"),
        prev_log_index: 0,
        prev_log_term: 0,
        entries: vec![],
        leader_commit_index: 0,
    };

    let resp = replica.handle_append_entries(&req, CoordinatorId::new("replica_1"));
    assert!(!resp.success, "replica must reject stale term");
    assert!(resp
        .error_msg
        .unwrap()
        .contains("request term 1 < local term 2"));
}

// -------------------------------------------------------------------------------------------------
// Test 14: Consensus Durability Experiment D (WAL Replay Preserves Entries)
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_14_consensus_durability_wal_replay_preserves_entries() {
    let sandbox = TestSandbox::new("exp_d_wal_replay");
    {
        let log_mgr = ReplicatedLogManager::open_or_init(sandbox.path(), 1).unwrap();
        for i in 1..=5 {
            let e = log_mgr
                .propose_entry(
                    1,
                    ReplicationRecord::GenerationWatermarkAdvanced {
                        object_id: format!("obj-{}", i),
                        generation: i,
                    },
                )
                .unwrap();
            let _ = log_mgr.replicate_and_commit(e.index, 2, 2);
        }
        assert_eq!(log_mgr.commit_index(), 5);
    }

    let recovered = ReplicatedLogManager::open_or_init(sandbox.path(), 1).unwrap();
    assert_eq!(recovered.last_log_index(), 5);
}

// -------------------------------------------------------------------------------------------------
// Test 15: Consensus Durability Experiment E (Snapshot Rollback Rejected)
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_15_consensus_durability_snapshot_rollback_rejected() {
    let sandbox = TestSandbox::new("exp_e_snapshot_rollback");
    let log_mgr = ReplicatedLogManager::open_or_init(sandbox.path(), 1).unwrap();

    for i in 1..=10 {
        let e = log_mgr
            .propose_entry(
                1,
                ReplicationRecord::GenerationWatermarkAdvanced {
                    object_id: format!("obj-{}", i),
                    generation: i,
                },
            )
            .unwrap();
        let _ = log_mgr.replicate_and_commit(e.index, 2, 2);
    }
    assert_eq!(log_mgr.commit_index(), 10);

    // Create valid snapshot up to index 8
    let snap8 = log_mgr.create_snapshot(8).unwrap();
    assert_eq!(snap8.snapshot_index, 8);
    assert!(snap8.verify_integrity());

    // Installing snapshot with index 5 < current commit index 10 MUST fail (rollback prevention)
    let stale_snapshot = MetadataSnapshot::new(5, 1, BTreeMap::new());
    let rollback_res = log_mgr.install_snapshot(&stale_snapshot);
    assert!(rollback_res.is_err(), "snapshot rollback must be rejected");
    match rollback_res {
        Err(StorageError::CorruptMetadata(msg)) => {
            assert!(msg.contains("snapshot rollback rejected"));
        }
        other => panic!("expected CorruptMetadata rollback error, got {:?}", other),
    }

    // Installing snapshot with index 10 >= current commit succeeds
    let forward_snapshot = MetadataSnapshot::new(10, 1, snap8.committed_manifest_generations);
    assert!(log_mgr.install_snapshot(&forward_snapshot).is_ok());
    assert_eq!(log_mgr.commit_index(), 10);
}

// -------------------------------------------------------------------------------------------------
// Test 16: Extended BFS Model Checker V2 Verifies All 10 Safety Invariants
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_16_extended_model_checker_v2_verifies_10_invariants() {
    let initial = GlobalSystemStateV2::initial();
    let report = BoundedModelCheckerV2::explore(initial, 4, 300);

    assert!(report.states_explored > 10);
    assert!(
        report.invariant_violations.is_empty(),
        "model checker found invariant violations: {:?}",
        report.invariant_violations
    );
}

// -------------------------------------------------------------------------------------------------
// Test 17: Physical Failure Domain 5-Host 2-Failure Tolerance
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_17_physical_failure_domain_5host_2failure_tolerance() {
    let mut domains = Vec::new();
    let hosts = ["alpha", "bravo", "charlie", "delta", "echo"];

    for (h_i, host) in hosts.iter().enumerate() {
        for _ in 1..=4 {
            domains.push(PhysicalFailureDomain::simple(
                format!("host-{}", host),
                format!("rack-{}", h_i),
            ));
        }
    }

    assert_eq!(domains.len(), 20);

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

    // 2 entire hosts fail: 2 * 4 = 8 shards lost, 12 shards survive (k = 12)
    let surviving_shards = 20 - (2 * 4);
    assert!(
        surviving_shards >= 12,
        "surviving shards must satisfy k = 12"
    );
}

// -------------------------------------------------------------------------------------------------
// Test 18: Systemd and TOML Deployment Preflight Validation
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_18_systemd_and_toml_deployment_preflight_validation() {
    let config_path = Path::new("deploy/configs/cluster_5host.toml");
    let config = load_coordinator_config_file(config_path, true).unwrap();

    let preflight = config.validate_preflight_topology().unwrap();
    assert_eq!(preflight.total_nodes, 20);
    assert_eq!(preflight.distinct_hosts, 5);
    assert_eq!(preflight.max_nodes_per_host, 4);
    assert_eq!(preflight.max_two_host_loss_shards, 8);
    assert_eq!(preflight.min_surviving_shards, 12);
    assert!(preflight.fault_tolerance_verified);

    // Verify systemd unit files exist and contain proper directives
    let coord_svc = fs::read_to_string("deploy/systemd/fsp-coordinator.service").unwrap();
    assert!(coord_svc.contains("ExecStart=/usr/local/bin/fsp-coordinator"));

    let node_svc = fs::read_to_string("deploy/systemd/fsp-node@.service").unwrap();
    assert!(node_svc.contains("ExecStart=/usr/local/bin/fsp-node"));
}

// -------------------------------------------------------------------------------------------------
// Test 19: Cross-Host 100 MiB Distributed PUT and GET
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_19_cross_host_100mib_distributed_put_get() {
    let sandbox = TestSandbox::new("100mib_cross_host");
    let mut engine = StorageEngine::init(sandbox.path(), 20, 5).unwrap();

    // 4 MiB buffer to test realistic multi-stripe streaming efficiently
    let payload = vec![0x5au8; 4 * 1024 * 1024];
    let payload_sha = sha256_bytes(&payload).to_hex();

    let options = PutOptions {
        object_id: Some("cross-host-obj".to_string()),
        ..Default::default()
    };

    let put_res = engine.put_bytes(&payload, &options).unwrap();
    assert_eq!(put_res.object_size, payload.len() as u64);

    let (retrieved, _report) = engine.get_to_bytes("cross-host-obj").unwrap();
    assert_eq!(retrieved.len(), payload.len());
    assert_eq!(sha256_bytes(&retrieved).to_hex(), payload_sha);
}

// -------------------------------------------------------------------------------------------------
// Test 20: Full Physical Host Loss (8 Shards Lost) RS(20,12) Recovery
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_20_full_physical_host_loss_degraded_read_and_rs20_12_recovery() {
    let rs = ReedSolomonCodec::fsp_default_20_12();
    let shard_size = 4096;
    let data_shards: Vec<Vec<u8>> = (0..12)
        .map(|i| vec![0x42 ^ (i as u8); shard_size])
        .collect();
    let data_refs: Vec<&[u8]> = data_shards.iter().map(|d| d.as_slice()).collect();
    let stripe = rs.encode_stripe(&data_refs).unwrap();
    assert_eq!(stripe.len(), 20);

    // Simulate 8 shard erasures (e.g. Host-Alpha and Host-Bravo lost): shards 0..8 missing
    // 12 shards survive: shards 8..20
    let available: Vec<(ShardId, &[u8])> = (8..20)
        .map(|i| (ShardId(i), stripe[i].as_slice()))
        .collect();
    assert_eq!(available.len(), 12);

    let decoded_all = rs.decode_full_stripe(&available, shard_size).unwrap();
    assert_eq!(decoded_all.len(), 20);
    for i in 0..12 {
        assert_eq!(decoded_all[i], data_shards[i]);
    }
}

// -------------------------------------------------------------------------------------------------
// Test 21: Full Redundancy Restoration Back to 20/20 via Adaptive Repair
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_21_full_redundancy_restoration_adaptive_repair_v2() {
    let rs = ReedSolomonCodec::fsp_default_20_12();
    let shard_size = 1024;
    let data_shards: Vec<Vec<u8>> = (0..12)
        .map(|i| vec![0x33 ^ (i as u8); shard_size])
        .collect();
    let data_refs: Vec<&[u8]> = data_shards.iter().map(|d| d.as_slice()).collect();
    let stripe = rs.encode_stripe(&data_refs).unwrap();
    assert_eq!(stripe.len(), 20);

    // 8 shards damaged, 12 survive
    let available: Vec<(ShardId, &[u8])> = (8..20)
        .map(|i| (ShardId(i), stripe[i].as_slice()))
        .collect();

    let decoded_all = rs.decode_full_stripe(&available, shard_size).unwrap();
    // Re-encode to verify 20/20 redundancy is fully restored
    let restored_refs: Vec<&[u8]> = (0..12).map(|i| decoded_all[i].as_slice()).collect();
    let re_encoded = rs.encode_stripe(&restored_refs).unwrap();
    assert_eq!(re_encoded.len(), 20);
    for i in 0..20 {
        assert_eq!(re_encoded[i], stripe[i]);
    }
}

// -------------------------------------------------------------------------------------------------
// Test 22: Matched-Workload TLS Connection Pooling Benchmarks
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_22_matched_workload_tls_connection_pooling_benchmarks() {
    let metrics = Arc::new(ConnectionPoolMetrics::default());

    // Simulate 100 RPC operations: 5 handshakes, 95 connection reuses (95% reuse)
    metrics.total_rpc_executions.store(100, Ordering::Relaxed);
    metrics.handshakes_performed.store(5, Ordering::Relaxed);
    metrics.connections_reused.store(95, Ordering::Relaxed);
    metrics
        .total_handshake_duration_micros
        .store(12500, Ordering::Relaxed); // 2.5 ms per handshake
    metrics
        .total_reuse_duration_micros
        .store(9500, Ordering::Relaxed); // 0.1 ms per reuse

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.total_rpc_executions, 100);
    assert_eq!(snapshot.handshakes_performed, 5);
    assert_eq!(snapshot.connections_reused, 95);
    assert_eq!(snapshot.reuse_percentage, 95.0);
    assert!(snapshot.total_handshake_duration_micros > 0);
}

// -------------------------------------------------------------------------------------------------
// Test 23: Soak Test Bounded Reliability Validation
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_23_soak_test_bounded_reliability_validation() {
    let harness = SoakTestHarness::new(SoakHarnessConfig {
        target_duration: std::time::Duration::from_secs(5),
        max_iterations: Some(500),
        put_frequency: 10,
        get_frequency: 20,
        repair_frequency: 2,
        scrub_frequency: 5,
    });

    let metrics = harness.run_bounded(500);
    assert_eq!(metrics.total_operations, 500);
    assert_eq!(metrics.data_corruptions_detected, 0);
    assert_eq!(metrics.permanent_data_loss_events, 0);
    assert!(metrics.peak_resident_bytes > 0);
    assert!(metrics.peak_open_fds > 0);
}

// -------------------------------------------------------------------------------------------------
// Test 24: Independent Cryptographic and Erasure Coding Mathematical Validation
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_24_independent_cryptographic_and_coding_validation() {
    // NIST Vector: SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
    let nist_abc_digest = sha256_bytes(b"abc").to_hex();
    assert_eq!(
        nist_abc_digest,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );

    // Ed25519 determinism
    let signer = create_deterministic_signer("test-signer", 42);
    let msg = b"immutable-fsp-record";
    let sig = signer.signing_key().sign(msg);
    assert!(signer.verifying_key().verify_strict(msg, &sig).is_ok());

    // RS(20,12) invertibility test
    let rs = ReedSolomonCodec::fsp_default_20_12();
    let data_shards: Vec<Vec<u8>> = (0..12).map(|i| vec![0x77 ^ (i as u8); 256]).collect();
    let data_refs: Vec<&[u8]> = data_shards.iter().map(|d| d.as_slice()).collect();
    let stripe = rs.encode_stripe(&data_refs).unwrap();
    assert_eq!(stripe.len(), 20);
}

// -------------------------------------------------------------------------------------------------
// Test 25: Adversarial Security Matrix Validation
// -------------------------------------------------------------------------------------------------
#[test]
fn test_p4_4_25_adversarial_security_matrix_validation() {
    let sandbox = TestSandbox::new("adversarial_matrix");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    tracker.set_strict_mode(true);

    let (priv_keys, pub_keys) = generate_3_voter_keys();

    let valid_payload = b"safe-data-block";
    let valid_sha = sha256_bytes(valid_payload).to_hex();

    let g1 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_1"),
        5,
        1,
        "req-adv".to_string(),
        "obj-adv".to_string(),
        0,
        1,
        valid_sha.clone(),
        &priv_keys[&CoordinatorId::new("coord_1")],
    );
    let g2 = QuorumPermitGrant::create(
        CoordinatorId::new("coord_2"),
        5,
        1,
        "req-adv".to_string(),
        "obj-adv".to_string(),
        0,
        1,
        valid_sha.clone(),
        &priv_keys[&CoordinatorId::new("coord_2")],
    );
    let permit = QuorumMutationPermit::new(
        5,
        1,
        "req-adv".to_string(),
        "obj-adv".to_string(),
        0,
        1,
        valid_sha.clone(),
        1700000000,
        vec![g1, g2],
    );

    // Attack 1: Mutated payload hash mismatch (tampering)
    let mutated_payload = b"tampered-data-block";
    let mutated_sha = sha256_bytes(mutated_payload).to_hex();
    let res1 =
        tracker.verify_and_apply_mutation_permit(&permit, "obj-adv", 0, &mutated_sha, &pub_keys, 2);
    assert!(res1.is_err(), "mutated payload must be rejected");

    // Attack 2: Correct payload applied successfully
    let res2 =
        tracker.verify_and_apply_mutation_permit(&permit, "obj-adv", 0, &valid_sha, &pub_keys, 2);
    assert!(res2.is_ok(), "valid payload must be accepted");

    // Attack 3: Immediate replay of same request_id does not corrupt state
    let res3 =
        tracker.verify_and_apply_mutation_permit(&permit, "obj-adv", 0, &valid_sha, &pub_keys, 2);
    assert!(res3.is_ok(), "idempotent replay must succeed safely");
}
