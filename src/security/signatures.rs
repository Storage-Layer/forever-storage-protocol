//! Ed25519 Signed Object Manifests, Signer Trust Policy, Replay Protection, and Coordinator
//! Metadata Backup & Restore (`src/security/signatures.rs`).

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::storage::filesystem::write_and_sync_file;
use crate::storage::integrity::{sha256_bytes, Sha256Digest};
use crate::storage::manifest::{validate_safe_identifier, ManifestValidationError, ObjectManifest};

/// Domain-separation tag prepended to every canonical manifest signing payload.
pub const MANIFEST_SIGNING_DOMAIN_TAG: &str = "FSP-MANIFEST-SIG-V1";

/// Supported signature algorithm identifier.
pub const SIGNATURE_ALGORITHM_ED25519: &str = "ed25519";

/// Errors returned during Ed25519 manifest signing, parsing, or verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureError {
    /// Underlying manifest failed structural or SHA-256 validation.
    ManifestInvalid(ManifestValidationError),
    /// Malformed JSON or missing signature metadata field.
    ParseError(String),
    /// Unsupported signature algorithm or domain format tag.
    UnsupportedAlgorithm(String),
    /// Invalid `signer_id` or `manifest_generation == 0`.
    InvalidSignerMetadata(String),
    /// Signer ID is not present in the [`TrustedSignerPolicy`].
    UnknownSigner(String),
    /// Public key in manifest does not match the pinned public key for `signer_id`.
    PublicKeyMismatch {
        signer_id: String,
        expected_hex: String,
        actual_hex: String,
    },
    /// Ed25519 cryptographic signature verification failed.
    InvalidSignature(String),
    /// Presented `manifest_generation` is older than or equal to the highest accepted generation.
    ReplayGenerationRejected {
        object_id: String,
        presented_generation: u64,
        recorded_generation: u64,
    },
    /// Coordinator metadata backup bundle integrity or signature failure.
    BackupIntegrityError(String),
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ManifestInvalid(e) => write!(f, "Signed manifest structural error: {}", e),
            Self::ParseError(msg) => write!(f, "Signed manifest parse error: {}", msg),
            Self::UnsupportedAlgorithm(alg) => {
                write!(f, "Unsupported signature algorithm/format: {}", alg)
            }
            Self::InvalidSignerMetadata(msg) => write!(f, "Invalid signer metadata: {}", msg),
            Self::UnknownSigner(id) => write!(f, "Untrusted/unknown signer_id: {:?}", id),
            Self::PublicKeyMismatch {
                signer_id,
                expected_hex,
                actual_hex,
            } => write!(
                f,
                "Public key mismatch for signer {:?}: expected {}, got {}",
                signer_id, expected_hex, actual_hex
            ),
            Self::InvalidSignature(msg) => {
                write!(f, "Ed25519 signature verification failed: {}", msg)
            }
            Self::ReplayGenerationRejected {
                object_id,
                presented_generation,
                recorded_generation,
            } => write!(
                f,
                "Rejected replayed/stale manifest generation for {}: presented gen {} < required gen {}",
                object_id, presented_generation, recorded_generation
            ),
            Self::BackupIntegrityError(msg) => {
                write!(f, "Metadata backup integrity error: {}", msg)
            }
        }
    }
}

impl std::error::Error for SignatureError {}

impl From<ManifestValidationError> for SignatureError {
    fn from(value: ManifestValidationError) -> Self {
        Self::ManifestInvalid(value)
    }
}

/// Deterministic Ed25519 keypair wrapper for a coordinator or manifest authority.
#[derive(Clone)]
pub struct ManifestSignerKeypair {
    signer_id: String,
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
}

impl fmt::Debug for ManifestSignerKeypair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManifestSignerKeypair")
            .field("signer_id", &self.signer_id)
            .field("public_key_hex", &self.public_key_hex())
            .finish()
    }
}

