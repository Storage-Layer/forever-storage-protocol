//! Secure Emergency Read Protocol & Recovery Trust Store (`src/security/recovery_auth.rs`).
//!
//! Provides cryptographically authenticated offline recovery capabilities:
//! - Recovery Capability Tokens scoped to object, namespace, principal, and expiration.
//! - Anti-rollback checkpoints and Recovery Trust Store.
//! - Recovery Freshness Status (`AuthenticLatestCommitted`, `AuthenticHistoricalVersion`, `UnverifiableFreshness`).
//! - Multi-stripe Reed-Solomon decoding with per-shard hash verification, bitrot rejection,
//!   and payload digest validation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::coding::reed_solomon::ReedSolomonCodec;
use crate::model::shard::ShardId;
use crate::security::signatures::{
    decode_hex_exact, encode_hex, SignedObjectManifest, TrustedSignerPolicy,
};
use crate::storage::container::ShardContainerReader;
use crate::storage::filesystem::write_and_sync_file;
use crate::storage::integrity::sha256_bytes;

/// Recovery Capability Token authorizing offline emergency reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryCapabilityToken {
    pub object_id: String,
    pub namespace: String,
    pub principal_id: String,
    pub expires_at_unix_secs: u64,
    pub signature_hex: String,
}

impl RecoveryCapabilityToken {
    pub const DOMAIN_TAG: &'static str = "FSP-RECOVERY-TOKEN-V1";

    /// Constructs the canonical message bytes for signing.
    pub fn canonical_signing_bytes(
        object_id: &str,
        namespace: &str,
        principal_id: &str,
        expires_at_unix_secs: u64,
    ) -> Vec<u8> {
        format!(
            "{}\nobject_id={}\nnamespace={}\nprincipal={}\nexpires={}\n",
            Self::DOMAIN_TAG,
            object_id,
            namespace,
            principal_id,
            expires_at_unix_secs
        )
        .into_bytes()
    }

    /// Creates and signs a new recovery capability token using the principal's private key.
    pub fn issue(
        object_id: impl Into<String>,
        namespace: impl Into<String>,
        principal_id: impl Into<String>,
        expires_at_unix_secs: u64,
        signing_key: &SigningKey,
    ) -> Self {
        let object_id = object_id.into();
        let namespace = namespace.into();
        let principal_id = principal_id.into();

        let msg = Self::canonical_signing_bytes(
            &object_id,
            &namespace,
            &principal_id,
            expires_at_unix_secs,
        );
        let sig = signing_key.sign(&msg);
        let signature_hex = encode_hex(&sig.to_bytes());

        Self {
            object_id,
            namespace,
            principal_id,
            expires_at_unix_secs,
            signature_hex,
        }
    }

    /// Verifies the token against a public key, current time, and requested target.
    pub fn verify(
        &self,
        principal_key: &VerifyingKey,
        current_time_unix_secs: u64,
        target_object_id: &str,
        target_namespace: &str,
    ) -> Result<(), String> {
        if self.object_id != target_object_id {
            return Err(format!(
                "Token object_id mismatch: token grants '{}', requested '{}'",
                self.object_id, target_object_id
            ));
        }

        if self.namespace != target_namespace {
            return Err(format!(
                "Token namespace mismatch: token grants '{}', requested '{}'",
                self.namespace, target_namespace
            ));
        }

        if self.expires_at_unix_secs < current_time_unix_secs {
            return Err(format!(
                "Token expired: expired at {} <= current time {}",
                self.expires_at_unix_secs, current_time_unix_secs
            ));
        }

        let sig_bytes = decode_hex_exact::<64>(&self.signature_hex)
            .map_err(|e| format!("Invalid signature hex: {}", e))?;
        let sig = Signature::from_bytes(&sig_bytes);

        let msg = Self::canonical_signing_bytes(
            &self.object_id,
            &self.namespace,
            &self.principal_id,
            self.expires_at_unix_secs,
        );

        principal_key
            .verify_strict(&msg, &sig)
            .map_err(|e| format!("Cryptographic signature verification failed: {}", e))
    }
}

