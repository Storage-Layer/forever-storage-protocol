# FSP Phase 4.1 — Manifest Generation Watermarking & Rollback Defense

## 1. Problem & Vulnerability

If a coordinator manifest directory was wiped or restored from an older backup snapshot, in-memory watermark tracking was reset, potentially allowing older signed manifest generations to be replayed or accepted as newer state.

## 2. Durable Watermark Tracking

Phase 4.1 extends [`TrustedSignerPolicy`](file:///home/rade/Project/Jobs/fsp/src/security/signatures.rs):
- Monotonic generation records are written to `<coord_root>/generations/<object_id>.gen`.
- Every generation advance fsyncs the new watermark before publishing the signed manifest.
- Replayed manifests with `generation <= recorded_gen` are rejected immediately with `SignatureError::ReplayGenerationRejected`.
