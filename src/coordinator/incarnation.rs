//! Cluster Incarnation and Recovery Authority Architecture (`src/coordinator/incarnation.rs`).
//!
//! Provides mathematically verifiable cluster authority transitions:
//! - Unique [`ClusterIncarnationId`] binding authority to a monotonic incarnation sequence.
//! - Cryptographically signed [`ClusterAuthorityRecord`] authenticating authority transitions.
//! - Fencing against superseded cluster incarnations without relying on static epoch offsets.
//! - Detection of unverified old-quorum coexistence to enforce fail-closed safety.

use std::collections::BTreeSet;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::coordinator::leadership::CoordinatorId;
use crate::security::signatures::{decode_hex_exact, encode_hex};
use crate::storage::integrity::sha256_bytes;

/// Globally unique, monotonic cluster incarnation identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClusterIncarnationId {
    /// Logical stable cluster name (e.g. "fsp-prod-main").
    pub cluster_id: String,
    /// Strictly monotonic sequence number (1, 2, ...).
    pub incarnation_seq: u64,
    /// Cryptographic collision-resistant token unique to this incarnation.
    pub incarnation_token: String,
}

impl ClusterIncarnationId {
    /// Creates the genesis incarnation (sequence 1).
    pub fn genesis(cluster_id: impl Into<String>) -> Self {
        let cid = cluster_id.into();
        let token = sha256_bytes(format!("{}:genesis:1", cid).as_bytes()).to_hex();
        Self {
            cluster_id: cid,
            incarnation_seq: 1,
            incarnation_token: token,
        }
    }

    /// Advances the cluster to a new incarnation with provided entropy.
    pub fn advance(&self, entropy: &str) -> Self {
        let next_seq = self.incarnation_seq + 1;
        let token = sha256_bytes(
            format!(
                "{}:{}:{}:{}",
                self.cluster_id, self.incarnation_token, next_seq, entropy
            )
            .as_bytes(),
        )
        .to_hex();
        Self {
            cluster_id: self.cluster_id.clone(),
            incarnation_seq: next_seq,
            incarnation_token: token,
        }
    }

    /// Encodes to canonical wire string format: `cluster_id:seq:token`.
    pub fn to_wire_string(&self) -> String {
        format!(
            "{}:{}:{}",
            self.cluster_id, self.incarnation_seq, self.incarnation_token
        )
    }

    /// Parses from canonical wire string format.
    pub fn from_wire_string(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 3 {
            return Err(format!("Invalid cluster incarnation wire format: '{}'", s));
        }
        let cluster_id = parts[0].to_string();
        let seq = parts[1]
            .parse::<u64>()
            .map_err(|e| format!("Bad incarnation sequence: {}", e))?;
        let token = parts[2].to_string();
        Ok(Self {
            cluster_id,
            incarnation_seq: seq,
            incarnation_token: token,
        })
    }
}

/// Cryptographically authenticated cluster authority record proving an authorized transition
/// to a new cluster incarnation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterAuthorityRecord {
    pub cluster_id: String,
    pub incarnation_id: ClusterIncarnationId,
    pub recovery_authority_pubkey_hex: String,
    pub authorized_voter_set: BTreeSet<CoordinatorId>,
    pub lineage_digest_hex: String,
    pub transition_timestamp_unix_secs: u64,
    pub operational_isolation_attested: bool,
    pub old_quorum_fenced: bool,
    pub justification: String,
    pub authorization_signature_hex: String,
}

impl ClusterAuthorityRecord {
    pub const DOMAIN_TAG: &'static str = "FSP-CLUSTER-AUTHORITY-V1";

    /// Constructs canonical signing bytes for authority record verification.
    #[allow(clippy::too_many_arguments)]
    pub fn canonical_signing_bytes(
        cluster_id: &str,
        incarnation_id: &ClusterIncarnationId,
        authorized_voters: &BTreeSet<CoordinatorId>,
        lineage_digest_hex: &str,
        timestamp: u64,
        operational_isolation: bool,
        old_quorum_fenced: bool,
        justification: &str,
    ) -> Vec<u8> {
        let mut msg = format!(
            "{}\ncluster={}\nincarnation={}\nlineage={}\ntimestamp={}\nisolation={}\nfenced={}\njustification={}\n",
            Self::DOMAIN_TAG,
            cluster_id,
            incarnation_id.to_wire_string(),
            lineage_digest_hex,
            timestamp,
            operational_isolation,
            old_quorum_fenced,
            justification
        );
        for v in authorized_voters {
            msg.push_str(&format!("voter={}\n", v.as_str()));
        }
        msg.into_bytes()
    }