/// Recovery Freshness Status determining data currency against known checkpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryFreshnessStatus {
    /// Reconstruction is guaranteed to be the global latest state backed by a quorum-certified checkpoint.
    AuthenticGlobalLatest {
        certified_generation: u64,
        quorum_voters_count: usize,
    },
    /// Reconstruction matches the local checkpoint watermark, but global quorum certification was not verified.
    AuthenticLocallyCurrent {
        reconstructed_generation: u64,
        local_checkpoint_generation: u64,
    },
    /// Reconstruction matches the highest verified durable generation watermark (Phase 5.1 legacy).
    AuthenticLatestCommitted,
    /// Reconstruction is cryptographically authentic, but a newer durable generation exists.
    AuthenticHistoricalVersion {
        reconstructed_generation: u64,
        known_latest_generation: u64,
    },
    /// Manifest signature is valid, but no durable checkpoint is available to guarantee freshness.
    UnverifiableFreshness { manifest_generation: u64 },
    /// Offline majority-signed checkpoint verifies committed status at term T, but subsequent unverified commits may exist.
    AuthenticatedCommittedCheckpoint {
        certified_generation: u64,
        quorum_voters_count: usize,
        unverified_subsequent_commits_possible: bool,
    },
    /// Divergent, conflicting authentic checkpoints detected from different or partitioned quorums.
    ConflictingAuthorityEvidence {
        object_id: String,
        conflicting_generations: Vec<u64>,
    },
}

/// A consensus-verified checkpoint proving that a specific object generation was committed
/// by a quorum of coordinator nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuorumCertifiedCheckpoint {
    pub object_id: String,
    pub generation: u64,
    pub epoch: u64,
    pub commit_index: u64,
    pub voter_signatures: BTreeMap<String, String>,
}

impl QuorumCertifiedCheckpoint {
    pub const DOMAIN_TAG: &'static str = "FSP-QUORUM-CHECKPOINT-V1";

    pub fn canonical_signing_bytes(
        object_id: &str,
        generation: u64,
        epoch: u64,
        commit_index: u64,
    ) -> Vec<u8> {
        format!(
            "{}\nobject={}\ngeneration={}\nepoch={}\ncommit_index={}\n",
            Self::DOMAIN_TAG,
            object_id,
            generation,
            epoch,
            commit_index
        )
        .into_bytes()
    }

    pub fn create(
        object_id: impl Into<String>,
        generation: u64,
        epoch: u64,
        commit_index: u64,
        voters: &[(&str, &SigningKey)],
    ) -> Self {
        let obj = object_id.into();
        let msg = Self::canonical_signing_bytes(&obj, generation, epoch, commit_index);
        let mut voter_signatures = BTreeMap::new();
        for (voter_id, sk) in voters {
            let sig = sk.sign(&msg);
            voter_signatures.insert(voter_id.to_string(), encode_hex(&sig.to_bytes()));
        }
        Self {
            object_id: obj,
            generation,
            epoch,
            commit_index,
            voter_signatures,
        }
    }

    pub fn verify(
        &self,
        voter_keys: &BTreeMap<String, VerifyingKey>,
        threshold: usize,
    ) -> Result<(), String> {
        if self.voter_signatures.len() < threshold {
            return Err(format!(
                "Insufficient voter signatures: {} < required threshold {}",
                self.voter_signatures.len(),
                threshold
            ));
        }

        let msg = Self::canonical_signing_bytes(
            &self.object_id,
            self.generation,
            self.epoch,
            self.commit_index,
        );

        let mut valid_count = 0usize;
        for (voter_id, sig_hex) in &self.voter_signatures {
            let vk = voter_keys
                .get(voter_id)
                .ok_or_else(|| format!("Unknown voter in quorum checkpoint: '{}'", voter_id))?;
            let sig_bytes = decode_hex_exact::<64>(sig_hex)
                .map_err(|e| format!("Bad signature hex for voter '{}': {}", voter_id, e))?;
            let sig = Signature::from_bytes(&sig_bytes);
            vk.verify_strict(&msg, &sig)
                .map_err(|e| format!("Invalid signature from voter '{}': {}", voter_id, e))?;
            valid_count += 1;
        }

        if valid_count < threshold {
            return Err(format!(
                "Verified voter signatures {} < required threshold {}",
                valid_count, threshold
            ));
        }

        Ok(())
    }
}

