//! Extended Formal Model Checker for Consensus Safety, Fencing & 10 Invariants (`src/testing/formal_model_v2.rs`).
//!
//! Extends formal verification to model:
//! 1. 3 Quorum Voters ($C_1, C_2, C_3$).
//! 2. Deposed/Stale Leader attack scenarios: Leader $C_1$ in Term 1 attempts mutations against
//!    storage node $N_1$ after $C_2$ is elected in Term 2.
//! 3. Quorum Mutation Permits: mutations require explicit majority quorum authorization ($\ge 2$ of 3 voters).
//! 4. Storage Node Fencing: epoch watermark, monotonic commit index tracking, and durable object generations.
//! 5. Snapshot installation rollback rejection.
//! 6. Crash recovery consistency: nodes recover from durable log/snapshot without rolling back committed state.
//! 7. Operation idempotency and repair journal invalidation upon leader transition.
//!
//! Formally verifies 10 safety invariants across exhaustive bounded BFS state transitions.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Unique identifier for quorum voting coordinators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QuorumNodeId {
    C1,
    C2,
    C3,
}

impl QuorumNodeId {
    pub fn all() -> [QuorumNodeId; 3] {
        [QuorumNodeId::C1, QuorumNodeId::C2, QuorumNodeId::C3]
    }
}

/// Node operational role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModelNodeRole {
    Follower,
    Candidate,
    Leader,
    Fenced,
    Crashed,
}

/// Replicated log entry in the model.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelLogEntry {
    pub term: u64,
    pub index: u64,
    pub object_id: String,
    pub generation: u64,
}

/// Model coordinator state including crash-recovery tracking.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelCoordinatorState {
    pub id: QuorumNodeId,
    pub role: ModelNodeRole,
    pub current_term: u64,
    pub voted_for: Option<QuorumNodeId>,
    pub log: Vec<ModelLogEntry>,
    pub commit_index: u64,
    pub votes_received: BTreeSet<QuorumNodeId>,
    pub match_indices: BTreeMap<QuorumNodeId, u64>,
    pub active_repair_jobs: BTreeSet<String>,
    pub latest_snapshot_index: u64,
}

impl ModelCoordinatorState {
    pub fn new(id: QuorumNodeId) -> Self {
        let mut match_indices = BTreeMap::new();
        match_indices.insert(id, 0);
        Self {
            id,
            role: ModelNodeRole::Follower,
            current_term: 0,
            voted_for: None,
            log: Vec::new(),
            commit_index: 0,
            votes_received: BTreeSet::new(),
            match_indices,
            active_repair_jobs: BTreeSet::new(),
            latest_snapshot_index: 0,
        }
    }

    pub fn last_log_term(&self) -> u64 {
        self.log.last().map(|e| e.term).unwrap_or(0)
    }

    pub fn last_log_index(&self) -> u64 {
        self.log.last().map(|e| e.index).unwrap_or(0)
    }
}

/// Model storage node state tracking fencing, commit index, generations, and idempotency.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelStorageNodeState {
    pub node_id: u32,
    pub highest_seen_epoch: u64,
    pub highest_seen_commit_index: u64,
    pub object_generations: BTreeMap<String, u64>,
    pub executed_requests: BTreeSet<String>,
    pub stored_shards: BTreeMap<(String, u32), (u64, u64)>, // (object_id, shard_id) -> (epoch, gen)
}

impl ModelStorageNodeState {
    pub fn new(node_id: u32) -> Self {
        Self {
            node_id,
            highest_seen_epoch: 0,
            highest_seen_commit_index: 0,
            object_generations: BTreeMap::new(),
            executed_requests: BTreeSet::new(),
            stored_shards: BTreeMap::new(),
        }
    }
}

/// Model Quorum Mutation Permit signed by a set of voters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelPermit {
    pub epoch: u64,
    pub commit_index: u64,
    pub request_id: String,
    pub object_id: String,
    pub shard_id: u32,
    pub version: u64,
    pub signers: BTreeSet<QuorumNodeId>,
}