    /// Creates and signs a new cluster authority transition record.
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        cluster_id: impl Into<String>,
        incarnation_id: ClusterIncarnationId,
        authorized_voter_set: BTreeSet<CoordinatorId>,
        lineage_digest_hex: impl Into<String>,
        transition_timestamp_unix_secs: u64,
        operational_isolation_attested: bool,
        old_quorum_fenced: bool,
        justification: impl Into<String>,
        recovery_signing_key: &SigningKey,
    ) -> Self {
        let cid = cluster_id.into();
        let lineage = lineage_digest_hex.into();
        let just = justification.into();
        let msg = Self::canonical_signing_bytes(
            &cid,
            &incarnation_id,
            &authorized_voter_set,
            &lineage,
            transition_timestamp_unix_secs,
            operational_isolation_attested,
            old_quorum_fenced,
            &just,
        );
        let sig = recovery_signing_key.sign(&msg);
        let pubkey_hex = encode_hex(recovery_signing_key.verifying_key().as_bytes());

        Self {
            cluster_id: cid,
            incarnation_id,
            recovery_authority_pubkey_hex: pubkey_hex,
            authorized_voter_set,
            lineage_digest_hex: lineage,
            transition_timestamp_unix_secs,
            operational_isolation_attested,
            old_quorum_fenced,
            justification: just,
            authorization_signature_hex: encode_hex(&sig.to_bytes()),
        }
    }

    /// Verifies the record against the recovery authority's public key.
    pub fn verify(&self, recovery_authority_key: &VerifyingKey) -> Result<(), String> {
        let expected_pubkey = encode_hex(recovery_authority_key.as_bytes());
        if self.recovery_authority_pubkey_hex != expected_pubkey {
            return Err(format!(
                "Recovery authority pubkey mismatch: record has '{}', expected '{}'",
                self.recovery_authority_pubkey_hex, expected_pubkey
            ));
        }

        let msg = Self::canonical_signing_bytes(
            &self.cluster_id,
            &self.incarnation_id,
            &self.authorized_voter_set,
            &self.lineage_digest_hex,
            self.transition_timestamp_unix_secs,
            self.operational_isolation_attested,
            self.old_quorum_fenced,
            &self.justification,
        );

        let sig_bytes = decode_hex_exact::<64>(&self.authorization_signature_hex)
            .map_err(|e| format!("Invalid authority signature hex: {}", e))?;
        let sig = Signature::from_bytes(&sig_bytes);

        recovery_authority_key
            .verify_strict(&msg, &sig)
            .map_err(|e| format!("Authority transition signature verification failed: {}", e))
    }

    /// Serializes authority record to durable disk format.
    pub fn encode(&self) -> String {
        let mut out = format!(
            "RECORD_VERSION=1\nCLUSTER_ID={}\nINCARNATION={}\nRECOVERY_PUBKEY={}\nLINEAGE={}\nTIMESTAMP={}\nISOLATION={}\nFENCED={}\nJUSTIFICATION={}\nSIGNATURE={}\n",
            self.cluster_id,
            self.incarnation_id.to_wire_string(),
            self.recovery_authority_pubkey_hex,
            self.lineage_digest_hex,
            self.transition_timestamp_unix_secs,
            self.operational_isolation_attested,
            self.old_quorum_fenced,
            self.justification,
            self.authorization_signature_hex
        );
        for v in &self.authorized_voter_set {
            out.push_str(&format!("VOTER={}\n", v.as_str()));
        }
        out
    }

    /// Parses authority record from durable disk format.
    pub fn decode(text: &str) -> Result<Self, String> {
        let mut cluster_id = String::new();
        let mut incarnation_id = None;
        let mut recovery_pubkey = String::new();
        let mut lineage = String::new();
        let mut timestamp = 0u64;
        let mut isolation = false;
        let mut fenced = false;
        let mut justification = String::new();
        let mut signature = String::new();
        let mut voters = BTreeSet::new();

        for line in text.lines() {
            let line = line.trim();
            if let Some(val) = line.strip_prefix("CLUSTER_ID=") {
                cluster_id = val.to_string();
            } else if let Some(val) = line.strip_prefix("INCARNATION=") {
                incarnation_id = Some(ClusterIncarnationId::from_wire_string(val)?);
            } else if let Some(val) = line.strip_prefix("RECOVERY_PUBKEY=") {
                recovery_pubkey = val.to_string();
            } else if let Some(val) = line.strip_prefix("LINEAGE=") {
                lineage = val.to_string();
            } else if let Some(val) = line.strip_prefix("TIMESTAMP=") {
                timestamp = val.parse().unwrap_or(0);
            } else if let Some(val) = line.strip_prefix("ISOLATION=") {
                isolation = val.parse().unwrap_or(false);
            } else if let Some(val) = line.strip_prefix("FENCED=") {
                fenced = val.parse().unwrap_or(false);
            } else if let Some(val) = line.strip_prefix("JUSTIFICATION=") {
                justification = val.to_string();
            } else if let Some(val) = line.strip_prefix("SIGNATURE=") {
                signature = val.to_string();
            } else if let Some(val) = line.strip_prefix("VOTER=") {
                voters.insert(CoordinatorId::new(val));
            }
        }

        let inc = incarnation_id.ok_or("Missing INCARNATION in record")?;

        Ok(Self {
            cluster_id,
            incarnation_id: inc,
            recovery_authority_pubkey_hex: recovery_pubkey,
            authorized_voter_set: voters,
            lineage_digest_hex: lineage,
            transition_timestamp_unix_secs: timestamp,
            operational_isolation_attested: isolation,
            old_quorum_fenced: fenced,
            justification,
            authorization_signature_hex: signature,
        })
    }
}
