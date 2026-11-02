//! Mutual TLS 1.3 (`mTLS`), X.509 Certificate Chain Verification, Node Identity Binding,
//! Certificate Rotation, Role-Based Authorization, and Authentication Failure Auditing.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{ClientConfig, RootCertStore, ServerConfig};

use crate::model::node::NodeId;
use crate::security::signatures::encode_hex;
use crate::storage::integrity::sha256_bytes;
use crate::storage::manifest::validate_safe_identifier;

/// Deployment security mode preventing development/test trust anchors from being silently accepted
/// in production configurations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustEnvironmentMode {
    /// Explicit local development / integration test sandbox mode.
    DevelopmentSandbox,
    /// Production mode: rejects certificates or CAs marked as test-only.
    ProductionStrict,
}

/// Authenticated peer role derived from a verified mTLS client or server certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerRole {
    /// Trusted FSP Coordinator authorized to issue `PutShard`, `DeleteShard`, `VerifyShard`, `GetShard`, etc.
    Coordinator { coordinator_id: String },
    /// Authenticated storage node daemon bound to a specific [`NodeId`].
    StorageNode { node_id: NodeId, node_name: String },
    /// Read-only observer / health monitor allowed only `Heartbeat` and `GetNodeStatus`.
    ReadOnlyObserver { observer_id: String },
}

/// Network RPC operations subject to role-based authorization on a storage node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcOperation {
    PutShard,
    GetShard,
    DeleteShard,
    VerifyShard,
    ListShards,
    Heartbeat,
    GetNodeStatus,
}

impl RpcOperation {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::PutShard => "PutShard",
            Self::GetShard => "GetShard",
            Self::DeleteShard => "DeleteShard",
            Self::VerifyShard => "VerifyShard",
            Self::ListShards => "ListShards",
            Self::Heartbeat => "Heartbeat",
            Self::GetNodeStatus => "GetNodeStatus",
        }
    }
}

/// Errors returned during TLS configuration, peer identity verification, or RPC authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// Certificate or key file I/O or PEM parsing error.
    CertificateLoadError(String),
    /// Peer did not present an X.509 certificate during the mTLS handshake.
    MissingPeerCertificate,
    /// Certificate is not registered in the [`ClusterTrustPolicy`].
    UnknownCertificateFingerprint(String),
    /// Certificate has expired (`now_unix_secs > not_after_unix_secs`).
    ExpiredCertificate {
        identity_id: String,
        not_after_unix_secs: u64,
        now_unix_secs: u64,
    },
    /// Peer's authenticated certificate identity does not match the claimed `NodeId` or `Coordinator`.
    NodeIdentityMismatch {
        claimed_identity: String,
        bound_identity: String,
    },
    /// Authenticated role is not authorized to execute the requested [`RpcOperation`].
    UnauthorizedOperation {
        identity_id: String,
        role: String,
        operation: &'static str,
    },
    /// Test-only trust anchor rejected in `ProductionStrict` mode.
    TestTrustRootRejectedInProduction(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CertificateLoadError(msg) => write!(f, "Certificate/key load error: {}", msg),
            Self::MissingPeerCertificate => {
                write!(f, "Peer failed to present an mTLS X.509 certificate")
            }
            Self::UnknownCertificateFingerprint(fp) => {
                write!(f, "Untrusted certificate SHA-256 fingerprint: {}", fp)
            }
            Self::ExpiredCertificate {
                identity_id,
                not_after_unix_secs,
                now_unix_secs,
            } => write!(
                f,
                "Expired certificate for {:?}: expired at {}, current time {}",
                identity_id, not_after_unix_secs, now_unix_secs
            ),
            Self::NodeIdentityMismatch {
                claimed_identity,
                bound_identity,
            } => write!(
                f,
                "Certificate identity mismatch: peer claimed {:?}, but certificate is bound to {:?}",
                claimed_identity, bound_identity
            ),
            Self::UnauthorizedOperation {
                identity_id,
                role,
                operation,
            } => write!(
                f,
                "Unauthorized operation {} attempted by {:?} (role={})",
                operation, identity_id, role
            ),
            Self::TestTrustRootRejectedInProduction(msg) => {
                write!(f, "Production policy rejected test trust root: {}", msg)
            }
        }
    }
}

