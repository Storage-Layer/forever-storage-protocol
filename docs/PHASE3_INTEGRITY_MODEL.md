# FSP Phase 3 — Content Integrity & Metadata Verification Model

## 1. Cryptographic Hash Hierarchy (FIPS 180-4 SHA-256)

FSP Phase 3 enforces a three-level cryptographic digest hierarchy using **SHA-256** (NIST FIPS PUB 180-4, 256-bit / 64-character lowercase hexadecimal output):

1. **Object-Level Digest (`object_digest`):**
   Computed incrementally over the raw unpadded original input stream during ingestion and verified over the reconstructed output stream before any `GET` / `RECONSTRUCT` operation returns success.
2. **Per-Stripe, Per-Shard Digest (`ShardPlacementRecord::sha256_digest`):**
   Computed over the exact `shard_length` bytes of every data and parity shard in every stripe. Verified:
   - After staging write (before transaction commit),
   - Before any shard is admitted as an input to erasure decoding,
   - After any missing/corrupted shard is reconstructed during `REPAIR`.
3. **Manifest Self-Integrity Digest (`manifest_digest`):**
   Computed over the canonical serialized body of the manifest (all fields up to `manifest_digest`). Any modification, bit flip, or truncation of `<object_id>.manifest.json` is rejected prior to shard loading.

---

## 2. Canonical Manifest Schema & Pre-Allocation Validation

Each committed object is described by `<root>/metadata/manifests/<object_id>.manifest.json`:

```json
{
  "format_version": 1,
  "object_id": "obj-100mb-benchmark",
  "object_size": 104857600,
  "stripe_size": 1048576,
  "coding": {
    "scheme": "reed-solomon",
    "n": 20,
    "k": 12,
    "l": null
  },
  "integrity": {
    "algorithm": "sha256",
    "object_digest": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "manifest_digest": "..."
  },
  "stripes": [
    {
      "stripe_index": 0,
      "payload_offset": 0,
      "payload_length": 1048576,
      "shard_length": 87384,
      "placements": [
        {
          "shard_id": 0,
          "node_id": 0,
          "domain_id": 0,
          "version": 1,
          "size_bytes": 87384,
          "sha256_digest": "...",
          "relative_path": "node-00/objects/obj-100mb-benchmark/stripe-000000.shard-00.v1.fsp"
        }
      ]
    }
  ]
}
```

### Mandatory Pre-Allocation Validation Rules (`ObjectManifest::validate`)
Before allocating stripe buffers or opening shard paths, `ObjectManifest::validate` checks:
1. `format_version == 1`.
2. `integrity.algorithm == "sha256"` and `object_digest` / `manifest_digest` are valid 64-character hex strings; recomputed `manifest_digest` matches stored `manifest_digest`.
3. `object_id` contains only safe ASCII characters (`[A-Za-z0-9_.-]`, length `1..=128`, not `.` or `..`).
4. `stripe_size` is within `[64, 67_108_864]` (`64 B` to `64 MiB`), preventing memory-exhaustion DoS from crafted manifests.
5. `coding` parameters satisfy $1 \le k \le n \le 64$ (and for `"lrc"`, $l \ge 1, l \mid k, k + k/l \le n$).
6. For every stripe $s$:
   - `stripe_index == s`, contiguous `payload_offset`, and $\sum_s \text{payload\_length}_s == \text{object\_size}$.
   - `shard_length` matches the deterministic formula $\max(64, \lceil \text{payload\_length}_s / (8k) \rceil \cdot 8)$.
   - Exactly $n$ placements with distinct `shard_id`s $0 \dots n-1$, valid 64-hex `sha256_digest`, `size_bytes == shard_length`, and safe `relative_path` free of `..` or leading `/`.

---

## 3. Explicit Security & Trust Boundary

> [!IMPORTANT]
> **Integrity vs. Byzantine Authenticity Boundary:**
> Unkeyed cryptographic hashes (SHA-256) guarantee **content integrity** against accidental bit-rot, disk sector corruption, torn writes, truncation, and stale shard versions, provided the manifest itself is authentic (or when the caller verifies `object_digest` out-of-band via a content-addressed identifier).
> An adversary with write access to **both** the shard directories and `<root>/metadata/manifests/` could replace both the shard files and the manifest with a different valid object and recompute `manifest_digest` unless the caller either:
> 1. Verifies the expected `object_digest` (content-addressed lookup), or
> 2. Signs the manifest with a cryptographic key (Ed25519 / HMAC-SHA256) stored outside the untrusted storage root.