/// Recovery Trust Store containing pinned keys and durability checkpoints.
#[derive(Debug, Clone, Default)]
pub struct RecoveryTrustStore {
    pub trusted_manifest_signers: BTreeMap<String, VerifyingKey>,
    pub trusted_recovery_principals: BTreeMap<String, VerifyingKey>,
    pub known_checkpoints: BTreeMap<String, u64>,
    pub quorum_certified_checkpoints: BTreeMap<String, QuorumCertifiedCheckpoint>,
    pub quorum_voter_keys: BTreeMap<String, VerifyingKey>,
    pub quorum_threshold: usize,
    pub revoked_signers: BTreeSet<String>,
    pub strict_quorum_freshness: bool,
    pub conflicting_checkpoints: BTreeMap<String, Vec<QuorumCertifiedCheckpoint>>,
    pub live_cluster_synchronized: bool,
    pub strict_freshness_proof: bool,
}

impl RecoveryTrustStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_manifest_signer(&mut self, signer_id: impl Into<String>, vk: VerifyingKey) {
        self.trusted_manifest_signers.insert(signer_id.into(), vk);
    }

    pub fn register_recovery_principal(
        &mut self,
        principal_id: impl Into<String>,
        vk: VerifyingKey,
    ) {
        self.trusted_recovery_principals
            .insert(principal_id.into(), vk);
    }

    pub fn set_checkpoint(&mut self, object_id: impl Into<String>, generation: u64) {
        self.known_checkpoints.insert(object_id.into(), generation);
    }

    pub fn get_checkpoint(&self, object_id: &str) -> Option<u64> {
        self.known_checkpoints.get(object_id).copied()
    }

    pub fn register_quorum_checkpoint(&mut self, checkpoint: QuorumCertifiedCheckpoint) {
        self.quorum_certified_checkpoints
            .insert(checkpoint.object_id.clone(), checkpoint);
    }

    pub fn register_checkpoint_with_conflict_detection(
        &mut self,
        checkpoint: QuorumCertifiedCheckpoint,
    ) -> Result<(), String> {
        let obj_id = checkpoint.object_id.clone();
        if let Some(existing) = self.quorum_certified_checkpoints.get(&obj_id) {
            if existing.generation != checkpoint.generation
                || existing.epoch != checkpoint.epoch
                || existing.commit_index != checkpoint.commit_index
            {
                let list = self
                    .conflicting_checkpoints
                    .entry(obj_id.clone())
                    .or_insert_with(|| vec![existing.clone()]);
                list.push(checkpoint.clone());
                return Err(format!(
                    "Conflicting quorum checkpoints detected for object '{}': (gen {}, epoch {}, ci {}) vs (gen {}, epoch {}, ci {})",
                    obj_id,
                    existing.generation, existing.epoch, existing.commit_index,
                    checkpoint.generation, checkpoint.epoch, checkpoint.commit_index
                ));
            }
        }
        self.quorum_certified_checkpoints.insert(obj_id, checkpoint);
        Ok(())
    }

    pub fn register_quorum_voter(&mut self, voter_id: impl Into<String>, vk: VerifyingKey) {
        self.quorum_voter_keys.insert(voter_id.into(), vk);
    }

    pub fn set_quorum_threshold(&mut self, threshold: usize) {
        self.quorum_threshold = threshold;
    }

    pub fn revoke_signer(&mut self, signer_id: impl Into<String>) {
        self.revoked_signers.insert(signer_id.into());
    }

    pub fn is_signer_revoked(&self, signer_id: &str) -> bool {
        self.revoked_signers.contains(signer_id)
    }

    pub fn set_strict_quorum_freshness(&mut self, strict: bool) {
        self.strict_quorum_freshness = strict;
    }

    pub fn set_live_cluster_synchronized(&mut self, syncd: bool) {
        self.live_cluster_synchronized = syncd;
    }

    pub fn set_strict_freshness_proof(&mut self, strict: bool) {
        self.strict_freshness_proof = strict;
    }
}

/// Options passed to `execute_secure_emergency_read`.
#[derive(Debug, Clone)]
pub struct SecureEmergencyReadOptions {
    pub object_id: String,
    pub namespace: String,
    pub token: RecoveryCapabilityToken,
    pub trust_store: RecoveryTrustStore,
    pub storage_roots: Vec<PathBuf>,
    pub output_path: PathBuf,
    pub current_time_unix_secs: u64,
}

