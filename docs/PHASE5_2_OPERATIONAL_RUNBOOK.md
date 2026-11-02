# Phase 5.2 Operational Runbook: Disaster Recovery & Control Plane Rebootstrap

## 1. Routine Safe Online Restore
Use when recovering a single coordinator instance while the remaining majority quorum is healthy.

```bash
# 1. Verify bundle integrity and scan for plaintext secrets
fsp-cli dr verify-bundle --bundle /var/backups/fsp/fsp_metadata_backup.bundle --operator-key /etc/fsp/keys/operator_public.key

# 2. Execute safe online restore (aborts if local commit is higher)
fsp-cli dr restore \
    --mode safe-online \
    --bundle /var/backups/fsp/fsp_metadata_backup.bundle \
    --target-dir /var/lib/fsp/coordinator \
    --operator-key /etc/fsp/keys/operator_public.key

# 3. Start coordinator service
systemctl start fsp-coordinator
```

## 2. Emergency Isolated Rebootstrap
Use ONLY when majority quorum is permanently destroyed and physical control plane recovery is required.

```bash
# Step 1: Place all remaining storage nodes into maintenance mode
ansible-playbook -i inventory/storage_nodes.ini playbooks/set_maintenance_mode.yml

# Step 2: Issue signed DisasterRebootstrapAuthorization
fsp-cli dr issue-rebootstrap-auth \
    --operator-id dr_operator_alpha \
    --incarnation-id "incarnation-$(uuidgen)" \
    --justification "Catastrophic quorum loss: authorized recovery" \
    --signing-key /etc/fsp/keys/operator_private.key \
    --output-auth /var/lib/fsp/rebootstrap_auth.json

# Step 3: Execute isolated rebootstrap restore
fsp-cli dr restore \
    --mode isolated-rebootstrap \
    --bundle /var/backups/fsp/fsp_metadata_backup.bundle \
    --target-dir /var/lib/fsp/coordinator \
    --auth-file /var/lib/fsp/rebootstrap_auth.json \
    --operator-key /etc/fsp/keys/operator_public.key

# Step 4: Verify recovery provenance
cat /var/lib/fsp/coordinator/recovery_provenance.rec

# Step 5: Start coordinator service (fenced epoch +100 active)
systemctl start fsp-coordinator
```

## 3. Offline Secure Emergency Read
Use when reconstructing critical objects directly from surviving disks without an active coordinator.

```bash
# 1. Issue recovery capability token
fsp-cli token issue \
    --object-id obj_critical_document \
    --namespace default \
    --principal recovery_admin \
    --expires 1800000000 \
    --signing-key /etc/fsp/keys/admin_private.key \
    --output-token /tmp/token.json

# 2. Execute emergency read across storage root mount points
fsp-cli recovery emergency-read \
    --object-id obj_critical_document \
    --namespace default \
    --token /tmp/token.json \
    --trust-store /etc/fsp/recovery_trust_store.json \
    --storage-roots /mnt/fsp_node01,/mnt/fsp_node02,/mnt/fsp_node03,... \
    --output /var/tmp/recovered_document.bin
```
