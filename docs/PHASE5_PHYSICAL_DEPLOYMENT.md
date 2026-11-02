# Phase 5 Physical Deployment Guide & Staging Status
**Forever Storage Protocol (FSP)**
**Date:** 2026-11-01  
**Author:** Principal Storage Infrastructure Architect & Site Reliability Engineer  
**Staging Artifacts:** [`deploy/phase5/`](file:///home/rade/Project/Jobs/fsp/deploy/phase5)

---

## 1. Physical Deployment Execution Status

> [!IMPORTANT]
> **Physical Bare-Metal Multi-Host Deployment: NOT EXECUTED**  
> In accordance with project test environment parameters, physical installation across five separate physical bare-metal hardware servers was **NOT EXECUTED** in this session.  
> All deployment artifacts, configurations, systemd units, network bindings, and PKI setups have been engineered, staged, and validated via local multi-process distributed emulation.

---

## 2. Prepared Deployment Architecture

The physical deployment specification is staged in [`deploy/phase5/`](file:///home/rade/Project/Jobs/fsp/deploy/phase5):

```
deploy/phase5/
├── cluster_5host.toml          # 5-host coordinator cluster configuration
├── coordinator.toml            # Coordinator daemon configuration
├── node.toml                   # Storage node daemon template
├── topology.json               # Formal failure domain topology descriptor
├── systemd/
│   ├── fsp-coordinator.service # Coordinator systemd service unit
│   └── fsp-node@.service       # Templated storage daemon systemd service unit
├── preflight.sh                # Preflight validation entrypoint script
├── emergency_read.sh           # Offline emergency recovery runner script
└── monitoring_prometheus.yml   # Prometheus scrape configuration
```

---

## 3. Server Hardware & Network Requirements

| Specification | Minimum Requirement | Recommended Production |
|---|---|---|
| **Server Nodes** | 5 physical machines | 5+ physical machines across $\ge 2$ racks |
| **CPU** | 8 cores (x86_64) | 16+ cores (AVX2/AVX-512 for GF(2^8) SIMD) |
| **RAM** | 32 GiB ECC DDR4/DDR5 | 64+ GiB ECC |
| **Storage per Host** | 4 distinct NVMe/SSD disks | 4+ enterprise NVMe drives |
| **Network Interface** | Dual 10 GbE bonded (LACP) | Dual 25/100 GbE bonded |
| **OS Distribution** | Ubuntu 24.04 LTS / Debian 12 | Ubuntu 24.04 Noble (kernel $\ge 6.8$) |
| **Time Sync** | chrony (NTP synchronization) | chrony with PTP hardware timestamping |

---

## 4. Step-by-Step Installation Procedure

### Step 1: Install Binaries
Compile and install optimized release binaries on each host:
```bash
cargo build --release
sudo cp target/release/fsp-coordinator /usr/local/bin/
sudo cp target/release/fsp-node /usr/local/bin/
sudo cp target/release/fsp-sim /usr/local/bin/fsp
```

### Step 2: Establish PKI & TLS Mutual Authentication
Generate CA and machine certificates:
```bash
# Certificate Authority
/usr/local/bin/fsp pki init-ca --out /etc/fsp/pki/ca.crt

# Host Alpha (IP 10.0.1.11)
/usr/local/bin/fsp pki sign-cert --ca /etc/fsp/pki/ca.crt --name host-alpha --ip 10.0.1.11
```

### Step 3: Run Preflight Validation
On the designated initial coordinator host:
```bash
/usr/local/bin/fsp preflight --config /etc/fsp/cluster_5host.toml
/usr/local/bin/fsp topology validate --config /etc/fsp/cluster_5host.toml
```

### Step 4: Configure & Enable Systemd Services
Install systemd templates:
```bash
sudo cp deploy/phase5/systemd/fsp-coordinator.service /etc/systemd/system/
sudo cp deploy/phase5/systemd/fsp-node@.service /etc/systemd/system/
sudo systemctl daemon-reload
```

Start coordinator:
```bash
sudo systemctl enable --now fsp-coordinator.service
```

Start the 4 storage node daemons on Host Alpha:
```bash
for node_id in 1 2 3 4; do
    sudo systemctl enable --now fsp-node@${node_id}.service
done
```

---

## 5. Production Pre-Commissioning Checklist

- [ ] All 5 physical machines synchronized within $\le 5$ ms via NTP/chrony.
- [ ] No two storage nodes on the same host share the same physical disk block device.
- [ ] TLS 1.3 cipher suites restricted to AES-256-GCM / ChaCha20-Poly1305.
- [ ] Firewall allows inbound TCP traffic on ports 19400 (coordinator) and 19401–19404 (nodes).
- [ ] Preflight validation confirms 20 nodes, 5 hosts, and $\ge 12$ min surviving shards under 2-host loss.