impl std::error::Error for AuthError {}

/// Bound identity entry in the [`ClusterTrustPolicy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundIdentityEntry {
    /// Canonical identity string (`"coordinator-01"`, `"node-00"`, etc.).
    pub identity_id: String,
    /// Assigned [`PeerRole`].
    pub role: PeerRole,
    /// Active and rotated leaf certificate SHA-256 hex fingerprints accepted for this identity.
    pub allowed_cert_fingerprints: BTreeSet<String>,
    /// Certificate expiration timestamp (Unix epoch seconds).
    pub not_after_unix_secs: u64,
    /// Whether this identity was provisioned by a development/test CA.
    pub is_test_only: bool,
}

/// Structured audit log entry recorded whenever authentication or authorization fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthAuditEvent {
    pub timestamp_unix_secs: u64,
    pub peer_fingerprint: Option<String>,
    pub claimed_identity: Option<String>,
    pub operation: Option<&'static str>,
    pub reason: String,
}

/// Thread-safe authentication and authorization policy manager with certificate rotation and
/// failure audit logging.
#[derive(Debug, Clone)]
pub struct ClusterTrustPolicy {
    mode: TrustEnvironmentMode,
    identities_by_id: BTreeMap<String, BoundIdentityEntry>,
    fingerprint_to_id: BTreeMap<String, String>,
    audit_log: Arc<Mutex<Vec<AuthAuditEvent>>>,
}

