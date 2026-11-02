//! Coordinator High Availability Leadership, Quorum Consensus & Epoch Authority.
//!
//! Enforces a strict 3-member metadata quorum to govern leadership epochs and prevent
//! split-brain behavior across coordinator failovers.
//!
//! Guarantees:
//! - Monotonically increasing leadership epochs backed by durable on-disk state.
//! - Majority quorum consensus ($\ge 2$ out of 3 votes required to acquire leadership).
//! - Fail-closed semantics: if a coordinator loses quorum, all mutating operations are blocked.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::security::signatures::{decode_hex_exact, encode_hex};
use crate::storage::backend::StorageError;
use crate::storage::filesystem::{sync_directory, write_and_sync_file};

/// Unique identifier for a coordinator or quorum witness node.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CoordinatorId(pub String);

impl CoordinatorId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        self.as_ref()
    }
}

impl AsRef<str> for CoordinatorId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CoordinatorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Current role of a coordinator instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoordinatorRole {
    /// Active leader holding an authorized leadership epoch backed by quorum consensus.
    Leader { epoch: u64, acquired_unix_secs: u64 },
    /// Standby replica shadowing metadata from the active leader.
    Standby {
        observed_leader: Option<CoordinatorId>,
        observed_epoch: u64,
    },
    /// Candidate attempting to acquire leadership by requesting quorum votes.
    Candidate {
        proposed_epoch: u64,
        votes_received: BTreeSet<CoordinatorId>,
    },
    /// Fenced or unauthoritative state due to quorum loss or superseded epoch (fail-closed).
    Fenced {
        reason: String,
        superseding_epoch: Option<u64>,
    },
}

impl CoordinatorRole {
    pub fn is_leader(&self) -> bool {
        matches!(self, Self::Leader { .. })
    }

    pub fn current_epoch(&self) -> Option<u64> {
        match self {
            Self::Leader { epoch, .. } => Some(*epoch),
            Self::Standby { observed_epoch, .. } => Some(*observed_epoch),
            Self::Candidate { proposed_epoch, .. } => Some(*proposed_epoch),
            Self::Fenced {
                superseding_epoch, ..
            } => *superseding_epoch,
        }
    }
}

/// Request sent to a quorum peer to solicit a leadership vote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuorumVoteRequest {
    pub candidate_id: CoordinatorId,
    pub proposed_epoch: u64,
    pub last_committed_manifest_gen: u64,
}

/// Response returned by a quorum peer evaluating a vote request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuorumVoteResponse {
    pub voter_id: CoordinatorId,
    pub vote_granted: bool,
    pub highest_seen_epoch: u64,
    pub reason: Option<String>,
}

/// A cryptographically signed vote grant issued by a quorum voter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuorumVoteGrant {
    pub voter_id: CoordinatorId,
    pub epoch: u64,
    pub candidate_id: CoordinatorId,
    pub signature_hex: String,
}

impl QuorumVoteGrant {
    pub const DOMAIN_TAG: &'static str = "FSP-QUORUM-VOTE-V1";

    pub fn canonical_message(
        epoch: u64,
        candidate_id: &CoordinatorId,
        voter_id: &CoordinatorId,
    ) -> Vec<u8> {
        format!(
            "{}:{}:{}:{}",
            Self::DOMAIN_TAG,
            epoch,
            candidate_id.as_str(),
            voter_id.as_str()
        )
        .into_bytes()
    }

    pub fn create(
        voter_id: CoordinatorId,
        epoch: u64,
        candidate_id: CoordinatorId,
        signing_key: &SigningKey,
    ) -> Self {
        let msg = Self::canonical_message(epoch, &candidate_id, &voter_id);
        let sig = signing_key.sign(&msg);
        let signature_hex = encode_hex(&sig.to_bytes());
        Self {
            voter_id,
            epoch,
            candidate_id,
            signature_hex,
        }
    }