impl ManifestSignerKeypair {
    /// Constructs a [`ManifestSignerKeypair`] from a 32-byte secret seed (`secret_bytes`).
    pub fn from_seed(signer_id: &str, secret_bytes: [u8; 32]) -> Result<Self, SignatureError> {
        if !validate_safe_identifier(signer_id) {
            return Err(SignatureError::InvalidSignerMetadata(format!(
                "unsafe signer_id {:?}",
                signer_id
            )));
        }
        let signing_key = SigningKey::from_bytes(&secret_bytes);
        let verifying_key = signing_key.verifying_key();
        Ok(Self {
            signer_id: signer_id.to_string(),
            signing_key,
            verifying_key,
        })
    }

    /// Generates a deterministic keypair from a numeric seed (for reproducible test clusters).
    pub fn deterministic(signer_id: &str, seed: u64) -> Result<Self, SignatureError> {
        let mut material = [0u8; 32];
        let digest = sha256_bytes(format!("FSP-ED25519-KEYGEN:{}:{}", signer_id, seed).as_bytes());
        material.copy_from_slice(&digest.0);
        Self::from_seed(signer_id, material)
    }

    /// Saves the 32-byte secret seed to `path` with `0o600` permissions on Unix.
    pub fn save_secret_key_file(&self, path: &Path) -> std::io::Result<()> {
        let hex = encode_hex(&self.signing_key.to_bytes());
        let content = format!("signer_id={}\nsecret_hex={}\n", self.signer_id, hex);
        fs::write(path, content.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    /// Loads a [`ManifestSignerKeypair`] from a secret key file.
    pub fn load_secret_key_file(path: &Path) -> Result<Self, SignatureError> {
        let content =
            fs::read_to_string(path).map_err(|e| SignatureError::ParseError(e.to_string()))?;
        let mut signer_id = None;
        let mut secret_hex = None;
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("signer_id=") {
                signer_id = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("secret_hex=") {
                secret_hex = Some(rest.trim().to_string());
            }
        }
        let sid =
            signer_id.ok_or_else(|| SignatureError::ParseError("missing signer_id".into()))?;
        let shex =
            secret_hex.ok_or_else(|| SignatureError::ParseError("missing secret_hex".into()))?;
        let bytes = decode_hex_exact::<32>(&shex)
            .map_err(|e| SignatureError::ParseError(format!("invalid secret_hex: {}", e)))?;
        Self::from_seed(&sid, bytes)
    }

    /// Signer identifier.
    pub fn signer_id(&self) -> &str {
        &self.signer_id
    }

    /// Returns the 32-byte Ed25519 [`VerifyingKey`].
    pub const fn verifying_key(&self) -> VerifyingKey {
        self.verifying_key
    }

    /// Returns a reference to the Ed25519 [`SigningKey`].
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// Lowercase 64-character hex encoding of the 32-byte Ed25519 public key.
    pub fn public_key_hex(&self) -> String {
        encode_hex(self.verifying_key.as_bytes())
    }

    /// Signs a validated [`ObjectManifest`] at `manifest_generation` ($\ge 1$).
    pub fn sign_manifest(
        &self,
        mut manifest: ObjectManifest,
        manifest_generation: u64,
    ) -> Result<SignedObjectManifest, SignatureError> {
        if manifest_generation == 0 {
            return Err(SignatureError::InvalidSignerMetadata(
                "manifest_generation must be >= 1".into(),
            ));
        }
        manifest.seal();
        manifest.validate()?;

        let mut unsigned = SignedObjectManifest {
            manifest,
            manifest_generation,
            signer_id: self.signer_id.clone(),
            signature_algorithm: SIGNATURE_ALGORITHM_ED25519.to_string(),
            signed_message_format: MANIFEST_SIGNING_DOMAIN_TAG.to_string(),
            public_key_hex: self.public_key_hex(),
            signature_hex: String::new(),
        };
        let msg = unsigned.canonical_signing_bytes();
        let sig: Signature = self.signing_key.sign(&msg);
        unsigned.signature_hex = encode_hex(&sig.to_bytes());
        Ok(unsigned)
    }
}

/// Cryptographically signed [`ObjectManifest`] with generation counter and signer identity binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedObjectManifest {
    /// Underlying validated [`ObjectManifest`].
    pub manifest: ObjectManifest,
    /// Monotonically increasing revision/generation number ($\ge 1$).
    pub manifest_generation: u64,
    /// Identity of the coordinator/authority that signed this manifest.
    pub signer_id: String,
    /// Signature algorithm (`"ed25519"`).
    pub signature_algorithm: String,
    /// Domain-separated signing message format (`"FSP-MANIFEST-SIG-V1"`).
    pub signed_message_format: String,
    /// 64-character hex Ed25519 public key (`32` bytes).
    pub public_key_hex: String,
    /// 128-character hex Ed25519 signature (`64` bytes).
    pub signature_hex: String,
}

impl SignedObjectManifest {
    /// Produces the canonical, domain-separated byte sequence covered by the Ed25519 signature.
    ///
    /// Binds:
    /// - `signed_message_format` (`"FSP-MANIFEST-SIG-V1"`)
    /// - `signer_id`
    /// - `signature_algorithm` (`"ed25519"`)
    /// - `public_key_hex`
    /// - `manifest_generation`
    /// - `format_version`, `object_id`, `object_size`, `coding`, `object_digest`, `manifest_digest`
    /// - Full canonical JSON representation of `manifest` (all stripes, shard digests, and placements)
    pub fn canonical_signing_bytes(&self) -> Vec<u8> {
        let canonical_manifest_json = self.manifest.to_canonical_json();
        let header = format!(
            "{}\nsigner_id={}\nalg={}\npubkey={}\ngeneration={}\nformat_version={}\nobject_id={}\nobject_size={}\nscheme={}:{}:{}:{:?}\nobject_digest={}\nmanifest_digest={}\n---\n",
            MANIFEST_SIGNING_DOMAIN_TAG,
            self.signer_id,
            self.signature_algorithm,
            self.public_key_hex,
            self.manifest_generation,
            self.manifest.format_version,
            self.manifest.object_id,
            self.manifest.object_size,
            self.manifest.coding.scheme,
            self.manifest.coding.n,
            self.manifest.coding.k,
            self.manifest.coding.l,
            self.manifest.object_digest,
            self.manifest.manifest_digest,
        );
        let mut bytes = Vec::with_capacity(header.len() + canonical_manifest_json.len());
        bytes.extend_from_slice(header.as_bytes());
        bytes.extend_from_slice(canonical_manifest_json.as_bytes());
        bytes
    }