impl ClusterTrustPolicy {
    /// Creates a new [`ClusterTrustPolicy`] for the given [`TrustEnvironmentMode`].
    pub fn new(mode: TrustEnvironmentMode) -> Self {
        Self {
            mode,
            identities_by_id: BTreeMap::new(),
            fingerprint_to_id: BTreeMap::new(),
            audit_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Registers an identity and its initial leaf certificate SHA-256 fingerprint.
    pub fn register_identity(&mut self, entry: BoundIdentityEntry) -> Result<(), AuthError> {
        if !validate_safe_identifier(&entry.identity_id) {
            return Err(AuthError::CertificateLoadError(format!(
                "unsafe identity_id {:?}",
                entry.identity_id
            )));
        }
        if self.mode == TrustEnvironmentMode::ProductionStrict && entry.is_test_only {
            return Err(AuthError::TestTrustRootRejectedInProduction(
                entry.identity_id.clone(),
            ));
        }
        for fp in &entry.allowed_cert_fingerprints {
            self.fingerprint_to_id
                .insert(fp.clone(), entry.identity_id.clone());
        }
        self.identities_by_id
            .insert(entry.identity_id.clone(), entry);
        Ok(())
    }

    /// Adds a newly rotated certificate fingerprint to an existing identity (and optionally revokes old ones).
    pub fn rotate_certificate(
        &mut self,
        identity_id: &str,
        new_cert_der: &[u8],
        new_not_after_unix_secs: u64,
        revoke_previous: bool,
    ) -> Result<String, AuthError> {
        let new_fp = cert_der_sha256_hex(new_cert_der);
        let entry = self.identities_by_id.get_mut(identity_id).ok_or_else(|| {
            AuthError::NodeIdentityMismatch {
                claimed_identity: identity_id.to_string(),
                bound_identity: "<unregistered>".to_string(),
            }
        })?;
        if revoke_previous {
            for old_fp in &entry.allowed_cert_fingerprints {
                self.fingerprint_to_id.remove(old_fp);
            }
            entry.allowed_cert_fingerprints.clear();
        }
        entry.allowed_cert_fingerprints.insert(new_fp.clone());
        entry.not_after_unix_secs = new_not_after_unix_secs;
        self.fingerprint_to_id
            .insert(new_fp.clone(), identity_id.to_string());
        Ok(new_fp)
    }

    /// Verifies a peer's presented leaf X.509 DER certificate, checks expiration at `now_unix_secs`,
    /// and optionally verifies that the certificate is bound to `expected_node_id`.
    pub fn verify_peer_certificate(
        &self,
        peer_certs: Option<&[CertificateDer<'_>]>,
        expected_node_id: Option<NodeId>,
        now_unix_secs: u64,
    ) -> Result<BoundIdentityEntry, AuthError> {
        let certs = peer_certs.ok_or_else(|| {
            self.record_failure(now_unix_secs, None, None, None, "missing peer certificate");
            AuthError::MissingPeerCertificate
        })?;
        let leaf = certs.first().ok_or_else(|| {
            self.record_failure(now_unix_secs, None, None, None, "empty certificate chain");
            AuthError::MissingPeerCertificate
        })?;

        let fp = cert_der_sha256_hex(leaf.as_ref());
        let Some(identity_id) = self.fingerprint_to_id.get(&fp) else {
            self.record_failure(
                now_unix_secs,
                Some(fp.clone()),
                None,
                None,
                "untrusted certificate fingerprint",
            );
            return Err(AuthError::UnknownCertificateFingerprint(fp));
        };

        let entry = self.identities_by_id.get(identity_id).unwrap();
        if self.mode == TrustEnvironmentMode::ProductionStrict && entry.is_test_only {
            self.record_failure(
                now_unix_secs,
                Some(fp),
                Some(identity_id.clone()),
                None,
                "test certificate rejected in production mode",
            );
            return Err(AuthError::TestTrustRootRejectedInProduction(
                identity_id.clone(),
            ));
        }

        if now_unix_secs > entry.not_after_unix_secs {
            self.record_failure(
                now_unix_secs,
                Some(fp),
                Some(identity_id.clone()),
                None,
                "certificate expired",
            );
            return Err(AuthError::ExpiredCertificate {
                identity_id: identity_id.clone(),
                not_after_unix_secs: entry.not_after_unix_secs,
                now_unix_secs,
            });
        }

        if let Some(exp_node) = expected_node_id {
            match &entry.role {
                PeerRole::StorageNode { node_id, .. } if *node_id == exp_node => {}
                _ => {
                    let claimed = format!("node-{:02}", exp_node.0);
                    self.record_failure(
                        now_unix_secs,
                        Some(fp),
                        Some(claimed.clone()),
                        None,
                        "node identity mismatch",
                    );
                    return Err(AuthError::NodeIdentityMismatch {
                        claimed_identity: claimed,
                        bound_identity: entry.identity_id.clone(),
                    });
                }
            }
        }

        Ok(entry.clone())
    }

    /// Verifies that `peer` is authorized to invoke `operation` on a storage node.
    pub fn authorize_operation(
        &self,
        peer: &BoundIdentityEntry,
        operation: RpcOperation,
        now_unix_secs: u64,
    ) -> Result<(), AuthError> {
        let allowed = match &peer.role {
            PeerRole::Coordinator { .. } => true,
            PeerRole::StorageNode { .. } => matches!(
                operation,
                RpcOperation::GetShard | RpcOperation::Heartbeat | RpcOperation::GetNodeStatus
            ),
            PeerRole::ReadOnlyObserver { .. } => {
                matches!(
                    operation,
                    RpcOperation::Heartbeat | RpcOperation::GetNodeStatus
                )
            }
        };

        if !allowed {
            let role_str = format!("{:?}", peer.role);
            self.record_failure(
                now_unix_secs,
                None,
                Some(peer.identity_id.clone()),
                Some(operation.as_str()),
                "unauthorized RPC operation for role",
            );
            return Err(AuthError::UnauthorizedOperation {
                identity_id: peer.identity_id.clone(),
                role: role_str,
                operation: operation.as_str(),
            });
        }
        Ok(())
    }

    /// Returns a snapshot of all recorded authentication/authorization failure events.
    pub fn audit_events(&self) -> Vec<AuthAuditEvent> {
        self.audit_log
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn record_failure(
        &self,
        timestamp_unix_secs: u64,
        peer_fingerprint: Option<String>,
        claimed_identity: Option<String>,
        operation: Option<&'static str>,
        reason: &str,
    ) {
        let mut log = self.audit_log.lock().unwrap_or_else(|p| p.into_inner());
        log.push(AuthAuditEvent {
            timestamp_unix_secs,
            peer_fingerprint,
            claimed_identity,
            operation,
            reason: reason.to_string(),
        });
    }
}

/// Computes the lowercase 64-character SHA-256 hex fingerprint of a DER-encoded X.509 certificate.
pub fn cert_der_sha256_hex(der: &[u8]) -> String {
    encode_hex(&sha256_bytes(der).0)
}

/// Loads PEM-encoded X.509 certificates from `path`.
pub fn load_certs_from_pem(path: &Path) -> Result<Vec<CertificateDer<'static>>, AuthError> {
    let file = File::open(path).map_err(|e| {
        AuthError::CertificateLoadError(format!("open cert {}: {}", path.display(), e))
    })?;
    let mut reader = BufReader::new(file);
    let mut certs = Vec::new();
    for item in rustls_pemfile::certs(&mut reader) {
        let c = item.map_err(|e| AuthError::CertificateLoadError(e.to_string()))?;
        certs.push(c);
    }
    if certs.is_empty() {
        return Err(AuthError::CertificateLoadError(format!(
            "no certificates found in {}",
            path.display()
        )));
    }
    Ok(certs)
}

/// Loads a PEM-encoded PKCS#8 / Ed25519 / ECDSA private key from `path`.
pub fn load_private_key_from_pem(path: &Path) -> Result<PrivateKeyDer<'static>, AuthError> {
    let file = File::open(path).map_err(|e| {
        AuthError::CertificateLoadError(format!("open key {}: {}", path.display(), e))
    })?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| AuthError::CertificateLoadError(e.to_string()))?
        .ok_or_else(|| {
            AuthError::CertificateLoadError(format!("no private key found in {}", path.display()))
        })
}

/// Builds a strict TLS 1.3 `rustls::ServerConfig` enforcing mutual TLS (`mTLS`) client certificate
/// chain verification against `ca_cert_pem_path`.
pub fn build_mtls_server_config(
    ca_cert_pem_path: &Path,
    server_cert_pem_path: &Path,
    server_key_pem_path: &Path,
) -> Result<Arc<ServerConfig>, AuthError> {
    let ca_certs = load_certs_from_pem(ca_cert_pem_path)?;
    let mut root_store = RootCertStore::empty();
    for cert in ca_certs {
        root_store
            .add(cert)
            .map_err(|e| AuthError::CertificateLoadError(format!("add CA cert: {}", e)))?;
    }

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let client_verifier =
        WebPkiClientVerifier::builder_with_provider(Arc::new(root_store), provider.clone())
            .build()
            .map_err(|e| AuthError::CertificateLoadError(format!("client verifier: {}", e)))?;

    let server_certs = load_certs_from_pem(server_cert_pem_path)?;
    let server_key = load_private_key_from_pem(server_key_pem_path)?;

    let cfg = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| AuthError::CertificateLoadError(e.to_string()))?
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(server_certs, server_key)
        .map_err(|e| AuthError::CertificateLoadError(format!("server cert/key: {}", e)))?;