    pub fn verify(&self, expected_voter_key: &VerifyingKey) -> Result<(), StorageError> {
        let msg = Self::canonical_message(self.epoch, &self.candidate_id, &self.voter_id);
        let sig_bytes = decode_hex_exact::<64>(&self.signature_hex).map_err(|e| {
            StorageError::CorruptMetadata(format!("invalid vote signature hex: {}", e))
        })?;
        let sig = Signature::from_bytes(&sig_bytes);
        expected_voter_key
            .verify_strict(&msg, &sig)
            .map_err(|_| StorageError::FencingViolation {
                current_epoch: self.epoch,
                attempted_epoch: self.epoch,
            })
    }

    pub fn to_wire_string(&self) -> String {
        format!(
            "GRANT:{}:{}:{}:{}",
            self.epoch,
            self.candidate_id.as_str(),
            self.voter_id.as_str(),
            self.signature_hex
        )
    }

    pub fn from_wire_string(s: &str) -> Result<Self, StorageError> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 5 || parts[0] != "GRANT" {
            return Err(StorageError::CorruptMetadata(
                "invalid vote grant wire format".into(),
            ));
        }
        let epoch = parts[1]
            .parse::<u64>()
            .map_err(|e| StorageError::CorruptMetadata(e.to_string()))?;
        let candidate_id = CoordinatorId::new(parts[2]);
        let voter_id = CoordinatorId::new(parts[3]);
        let signature_hex = parts[4].to_string();
        Ok(Self {
            voter_id,
            epoch,
            candidate_id,
            signature_hex,
        })
    }
}

/// A verifiable certificate proving majority quorum authorization for a leadership epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadershipCertificate {
    pub epoch: u64,
    pub leader_id: CoordinatorId,
    pub grants: Vec<QuorumVoteGrant>,
}

impl LeadershipCertificate {
    pub fn new(epoch: u64, leader_id: CoordinatorId, grants: Vec<QuorumVoteGrant>) -> Self {
        Self {
            epoch,
            leader_id,
            grants,
        }
    }

    /// Validates that this certificate represents an authoritative quorum majority.
    pub fn verify_quorum(
        &self,
        voter_keys: &BTreeMap<CoordinatorId, VerifyingKey>,
        threshold: usize,
    ) -> Result<(), StorageError> {
        if self.grants.len() < threshold {
            return Err(StorageError::FencingViolation {
                current_epoch: self.epoch,
                attempted_epoch: self.epoch,
            });
        }

        let mut seen_voters = BTreeSet::new();
        for grant in &self.grants {
            if grant.epoch != self.epoch || grant.candidate_id != self.leader_id {
                return Err(StorageError::FencingViolation {
                    current_epoch: self.epoch,
                    attempted_epoch: grant.epoch,
                });
            }
            if !seen_voters.insert(grant.voter_id.clone()) {
                return Err(StorageError::CorruptMetadata(format!(
                    "duplicate vote from voter {}",
                    grant.voter_id
                )));
            }
            let voter_key = voter_keys.get(&grant.voter_id).ok_or_else(|| {
                StorageError::CorruptMetadata(format!("unknown voter {}", grant.voter_id))
            })?;
            grant.verify(voter_key)?;
        }

        if seen_voters.len() < threshold {
            return Err(StorageError::FencingViolation {
                current_epoch: self.epoch,
                attempted_epoch: self.epoch,
            });
        }

        Ok(())
    }

    pub fn to_wire_string(&self) -> String {
        let grants_str = self
            .grants
            .iter()
            .map(|g| g.to_wire_string())
            .collect::<Vec<_>>()
            .join(";");
        format!(
            "LEADERCERT:{}:{}:{}",
            self.epoch,
            self.leader_id.as_str(),
            grants_str
        )
    }

    pub fn from_wire_string(s: &str) -> Result<Self, StorageError> {
        let parts: Vec<&str> = s.splitn(4, ':').collect();
        if parts.len() != 4 || parts[0] != "LEADERCERT" {
            return Err(StorageError::CorruptMetadata(
                "invalid leadership cert wire format".into(),
            ));
        }
        let epoch = parts[1]
            .parse::<u64>()
            .map_err(|e| StorageError::CorruptMetadata(e.to_string()))?;
        let leader_id = CoordinatorId::new(parts[2]);
        let grants_raw = parts[3];
        let mut grants = Vec::new();
        if !grants_raw.is_empty() {
            for g_str in grants_raw.split(';') {
                if !g_str.is_empty() {
                    grants.push(QuorumVoteGrant::from_wire_string(g_str)?);
                }
            }
        }
        Ok(Self {
            epoch,
            leader_id,
            grants,
        })
    }
}