    /// Serializes the [`SignedObjectManifest`] into deterministic JSON.
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(2048);
        out.push_str("{\n");
        out.push_str("  \"signature_envelope\": {\n");
        out.push_str(&format!(
            "    \"signed_message_format\": \"{}\",\n",
            self.signed_message_format
        ));
        out.push_str(&format!(
            "    \"signature_algorithm\": \"{}\",\n",
            self.signature_algorithm
        ));
        out.push_str(&format!("    \"signer_id\": \"{}\",\n", self.signer_id));
        out.push_str(&format!(
            "    \"manifest_generation\": {},\n",
            self.manifest_generation
        ));
        out.push_str(&format!(
            "    \"public_key_hex\": \"{}\",\n",
            self.public_key_hex
        ));
        out.push_str(&format!(
            "    \"signature_hex\": \"{}\"\n",
            self.signature_hex
        ));
        out.push_str("  },\n");
        out.push_str("  \"manifest_payload\": ");
        out.push_str(&self.manifest.to_canonical_json());
        out.push_str("}\n");
        out
    }

    /// Parses a [`SignedObjectManifest`] from JSON and validates its structural manifest invariants.
    /// Note: Call [`TrustedSignerPolicy::verify_manifest`] to verify the Ed25519 signature and trust policy!
    pub fn from_json(json: &str) -> Result<Self, SignatureError> {
        let env_pos = json
            .find("\"signature_envelope\":")
            .ok_or_else(|| SignatureError::ParseError("missing signature_envelope block".into()))?;
        let payload_pos = json
            .find("\"manifest_payload\":")
            .ok_or_else(|| SignatureError::ParseError("missing manifest_payload block".into()))?;
        let env_sub = &json[env_pos..payload_pos];

        let signed_message_format = extract_str(env_sub, "\"signed_message_format\":")?;
        let signature_algorithm = extract_str(env_sub, "\"signature_algorithm\":")?;
        let signer_id = extract_str(env_sub, "\"signer_id\":")?;
        let manifest_generation = extract_u64(env_sub, "\"manifest_generation\":")?;
        let public_key_hex = extract_str(env_sub, "\"public_key_hex\":")?;
        let signature_hex = extract_str(env_sub, "\"signature_hex\":")?;

        let after_payload_key = &json[payload_pos + "\"manifest_payload\":".len()..];
        let open_brace = after_payload_key
            .find('{')
            .ok_or_else(|| SignatureError::ParseError("missing manifest_payload '{'".into()))?;
        let close_brace = after_payload_key
            .rfind('}')
            .ok_or_else(|| SignatureError::ParseError("missing outer '}'".into()))?;
        if close_brace <= open_brace {
            return Err(SignatureError::ParseError(
                "malformed manifest_payload braces".into(),
            ));
        }
        // Strip outer closing brace to isolate inner manifest JSON
        let inner_region = &after_payload_key[open_brace..close_brace];
        let inner_end = inner_region
            .rfind('}')
            .ok_or_else(|| SignatureError::ParseError("missing inner manifest '}'".into()))?;
        let manifest_json = &inner_region[..=inner_end];

        let manifest = ObjectManifest::from_json(manifest_json)?;
        Ok(Self {
            manifest,
            manifest_generation,
            signer_id,
            signature_algorithm,
            signed_message_format,
            public_key_hex,
            signature_hex,
        })
    }
}

