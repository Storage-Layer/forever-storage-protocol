//! Cluster Launcher and Lifecycle Orchestrator for Phase 5.4.
//!
//! Provides production-like cluster deployment harness capable of:
//! - Provisioning cluster PKI (root CA, Ed25519 signer, node certificates, coordinator certificates).
//! - Spawning 20 isolated storage nodes as genuine OS child processes (`fsp-node`) or in-process servers.
//! - Enforcing 5 failure zones (4 nodes per zone) with zone-aware port allocation.
//! - Synchronizing on child process `--ready-file` signals.
//! - Injecting SIGKILL / crash faults directly to specific nodes or entire zones.
//! - Restarting killed nodes and validating crash consistency.
//! - Automated cluster teardown on drop.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::cluster::topology::{ClusterZoneTopology, ZoneId};
use crate::coordinator::transactions::{
    CoordinatorCrashRecoveryReport, DistributedCoordinator, RegisteredStorageNode,
};
use crate::model::node::NodeId;
use crate::network::client::NodeRpcClient;
use crate::network::server::NodeRpcServer;
use crate::node::daemon::{StorageNodeConfig, StorageNodeDaemon};
use crate::node::identity::NodeFailureDomainTopology;
use crate::security::authorization::{
    build_mtls_client_config, build_mtls_server_config, cert_der_sha256_hex, load_certs_from_pem,
    provision_cluster_pki, ProvisionedClusterPki,
};
use crate::security::signatures::{ManifestSignerKeypair, TrustedSignerPolicy};

/// Represents a running storage node, either as a real OS child process or in-process server.
pub enum StorageNodeExecutionMode {
    Subprocess {
        child: Child,
        ready_file: PathBuf,
        stdout_log: PathBuf,
        stderr_log: PathBuf,
    },
    InProcess {
        server: NodeRpcServer,
    },
    Killed,
}

/// Metadata and state for an orchestrated storage node.
pub struct ManagedNode {
    pub node_id: NodeId,
    pub zone_id: ZoneId,
    pub storage_root: PathBuf,
    pub listen_addr: String,
    pub cert_pem: PathBuf,
    pub key_pem: PathBuf,
    pub mode: StorageNodeExecutionMode,
}

impl ManagedNode {
    pub fn child_pid(&self) -> Option<u32> {
        match &self.mode {
            StorageNodeExecutionMode::Subprocess { child, .. } => Some(child.id()),
            _ => None,
        }
    }

    pub fn is_running(&mut self) -> bool {
        match &mut self.mode {
            StorageNodeExecutionMode::Subprocess { child, .. } => {
                matches!(child.try_wait(), Ok(None))
            }
            StorageNodeExecutionMode::InProcess { .. } => true,
            StorageNodeExecutionMode::Killed => false,
        }
    }

    pub fn kill_sigkill(&mut self) -> Result<(), String> {
        match &mut self.mode {
            StorageNodeExecutionMode::Subprocess { child, .. } => {
                child
                    .kill()
                    .map_err(|e| format!("failed to kill node {}: {}", self.node_id.0, e))?;
                let _ = child.wait();
                self.mode = StorageNodeExecutionMode::Killed;
                Ok(())
            }
            StorageNodeExecutionMode::InProcess { .. } => {
                self.mode = StorageNodeExecutionMode::Killed;
                Ok(())
            }
            StorageNodeExecutionMode::Killed => Ok(()),
        }
    }
}

/// Cluster Orchestrator managing cluster instances.
pub struct ClusterLauncher {
    pub topology: ClusterZoneTopology,
    pub pki: ProvisionedClusterPki,
    pub nodes: BTreeMap<NodeId, ManagedNode>,
    pub coordinator: Option<Arc<DistributedCoordinator>>,
    pub cluster_root: PathBuf,
    pub binary_path: Option<PathBuf>,
}