/// Persistent epoch record stored on disk under `<coord_root>/leadership/epoch.rec`.
#[derive(Debug, Clone)]
pub struct DurableEpochStore {
    file_path: PathBuf,
    current_epoch: Arc<AtomicU64>,
}

impl DurableEpochStore {
    pub const FILE_NAME: &'static str = "epoch.rec";

    /// Opens or initializes the epoch store inside `<coord_root>/leadership/`.
    pub fn open_or_init(dir: &Path) -> Result<Self, StorageError> {
        let leadership_dir = dir.join("leadership");
        fs::create_dir_all(&leadership_dir).map_err(StorageError::Io)?;

        let file_path = leadership_dir.join(Self::FILE_NAME);
        let current_epoch = if file_path.exists() {
            let mut file = File::open(&file_path).map_err(StorageError::Io)?;
            let mut content = String::new();
            file.read_to_string(&mut content)
                .map_err(StorageError::Io)?;
            Self::parse_epoch(&content)?
        } else {
            let init_val = 0u64;
            let text = format!("EPOCH={}\nUPDATED_UNIX_SECS=0\n", init_val);
            write_and_sync_file(&file_path, text.as_bytes())?;
            sync_directory(&leadership_dir)?;
            init_val
        };

        Ok(Self {
            file_path,
            current_epoch: Arc::new(AtomicU64::new(current_epoch)),
        })
    }

    fn parse_epoch(content: &str) -> Result<u64, StorageError> {
        for line in content.lines() {
            if let Some(val) = line.strip_prefix("EPOCH=") {
                return val
                    .trim()
                    .parse::<u64>()
                    .map_err(|e| StorageError::CorruptMetadata(format!("invalid epoch: {}", e)));
            }
        }
        Err(StorageError::CorruptMetadata(
            "missing EPOCH= header in epoch.rec".into(),
        ))
    }

    pub fn current_epoch(&self) -> u64 {
        self.current_epoch.load(Ordering::SeqCst)
    }

    /// Atomically advances the stored epoch to `new_epoch` and fsyncs to disk.
    /// Fails if `new_epoch <= current_epoch`.
    pub fn advance_epoch(&self, new_epoch: u64, unix_secs: u64) -> Result<(), StorageError> {
        let cur = self.current_epoch.load(Ordering::SeqCst);
        if new_epoch <= cur {
            return Err(StorageError::FencingViolation {
                current_epoch: cur,
                attempted_epoch: new_epoch,
            });
        }

        let text = format!("EPOCH={}\nUPDATED_UNIX_SECS={}\n", new_epoch, unix_secs);
        write_and_sync_file(&self.file_path, text.as_bytes())?;
        if let Some(parent) = self.file_path.parent() {
            let _ = sync_directory(parent);
        }

        self.current_epoch.store(new_epoch, Ordering::SeqCst);
        Ok(())
    }
}

/// Metadata Quorum state machine managing 3-member consensus.
pub struct QuorumState {
    pub local_id: CoordinatorId,
    pub peers: BTreeSet<CoordinatorId>,
    pub epoch_store: DurableEpochStore,
    pub role: RwLock<CoordinatorRole>,
    pub voted_for_in_epoch: RwLock<Option<(u64, CoordinatorId)>>,
    pub leadership_cert: RwLock<Option<LeadershipCertificate>>,
    pub collected_grants: RwLock<BTreeMap<u64, Vec<QuorumVoteGrant>>>,
}

impl QuorumState {
    /// Creates a new quorum coordinator with a set of peers (must total 3 members including local).
    pub fn new(
        local_id: CoordinatorId,
        peers: BTreeSet<CoordinatorId>,
        storage_root: &Path,
    ) -> Result<Self, StorageError> {
        let epoch_store = DurableEpochStore::open_or_init(storage_root)?;
        let init_epoch = epoch_store.current_epoch();

        let initial_role = CoordinatorRole::Standby {
            observed_leader: None,
            observed_epoch: init_epoch,
        };

        Ok(Self {
            local_id,
            peers,
            epoch_store,
            role: RwLock::new(initial_role),
            voted_for_in_epoch: RwLock::new(None),
            leadership_cert: RwLock::new(None),
            collected_grants: RwLock::new(BTreeMap::new()),
        })
    }

