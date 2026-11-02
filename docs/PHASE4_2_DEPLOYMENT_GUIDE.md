# FSP Phase 4.2: Physical Multi-Host Deployment Guide

## 1. Scope & Execution Notice

> [!WARNING]
> **Physical Multi-Host Deployment: NOT EXECUTED**
> In accordance with safety constraints, physical multi-host hardware deployment across external physical servers, external SSH commands, and physical network changes was **NOT EXECUTED** in this local workspace.
> The deployment artifacts, configuration files, and systemd units provided below are strictly validated and ready for production staging environments.

---

## 2. Configuration Files

1. **Primary Coordinator:** [`deploy/coordinator-primary.toml`](file:///home/rade/Project/Jobs/fsp/deploy/coordinator-primary.toml)
   - Binds to `0.0.0.0:19443`
   - Governs placement policy, stripe sizes, and cluster membership.
2. **Standby Coordinator:** [`deploy/coordinator-standby.toml`](file:///home/rade/Project/Jobs/fsp/deploy/coordinator-standby.toml)
   - Shadows metadata via WAL replication and participates in 3-member quorum elections.
3. **Quorum Witness:** [`deploy/witness-quorum.toml`](file:///home/rade/Project/Jobs/fsp/deploy/witness-quorum.toml)
   - Lightweight third voting member to guarantee majority elections without holding full object data.
4. **Storage Node Daemons:** [`deploy/node.example.toml`](file:///home/rade/Project/Jobs/fsp/deploy/node.example.toml)
   - Configures storage root, capacity limits, and physical failure domain parameters (`host_domain`, `rack_domain`, `region_domain`).

---

## 3. Systemd Unit Installation

```bash
# Copy binaries
sudo cp target/release/fsp-node /usr/local/bin/

# Install systemd unit files
sudo cp deploy/fsp-coordinator.service /etc/systemd/system/
sudo cp deploy/fsp-witness.service /etc/systemd/system/
sudo cp deploy/fsp-node.service /etc/systemd/system/fsp-node@.service

# Reload and enable
sudo systemctl daemon-reload
sudo systemctl enable --now fsp-node@00
sudo systemctl enable --now fsp-coordinator
```