impl ClusterLauncher {
    /// Launches a 20-node cluster using genuine OS subprocesses running `fsp-node`.
    pub fn launch_subprocess_cluster(
        cluster_root: &Path,
        binary_path: &Path,
        capacity_bytes: u64,
    ) -> Result<Self, String> {
        if !binary_path.exists() {
            return Err(format!(
                "fsp-node binary not found at {}",
                binary_path.display()
            ));
        }

        let pki_dir = cluster_root.join("pki");
        let pki = provision_cluster_pki(&pki_dir, 20)
            .map_err(|e| format!("failed to provision cluster PKI: {}", e))?;

        let topology = ClusterZoneTopology::standard_5_zones_20_nodes(0, capacity_bytes);
        let mut nodes = BTreeMap::new();

        for (&node_id, meta) in &topology.nodes {
            let (cert_pem, key_pem) = pki
                .node_certs
                .get(&node_id)
                .ok_or_else(|| format!("missing certs for node {}", node_id.0))?
                .clone();

            let node_root = cluster_root.join(format!("node-{:02}", node_id.0));
            fs::create_dir_all(&node_root)
                .map_err(|e| format!("failed to create storage root: {}", e))?;

            let ready_file = cluster_root.join(format!("node-{:02}.ready", node_id.0));
            if ready_file.exists() {
                let _ = fs::remove_file(&ready_file);
            }

            let stdout_log = cluster_root.join(format!("node-{:02}.stdout.log", node_id.0));
            let stderr_log = cluster_root.join(format!("node-{:02}.stderr.log", node_id.0));

            let out_file = File::create(&stdout_log)
                .map_err(|e| format!("failed to create stdout log: {}", e))?;
            let err_file = File::create(&stderr_log)
                .map_err(|e| format!("failed to create stderr log: {}", e))?;

            let mut cmd = Command::new(binary_path);
            cmd.args([
                "--node-id",
                &node_id.0.to_string(),
                "--listen",
                "127.0.0.1:0",
                "--storage-root",
                node_root.to_str().unwrap(),
                "--ca-cert",
                pki.ca_cert_pem.to_str().unwrap(),
                "--node-cert",
                cert_pem.to_str().unwrap(),
                "--node-key",
                key_pem.to_str().unwrap(),
                "--coord-cert",
                pki.coordinator_cert_pem.to_str().unwrap(),
                "--capacity-bytes",
                &capacity_bytes.to_string(),
                "--ready-file",
                ready_file.to_str().unwrap(),
            ])
            .stdout(Stdio::from(out_file))
            .stderr(Stdio::from(err_file));

            #[cfg(target_os = "linux")]
            unsafe {
                use std::os::unix::process::CommandExt;
                cmd.pre_exec(|| {
                    extern "C" {
                        fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
                    }
                    const PR_SET_PDEATHSIG: i32 = 1;
                    const SIGKILL: u64 = 9;
                    let ret = prctl(PR_SET_PDEATHSIG, SIGKILL, 0, 0, 0);
                    if ret != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }

            let child = cmd
                .spawn()
                .map_err(|e| format!("failed to spawn fsp-node {}: {}", node_id.0, e))?;

            // Wait for node to write ADDR to ready_file
            let mut bound_addr = String::new();
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(10) {
                if ready_file.exists() {
                    if let Ok(content) = fs::read_to_string(&ready_file) {
                        for line in content.lines() {
                            if let Some(addr) = line.strip_prefix("ADDR=") {
                                bound_addr = addr.trim().to_string();
                                break;
                            }
                        }
                    }
                    if !bound_addr.is_empty() {
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(20));
            }

            if bound_addr.is_empty() {
                return Err(format!(
                    "node {} failed to report bound address in ready_file",
                    node_id.0
                ));
            }

            nodes.insert(
                node_id,
                ManagedNode {
                    node_id,
                    zone_id: meta.zone_id.clone(),
                    storage_root: node_root,
                    listen_addr: bound_addr,
                    cert_pem,
                    key_pem,
                    mode: StorageNodeExecutionMode::Subprocess {
                        child,
                        ready_file,
                        stdout_log,
                        stderr_log,
                    },
                },
            );
        }

        let launcher = Self {
            topology,
            pki,
            nodes,
            coordinator: None,
            cluster_root: cluster_root.to_path_buf(),
            binary_path: Some(binary_path.to_path_buf()),
        };
        let _ = launcher.update_pids_file();
        Ok(launcher)
    }

    /// Launches a 20-node cluster using in-process daemons with real mTLS TCP listeners.
    pub fn launch_inprocess_cluster(
        cluster_root: &Path,
        capacity_bytes: u64,
    ) -> Result<Self, String> {
        let pki_dir = cluster_root.join("pki");
        let pki = provision_cluster_pki(&pki_dir, 20)
            .map_err(|e| format!("failed to provision cluster PKI: {}", e))?;

        let topology = ClusterZoneTopology::standard_5_zones_20_nodes(0, capacity_bytes);
        let mut nodes = BTreeMap::new();

        for (&node_id, meta) in &topology.nodes {
            let (cert_pem, key_pem) = pki
                .node_certs
                .get(&node_id)
                .ok_or_else(|| format!("missing certs for node {}", node_id.0))?
                .clone();

            let certs =
                load_certs_from_pem(&cert_pem).map_err(|e| format!("load cert failed: {}", e))?;
            let fp = cert_der_sha256_hex(certs[0].as_ref());

            let node_root = cluster_root.join(format!("node-{:02}", node_id.0));
            fs::create_dir_all(&node_root)
                .map_err(|e| format!("failed to create storage root: {}", e))?;

            let cfg = StorageNodeConfig::localhost(
                node_id,
                "127.0.0.1:0",
                &node_root,
                &fp,
                capacity_bytes,
            );
            let (daemon, _) = StorageNodeDaemon::init_or_open(cfg)
                .map_err(|e| format!("init daemon failed: {}", e))?;

            let server_tls = build_mtls_server_config(&pki.ca_cert_pem, &cert_pem, &key_pem)
                .map_err(|e| format!("build server tls failed: {}", e))?;

            let server = NodeRpcServer::start(
                daemon,
                server_tls,
                pki.trust_policy.clone(),
                Duration::from_secs(5),
            )
            .map_err(|e| format!("start rpc server failed: {}", e))?;

            let bound_addr = server.bound_addr().to_string();

            nodes.insert(
                node_id,
                ManagedNode {
                    node_id,
                    zone_id: meta.zone_id.clone(),
                    storage_root: node_root,
                    listen_addr: bound_addr,
                    cert_pem,
                    key_pem,
                    mode: StorageNodeExecutionMode::InProcess { server },
                },
            );
        }

        Ok(Self {
            topology,
            pki,
            nodes,
            coordinator: None,
            cluster_root: cluster_root.to_path_buf(),
            binary_path: None,
        })
    }

    /// Initializes and starts the distributed coordinator connected to all healthy storage nodes.
    pub fn start_coordinator(
        &mut self,
        coord_root: &Path,
    ) -> Result<CoordinatorCrashRecoveryReport, String> {
        let client_tls = build_mtls_client_config(
            &self.pki.ca_cert_pem,
            &self.pki.coordinator_cert_pem,
            &self.pki.coordinator_key_pem,
        )
        .map_err(|e| format!("build client tls failed: {}", e))?;

        let rpc_client = NodeRpcClient::new(
            client_tls,
            self.pki.trust_policy.clone(),
            Duration::from_secs(60),
        );

        let signer_keypair = ManifestSignerKeypair::from_seed("coordinator-primary", [0x54u8; 32])
            .map_err(|e| format!("signer keypair failed: {}", e))?;

        let mut signer_policy = TrustedSignerPolicy::new();
        signer_policy
            .register_signer("coordinator-primary", signer_keypair.verifying_key())
            .map_err(|e| format!("register signer failed: {}", e))?;

        let mut reg_nodes = Vec::new();
        for node in self.nodes.values() {
            let topology = NodeFailureDomainTopology::distinct_physical_host(
                node.node_id,
                &format!("host-{:02}", node.node_id.0),
                &format!("disk-{:02}", node.node_id.0),
                &format!("rack-{}", node.node_id.0 % 5),
                &node.zone_id.0,
            );
            reg_nodes.push(RegisteredStorageNode {
                node_id: node.node_id,
                listen_addr: node.listen_addr.clone(),
                topology,
            });
        }

        let (coord, recovery) = DistributedCoordinator::init_or_open(
            coord_root,
            rpc_client,
            signer_keypair,
            signer_policy,
            reg_nodes,
        )
        .map_err(|e| format!("init coordinator failed: {}", e))?;

        self.coordinator = Some(coord);
        Ok(recovery)
    }

    /// Returns a reference to the active coordinator.
    pub fn coordinator(&self) -> Result<&Arc<DistributedCoordinator>, String> {
        self.coordinator
            .as_ref()
            .ok_or_else(|| "coordinator not started".to_string())
    }

    /// Kills a specific storage node via SIGKILL.
    pub fn kill_node(&mut self, node_id: NodeId) -> Result<(), String> {
        let node = self
            .nodes
            .get_mut(&node_id)
            .ok_or_else(|| format!("node {} not found", node_id.0))?;
        node.kill_sigkill()
    }

    /// Kills all nodes residing in the specified zone.
    pub fn kill_zone(&mut self, zone_name: &str) -> Result<usize, String> {
        let target_zone = ZoneId::new(zone_name);
        let mut count = 0;
        for node in self.nodes.values_mut() {
            if node.zone_id == target_zone {
                node.kill_sigkill()?;
                count += 1;
            }
        }
        Ok(count)
    }

    /// Restarts a killed node daemon process.
    pub fn restart_node(&mut self, node_id: NodeId) -> Result<String, String> {
        let node = self
            .nodes
            .get_mut(&node_id)
            .ok_or_else(|| format!("node {} not found", node_id.0))?;

        match &mut node.mode {
            StorageNodeExecutionMode::Subprocess { .. } | StorageNodeExecutionMode::Killed => {
                let bin = self
                    .binary_path
                    .as_ref()
                    .ok_or_else(|| "cannot restart subprocess without binary path".to_string())?;

                let ready_file = self
                    .cluster_root
                    .join(format!("node-{:02}.restart.ready", node_id.0));
                if ready_file.exists() {
                    let _ = fs::remove_file(&ready_file);
                }

                let stdout_log = self
                    .cluster_root
                    .join(format!("node-{:02}.restart.stdout.log", node_id.0));
                let stderr_log = self
                    .cluster_root
                    .join(format!("node-{:02}.restart.stderr.log", node_id.0));

                let out_file = File::create(&stdout_log)
                    .map_err(|e| format!("failed to create stdout log: {}", e))?;
                let err_file = File::create(&stderr_log)
                    .map_err(|e| format!("failed to create stderr log: {}", e))?;

                let mut cmd = Command::new(bin);
                cmd.args([
                    "--node-id",
                    &node_id.0.to_string(),
                    "--listen",
                    "127.0.0.1:0",
                    "--storage-root",
                    node.storage_root.to_str().unwrap(),
                    "--ca-cert",
                    self.pki.ca_cert_pem.to_str().unwrap(),
                    "--node-cert",
                    node.cert_pem.to_str().unwrap(),
                    "--node-key",
                    node.key_pem.to_str().unwrap(),
                    "--coord-cert",
                    self.pki.coordinator_cert_pem.to_str().unwrap(),
                    "--capacity-bytes",
                    "52428800",
                    "--ready-file",
                    ready_file.to_str().unwrap(),
                ])
                .stdout(Stdio::from(out_file))
                .stderr(Stdio::from(err_file));

                #[cfg(target_os = "linux")]
                unsafe {
                    use std::os::unix::process::CommandExt;
                    cmd.pre_exec(|| {
                        extern "C" {
                            fn prctl(
                                option: i32,
                                arg2: u64,
                                arg3: u64,
                                arg4: u64,
                                arg5: u64,
                            ) -> i32;
                        }
                        const PR_SET_PDEATHSIG: i32 = 1;
                        const SIGKILL: u64 = 9;
                        let ret = prctl(PR_SET_PDEATHSIG, SIGKILL, 0, 0, 0);
                        if ret != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }

                let child = cmd
                    .spawn()
                    .map_err(|e| format!("failed to restart fsp-node {}: {}", node_id.0, e))?;

                let mut bound_addr = String::new();
                let start = Instant::now();
                while start.elapsed() < Duration::from_secs(10) {
                    if ready_file.exists() {
                        if let Ok(content) = fs::read_to_string(&ready_file) {
                            for line in content.lines() {
                                if let Some(addr) = line.strip_prefix("ADDR=") {
                                    bound_addr = addr.trim().to_string();
                                    break;
                                }
                            }
                        }
                        if !bound_addr.is_empty() {
                            break;
                        }
                    }
                    thread::sleep(Duration::from_millis(20));
                }

                if bound_addr.is_empty() {
                    return Err(format!(
                        "restarted node {} failed to write ready_file",
                        node_id.0
                    ));
                }

                node.listen_addr = bound_addr.clone();
                node.mode = StorageNodeExecutionMode::Subprocess {
                    child,
                    ready_file,
                    stdout_log,
                    stderr_log,
                };

                // Update coordinator registration if coordinator is active
                if let Some(coord) = &self.coordinator {
                    coord.update_node_address(node_id, &bound_addr);
                }

                Ok(bound_addr)
            }
            StorageNodeExecutionMode::InProcess { .. } => Err(
                "cannot restart in-process node with restart_node; use restart_inprocess_node"
                    .to_string(),
            ),
        }
    }

    /// Returns all active child subprocess PIDs.
    pub fn child_pids(&self) -> Vec<(NodeId, u32)> {
        self.nodes
            .iter()
            .filter_map(|(&id, node)| node.child_pid().map(|pid| (id, pid)))
            .collect()
    }

    /// Extracts the start time (field 22 in /proc/{pid}/stat) to uniquely identify process instantiation.
    pub fn get_process_starttime(pid: u32) -> Option<u64> {
        let stat_content = fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
        let rparen = stat_content.rfind(')')?;
        let rest = stat_content[rparen + 1..].trim_start();
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // In the suffix following comm, field 0 is state (field 3 of /proc/pid/stat),
        // so starttime (field 22) is index 19 (22 - 3 = 19).
        fields.get(19).and_then(|s| s.parse::<u64>().ok())
    }

    /// Updates the `cluster.pids` tracking file with the launcher and active child PIDs,
    /// recording both PID and process start time to prevent PID reuse hazards.
    pub fn update_pids_file(&self) -> Result<(), String> {
        let pids_path = self.cluster_root.join("cluster.pids");
        let launcher_pid = std::process::id();
        let launcher_st = Self::get_process_starttime(launcher_pid)
            .map(|st| format!(" START_TIME={}", st))
            .unwrap_or_default();
        let mut content = format!("LAUNCHER_PID={}{}\n", launcher_pid, launcher_st);
        for (node_id, node) in &self.nodes {
            if let Some(pid) = node.child_pid() {
                let start_time_str = Self::get_process_starttime(pid)
                    .map(|st| format!(" START_TIME={}", st))
                    .unwrap_or_default();
                content.push_str(&format!(
                    "NODE={} PID={}{}\n",
                    node_id.0, pid, start_time_str
                ));
            }
        }
        fs::write(&pids_path, content).map_err(|e| e.to_string())
    }

    /// Validates whether a running process corresponds to an FSP daemon or launcher,
    /// preventing inadvertent termination of unrelated processes if a PID was reused by the OS.
    pub fn is_fsp_process(pid: u32) -> bool {
        let cmdline_path = format!("/proc/{}/cmdline", pid);
        if let Ok(cmdline) = fs::read_to_string(&cmdline_path) {
            cmdline.contains("fsp-node")
                || cmdline.contains("fsp")
                || cmdline.contains("target/debug")
                || cmdline.contains("target/release")
        } else {
            let comm_path = format!("/proc/{}/comm", pid);
            if let Ok(comm) = fs::read_to_string(&comm_path) {
                comm.contains("fsp-node") || comm.contains("fsp")
            } else {
                false
            }
        }
    }

    /// Audits `cluster.pids` to discover any surviving orphaned child processes whose launcher parent is dead.
    pub fn audit_orphans(cluster_root: &Path) -> Vec<u32> {
        let pids_path = cluster_root.join("cluster.pids");
        if !pids_path.exists() {
            return Vec::new();
        }
        let content = match fs::read_to_string(&pids_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let mut launcher_pid: Option<i32> = None;
        let mut launcher_start_time: Option<u64> = None;
        let mut child_records: Vec<(u32, Option<u64>)> = Vec::new();

        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("LAUNCHER_PID=") {
                let parts: Vec<&str> = rest.split_whitespace().collect();
                if let Some(p_str) = parts.first() {
                    launcher_pid = p_str.parse::<i32>().ok();
                }
                for part in parts.iter().skip(1) {
                    if let Some(st_str) = part.strip_prefix("START_TIME=") {
                        launcher_start_time = st_str.parse::<u64>().ok();
                    }
                }
            } else if let Some(idx) = line.find("PID=") {
                let rest = &line[idx + 4..];
                let parts: Vec<&str> = rest.split_whitespace().collect();
                if let Some(p_str) = parts.first() {
                    if let Ok(pid) = p_str.parse::<u32>() {
                        let mut st = None;
                        for part in parts.iter().skip(1) {
                            if let Some(st_str) = part.strip_prefix("START_TIME=") {
                                st = st_str.parse::<u64>().ok();
                            }
                        }
                        child_records.push((pid, st));
                    }
                }
            }
        }

        let mut orphans = Vec::new();
        let parent_dead = match launcher_pid {
            Some(lpid) => {
                let alive = unsafe {
                    extern "C" {
                        fn kill(pid: i32, sig: i32) -> i32;
                    }
                    kill(lpid, 0) == 0
                };
                if alive {
                    // If launcher PID is alive, check if it's still the SAME launcher (check starttime)
                    if let Some(expected_st) = launcher_start_time {
                        if let Some(actual_st) = Self::get_process_starttime(lpid as u32) {
                            actual_st != expected_st // if start_time changed, original launcher is dead!
                        } else {
                            true
                        }
                    } else {
                        // Without starttime tracking, if kill(0) == 0, parent might be alive
                        false
                    }
                } else {
                    true
                }
            }
            None => true,
        };

        if parent_dead {
            for (pid, expected_st) in child_records {
                let alive = unsafe {
                    extern "C" {
                        fn kill(pid: i32, sig: i32) -> i32;
                    }
                    kill(pid as i32, 0) == 0
                };
                if alive {
                    if let Some(expected) = expected_st {
                        if let Some(actual) = Self::get_process_starttime(pid) {
                            if actual != expected {
                                // PID was recycled by the OS
                                continue;
                            }
                        } else {
                            continue;
                        }
                    }
                    if Self::is_fsp_process(pid) {
                        orphans.push(pid);
                    }
                }
            }
        }

        orphans
    }

    /// Terminates an orphan process safely using Linux `pidfd_open` (syscall 434) and
    /// `pidfd_send_signal` (syscall 424) to eliminate TOCTOU races against PID recycling.
    pub fn kill_process_safely(pid: u32) -> bool {
        if !Self::is_fsp_process(pid) {
            return false;
        }

        #[cfg(target_os = "linux")]
        unsafe {
            extern "C" {
                fn syscall(num: i64, ...) -> i64;
                fn close(fd: i32) -> i32;
                fn kill(pid: i32, sig: i32) -> i32;
            }
            // SYS_pidfd_open = 434
            let pidfd = syscall(434, pid as i64, 0i64);
            if pidfd >= 0 {
                let fd = pidfd as i32;
                // Re-verify that the pinned process in pidfd is indeed an FSP process
                if !Self::is_fsp_process(pid) {
                    close(fd);
                    return false;
                }
                // SYS_pidfd_send_signal = 424, signal 9 (SIGKILL)
                let ret = syscall(424, pidfd, 9i64, 0i64, 0i64);
                close(fd);
                ret == 0
            } else {
                // Fallback to traditional kill if pidfd is not supported
                kill(pid as i32, 9) == 0
            }
        }

        #[cfg(not(target_os = "linux"))]
        unsafe {
            extern "C" {
                fn kill(pid: i32, sig: i32) -> i32;
            }
            kill(pid as i32, 9) == 0
        }
    }

    /// Forcefully terminates all orphaned child processes from `cluster.pids`, verifying process identity to guard against PID reuse.
    pub fn cleanup_orphans(cluster_root: &Path) -> usize {
        let orphans = Self::audit_orphans(cluster_root);
        let mut count = 0;
        for pid in orphans {
            if Self::kill_process_safely(pid) {
                count += 1;
            }
        }
        let pids_path = cluster_root.join("cluster.pids");
        let _ = fs::remove_file(&pids_path);
        count
    }

    /// Returns count of healthy running nodes.
    pub fn running_node_count(&mut self) -> usize {
        let mut count = 0;
        for node in self.nodes.values_mut() {
            if node.is_running() {
                count += 1;
            }
        }
        count
    }

    /// Graceful cluster teardown.
    pub fn teardown(&mut self) {
        for node in self.nodes.values_mut() {
            let _ = node.kill_sigkill();
        }
        let pids_path = self.cluster_root.join("cluster.pids");
        let _ = fs::remove_file(&pids_path);
    }
}

impl Drop for ClusterLauncher {
    fn drop(&mut self) {
        self.teardown();
    }
}