    /// Total voting members = peers + self.
    pub fn cluster_size(&self) -> usize {
        self.peers.len() + 1
    }

    /// Majority threshold for quorum: $\lfloor N/2 \rfloor + 1$.
    pub fn quorum_threshold(&self) -> usize {
        (self.cluster_size() / 2) + 1
    }

    pub fn current_role(&self) -> CoordinatorRole {
        self.role.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn current_epoch(&self) -> u64 {
        self.epoch_store.current_epoch()
    }

    /// Evaluates an incoming vote request from a candidate peer.
    pub fn handle_vote_request(&self, req: &QuorumVoteRequest) -> QuorumVoteResponse {
        let current_ep = self.epoch_store.current_epoch();

        // 1. Candidate's proposed epoch must be strictly greater than current epoch
        if req.proposed_epoch <= current_ep {
            return QuorumVoteResponse {
                voter_id: self.local_id.clone(),
                vote_granted: false,
                highest_seen_epoch: current_ep,
                reason: Some(format!(
                    "proposed epoch {} <= current epoch {}",
                    req.proposed_epoch, current_ep
                )),
            };
        }

        // 2. Check if we have already voted for a different candidate in this epoch
        let mut voted = self
            .voted_for_in_epoch
            .write()
            .unwrap_or_else(|p| p.into_inner());
        if let Some((voted_ep, ref candidate)) = *voted {
            if voted_ep == req.proposed_epoch && *candidate != req.candidate_id {
                return QuorumVoteResponse {
                    voter_id: self.local_id.clone(),
                    vote_granted: false,
                    highest_seen_epoch: current_ep,
                    reason: Some(format!(
                        "already voted for {} in epoch {}",
                        candidate, voted_ep
                    )),
                };
            }
        }

        // 3. Grant vote and advance local epoch record
        *voted = Some((req.proposed_epoch, req.candidate_id.clone()));
        if let Err(e) = self.epoch_store.advance_epoch(req.proposed_epoch, 0) {
            return QuorumVoteResponse {
                voter_id: self.local_id.clone(),
                vote_granted: false,
                highest_seen_epoch: self.epoch_store.current_epoch(),
                reason: Some(format!("failed to persist epoch advance: {}", e)),
            };
        }

        // Transition local role to standby tracking candidate
        let mut role = self.role.write().unwrap_or_else(|p| p.into_inner());
        *role = CoordinatorRole::Standby {
            observed_leader: Some(req.candidate_id.clone()),
            observed_epoch: req.proposed_epoch,
        };

        QuorumVoteResponse {
            voter_id: self.local_id.clone(),
            vote_granted: true,
            highest_seen_epoch: req.proposed_epoch,
            reason: None,
        }
    }

    /// Transitions to Candidate and self-votes for `proposed_epoch`.
    pub fn start_election(&self, proposed_epoch: u64) -> Result<QuorumVoteRequest, StorageError> {
        let cur_ep = self.epoch_store.current_epoch();
        if proposed_epoch <= cur_ep {
            return Err(StorageError::FencingViolation {
                current_epoch: cur_ep,
                attempted_epoch: proposed_epoch,
            });
        }

        let mut votes = BTreeSet::new();
        votes.insert(self.local_id.clone()); // Vote for self

        let mut role = self.role.write().unwrap_or_else(|p| p.into_inner());
        *role = CoordinatorRole::Candidate {
            proposed_epoch,
            votes_received: votes,
        };

        *self
            .voted_for_in_epoch
            .write()
            .unwrap_or_else(|p| p.into_inner()) = Some((proposed_epoch, self.local_id.clone()));

        Ok(QuorumVoteRequest {
            candidate_id: self.local_id.clone(),
            proposed_epoch,
            last_committed_manifest_gen: 0,
        })
    }

    /// Records a received vote and claims leadership if majority threshold is achieved.
    pub fn record_vote(
        &self,
        voter: CoordinatorId,
        vote_granted: bool,
        unix_secs: u64,
    ) -> Result<CoordinatorRole, StorageError> {
        let mut role = self.role.write().unwrap_or_else(|p| p.into_inner());
        let threshold = self.quorum_threshold();

        match &mut *role {
            CoordinatorRole::Candidate {
                proposed_epoch,
                votes_received,
            } => {
                if vote_granted {
                    votes_received.insert(voter);
                }

                if votes_received.len() >= threshold {
                    // Quorum achieved! Advance durable epoch store and become Leader
                    let won_epoch = *proposed_epoch;
                    self.epoch_store.advance_epoch(won_epoch, unix_secs)?;

                    let new_role = CoordinatorRole::Leader {
                        epoch: won_epoch,
                        acquired_unix_secs: unix_secs,
                    };
                    *role = new_role.clone();
                    Ok(new_role)
                } else {
                    Ok(role.clone())
                }
            }
            other => Ok(other.clone()),
        }
    }

    /// Enforces fail-closed behavior: steps down from Leader to Fenced.
    pub fn step_down_to_fenced(&self, reason: String, superseding_epoch: Option<u64>) {
        let mut role = self.role.write().unwrap_or_else(|p| p.into_inner());
        *role = CoordinatorRole::Fenced {
            reason,
            superseding_epoch,
        };
        *self
            .leadership_cert
            .write()
            .unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// Returns the currently active [`LeadershipCertificate`], if leader.
    pub fn leadership_certificate(&self) -> Option<LeadershipCertificate> {
        self.leadership_cert
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Evaluates an incoming vote request and, if granted, returns a cryptographically signed [`QuorumVoteGrant`].
    pub fn handle_vote_request_signed(
        &self,
        req: &QuorumVoteRequest,
        voter_signing_key: &SigningKey,
    ) -> Result<QuorumVoteGrant, StorageError> {
        let resp = self.handle_vote_request(req);
        if !resp.vote_granted {
            return Err(StorageError::FencingViolation {
                current_epoch: self.epoch_store.current_epoch(),
                attempted_epoch: req.proposed_epoch,
            });
        }
        Ok(QuorumVoteGrant::create(
            self.local_id.clone(),
            req.proposed_epoch,
            req.candidate_id.clone(),
            voter_signing_key,
        ))
    }

    /// Records a cryptographically signed vote grant from a quorum peer.
    /// If the majority threshold is satisfied with valid signatures, advances the durable epoch store,
    /// sets the local role to Leader, stores the [`LeadershipCertificate`], and returns it.
    pub fn record_signed_grant(
        &self,
        grant: QuorumVoteGrant,
        voter_keys: &BTreeMap<CoordinatorId, VerifyingKey>,
        unix_secs: u64,
    ) -> Result<Option<LeadershipCertificate>, StorageError> {
        let expected_key = voter_keys.get(&grant.voter_id).ok_or_else(|| {
            StorageError::CorruptMetadata(format!("unknown voter {}", grant.voter_id))
        })?;
        grant.verify(expected_key)?;

        let mut collected = self
            .collected_grants
            .write()
            .unwrap_or_else(|p| p.into_inner());
        let list = collected.entry(grant.epoch).or_default();
        if !list.iter().any(|g| g.voter_id == grant.voter_id) {
            list.push(grant.clone());
        }

        let threshold = self.quorum_threshold();
        if list.len() >= threshold {
            let cert = LeadershipCertificate::new(grant.epoch, grant.candidate_id, list.clone());
            cert.verify_quorum(voter_keys, threshold)?;

            // Advance durable epoch store
            self.epoch_store.advance_epoch(grant.epoch, unix_secs)?;

            let mut role = self.role.write().unwrap_or_else(|p| p.into_inner());
            *role = CoordinatorRole::Leader {
                epoch: grant.epoch,
                acquired_unix_secs: unix_secs,
            };

            *self
                .leadership_cert
                .write()
                .unwrap_or_else(|p| p.into_inner()) = Some(cert.clone());

            Ok(Some(cert))
        } else {
            Ok(None)
        }
    }
}