/// Explicit trust policy registry mapping trusted `signer_id`s to their pinned Ed25519 public keys,
/// and tracking per-object `manifest_generation` watermarks to prevent replay of older generations.
#[derive(Debug, Clone, Default)]
pub struct TrustedSignerPolicy {
    trusted_signers: BTreeMap<String, VerifyingKey>,
    object_generations: BTreeMap<String, u64>,
    generations_dir: Option<PathBuf>,
}

impl TrustedSignerPolicy {
    /// Creates an empty [`TrustedSignerPolicy`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets an optional durable on-disk directory for persistent generation tracking
    /// and pre-loads all existing monotonic generation files.
    pub fn set_generations_dir<P: AsRef<Path>>(&mut self, dir: P) -> Result<(), SignatureError> {
        let p = dir.as_ref().to_path_buf();
        if !p.exists() {
            fs::create_dir_all(&p).map_err(|e| SignatureError::ParseError(e.to_string()))?;
        }
        for entry in fs::read_dir(&p).map_err(|e| SignatureError::ParseError(e.to_string()))? {
            let entry = entry.map_err(|e| SignatureError::ParseError(e.to_string()))?;
            let path = entry.path();
            if let Some(fname) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(obj_id) = fname.strip_suffix(".gen") {
                    if let Ok(content) = fs::read_to_string(&path) {
                        if let Ok(gen) = content.trim().parse::<u64>() {
                            let curr = self
                                .object_generations
                                .entry(obj_id.to_string())
                                .or_insert(0);
                            *curr = (*curr).max(gen);
                        }
                    }
                }
            }
        }
        self.generations_dir = Some(p);
        Ok(())
    }

    /// Registers a trusted `signer_id` and its pinned 32-byte Ed25519 [`VerifyingKey`].
    pub fn register_signer(
        &mut self,
        signer_id: &str,
        verifying_key: VerifyingKey,
    ) -> Result<(), SignatureError> {
        if !validate_safe_identifier(signer_id) {
            return Err(SignatureError::InvalidSignerMetadata(format!(
                "invalid signer_id {:?}",
                signer_id
            )));
        }
        self.trusted_signers
            .insert(signer_id.to_string(), verifying_key);
        Ok(())
    }

    /// Registers a trusted signer from a 64-character hex public key.
    pub fn register_signer_hex(
        &mut self,
        signer_id: &str,
        public_key_hex: &str,
    ) -> Result<(), SignatureError> {
        let pk_bytes = decode_hex_exact::<32>(public_key_hex)
            .map_err(|e| SignatureError::InvalidSignerMetadata(e.to_string()))?;
        let vk = VerifyingKey::from_bytes(&pk_bytes)
            .map_err(|e| SignatureError::InvalidSignerMetadata(e.to_string()))?;
        self.register_signer(signer_id, vk)
    }

    /// Returns the recorded minimum required generation for `object_id`, if any.
    pub fn recorded_generation(&self, object_id: &str) -> Option<u64> {
        self.object_generations.get(object_id).copied()
    }

    /// Directly records or advances the generation watermark for `object_id` (used during failover replay).
    pub fn record_generation_watermark(&mut self, object_id: &str, gen: u64) {
        let cur = self
            .object_generations
            .entry(object_id.to_string())
            .or_insert(0);
        if gen > *cur {
            *cur = gen;
            if let Some(ref dir) = self.generations_dir {
                let p = dir.join(format!("{}.gen", object_id));
                let _ = fs::write(&p, format!("{}\n", gen));
            }
        }
    }

    /// Verifies a [`SignedObjectManifest`] against structural rules, pinned signer public key,
    /// strict Ed25519 signature verification, and monotonic generation watermark (`>= recorded`).
    pub fn verify_manifest(&self, signed: &SignedObjectManifest) -> Result<(), SignatureError> {
        // 1. Validate underlying manifest structure and SHA-256 self-digest
        signed.manifest.validate()?;

        // 2. Validate envelope metadata & domain separation
        if signed.signed_message_format != MANIFEST_SIGNING_DOMAIN_TAG {
            return Err(SignatureError::UnsupportedAlgorithm(format!(
                "unexpected signed_message_format {:?}",
                signed.signed_message_format
            )));
        }
        if signed.signature_algorithm != SIGNATURE_ALGORITHM_ED25519 {
            return Err(SignatureError::UnsupportedAlgorithm(
                signed.signature_algorithm.clone(),
            ));
        }
        if !validate_safe_identifier(&signed.signer_id) {
            return Err(SignatureError::InvalidSignerMetadata(format!(
                "invalid signer_id {:?}",
                signed.signer_id
            )));
        }
        if signed.manifest_generation == 0 {
            return Err(SignatureError::InvalidSignerMetadata(
                "manifest_generation must be >= 1".into(),
            ));
        }

        // 3. Look up pinned verifying key in trust policy
        let pinned_vk = self
            .trusted_signers
            .get(&signed.signer_id)
            .ok_or_else(|| SignatureError::UnknownSigner(signed.signer_id.clone()))?;

        let expected_pk_hex = encode_hex(pinned_vk.as_bytes());
        if signed.public_key_hex != expected_pk_hex {
            return Err(SignatureError::PublicKeyMismatch {
                signer_id: signed.signer_id.clone(),
                expected_hex: expected_pk_hex,
                actual_hex: signed.public_key_hex.clone(),
            });
        }

        // 4. Parse and verify strict Ed25519 signature over canonical domain-separated message
        let sig_bytes = decode_hex_exact::<64>(&signed.signature_hex)
            .map_err(|e| SignatureError::InvalidSignature(format!("invalid sig hex: {}", e)))?;
        let signature = Signature::from_bytes(&sig_bytes);
        let msg = signed.canonical_signing_bytes();

        pinned_vk
            .verify_strict(&msg, &signature)
            .map_err(|e| SignatureError::InvalidSignature(e.to_string()))?;

        // 5. Check generation replay protection against recorded watermark
        if let Some(&recorded_gen) = self.object_generations.get(&signed.manifest.object_id) {
            if signed.manifest_generation < recorded_gen {
                return Err(SignatureError::ReplayGenerationRejected {
                    object_id: signed.manifest.object_id.clone(),
                    presented_generation: signed.manifest_generation,
                    recorded_generation: recorded_gen,
                });
            }
        }

        Ok(())
    }

    /// Verifies a [`SignedObjectManifest`] as a strictly newer revision (`manifest_generation > recorded_gen`
    /// if `object_id` already exists) and updates the recorded generation watermark upon success.
    pub fn verify_and_advance_generation(
        &mut self,
        signed: &SignedObjectManifest,
        require_strictly_newer: bool,
    ) -> Result<(), SignatureError> {
        self.verify_manifest(signed)?;
        if let Some(&recorded_gen) = self.object_generations.get(&signed.manifest.object_id) {
            if require_strictly_newer && signed.manifest_generation <= recorded_gen {
                return Err(SignatureError::ReplayGenerationRejected {
                    object_id: signed.manifest.object_id.clone(),
                    presented_generation: signed.manifest_generation,
                    recorded_generation: recorded_gen + 1,
                });
            }
        }
        self.object_generations.insert(
            signed.manifest.object_id.clone(),
            signed.manifest_generation,
        );
        if let Some(ref dir) = self.generations_dir {
            let p = dir.join(format!("{}.gen", signed.manifest.object_id));
            let _ = fs::write(&p, format!("{}\n", signed.manifest_generation));
        }
        Ok(())
    }

    /// Returns an iterator over trusted `(signer_id, public_key_hex)` pairs.
    pub fn trusted_signers_hex(&self) -> Vec<(String, String)> {
        self.trusted_signers
            .iter()
            .map(|(k, v)| (k.clone(), encode_hex(v.as_bytes())))
            .collect()
    }
}

