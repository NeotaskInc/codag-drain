/// Benchmark comparing synchronous Drain vs ConcurrentDrain at various scales.
///
/// All paths now include regex-based log masking (the dominant preprocessing
/// cost), which gives worker threads real work to parallelize.
///
/// Run with: cargo test --release bench_scale -- --nocapture --ignored
use crate::drain::Drain;
use crate::masking::{default_masking_instructions, LogMasker};
use crate::pipeline::ConcurrentDrain;
use std::sync::Arc;
use std::time::Instant;

/// Generate realistic-looking log lines with variation.
fn generate_logs(n: usize) -> Vec<String> {
    let templates = [
        "Dec 10 07:07:38 LabSZ sshd[24206]: input_userauth_request: invalid user {user} [preauth]",
        "Dec 10 09:12:32 LabSZ sshd[24490]: Failed password for invalid user {user} from 0.0.0.0 port {port} ssh2",
        "Dec 10 06:55:46 LabSZ sshd[24200]: reverse mapping checking getaddrinfo for {host} failed - POSSIBLE BREAK-IN ATTEMPT!",
        "Dec 10 10:30:01 LabSZ CRON[25455]: pam_unix(cron:session): session opened for user {user} by (uid=0)",
        "Dec 10 10:30:01 LabSZ CRON[25455]: pam_unix(cron:session): session closed for user {user}",
        "Dec 10 11:03:44 LabSZ sshd[25500]: Accepted publickey for {user} from 192.168.1.{ip} port {port} ssh2",
        "Dec 10 11:03:44 LabSZ sshd[25500]: pam_unix(sshd:session): session opened for user {user} by (uid=0)",
        "Dec 10 12:00:00 LabSZ kernel: [UFW BLOCK] IN=eth0 OUT= MAC=00:00:00:00:00:00 SRC=10.0.0.{ip} DST=192.168.1.1 PROTO=TCP SPT={port} DPT=22",
    ];
    let users = ["root", "admin", "test", "ftpuser", "postgres", "www-data", "pi", "ubuntu"];
    let hosts = ["scanner1.example.com", "attacker.evil.org", "probe.test.net"];

    (0..n)
        .map(|i| {
            let tmpl = templates[i % templates.len()];
            let user = users[i % users.len()];
            let port = 10000 + (i % 55000);
            let ip = 1 + (i % 254);
            let host = hosts[i % hosts.len()];
            tmpl.replace("{user}", user)
                .replace("{port}", &port.to_string())
                .replace("{ip}", &ip.to_string())
                .replace("{host}", host)
        })
        .collect()
}

fn make_drain_with_masking() -> Drain {
    let mut drain = Drain::default();
    drain.set_masker(LogMasker::new(default_masking_instructions()));
    drain
}

fn bench_sync(logs: &[String]) -> (std::time::Duration, usize) {
    let mut drain = make_drain_with_masking();
    let start = Instant::now();
    for log in logs {
        drain.add_log_message(log);
    }
    let elapsed = start.elapsed();
    let clusters = drain.cluster_count();
    (elapsed, clusters)
}

async fn bench_concurrent_parallel(
    logs: &[String],
    concurrency: usize,
) -> std::time::Duration {
    let drain = make_drain_with_masking();
    let cd = Arc::new(ConcurrentDrain::new(drain, 1024));

    // Split logs into chunks for each worker
    let chunk_size = (logs.len() + concurrency - 1) / concurrency;
    let chunks: Vec<Vec<String>> = logs
        .chunks(chunk_size)
        .map(|c| c.to_vec())
        .collect();

    let start = Instant::now();

    let mut handles = Vec::new();
    for chunk in chunks {
        let cd = Arc::clone(&cd);
        handles.push(tokio::spawn(async move {
            for log in &chunk {
                cd.add_log_message(log).await;
            }
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    let elapsed = start.elapsed();

    Arc::try_unwrap(cd).ok().unwrap().shutdown().await;
    elapsed
}

async fn bench_concurrent_sequential(logs: &[String]) -> std::time::Duration {
    let drain = make_drain_with_masking();
    let cd = ConcurrentDrain::new(drain, 1024);

    let start = Instant::now();
    for log in logs {
        cd.add_log_message(log).await;
    }
    let elapsed = start.elapsed();

    cd.shutdown().await;
    elapsed
}

#[tokio::test(flavor = "multi_thread")]
#[ignore] // run with: cargo test --release bench_scale -- --nocapture --ignored
async fn bench_scale() {
    let scales: Vec<usize> = vec![1_000, 5_000, 10_000, 50_000, 100_000];
    let concurrency_levels: Vec<usize> = vec![2, 4, 8];

    println!();
    println!("╔══════════════════════════════════════════════════════════════════════════════╗");
    println!("║         Drain3 Benchmark: Sync vs Concurrent (with regex masking)           ║");
    println!("╠══════════════════════════════════════════════════════════════════════════════╣");
    println!(
        "║ {:>8} │ {:>12} │ {:>12} │ {:>12} │ {:>7} │ {:>7} ║",
        "Messages", "Sync", "Conc(seq)", "Conc(par)", "Workers", "Speedup"
    );
    println!("╠══════════════════════════════════════════════════════════════════════════════╣");

    for &n in &scales {
        let logs = generate_logs(n);

        // Sync baseline
        let (sync_time, _) = bench_sync(&logs);

        // Concurrent sequential (measures channel overhead)
        let conc_seq_time = bench_concurrent_sequential(&logs).await;

        // Concurrent parallel at various worker counts
        for &workers in &concurrency_levels {
            let conc_time = bench_concurrent_parallel(&logs, workers).await;

            let speedup = sync_time.as_secs_f64() / conc_time.as_secs_f64();

            println!(
                "║ {:>8} │ {:>10.2}ms │ {:>10.2}ms │ {:>10.2}ms │ {:>5}w  │ {:>6.2}x ║",
                n,
                sync_time.as_secs_f64() * 1000.0,
                conc_seq_time.as_secs_f64() * 1000.0,
                conc_time.as_secs_f64() * 1000.0,
                workers,
                speedup,
            );
        }
        if n != 100_000 {
            println!("║──────────┼──────────────┼──────────────┼──────────────┼─────────┼─────────║");
        }
    }
    println!("╚══════════════════════════════════════════════════════════════════════════════╝");
    println!();
    println!("Legend:");
    println!("  Sync      = single-threaded Drain::add_log_message (with masking) in a loop");
    println!("  Conc(seq) = ConcurrentDrain with sequential awaits (masking + channel overhead)");
    println!("  Conc(par) = ConcurrentDrain with N worker tasks sending in parallel");
    println!("  Speedup   = Sync time / Conc(par) time");
}