    Ok(Arc::new(cfg))
}

/// Builds a strict TLS 1.3 `rustls::ClientConfig` presenting a client certificate (`mTLS`) and
/// verifying the server's X.509 certificate chain against `ca_cert_pem_path`.
pub fn build_mtls_client_config(
    ca_cert_pem_path: &Path,
    client_cert_pem_path: &Path,
    client_key_pem_path: &Path,
) -> Result<Arc<ClientConfig>, AuthError> {
    let ca_certs = load_certs_from_pem(ca_cert_pem_path)?;
    let mut root_store = RootCertStore::empty();
    for cert in ca_certs {
        root_store
            .add(cert)
            .map_err(|e| AuthError::CertificateLoadError(format!("add CA cert: {}", e)))?;
    }

    let client_certs = load_certs_from_pem(client_cert_pem_path)?;
    let client_key = load_private_key_from_pem(client_key_pem_path)?;

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| AuthError::CertificateLoadError(e.to_string()))?
        .with_root_certificates(root_store)
        .with_client_auth_cert(client_certs, client_key)
        .map_err(|e| AuthError::CertificateLoadError(format!("client auth cert/key: {}", e)))?;

    Ok(Arc::new(cfg))
}

/// Bundle of provisioned X.509 v3 PEM paths and [`ClusterTrustPolicy`] for an mTLS cluster.
#[derive(Debug, Clone)]
pub struct ProvisionedClusterPki {
    pub pki_dir: PathBuf,
    pub ca_cert_pem: PathBuf,
    pub coordinator_cert_pem: PathBuf,
    pub coordinator_key_pem: PathBuf,
    pub observer_cert_pem: PathBuf,
    pub observer_key_pem: PathBuf,
    pub rogue_cert_pem: PathBuf,
    pub rogue_key_pem: PathBuf,
    pub node_certs: BTreeMap<NodeId, (PathBuf, PathBuf)>,
    pub trust_policy: ClusterTrustPolicy,
}