/// Signed coordinator metadata backup bundle for recovering lost coordinator metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataBackupBundle {
    /// Bundle format version (`1`).
    pub bundle_version: u32,
    /// Signer ID that produced this backup bundle.
    pub signer_id: String,
    /// Hex public key of the backup signer.
    pub public_key_hex: String,
    /// Signed object manifests included in the backup.
    pub signed_manifests: Vec<SignedObjectManifest>,
    /// SHA-256 hex digest over all signed manifests in the bundle.
    pub bundle_digest: String,
    /// Ed25519 signature over `"FSP-METADATA-BACKUP-V1\n" + bundle_digest`.
    pub bundle_signature_hex: String,
}

impl MetadataBackupBundle {
    /// Creates and signs a [`MetadataBackupBundle`] from a slice of verified [`SignedObjectManifest`]s.
    pub fn create(
        signer: &ManifestSignerKeypair,
        manifests: Vec<SignedObjectManifest>,
    ) -> Result<Self, SignatureError> {
        let mut concat = String::new();
        for sm in &manifests {
            concat.push_str(&sm.manifest.object_id);
            concat.push(':');
            concat.push_str(&sm.manifest_generation.to_string());
            concat.push(':');
            concat.push_str(&sm.signature_hex);
            concat.push('\n');
        }
        let bundle_digest = sha256_bytes(concat.as_bytes()).to_hex();
        let signing_msg = format!(
            "FSP-METADATA-BACKUP-V1\nsigner={}\ncount={}\ndigest={}\n",
            signer.signer_id(),
            manifests.len(),
            bundle_digest
        );
        let sig = signer.signing_key.sign(signing_msg.as_bytes());
        Ok(Self {
            bundle_version: 1,
            signer_id: signer.signer_id().to_string(),
            public_key_hex: signer.public_key_hex(),
            signed_manifests: manifests,
            bundle_digest,
            bundle_signature_hex: encode_hex(&sig.to_bytes()),
        })
    }