/// Result report from an authenticated emergency read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecureEmergencyReadReport {
    pub object_id: String,
    pub object_size: u64,
    pub object_digest: String,
    pub manifest_generation: u64,
    pub freshness_status: RecoveryFreshnessStatus,
    pub shards_read: usize,
    pub corrupted_shards_detected: usize,
    pub output_path: PathBuf,
}

/// Executes a secure offline emergency read against surviving storage node roots:
/// 1. Verifies the presented RecoveryCapabilityToken against trusted recovery principals.
/// 2. Discovers and validates the highest signed object manifest against trusted signers.
/// 3. Determines data freshness against the trust store checkpoints.
/// 4. Gathers surviving shards across all stripes, verifying per-shard SHA-256 digests.
/// 5. Automatically detects and discards corrupted/tampered shards.
/// 6. Decodes payload using Reed-Solomon RS(20,12) erasure coding.
/// 7. Trims decoded stream to exact object length and validates total payload SHA-256.
/// 8. Atomically writes the verified output file to disk.
#[allow(clippy::map_entry)]
pub fn execute_secure_emergency_read(
    opts: &SecureEmergencyReadOptions,
) -> Result<SecureEmergencyReadReport, String> {
    // Step 1: Verify token authenticity & scoping
    let principal_key = opts
        .trust_store
        .trusted_recovery_principals
        .get(&opts.token.principal_id)
        .ok_or_else(|| {
            format!(
                "Unknown or untrusted recovery principal: '{}'",
                opts.token.principal_id
            )
        })?;

    opts.token.verify(
        principal_key,
        opts.current_time_unix_secs,
        &opts.object_id,
        &opts.namespace,
    )?;

    // Step 2: Discover and verify SignedObjectManifest across storage roots
    let mut candidate_manifests = Vec::new();
    let mut policy = TrustedSignerPolicy::new();
    for (signer_id, vk) in &opts.trust_store.trusted_manifest_signers {
        let _ = policy.register_signer(signer_id, *vk);
    }

    for root in &opts.storage_roots {
        let candidates = vec![
            root.join("metadata")
                .join("manifests")
                .join(format!("{}.sig.json", opts.object_id)),
            root.join("metadata")
                .join("manifests")
                .join(format!("{}.json", opts.object_id)),
            root.join("objects")
                .join(&opts.object_id)
                .join("manifest.sig.json"),
            root.join("objects")
                .join(&opts.object_id)
                .join("manifest.json"),
        ];

        for p in candidates {
            if p.exists() {
                if let Ok(content) = fs::read_to_string(&p) {
                    if let Ok(signed_manifest) = SignedObjectManifest::from_json(&content) {
                        if !opts
                            .trust_store
                            .is_signer_revoked(&signed_manifest.signer_id)
                            && policy.verify_manifest(&signed_manifest).is_ok()
                            && signed_manifest.manifest.object_id == opts.object_id
                        {
                            candidate_manifests.push(signed_manifest);
                        }
                    }
                }
            }
        }
    }

    if candidate_manifests.is_empty() {
        return Err(format!(
            "No cryptographically valid SignedObjectManifest found for object '{}'",
            opts.object_id
        ));
    }

    // Pick manifest with highest generation
    candidate_manifests.sort_by_key(|m| m.manifest_generation);
    let chosen_signed = candidate_manifests.last().unwrap().clone();
    let manifest = chosen_signed.manifest;
    let manifest_gen = chosen_signed.manifest_generation;

    // Step 3: Determine freshness status
    let freshness_status = if let Some(conflicting) = opts
        .trust_store
        .conflicting_checkpoints
        .get(&opts.object_id)
    {
        let mut gens: Vec<u64> = conflicting.iter().map(|c| c.generation).collect();
        gens.sort_unstable();
        gens.dedup();
        RecoveryFreshnessStatus::ConflictingAuthorityEvidence {
            object_id: opts.object_id.clone(),
            conflicting_generations: gens,
        }
    } else if let Some(q_cert) = opts
        .trust_store
        .quorum_certified_checkpoints
        .get(&opts.object_id)
    {
        let thresh = if opts.trust_store.quorum_threshold > 0 {
            opts.trust_store.quorum_threshold
        } else {
            1
        };
        q_cert
            .verify(&opts.trust_store.quorum_voter_keys, thresh)
            .map_err(|e| format!("Quorum certified checkpoint verification failed: {}", e))?;

        if manifest_gen == q_cert.generation {
            if opts.trust_store.strict_freshness_proof
                && !opts.trust_store.live_cluster_synchronized
            {
                RecoveryFreshnessStatus::AuthenticatedCommittedCheckpoint {
                    certified_generation: q_cert.generation,
                    quorum_voters_count: q_cert.voter_signatures.len(),
                    unverified_subsequent_commits_possible: true,
                }
            } else {
                RecoveryFreshnessStatus::AuthenticGlobalLatest {
                    certified_generation: q_cert.generation,
                    quorum_voters_count: q_cert.voter_signatures.len(),
                }
            }
        } else if manifest_gen < q_cert.generation {
            RecoveryFreshnessStatus::AuthenticHistoricalVersion {
                reconstructed_generation: manifest_gen,
                known_latest_generation: q_cert.generation,
            }
        } else if opts.trust_store.strict_quorum_freshness {
            RecoveryFreshnessStatus::UnverifiableFreshness {
                manifest_generation: manifest_gen,
            }
        } else {
            RecoveryFreshnessStatus::AuthenticLatestCommitted
        }
    } else if let Some(&known_gen) = opts.trust_store.known_checkpoints.get(&opts.object_id) {
        if manifest_gen == known_gen {
            if opts.trust_store.strict_quorum_freshness {
                RecoveryFreshnessStatus::AuthenticLocallyCurrent {
                    reconstructed_generation: manifest_gen,
                    local_checkpoint_generation: known_gen,
                }
            } else {
                RecoveryFreshnessStatus::AuthenticLatestCommitted
            }
        } else if manifest_gen < known_gen {
            RecoveryFreshnessStatus::AuthenticHistoricalVersion {
                reconstructed_generation: manifest_gen,
                known_latest_generation: known_gen,
            }
        } else {
            RecoveryFreshnessStatus::UnverifiableFreshness {
                manifest_generation: manifest_gen,
            }
        }
    } else {
        RecoveryFreshnessStatus::UnverifiableFreshness {
            manifest_generation: manifest_gen,
        }
    };

    // Step 4: Gather shards per stripe and verify individual digests
    let rs = ReedSolomonCodec::fsp_default_20_12();
    let stripe_count = manifest.stripes.len();
    if stripe_count == 0 {
        return Err("Manifest contains 0 stripes".to_string());
    }

    let mut total_shards_read = 0usize;
    let mut corrupted_shards_detected = 0usize;
    let mut reconstructed_payload = Vec::new();

    // Reconstruct each stripe
    for (stripe_idx, stripe_info) in manifest.stripes.iter().enumerate() {
        let mut valid_shards: BTreeMap<u32, Vec<u8>> = BTreeMap::new();

        // Scan all storage roots for shards of this stripe
        for root in &opts.storage_roots {
            let obj_dir = root.join("objects").join(&opts.object_id);
            if !obj_dir.exists() {
                continue;
            }

            if let Ok(entries) = fs::read_dir(&obj_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    let fname = match p.file_name().and_then(|n| n.to_str()) {
                        Some(f) => f.to_string(),
                        None => continue,
                    };

                    // Check container v2 file (.fspc)
                    if fname.ends_with(".fspc") {
                        match ShardContainerReader::open(&p) {
                            Ok(mut reader) => {
                                let shard_id = reader.header().shard_id as u32;
                                if !valid_shards.contains_key(&shard_id) {
                                    match reader.read_stripe(stripe_idx) {
                                        Ok(data) => {
                                            total_shards_read += 1;
                                            let actual_digest = sha256_bytes(&data).to_hex();
                                            if let Some(placement) = stripe_info
                                                .placements
                                                .iter()
                                                .find(|s| s.shard_id == shard_id as usize)
                                            {
                                                if actual_digest == placement.sha256_digest {
                                                    valid_shards.insert(shard_id, data);
                                                } else {
                                                    corrupted_shards_detected += 1;
                                                }
                                            }
                                        }
                                        Err(_) => {
                                            corrupted_shards_detected += 1;
                                        }
                                    }
                                }
                            }
                            Err(_) => {
                                corrupted_shards_detected += 1;
                            }
                        }
                    }
                    // Check legacy stripe-specific shard file (.shard)
                    else if fname.ends_with(".shard")
                        && fname.contains(&format!("s{:04}", stripe_idx))
                    {
                        let shard_num = fname
                            .split('.')
                            .next()
                            .and_then(|s| s.split("shard").nth(1))
                            .and_then(|s| s.parse::<u32>().ok());

                        if let Some(s_id) = shard_num {
                            if !valid_shards.contains_key(&s_id) {
                                if let Ok(data) = fs::read(&p) {
                                    total_shards_read += 1;
                                    let actual_digest = sha256_bytes(&data).to_hex();
                                    if let Some(placement) = stripe_info
                                        .placements
                                        .iter()
                                        .find(|s| s.shard_id == s_id as usize)
                                    {
                                        if actual_digest == placement.sha256_digest {
                                            valid_shards.insert(s_id, data);
                                        } else {
                                            corrupted_shards_detected += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Single-stripe fallback named shardXX.shard
                    else if fname.ends_with(".shard") && stripe_count == 1 {
                        let shard_num = fname
                            .split('.')
                            .next()
                            .and_then(|s| s.split("shard").nth(1))
                            .and_then(|s| s.parse::<u32>().ok());

                        if let Some(s_id) = shard_num {
                            if !valid_shards.contains_key(&s_id) {
                                if let Ok(data) = fs::read(&p) {
                                    total_shards_read += 1;
                                    let actual_digest = sha256_bytes(&data).to_hex();
                                    if let Some(placement) = stripe_info
                                        .placements
                                        .iter()
                                        .find(|s| s.shard_id == s_id as usize)
                                    {
                                        if actual_digest == placement.sha256_digest {
                                            valid_shards.insert(s_id, data);
                                        } else {
                                            corrupted_shards_detected += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Check if we have at least 12 valid shards for this stripe
        if valid_shards.len() < 12 {
            return Err(format!(
                "Insufficient valid surviving shards for stripe {}: found {} < 12 required (corrupted={})",
                stripe_idx,
                valid_shards.len(),
                corrupted_shards_detected
            ));
        }

        let shard_len = valid_shards.values().next().map(|v| v.len()).unwrap_or(0);
        if shard_len == 0 {
            return Err(format!("Zero-length shards in stripe {}", stripe_idx));
        }

        let available: Vec<(ShardId, &[u8])> = valid_shards
            .iter()
            .take(12)
            .map(|(&id, vec)| (ShardId(id as usize), vec.as_slice()))
            .collect();

        let decoded = rs
            .decode_full_stripe(&available, shard_len)
            .map_err(|e| format!("Erasure decoding failure on stripe {}: {}", stripe_idx, e))?;

        let mut stripe_payload = Vec::with_capacity(shard_len * 12);
        for d in decoded.iter().take(12) {
            stripe_payload.extend_from_slice(d);
        }
        if stripe_payload.len() > stripe_info.payload_length {
            stripe_payload.truncate(stripe_info.payload_length);
        }
        reconstructed_payload.extend_from_slice(&stripe_payload);
    }

    // Step 5: Trim reconstructed payload to exact manifest object_size
    if (reconstructed_payload.len() as u64) < manifest.object_size {
        return Err(format!(
            "Reconstructed payload length {} < manifest expected size {}",
            reconstructed_payload.len(),
            manifest.object_size
        ));
    }
    reconstructed_payload.truncate(manifest.object_size as usize);

    // Step 6: Verify total payload SHA-256 against manifest digest
    let final_digest = sha256_bytes(&reconstructed_payload).to_hex();
    if final_digest != manifest.object_digest {
        return Err(format!(
            "Payload SHA-256 mismatch: reconstructed {}, manifest expected {}",
            final_digest, manifest.object_digest
        ));
    }

    // Step 7: Atomically write to output_path
    if let Some(parent) = opts.output_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    write_and_sync_file(&opts.output_path, &reconstructed_payload)
        .map_err(|e| format!("Failed to write output file: {}", e))?;

    Ok(SecureEmergencyReadReport {
        object_id: opts.object_id.clone(),
        object_size: manifest.object_size,
        object_digest: final_digest,
        manifest_generation: manifest_gen,
        freshness_status,
        shards_read: total_shards_read,
        corrupted_shards_detected,
        output_path: opts.output_path.clone(),
    })
}