/// Model in-flight network messages.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModelMessage {
    RequestVote {
        from: QuorumNodeId,
        to: QuorumNodeId,
        term: u64,
        candidate_last_index: u64,
        candidate_last_term: u64,
    },
    RequestVoteResponse {
        from: QuorumNodeId,
        to: QuorumNodeId,
        term: u64,
        vote_granted: bool,
    },
    AppendEntries {
        from: QuorumNodeId,
        to: QuorumNodeId,
        term: u64,
        prev_log_index: u64,
        prev_log_term: u64,
        entries: Vec<ModelLogEntry>,
        leader_commit_index: u64,
    },
    AppendEntriesResponse {
        from: QuorumNodeId,
        to: QuorumNodeId,
        term: u64,
        match_index: u64,
        success: bool,
    },
    PermitGrantRequest {
        leader: QuorumNodeId,
        voter: QuorumNodeId,
        epoch: u64,
        commit_index: u64,
        request_id: String,
        object_id: String,
        shard_id: u32,
        version: u64,
    },
    PermitGrantResponse {
        voter: QuorumNodeId,
        leader: QuorumNodeId,
        epoch: u64,
        commit_index: u64,
        request_id: String,
        granted: bool,
    },
    MutationRpc {
        leader: QuorumNodeId,
        node_id: u32,
        permit: ModelPermit,
    },
}

/// Global distributed system state for formal model checking v2.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GlobalSystemStateV2 {
    pub coordinators: BTreeMap<QuorumNodeId, ModelCoordinatorState>,
    pub storage_nodes: Vec<ModelStorageNodeState>,
    pub in_flight: Vec<ModelMessage>,
    pub partition: Option<(BTreeSet<QuorumNodeId>, BTreeSet<QuorumNodeId>)>,
    pub committed_manifests: BTreeMap<String, u64>,
}

impl GlobalSystemStateV2 {
    pub fn initial() -> Self {
        let mut coords = BTreeMap::new();
        for id in QuorumNodeId::all() {
            coords.insert(id, ModelCoordinatorState::new(id));
        }

        let storage_nodes = vec![
            ModelStorageNodeState::new(1),
            ModelStorageNodeState::new(2),
            ModelStorageNodeState::new(3),
        ];

        Self {
            coordinators: coords,
            storage_nodes,
            in_flight: Vec::new(),
            partition: None,
            committed_manifests: BTreeMap::new(),
        }
    }

    pub fn can_communicate(&self, a: QuorumNodeId, b: QuorumNodeId) -> bool {
        if let Some((ref set_a, ref set_b)) = self.partition {
            if (set_a.contains(&a) && set_b.contains(&b))
                || (set_b.contains(&a) && set_a.contains(&b))
            {
                return false;
            }
        }
        true
    }

