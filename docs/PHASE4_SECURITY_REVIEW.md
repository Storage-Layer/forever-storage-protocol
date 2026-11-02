# Phase 4 Security Review (self-review, not an audit)
Covered by tests: see `results/phase4/security_test_results.json`. Findings / residual risks:
1. Coordinator is a single trusted component and SPOF; its signer key is the root of integrity.
2. Scrub challenge is not verified against an independent expected value (see recovery doc) -> no proof-of-storage guarantee.
3. Generation watermark not persisted separately -> rollback possible after metadata loss.
4. No rate limiting / connection limits (thread per connection) -> DoS exposure.
5. No Byzantine tolerance: authenticated but malicious nodes can withhold data; corruption is detected via SHA-256, not prevented.
6. Key files are created 0600; node/coordinator private keys are passed by path; no HSM/KMS.
7. Test PKI uses `openssl` CLI and expires far in the future; production provisioning is not implemented.
Do not treat this system as production ready.