    /// Verifies the backup bundle's SHA-256 digest, bundle-level Ed25519 signature, and every
    /// contained [`SignedObjectManifest`], advancing the [`TrustedSignerPolicy`] generation table.
    pub fn verify_and_restore(
        &self,
        policy: &mut TrustedSignerPolicy,
    ) -> Result<Vec<SignedObjectManifest>, SignatureError> {
        if self.bundle_version != 1 {
            return Err(SignatureError::BackupIntegrityError(format!(
                "unsupported bundle_version={}",
                self.bundle_version
            )));
        }
        let pinned_vk = policy
            .trusted_signers
            .get(&self.signer_id)
            .ok_or_else(|| SignatureError::UnknownSigner(self.signer_id.clone()))?;
        if encode_hex(pinned_vk.as_bytes()) != self.public_key_hex {
            return Err(SignatureError::PublicKeyMismatch {
                signer_id: self.signer_id.clone(),
                expected_hex: encode_hex(pinned_vk.as_bytes()),
                actual_hex: self.public_key_hex.clone(),
            });
        }

        let mut concat = String::new();
        for sm in &self.signed_manifests {
            concat.push_str(&sm.manifest.object_id);
            concat.push(':');
            concat.push_str(&sm.manifest_generation.to_string());
            concat.push(':');
            concat.push_str(&sm.signature_hex);
            concat.push('\n');
        }
        let expected_digest = sha256_bytes(concat.as_bytes()).to_hex();
        if expected_digest != self.bundle_digest {
            return Err(SignatureError::BackupIntegrityError(format!(
                "bundle_digest mismatch: expected {}, got {}",
                expected_digest, self.bundle_digest
            )));
        }

        let signing_msg = format!(
            "FSP-METADATA-BACKUP-V1\nsigner={}\ncount={}\ndigest={}\n",
            self.signer_id,
            self.signed_manifests.len(),
            self.bundle_digest
        );
        let sig_bytes = decode_hex_exact::<64>(&self.bundle_signature_hex)
            .map_err(SignatureError::BackupIntegrityError)?;
        let sig = Signature::from_bytes(&sig_bytes);
        pinned_vk
            .verify_strict(signing_msg.as_bytes(), &sig)
            .map_err(|e| SignatureError::BackupIntegrityError(e.to_string()))?;

        for sm in &self.signed_manifests {
            policy.verify_and_advance_generation(sm, false)?;
        }
        Ok(self.signed_manifests.clone())
    }

