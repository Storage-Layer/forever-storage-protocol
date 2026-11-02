# Phase 4.4 Multi-Host Deployment Guide & Systemd Architecture

**Document ID:** `PHASE4_4_MULTIHOST_DEPLOYMENT.md`  
**Protocol Version:** FSP v0.4.4  
**Date:** 2026-11-01  
**Author:** Site Reliability Engineer, Distributed Systems Architect  

---

## 1. Deployment Overview & Status

This document defines the deployment artifacts, systemd unit templates, directory structures, and configuration files for staging an FSP cluster across 5 physical hosts.

> [!WARNING]
> **Physical Multi-Host Deployment Status: NOT EXECUTED**  
> Physical bare-metal server deployment to distinct remote network nodes was **NOT EXECUTED** in this testing cycle because external physical host infrastructure was not provisioned. The deployment templates, systemd units, configuration validation, and multi-node network protocols were verified using multi-process localhost Linux network namespaces.

---

## 2. Systemd Service Unit Files

### 1. Quorum Coordinator Daemon (`deploy/systemd/fsp-coordinator.service`)
Configured to run the primary or standby coordinator with automatic restart, process hardening, and strict path protection.
File: [`deploy/systemd/fsp-coordinator.service`](file:///home/rade/Project/Jobs/fsp/deploy/systemd/fsp-coordinator.service#L1-L24)

```ini
[Unit]
Description=Forever Storage Protocol (FSP) Quorum Coordinator Daemon
Documentation=https://internal.fsp.storage/docs/PHASE4_4_MULTIHOST_DEPLOYMENT.md
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=fsp
Group=fsp
UMask=0077
ExecStart=/usr/local/bin/fsp-coordinator --config /etc/fsp/coordinator.toml
Restart=on-failure
RestartSec=3s
LimitNOFILE=65536
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=/var/lib/fsp/coordinator

[Install]
WantedBy=multi-user.target
```

### 2. Multi-Instance Storage Node Daemon (`deploy/systemd/fsp-node@.service`)
Systemd template unit allowing instantiation of multiple node processes per host (`fsp-node@1.service` .. `fsp-node@4.service`).
File: [`deploy/systemd/fsp-node@.service`](file:///home/rade/Project/Jobs/fsp/deploy/systemd/fsp-node@.service#L1-L27)

```ini
[Unit]
Description=Forever Storage Protocol (FSP) Storage Node Daemon (Instance %i)
Documentation=https://internal.fsp.storage/docs/PHASE4_4_MULTIHOST_DEPLOYMENT.md
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=fsp
Group=fsp
UMask=0077
ExecStart=/usr/local/bin/fsp-node \
  --node-id %i \
  --config /etc/fsp/nodes/node-%i.toml
Restart=on-failure
RestartSec=5s
LimitNOFILE=65536
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=/var/lib/fsp/node-%i

[Install]
WantedBy=multi-user.target
```

---

## 3. Host Staging Instructions

For each physical host `host-alpha` through `host-echo`:

1. **User and Permissions:**
   ```bash
   sudo useradd -r -s /bin/false -d /var/lib/fsp fsp
   sudo mkdir -p /etc/fsp/pki /etc/fsp/nodes /var/lib/fsp
   sudo chown -R fsp:fsp /var/lib/fsp
   sudo chmod 700 /var/lib/fsp
   ```

2. **Install Binaries:**
   ```bash
   sudo cp target/release/fsp-coordinator /usr/local/bin/
   sudo cp target/release/fsp-node /usr/local/bin/
   ```

3. **Install Systemd Units:**
   ```bash
   sudo cp deploy/systemd/fsp-coordinator.service /etc/systemd/system/
   sudo cp deploy/systemd/fsp-node@.service /etc/systemd/system/
   sudo systemctl daemon-reload
   ```

4. **Enable & Start Node Daemons (Instances 1..4 on Host Alpha):**
   ```bash
   for i in 1 2 3 4; do
     sudo systemctl enable --now fsp-node@$i.service
   done
   ```

5. **Preflight Verification:**
   Run the preflight validator against cluster configuration:
   ```bash
   fsp-coordinator --validate-topology deploy/configs/cluster_5host.toml
   ```
