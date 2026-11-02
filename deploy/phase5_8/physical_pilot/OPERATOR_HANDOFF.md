# FSP Phase 5.8: Physical Two-Host Pilot Operator Handoff & Authorization Guide

## 1. Executive Status: Physical Pilot Blocked

```
+─────────────────────────────────────────────────────────────────────────────+
| PHYSICAL_PILOT_STATUS = BLOCKED                                             |
| REASON = NO_AUTHORIZED_SECOND_PHYSICAL_HOST                                  |
+─────────────────────────────────────────────────────────────────────────────+
```

In accordance with strict security engineering guidelines:
- Host A (Intel Xeon X5650, 62 GiB RAM, Linux Mint 22.3) has been fully audited, configured, and verified.
- The candidate secondary physical host (Lenovo Laptop, AMD Ryzen 7, ~40 GB RAM) has **not** been explicitly provisioned with an authorized IP endpoint, SSH keypair, or designated storage root.
- Unauthenticated subnet scanning (`nmap`/ARP sweeps), password guessing, or probing arbitrary machines is prohibited.

To transition the physical pilot from `BLOCKED` to active execution, the operator must provide the items below.

---

## 2. Minimum Operator Configuration Requirements

| Requirement | Description | Example / Recommended Value |
| :--- | :--- | :--- |
| **Host B IP Address** | Static or DHCP-reserved IP reachable from Host A via `enp2s0` or `wlx7c8bca05e8a9` | `10.77.77.3` or `192.168.1.104` |
| **Host B SSH Access** | Dedicated non-root user with key-based SSH authentication from Host A | User `fsp` with `~/.ssh/authorized_keys` |
| **Approved Storage Root** | Persistent filesystem path for storage node daemons 10..19 | `/var/lib/fsp/data` or `/tmp/fsp_host_b_data` |
| **Service Port Range** | 10 dedicated TCP ports for storage daemons + 1 for standby coordinator | TCP `9110` - `9119` (nodes), TCP `9000` (coord) |
| **Firewall (UFW / iptables)** | Inbound traffic permitted from Host A (`10.77.77.2` or `192.168.1.103`) | `ufw allow from 10.77.77.2 to any port 9110:9119 proto tcp` |

---

## 3. Remote Host (Host B) Preparation Procedure

### Step 1: Create Dedicated User and Storage Root on Host B
```bash
sudo useradd -m -s /bin/bash fsp
sudo mkdir -p /var/lib/fsp/data /etc/fsp/pki
sudo chown -R fsp:fsp /var/lib/fsp /etc/fsp
```

### Step 2: Authorize SSH Key from Host A
On Host A:
```bash
cat ~/.ssh/id_ed25519.pub
```
On Host B:
```bash
sudo -u fsp mkdir -p /home/fsp/.ssh && sudo -u fsp chmod 700 /home/fsp/.ssh
echo "<HOST_A_PUBLIC_KEY>" | sudo -u fsp tee -a /home/fsp/.ssh/authorized_keys
sudo chmod 600 /home/fsp/.ssh/authorized_keys
```

### Step 3: Configure Firewall on Host B
```bash
sudo ufw allow from 10.77.77.2 to any port 9110:9119 proto tcp comment "FSP Storage Nodes"
sudo ufw allow from 10.77.77.2 to any port 9000 proto tcp comment "FSP Coordinator"
```

---

## 4. Binary & PKI Deployment Procedure

### Step 1: Transfer Pre-Built Release Binary to Host B
```bash
scp target/release/fsp-node fsp@<HOST_B_IP>:/usr/local/bin/fsp-node
ssh fsp@<HOST_B_IP> "chmod +x /usr/local/bin/fsp-node"
```

### Step 2: Generate and Provision Cluster PKI
Generate mTLS certificates including Host B's verified IP in the Subject Alternative Name (SAN):
```bash
# On Host A:
./deploy/phase5_8/certificates/generate_pki.sh /tmp/fsp_pki

# Copy Host B specific certs and cluster CA:
scp /tmp/fsp_pki/ca.crt fsp@<HOST_B_IP>:/etc/fsp/pki/
scp /tmp/fsp_pki/coordinator.crt fsp@<HOST_B_IP>:/etc/fsp/pki/
for i in $(seq 10 19); do
    padded=$(printf "%02d" "$i")
    scp /tmp/fsp_pki/node-$padded.crt /tmp/fsp_pki/node-$padded.key fsp@<HOST_B_IP>:/etc/fsp/pki/
done
```

---

## 5. Execution Orchestration Commands (Post-Authorization)

Once the operator exports the authorization variables:
```bash
export FSP_REMOTE_HOST="<HOST_B_IP>"
export FSP_REMOTE_USER="fsp"
```

Execute the automated physical pilot runner:
```bash
./deploy/phase5_8/scripts/deploy_two_host.sh
```

This will automatically:
1. Validate SSH connectivity and machine architecture on Host B.
2. Launch Storage Nodes 0..9 on Host A and Storage Nodes 10..19 on Host B.
3. Establish cross-host mTLS 1.3 TCP connections.
4. Execute authenticated 100 MiB RS(20,12) PUT across the physical network.
5. Remotely terminate and restart Host B storage daemons.
6. Execute client GET and verify byte-for-byte SHA-256 reconstruction.
7. Record physical network interface byte counters and transfer latencies.
