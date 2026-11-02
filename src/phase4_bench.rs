//! Phase 4 benchmark runner (`fsp-sim --phase4`): local v1 vs v2 container storage and a real
//! 20-node localhost mTLS cluster. Everything runs on ONE host; no physical multi-host run is
//! performed or claimed.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fsp_sim::coding::CodingScheme;
use fsp_sim::coordinator::placement::PlacementPolicy;
use fsp_sim::coordinator::scheduler::DistributedRepairScheduler;
use fsp_sim::coordinator::transactions::{
    DistributedCoordinator, DistributedPutOptions, RegisteredStorageNode,
};
use fsp_sim::model::node::NodeId;
use fsp_sim::network::client::NodeRpcClient;
use fsp_sim::network::server::NodeRpcServer;
use fsp_sim::node::daemon::{StorageNodeConfig, StorageNodeDaemon};
use fsp_sim::node::identity::NodeFailureDomainTopology;
use fsp_sim::security::authorization::{
    build_mtls_client_config, build_mtls_server_config, cert_der_sha256_hex, load_certs_from_pem,
    provision_cluster_pki,
};
use fsp_sim::security::signatures::{ManifestSignerKeypair, TrustedSignerPolicy};
use fsp_sim::storage::engine::StorageEngine;
use fsp_sim::storage::object::PutOptions;
use fsp_sim::strategies::adaptive_v2::AdaptiveV2Config;

const SIZES: [(&str, usize); 3] = [("1KiB", 1024), ("1MiB", 1 << 20), ("100MiB", 100 << 20)];

fn payload(len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len];
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    for c in v.chunks_mut(8) {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let b = (s ^ (s >> 29)).to_le_bytes();
        let n = c.len();
        c.copy_from_slice(&b[..n]);
    }
    v
}

fn mibps(bytes: usize, d: Duration) -> f64 {
    (bytes as f64 / 1_048_576.0) / d.as_secs_f64().max(1e-9)
}

fn bench_local(root: &Path, use_v2: bool, label: &str, size: usize) -> (f64, f64, usize) {
    let dir = root.join(format!(
        "local_{}_{}",
        if use_v2 { "v2" } else { "v1" },
        label
    ));
    let _ = fs::remove_dir_all(&dir);
    let mut engine = StorageEngine::init(&dir, 20, 20).expect("init engine");
    let data = payload(size);
    let opts = PutOptions {
        object_id: Some("bench".to_string()),
        stripe_size: 1 << 20,
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        crash_trigger: None,
        use_container_v2: use_v2,
    };
    let t = Instant::now();
    engine.put_bytes(&data, &opts).expect("put");
    let put = t.elapsed();
    let t = Instant::now();
    let (got, _) = engine.get_to_bytes("bench").expect("get");
    let get = t.elapsed();
    assert_eq!(got.len(), size);
    let mut files = 0usize;
    for n in 0..20 {
        let d = dir.join(format!("node-{:02}/objects/bench", n));
        if let Ok(rd) = fs::read_dir(d) {
            files += rd.count();
        }
    }
    let _ = fs::remove_dir_all(&dir);
    (mibps(size, put), mibps(size, get), files)
}

