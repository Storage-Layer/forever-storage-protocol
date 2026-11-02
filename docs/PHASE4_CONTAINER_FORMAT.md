# Phase 4 Container Format v2
Per-shard multi-stripe file `shard-YY.vZ.fspc` ([container.rs](file:///home/rade/Project/Jobs/fsp/src/storage/container.rs)).
- 256-byte header: magic `FSPC\0\2\0\0`, version, scheme (RS/LRC n,k,l), shard_id, version, stripe_count, offsets, object_id, body SHA-256 (bytes 192..224), header+index SHA-256 (224..256).
- Data region (offset 256) holds stripe shard bytes contiguously; index table (52 B/entry: stripe idx, payload_len, shard_len, offset, SHA-256) follows.
- `open()` validates magic, version, offsets (checked arithmetic), file size, and header+index digest before any read. `read_stripe` verifies per-stripe SHA-256.
- Effect: 100 MiB RS(20,12) object = 20 files instead of 2,000 (measured, see benchmarks).
- v1 `.fsp` remains readable; `migrate_object_v1_to_v2` converts (test_p4_04).