/// Provisions a complete ECDSA P-256 X.509 v3 PKI hierarchy inside `pki_dir` (for local multi-process
/// tests or testbench clusters):
/// - Root CA (`ca.crt`)
/// - Coordinator mTLS cert/key (`coordinator.crt`, `coordinator.key`)
/// - `num_nodes` Storage Node mTLS certs/keys (`node-00.crt`, `node-00.key`, ...) with SAN `DNS:localhost,IP:127.0.0.1`
/// - Read-only observer cert/key (`observer.crt`, `observer.key`)
/// - Untrusted rogue CA + client cert/key (`rogue.crt`, `rogue.key`) for negative security tests.
///
/// Enforces `0o600` permissions on all private key files.
pub fn provision_cluster_pki(
    pki_dir: &Path,
    num_nodes: usize,
) -> Result<ProvisionedClusterPki, AuthError> {
    fs::create_dir_all(pki_dir).map_err(|e| AuthError::CertificateLoadError(e.to_string()))?;

    let ca_key = pki_dir.join("ca.key");
    let ca_cert = pki_dir.join("ca.crt");
    run_openssl(&[
        "req",
        "-x509",
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:prime256v1",
        "-nodes",
        "-keyout",
        ca_key.to_str().unwrap(),
        "-out",
        ca_cert.to_str().unwrap(),
        "-days",
        "365",
        "-subj",
        "/CN=FSP-Cluster-Test-Root-CA",
        "-addext",
        "basicConstraints=critical,CA:TRUE",
        "-addext",
        "keyUsage=critical,keyCertSign,cRLSign",
    ])?;
    restrict_key_permissions(&ca_key)?;

    let mut trust_policy = ClusterTrustPolicy::new(TrustEnvironmentMode::DevelopmentSandbox);
    let not_after = 4_102_444_800u64; // 2100-01-01

    // Issue Coordinator certificate
    let (coord_cert, coord_key, coord_fp) = issue_leaf_cert(
        pki_dir,
        &ca_cert,
        &ca_key,
        "coordinator-primary",
        "DNS:localhost,IP:127.0.0.1",
    )?;
    trust_policy.register_identity(BoundIdentityEntry {
        identity_id: "coordinator-primary".to_string(),
        role: PeerRole::Coordinator {
            coordinator_id: "coordinator-primary".to_string(),
        },
        allowed_cert_fingerprints: [coord_fp].into_iter().collect(),
        not_after_unix_secs: not_after,
        is_test_only: true,
    })?;

    // Issue Read-Only Observer certificate (for unauthorized write testing)
    let (obs_cert, obs_key, obs_fp) = issue_leaf_cert(
        pki_dir,
        &ca_cert,
        &ca_key,
        "observer-readonly",
        "DNS:localhost,IP:127.0.0.1",
    )?;
    trust_policy.register_identity(BoundIdentityEntry {
        identity_id: "observer-readonly".to_string(),
        role: PeerRole::ReadOnlyObserver {
            observer_id: "observer-readonly".to_string(),
        },
        allowed_cert_fingerprints: [obs_fp].into_iter().collect(),
        not_after_unix_secs: not_after,
        is_test_only: true,
    })?;

    // Issue Node certificates
    let mut node_certs = BTreeMap::new();
    for i in 0..num_nodes {
        let node_id = NodeId(i);
        let name = format!("node-{:02}", i);
        let san = format!("DNS:{}.fsp.internal,DNS:localhost,IP:127.0.0.1", name);
        let (n_cert, n_key, n_fp) = issue_leaf_cert(pki_dir, &ca_cert, &ca_key, &name, &san)?;
        trust_policy.register_identity(BoundIdentityEntry {
            identity_id: name.clone(),
            role: PeerRole::StorageNode {
                node_id,
                node_name: name,
            },
            allowed_cert_fingerprints: [n_fp].into_iter().collect(),
            not_after_unix_secs: not_after,
            is_test_only: true,
        })?;
        node_certs.insert(node_id, (n_cert, n_key));
    }

    // Issue Rogue CA + Rogue Client certificate (not signed by Cluster Root CA)
    let rogue_ca_key = pki_dir.join("rogue_ca.key");
    let rogue_ca_cert = pki_dir.join("rogue_ca.crt");
    run_openssl(&[
        "req",
        "-x509",
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:prime256v1",
        "-nodes",
        "-keyout",
        rogue_ca_key.to_str().unwrap(),
        "-out",
        rogue_ca_cert.to_str().unwrap(),
        "-days",
        "365",
        "-subj",
        "/CN=Rogue-Untrusted-CA",
        "-addext",
        "basicConstraints=critical,CA:TRUE",
    ])?;
    restrict_key_permissions(&rogue_ca_key)?;
    let (rogue_cert, rogue_key, _) = issue_leaf_cert(
        pki_dir,
        &rogue_ca_cert,
        &rogue_ca_key,
        "rogue-client",
        "DNS:localhost,IP:127.0.0.1",
    )?;

    Ok(ProvisionedClusterPki {
        pki_dir: pki_dir.to_path_buf(),
        ca_cert_pem: ca_cert,
        coordinator_cert_pem: coord_cert,
        coordinator_key_pem: coord_key,
        observer_cert_pem: obs_cert,
        observer_key_pem: obs_key,
        rogue_cert_pem: rogue_cert,
        rogue_key_pem: rogue_key,
        node_certs,
        trust_policy,
    })
}