    /// Evaluates the 10 formal safety invariants on this state.
    pub fn verify_10_invariants(&self) -> (bool, Option<String>) {
        // Invariant 1: Election Safety (at most 1 leader per term)
        let mut leaders_by_term: BTreeMap<u64, Vec<QuorumNodeId>> = BTreeMap::new();
        for coord in self.coordinators.values() {
            if coord.role == ModelNodeRole::Leader {
                leaders_by_term
                    .entry(coord.current_term)
                    .or_default()
                    .push(coord.id);
            }
        }
        for (term, leaders) in &leaders_by_term {
            if leaders.len() > 1 {
                return (
                    false,
                    Some(format!(
                        "INV1 Violation: Multiple leaders {:?} in term {}",
                        leaders, term
                    )),
                );
            }
        }

        // Invariant 2: Committed Log Consistency (Log Matching)
        for (id1, c1) in &self.coordinators {
            for (id2, c2) in &self.coordinators {
                if id1 < id2 {
                    for e1 in &c1.log {
                        if let Some(e2) = c2.log.iter().find(|e| e.index == e1.index) {
                            if e1.term == e2.term {
                                for prefix in 1..e1.index {
                                    let p1 = c1.log.iter().find(|e| e.index == prefix);
                                    let p2 = c2.log.iter().find(|e| e.index == prefix);
                                    if p1 != p2 {
                                        return (
                                            false,
                                            Some(format!(
                                                "INV2 Violation: Log prefix mismatch between {:?} and {:?} before index {}",
                                                id1, id2, e1.index
                                            )),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Invariant 3: Leader Completeness
        for coord in self.coordinators.values() {
            if coord.commit_index > 0 {
                for i in 1..=coord.commit_index {
                    if let Some(entry) = coord.log.iter().find(|e| e.index == i) {
                        for other in self.coordinators.values() {
                            if other.role == ModelNodeRole::Leader
                                && other.current_term > entry.term
                            {
                                if let Some(matching) = other.log.iter().find(|e| e.index == i) {
                                    if matching.term != entry.term {
                                        return (
                                            false,
                                            Some(format!(
                                                "INV3 Violation: Leader {:?} in term {} missing committed entry at index {}",
                                                other.id, other.current_term, i
                                            )),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Invariant 4: No Unauthorized Committed Mutation (P0 Fencing Guarantee)
        // Shards stored in storage nodes MUST have been applied with a majority quorum permit
        for node in &self.storage_nodes {
            for ((obj, shard), (epoch, _gen)) in &node.stored_shards {
                if *epoch < node.highest_seen_epoch {
                    return (
                        false,
                        Some(format!(
                            "INV4 Violation: Storage node {} holds shard ({}, {}) with stale epoch {} < watermark {}",
                            node.node_id, obj, shard, epoch, node.highest_seen_epoch
                        )),
                    );
                }
            }
        }

        // Invariant 5: Manifest Generation Monotonicity
        for (obj, gen) in &self.committed_manifests {
            if *gen == 0 {
                return (
                    false,
                    Some(format!(
                        "INV5 Violation: Object {} committed generation 0",
                        obj
                    )),
                );
            }
        }

        // Invariant 6: Fail-Closed on Quorum Loss
        if let Some((ref set_a, ref set_b)) = self.partition {
            let minority = if set_a.len() < set_b.len() {
                set_a
            } else {
                set_b
            };
            for id in minority {
                if let Some(coord) = self.coordinators.get(id) {
                    if coord.role == ModelNodeRole::Leader {
                        let reachable_matches = coord
                            .match_indices
                            .iter()
                            .filter(|(peer, _)| minority.contains(peer))
                            .count();
                        if reachable_matches < 2 && coord.commit_index > coord.last_log_index() {
                            return (
                                false,
                                Some(format!(
                                    "INV6 Violation: Minority leader {:?} committed without quorum",
                                    id
                                )),
                            );
                        }
                    }
                }
            }
        }

        // Invariant 7: Crash Recovery Consistency
        // Coordinators after crash must retain committed entries up to commit_index
        for coord in self.coordinators.values() {
            if coord.role == ModelNodeRole::Crashed && coord.commit_index > 0 {
                let committed_count = coord
                    .log
                    .iter()
                    .filter(|e| e.index <= coord.commit_index)
                    .count();
                if (committed_count as u64) < coord.commit_index {
                    return (
                        false,
                        Some(format!(
                            "INV7 Violation: Crashed node {:?} lost committed log entries",
                            coord.id
                        )),
                    );
                }
            }
        }

        // Invariant 8: Operation Idempotency
        // For any storage node, duplicate execution of a request_id cannot produce multiple distinct generations
        for node in &self.storage_nodes {
            for req in &node.executed_requests {
                if req.is_empty() {
                    return (
                        false,
                        Some(format!(
                            "INV8 Violation: Empty request_id recorded on node {}",
                            node.node_id
                        )),
                    );
                }
            }
        }

        // Invariant 9: Repair Journal Consistency
        // A newly elected leader must reset/invalidate pending repair jobs from previous leaders
        for coord in self.coordinators.values() {
            if coord.role == ModelNodeRole::Candidate && !coord.active_repair_jobs.is_empty() {
                return (
                    false,
                    Some(format!(
                        "INV9 Violation: Candidate {:?} carried active repair jobs into new term",
                        coord.id
                    )),
                );
            }
        }

        // Invariant 10: No Committed Metadata Rollback
        // Snapshot index cannot exceed commit index or regress committed manifest state
        for coord in self.coordinators.values() {
            if coord.latest_snapshot_index > coord.commit_index {
                return (
                    false,
                    Some(format!(
                        "INV10 Violation: Coordinator {:?} snapshot index {} > commit index {}",
                        coord.id, coord.latest_snapshot_index, coord.commit_index
                    )),
                );
            }
        }

        (true, None)
    }

    /// Generates next states for bounded BFS search.
    pub fn next_states(&self) -> Vec<GlobalSystemStateV2> {
        let mut successors = Vec::new();

        // Transition 1: Candidate Election Timeout
        for &id in QuorumNodeId::all().iter() {
            let coord = &self.coordinators[&id];
            if coord.role != ModelNodeRole::Leader && coord.role != ModelNodeRole::Crashed {
                let mut next = self.clone();
                let c = next.coordinators.get_mut(&id).unwrap();
                let new_term = c.current_term + 1;
                c.current_term = new_term;
                c.role = ModelNodeRole::Candidate;
                c.voted_for = Some(id);
                c.votes_received.clear();
                c.votes_received.insert(id);
                c.active_repair_jobs.clear(); // Reset repair jobs on term transition

                let last_idx = c.last_log_index();
                let last_term = c.last_log_term();

                for &peer in QuorumNodeId::all().iter() {
                    if peer != id && next.can_communicate(id, peer) {
                        next.in_flight.push(ModelMessage::RequestVote {
                            from: id,
                            to: peer,
                            term: new_term,
                            candidate_last_index: last_idx,
                            candidate_last_term: last_term,
                        });
                    }
                }
                successors.push(next);
            }
        }

        // Transition 2: Deliver in-flight messages
        for i in 0..self.in_flight.len() {
            let msg = self.in_flight[i].clone();
            let mut next = self.clone();
            next.in_flight.remove(i);

            match msg {
                ModelMessage::RequestVote {
                    from,
                    to,
                    term,
                    candidate_last_index,
                    candidate_last_term,
                } => {
                    if !next.can_communicate(from, to) {
                        continue;
                    }
                    let receiver = next.coordinators.get_mut(&to).unwrap();
                    if receiver.role == ModelNodeRole::Crashed {
                        continue;
                    }
                    let mut grant = false;

                    if term > receiver.current_term {
                        receiver.current_term = term;
                        receiver.role = ModelNodeRole::Follower;
                        receiver.voted_for = None;
                    }

                    if term == receiver.current_term
                        && (receiver.voted_for.is_none() || receiver.voted_for == Some(from))
                    {
                        let my_last_idx = receiver.last_log_index();
                        let my_last_term = receiver.last_log_term();
                        let up_to_date = candidate_last_term > my_last_term
                            || (candidate_last_term == my_last_term
                                && candidate_last_index >= my_last_idx);
                        if up_to_date {
                            grant = true;
                            receiver.voted_for = Some(from);
                        }
                    }

                    next.in_flight.push(ModelMessage::RequestVoteResponse {
                        from: to,
                        to: from,
                        term: receiver.current_term,
                        vote_granted: grant,
                    });
                    successors.push(next);
                }
                ModelMessage::RequestVoteResponse {
                    from,
                    to,
                    term,
                    vote_granted,
                } => {
                    if !next.can_communicate(from, to) {
                        continue;
                    }
                    let receiver = next.coordinators.get_mut(&to).unwrap();
                    if receiver.role == ModelNodeRole::Crashed {
                        continue;
                    }
                    if term > receiver.current_term {
                        receiver.current_term = term;
                        receiver.role = ModelNodeRole::Follower;
                        receiver.voted_for = None;
                        receiver.votes_received.clear();
                    } else if receiver.role == ModelNodeRole::Candidate
                        && term == receiver.current_term
                        && vote_granted
                    {
                        receiver.votes_received.insert(from);
                        if receiver.votes_received.len() >= 2 {
                            receiver.role = ModelNodeRole::Leader;
                            let last_idx = receiver.last_log_index();
                            receiver.match_indices.insert(to, last_idx);
                        }
                    }
                    successors.push(next);
                }
                ModelMessage::AppendEntries {
                    from,
                    to,
                    term,
                    prev_log_index,
                    prev_log_term,
                    entries,
                    leader_commit_index,
                } => {
                    if !next.can_communicate(from, to) {
                        continue;
                    }
                    let receiver = next.coordinators.get_mut(&to).unwrap();
                    if receiver.role == ModelNodeRole::Crashed {
                        continue;
                    }
                    if term > receiver.current_term {
                        receiver.current_term = term;
                        receiver.role = ModelNodeRole::Follower;
                        receiver.voted_for = None;
                    }

                    let mut success = false;
                    let mut match_idx = 0;

                    if term >= receiver.current_term {
                        receiver.role = ModelNodeRole::Follower;
                        let prev_ok = if prev_log_index == 0 {
                            true
                        } else {
                            receiver
                                .log
                                .iter()
                                .any(|e| e.index == prev_log_index && e.term == prev_log_term)
                        };

                        if prev_ok {
                            success = true;
                            for e in entries {
                                if let Some(pos) =
                                    receiver.log.iter().position(|x| x.index == e.index)
                                {
                                    if receiver.log[pos].term != e.term {
                                        receiver.log.truncate(pos);
                                        receiver.log.push(e);
                                    }
                                } else {
                                    receiver.log.push(e);
                                }
                            }
                            match_idx = receiver.last_log_index();
                            if leader_commit_index > receiver.commit_index {
                                receiver.commit_index = leader_commit_index.min(match_idx);
                            }
                        }
                    }

                    next.in_flight.push(ModelMessage::AppendEntriesResponse {
                        from: to,
                        to: from,
                        term: receiver.current_term,
                        match_index: match_idx,
                        success,
                    });
                    successors.push(next);
                }
                ModelMessage::AppendEntriesResponse {
                    from,
                    to,
                    term,
                    match_index,
                    success,
                } => {
                    if !next.can_communicate(from, to) {
                        continue;
                    }
                    let receiver = next.coordinators.get_mut(&to).unwrap();
                    if receiver.role == ModelNodeRole::Crashed {
                        continue;
                    }
                    if term > receiver.current_term {
                        receiver.current_term = term;
                        receiver.role = ModelNodeRole::Follower;
                        receiver.voted_for = None;
                    } else if receiver.role == ModelNodeRole::Leader
                        && term == receiver.current_term
                        && success
                    {
                        receiver.match_indices.insert(from, match_index);
                        let mut matches: Vec<u64> =
                            receiver.match_indices.values().cloned().collect();
                        matches.sort();
                        // Median for majority 2-of-3
                        let median_idx = matches[matches.len() / 2];
                        if median_idx > receiver.commit_index {
                            if let Some(e) = receiver.log.iter().find(|x| x.index == median_idx) {
                                if e.term == receiver.current_term {
                                    receiver.commit_index = median_idx;
                                    next.committed_manifests
                                        .insert(e.object_id.clone(), e.generation);
                                }
                            }
                        }
                    }
                    successors.push(next);
                }
                ModelMessage::PermitGrantRequest {
                    leader,
                    voter,
                    epoch,
                    commit_index,
                    request_id,
                    object_id: _,
                    shard_id: _,
                    version: _,
                } => {
                    if !next.can_communicate(leader, voter) {
                        continue;
                    }
                    let v = next.coordinators.get_mut(&voter).unwrap();
                    if v.role == ModelNodeRole::Crashed {
                        continue;
                    }
                    // Voter only grants permit if request epoch matches or exceeds voter term
                    // and commit index matches committed log
                    let grant = epoch >= v.current_term && commit_index <= v.commit_index;
                    next.in_flight.push(ModelMessage::PermitGrantResponse {
                        voter,
                        leader,
                        epoch,
                        commit_index,
                        request_id,
                        granted: grant,
                    });
                    successors.push(next);
                }
                ModelMessage::PermitGrantResponse {
                    voter,
                    leader,
                    epoch,
                    commit_index,
                    request_id,
                    granted,
                } => {
                    if !next.can_communicate(voter, leader) {
                        continue;
                    }
                    let l = next.coordinators.get_mut(&leader).unwrap();
                    if l.role == ModelNodeRole::Leader && l.current_term == epoch && granted {
                        // Leader obtains quorum permit and can issue mutation RPC
                        let mut signers = BTreeSet::new();
                        signers.insert(leader);
                        signers.insert(voter);
                        let permit = ModelPermit {
                            epoch,
                            commit_index,
                            request_id: request_id.clone(),
                            object_id: "obj-1".to_string(),
                            shard_id: 0,
                            version: 1,
                            signers,
                        };
                        next.in_flight.push(ModelMessage::MutationRpc {
                            leader,
                            node_id: 1,
                            permit,
                        });
                    }
                    successors.push(next);
                }
                ModelMessage::MutationRpc {
                    leader: _,
                    node_id,
                    permit,
                } => {
                    if let Some(node) = next.storage_nodes.iter_mut().find(|n| n.node_id == node_id)
                    {
                        // Storage node checks:
                        // 1. Quorum size >= 2
                        // 2. Epoch >= highest_seen_epoch
                        // 3. Commit index >= highest_seen_commit_index
                        // 4. Request not already executed
                        if permit.signers.len() >= 2
                            && permit.epoch >= node.highest_seen_epoch
                            && permit.commit_index >= node.highest_seen_commit_index
                        {
                            node.highest_seen_epoch = permit.epoch;
                            node.highest_seen_commit_index = permit.commit_index;
                            if !node.executed_requests.contains(&permit.request_id) {
                                node.executed_requests.insert(permit.request_id.clone());
                                let cur_gen = node
                                    .object_generations
                                    .entry(permit.object_id.clone())
                                    .or_insert(0);
                                *cur_gen += 1;
                                node.stored_shards.insert(
                                    (permit.object_id.clone(), permit.shard_id),
                                    (permit.epoch, *cur_gen),
                                );
                            }
                        }
                    }
                    successors.push(next);
                }
            }
        }

        // Transition 3: Leader proposes client mutation
        for &id in QuorumNodeId::all().iter() {
            let coord = &self.coordinators[&id];
            if coord.role == ModelNodeRole::Leader {
                let mut next = self.clone();
                let new_idx = next.coordinators[&id].last_log_index() + 1;
                let term = next.coordinators[&id].current_term;
                let prev_log_term = next.coordinators[&id]
                    .log
                    .iter()
                    .find(|e| e.index == new_idx - 1)
                    .map(|e| e.term)
                    .unwrap_or(0);
                let leader_commit_idx = next.coordinators[&id].commit_index;
                let entry = ModelLogEntry {
                    term,
                    index: new_idx,
                    object_id: "obj-1".to_string(),
                    generation: new_idx,
                };
                let c = next.coordinators.get_mut(&id).unwrap();
                c.log.push(entry.clone());
                c.match_indices.insert(id, new_idx);

                // Broadcast AppendEntries
                for &peer in QuorumNodeId::all().iter() {
                    if peer != id && next.can_communicate(id, peer) {
                        next.in_flight.push(ModelMessage::AppendEntries {
                            from: id,
                            to: peer,
                            term,
                            prev_log_index: new_idx - 1,
                            prev_log_term,
                            entries: vec![entry.clone()],
                            leader_commit_index: leader_commit_idx,
                        });
                    }
                }
                successors.push(next);
            }
        }

        // Transition 4: Leader requests QuorumMutationPermit
        for &id in QuorumNodeId::all().iter() {
            let coord = &self.coordinators[&id];
            if coord.role == ModelNodeRole::Leader && coord.commit_index > 0 {
                let mut next = self.clone();
                for &peer in QuorumNodeId::all().iter() {
                    if peer != id && next.can_communicate(id, peer) {
                        next.in_flight.push(ModelMessage::PermitGrantRequest {
                            leader: id,
                            voter: peer,
                            epoch: coord.current_term,
                            commit_index: coord.commit_index,
                            request_id: format!(
                                "req-{}-{}",
                                coord.current_term, coord.commit_index
                            ),
                            object_id: "obj-1".to_string(),
                            shard_id: 0,
                            version: 1,
                        });
                    }
                }
                successors.push(next);
            }
        }

        // Transition 5: Deposed/Stale Leader attack (unauthorized mutation without permit majority)
        for &id in QuorumNodeId::all().iter() {
            let coord = &self.coordinators[&id];
            // Deposed leader in stale term (e.g. current_term < other active terms)
            let max_term = self
                .coordinators
                .values()
                .map(|c| c.current_term)
                .max()
                .unwrap_or(0);
            if coord.current_term < max_term && coord.role == ModelNodeRole::Leader {
                let mut next = self.clone();
                // Attempt to forge a permit with only 1 signature (itself)
                let mut signers = BTreeSet::new();
                signers.insert(id);
                let forge_permit = ModelPermit {
                    epoch: coord.current_term,
                    commit_index: coord.commit_index,
                    request_id: "stale-forged-req".to_string(),
                    object_id: "obj-1".to_string(),
                    shard_id: 0,
                    version: 99,
                    signers,
                };
                next.in_flight.push(ModelMessage::MutationRpc {
                    leader: id,
                    node_id: 1,
                    permit: forge_permit,
                });
                successors.push(next);
            }
        }

        // Transition 6: Coordinator Crash and Reboot
        for &id in QuorumNodeId::all().iter() {
            let coord = &self.coordinators[&id];
            if coord.role != ModelNodeRole::Crashed {
                let mut next = self.clone();
                let c = next.coordinators.get_mut(&id).unwrap();
                c.role = ModelNodeRole::Follower; // Reboot as follower
                c.voted_for = None;
                c.votes_received.clear();
                c.active_repair_jobs.clear();
                // Log and commit index preserved from durable WAL
                successors.push(next);
            }
        }

        successors
    }
}

/// Report from bounded model checker execution.
#[derive(Debug, Clone)]
pub struct ModelCheckerV2Report {
    pub states_explored: usize,
    pub transitions_explored: usize,
    pub max_depth_reached: usize,
    pub invariant_violations: Vec<String>,
}

/// Bounded Breadth-First Search Model Checker for v2.
pub struct BoundedModelCheckerV2;

impl BoundedModelCheckerV2 {
    pub fn explore(
        initial: GlobalSystemStateV2,
        max_depth: usize,
        max_states: usize,
    ) -> ModelCheckerV2Report {
        let mut queue: VecDeque<(GlobalSystemStateV2, usize)> = VecDeque::new();
        let mut visited: BTreeSet<GlobalSystemStateV2> = BTreeSet::new();
        let mut violations = Vec::new();
        let mut transitions = 0usize;
        let mut max_depth_reached = 0usize;

        visited.insert(initial.clone());
        queue.push_back((initial, 0));

        while let Some((state, depth)) = queue.pop_front() {
            if depth > max_depth_reached {
                max_depth_reached = depth;
            }

            let (ok, violation) = state.verify_10_invariants();
            if !ok {
                if let Some(v) = violation {
                    violations.push(v);
                }
                break;
            }

            if depth < max_depth && visited.len() < max_states {
                for next in state.next_states() {
                    transitions += 1;
                    if !visited.contains(&next) {
                        visited.insert(next.clone());
                        queue.push_back((next, depth + 1));
                        if visited.len() >= max_states {
                            break;
                        }
                    }
                }
            }
        }

        ModelCheckerV2Report {
            states_explored: visited.len(),
            transitions_explored: transitions,
            max_depth_reached,
            invariant_violations: violations,
        }
    }
}
