# FSP Phase 4.1 — Independent Coordinator Scrubbing

## 1. Threat Model & Rationale

In Phase 4, storage nodes reported their own health status and responded to challenges. A compromised or buggy node could:
- Compute challenge responses from retained hashes without holding data.
- Return false "Healthy" flags while data blocks suffered silent bit rot.

## 2. Coordinator-Side Independent Scrub Verification

Phase 4.1 adds independent verification to [`BackgroundScrubber`](file:///home/rade/Project/Jobs/fsp/src/coordinator/scheduler.rs):
- Coordinator issues `GetShard` requests to stream shard bytes directly into local memory.
- Bounded chunk processing (64 KiB buffers) limits coordinator RAM usage.
- Locally computed SHA-256 digest is compared against the trusted signed manifest placement record.
- Any discrepancy flags the shard as corrupted and triggers immediate repair queueing.