/// Issues a new rotated leaf certificate signed by `ca_cert`/`ca_key` for `common_name`.
pub fn issue_leaf_cert(
    pki_dir: &Path,
    ca_cert: &Path,
    ca_key: &Path,
    common_name: &str,
    san: &str,
) -> Result<(PathBuf, PathBuf, String), AuthError> {
    let key_path = pki_dir.join(format!("{}.key", common_name));
    let csr_path = pki_dir.join(format!("{}.csr", common_name));
    let cert_path = pki_dir.join(format!("{}.crt", common_name));
    let ext_path = pki_dir.join(format!("{}.ext", common_name));

    let ext_contents = format!(
        "basicConstraints=CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth,clientAuth\nsubjectAltName={}\n",
        san
    );
    fs::write(&ext_path, ext_contents.as_bytes())
        .map_err(|e| AuthError::CertificateLoadError(e.to_string()))?;

    run_openssl(&[
        "req",
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:prime256v1",
        "-nodes",
        "-keyout",
        key_path.to_str().unwrap(),
        "-out",
        csr_path.to_str().unwrap(),
        "-subj",
        &format!("/CN={}", common_name),
    ])?;
    restrict_key_permissions(&key_path)?;

    run_openssl(&[
        "x509",
        "-req",
        "-in",
        csr_path.to_str().unwrap(),
        "-CA",
        ca_cert.to_str().unwrap(),
        "-CAkey",
        ca_key.to_str().unwrap(),
        "-CAcreateserial",
        "-out",
        cert_path.to_str().unwrap(),
        "-days",
        "365",
        "-extfile",
        ext_path.to_str().unwrap(),
    ])?;

    let _ = fs::remove_file(&csr_path);
    let _ = fs::remove_file(&ext_path);

    let certs = load_certs_from_pem(&cert_path)?;
    let fp = cert_der_sha256_hex(certs[0].as_ref());
    Ok((cert_path, key_path, fp))
}

fn run_openssl(args: &[&str]) -> Result<(), AuthError> {
    let out = Command::new("openssl")
        .args(args)
        .output()
        .map_err(|e| AuthError::CertificateLoadError(format!("exec openssl: {}", e)))?;
    if !out.status.success() {
        return Err(AuthError::CertificateLoadError(format!(
            "openssl {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

fn restrict_key_permissions(path: &Path) -> Result<(), AuthError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|e| AuthError::CertificateLoadError(e.to_string()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