    /// Saves a [`SignedObjectManifest`] atomically to `<path>`.
    pub fn save_signed_manifest_file(
        path: &Path,
        signed: &SignedObjectManifest,
    ) -> Result<(), String> {
        let tmp = path.with_extension("sig.json.tmp");
        write_and_sync_file(&tmp, signed.to_json().as_bytes()).map_err(|e| e.to_string())?;
        fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Encodes arbitrary bytes as lowercase hexadecimal.
pub fn encode_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{:02x}", b);
    }
    s
}

/// Decodes a fixed-size byte array `N` from a hexadecimal string.
pub fn decode_hex_exact<const N: usize>(hex: &str) -> Result<[u8; N], String> {
    if hex.len() != N * 2 {
        return Err(format!("expected {} hex chars, got {}", N * 2, hex.len()));
    }
    let mut out = [0u8; N];
    let bytes = hex.as_bytes();
    for i in 0..N {
        let hi = (bytes[i * 2] as char)
            .to_digit(16)
            .ok_or_else(|| format!("invalid hex char at {}", i * 2))?;
        let lo = (bytes[i * 2 + 1] as char)
            .to_digit(16)
            .ok_or_else(|| format!("invalid hex char at {}", i * 2 + 1))?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Ok(out)
}

fn extract_str(haystack: &str, key: &str) -> Result<String, SignatureError> {
    let pos = haystack
        .find(key)
        .ok_or_else(|| SignatureError::ParseError(format!("missing {}", key)))?;
    let rest = haystack[pos + key.len()..].trim_start();
    if !rest.starts_with('"') {
        return Err(SignatureError::ParseError(format!(
            "expected '\"' after {}",
            key
        )));
    }
    let after = &rest[1..];
    let end = after
        .find('"')
        .ok_or_else(|| SignatureError::ParseError(format!("unclosed string after {}", key)))?;
    Ok(after[..end].to_string())
}

fn extract_u64(haystack: &str, key: &str) -> Result<u64, SignatureError> {
    let pos = haystack
        .find(key)
        .ok_or_else(|| SignatureError::ParseError(format!("missing {}", key)))?;
    let rest = haystack[pos + key.len()..].trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if end == 0 {
        return Err(SignatureError::ParseError(format!(
            "expected integer after {}",
            key
        )));
    }
    rest[..end]
        .parse::<u64>()
        .map_err(|e| SignatureError::ParseError(format!("invalid integer after {}: {}", key, e)))
}

#[allow(dead_code)]
fn _use_sha256_digest(_d: Sha256Digest) {}