pub fn run() {
    let t_all = Instant::now();
    let root = std::env::temp_dir().join(format!("fsp_phase4_bench_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all("results/phase4").unwrap();

    // ---- local v1 vs v2 ----
    let mut rows: Vec<String> = Vec::new();
    for (label, size) in SIZES {
        for v2 in [false, true] {
            let (p, g, files) = bench_local(&root, v2, label, size);
            println!(
                "LOCAL {} {:>6}: PUT {:8.2} MiB/s GET {:8.2} MiB/s files={}",
                if v2 { "v2" } else { "v1" },
                label,
                p,
                g,
                files
            );
            rows.push(format!(
                "    {{\"mode\":\"local_{}\",\"size\":\"{}\",\"put_mib_s\":{:.3},\"get_mib_s\":{:.3},\"shard_files_on_disk\":{}}}",
                if v2 { "container_v2" } else { "shard_files_v1" }, label, p, g, files
            ));
        }
    }

    // ---- 20-node localhost mTLS cluster ----
    let pki = provision_cluster_pki(&root.join("pki"), 22).expect("pki");
    let mut servers = Vec::new();
    let mut reg = Vec::new();
    for i in 0..22usize {
        let nid = NodeId(i);
        let (c, k) = pki.node_certs.get(&nid).unwrap();
        let fp = cert_der_sha256_hex(load_certs_from_pem(c).unwrap()[0].as_ref());
        let cfg = StorageNodeConfig::localhost(
            nid,
            "127.0.0.1:0",
            &root.join(format!("node-{:02}", i)),
            &fp,
            4u64 << 30,
        );
        let (d, _) = StorageNodeDaemon::init_or_open(cfg).unwrap();
        let tls = build_mtls_server_config(&pki.ca_cert_pem, c, k).unwrap();
        let s = NodeRpcServer::start(d, tls, pki.trust_policy.clone(), Duration::from_secs(60))
            .unwrap();
        reg.push(RegisteredStorageNode {
            node_id: nid,
            listen_addr: s.bound_addr().to_string(),
            topology: NodeFailureDomainTopology::single_host_local_daemon(nid),
        });
        servers.push(s);
    }
    let ctls = build_mtls_client_config(
        &pki.ca_cert_pem,
        &pki.coordinator_cert_pem,
        &pki.coordinator_key_pem,
    )
    .unwrap();
    let client = NodeRpcClient::new(ctls, pki.trust_policy.clone(), Duration::from_secs(60));
    let kp = ManifestSignerKeypair::from_seed("coordinator-primary", [0x42; 32]).unwrap();
    let mut pol = TrustedSignerPolicy::new();
    pol.register_signer("coordinator-primary", kp.verifying_key())
        .unwrap();
    let (coord, _) =
        DistributedCoordinator::init_or_open(&root.join("coord"), client, kp, pol, reg).unwrap();

    let mut recovery: BTreeMap<&str, String> = BTreeMap::new();
    for (label, size) in SIZES {
        let data = payload(size);
        let id = format!("dist-{}", label.to_lowercase());
        let t = Instant::now();
        let rep = coord
            .distributed_put_bytes(&id, &data, DistributedPutOptions::default())
            .expect("dist put");
        let put = t.elapsed();
        let t = Instant::now();
        let (got, _) = coord.distributed_get_to_bytes(&id).expect("dist get");
        let get = t.elapsed();
        assert_eq!(got, data);
        println!(
            "DIST mTLS {:>6}: PUT {:8.2} MiB/s GET {:8.2} MiB/s net_bytes={}",
            label,
            mibps(size, put),
            mibps(size, get),
            rep.total_network_bytes_sent
        );
        rows.push(format!(
            "    {{\"mode\":\"distributed_20node_localhost_mtls\",\"size\":\"{}\",\"put_mib_s\":{:.3},\"get_mib_s\":{:.3},\"network_bytes_sent\":{}}}",
            label, mibps(size, put), mibps(size, get), rep.total_network_bytes_sent
        ));
    }
    {
        {
            let label = "1MiB";
            let id = format!("dist-{}", label.to_lowercase());
            let data = payload(1 << 20);
            // Failure/recovery: stop 8 nodes, degraded GET, then repair onto spare nodes.
            for _ in 0..8 {
                let mut s = servers.remove(0);
                s.shutdown();
            }
            let t = Instant::now();
            let ok = coord.distributed_get_to_bytes(&id).map(|(b, _)| b == data);
            let deg = t.elapsed();
            recovery.insert(
                "degraded_get_8_nodes_down_ms",
                format!("{:.1}", deg.as_secs_f64() * 1e3),
            );
            recovery.insert("degraded_get_correct", format!("{}", ok.unwrap_or(false)));
            let mut sched = DistributedRepairScheduler::new(
                Arc::clone(&coord),
                AdaptiveV2Config::default(),
                PlacementPolicy::default(),
            );
            for sh in 0..20 {
                sched.enqueue_repair(&id, sh, "bench");
            }
            let t = Instant::now();
            let r = sched.run_repair_pass(7200.0);
            let rt = t.elapsed();
            match r {
                Ok(r) => {
                    recovery.insert("repair_shards_repaired", r.shards_repaired.to_string());
                    recovery.insert(
                        "repair_failed_attempts",
                        r.failed_repair_attempts.to_string(),
                    );
                }
                Err(e) => {
                    recovery.insert("repair_error", format!("\"{}\"", e));
                }
            }
            recovery.insert("repair_pass_ms", format!("{:.1}", rt.as_secs_f64() * 1e3));
            println!("RECOVERY: {:?}", recovery);
        }
    }

    let cfg = "{\n  \"host\": \"single machine, localhost only (physical multi-host test NOT EXECUTED)\",\n  \"coding\": \"RS(20,12)\",\n  \"stripe_size_bytes\": 1048576,\n  \"cluster_nodes\": 22,\n  \"transport\": \"TLS 1.3 mTLS over loopback\",\n  \"sizes\": [\"1KiB\",\"1MiB\",\"100MiB\"]\n}\n";
    fs::write("results/phase4/benchmark_config.json", cfg).unwrap();
    fs::write(
        "results/phase4/benchmark_results.json",
        format!("{{\n  \"results\": [\n{}\n  ]\n}}\n", rows.join(",\n")),
    )
    .unwrap();
    let rec: Vec<String> = recovery
        .iter()
        .map(|(k, v)| {
            if v == "true" || v == "false" || v.parse::<f64>().is_ok() || v.starts_with('"') {
                format!("  \"{}\": {}", k, v)
            } else {
                format!("  \"{}\": \"{}\"", k, v)
            }
        })
        .collect();
    fs::write(
        "results/phase4/failure_recovery_results.json",
        format!("{{\n{}\n}}\n", rec.join(",\n")),
    )
    .unwrap();
    for mut s in servers {
        s.shutdown();
    }
    let _ = fs::remove_dir_all(&root);
    println!(
        "Phase 4 benchmarks done in {:.1}s",
        t_all.elapsed().as_secs_f64()
    );
}
