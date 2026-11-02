//! Phase 4 Cryptographic Security Subsystem (`security`).
//!
//! Provides:
//! - RFC 8032 Ed25519 manifest signing, canonical domain separation, signer trust policies,
//!   monotonic generation replay protection, and signed coordinator metadata backup/restore (`signatures`).
//! - Mutual TLS 1.3 (`mTLS`) X.509 certificate provisioning, node identity binding, role-based
//!   RPC authorization, and structured authentication failure auditing (`authorization`).

pub mod authorization;
pub mod recovery_auth;
pub mod signatures;
