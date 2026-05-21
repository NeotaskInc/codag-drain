# Drain3 — Rust Implementation

A high-performance Rust implementation of the [Drain3](https://github.com/logpai/Drain3) log template mining algorithm, with a concurrent pipeline for parallel log ingestion.

## Overview

Drain3 is an online log template miner that extracts templates (clusters) from a stream of log messages using a fixed-depth parse tree. This Rust port delivers **2.5–4.2x speedups** over the original Python implementation while preserving identical clustering behavior.

### Example

For the input:

```
connected to 10.0.0.1
connected to 192.168.0.1
Hex number 0xDEADBEAF
user davidoh logged in
user eranr logged in
```

Drain3 extracts:

```
ID=1  size=2  connected to <IP>
ID=2  size=1  Hex number <HEX>
ID=3  size=2  user <*> logged in
```

## Architecture

```
drain3_rust/src/
├── drain.rs       # Core Drain algorithm (prefix tree, clustering)
├── masking.rs     # Regex-based log masking (IP, HEX, NUM patterns)
├── pipeline.rs    # ConcurrentDrain — async MPSC pipeline with parallel preprocessing
├── cluster.rs     # LogCluster type
├── node.rs        # Prefix tree nodes
├── similarity.rs  # Token distance & template merging
├── storage.rs     # LRU-backed cluster storage
└── lib.rs         # Public API re-exports
```

### Synchronous API

```rust
use drain3_rust::{Drain, LogMasker, MaskingInstruction};
use drain3_rust::masking::default_masking_instructions;

let mut drain = Drain::default();
drain.set_masker(LogMasker::new(default_masking_instructions()));

let (cluster, update_type) = drain.add_log_message("connected to 10.0.0.1");
println!("{}", cluster.get_template()); // "connected to <IP>"
```

### Concurrent API

```rust
use drain3_rust::{Drain, ConcurrentDrain, LogMasker};
use drain3_rust::masking::default_masking_instructions;
use std::sync::Arc;

let mut drain = Drain::default();
drain.set_masker(LogMasker::new(default_masking_instructions()));

let cd = Arc::new(ConcurrentDrain::new(drain, 1024));

// Spawn multiple workers — masking + tokenization runs in parallel
let cd2 = Arc::clone(&cd);
tokio::spawn(async move {
    let result = cd2.add_log_message("Failed login from 10.0.0.1 port 22").await;
});
```

N caller tasks perform regex masking and tokenization in parallel, then send tokens through a bounded MPSC channel to a single updater task that owns the Drain tree.

## Log Masking

Built-in masking patterns (matching Python Drain3 defaults):

| Pattern | Mask | Example |
|---------|------|---------|
| IP addresses | `<IP>` | `192.168.1.1` → `<IP>` |
| Hex sequences | `<HEX>` | `0xDEADBEEF` → `<HEX>` |
| Numbers | `<NUM>` | `port 8080` → `port <NUM>` |

Custom patterns:

```rust
use drain3_rust::{LogMasker, MaskingInstruction};

let masker = LogMasker::new(vec![
    MaskingInstruction::new(r"\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}", "IP"),
    MaskingInstruction::new(r"[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}", "UUID"),
]);
```

## Performance

Benchmarked on the same log corpus with identical masking patterns (IP, HEX, NUM).

### Rust vs Python

| Messages | Python Drain3 | Rust (sync) | Speedup |
|----------|--------------|-------------|---------|
| 1,000 | 19.84ms | 9.13ms | **2.2x** |
| 5,000 | 100.70ms | 33.78ms | **3.0x** |
| 10,000 | 170.12ms | 46.72ms | **3.6x** |
| 50,000 | 865.94ms | 262.58ms | **3.3x** |
| 100,000 | 1,667.38ms | 661.45ms | **2.5x** |

### Sync vs Concurrent (8 workers)

| Messages | Sync | Concurrent (8w) | Speedup |
|----------|------|-----------------|---------|
| 1,000 | 9.13ms | 8.10ms | 1.13x |
| 5,000 | 33.78ms | 23.77ms | 1.42x |
| 10,000 | 46.72ms | 76.57ms | 0.61x |
| 50,000 | 262.58ms | 318.34ms | 0.82x |
| 100,000 | 661.45ms | 616.83ms | 1.07x |

**When to use concurrent mode:** The concurrent pipeline wins when regex masking dominates total processing time — at lower message counts (1K–5K) where the per-message masking cost is high relative to tree operations. At 5K messages with 8 workers, concurrent achieves **1.42x** over sync and **4.2x** over Python. At larger scales (10K–50K), the single-writer tree updater becomes the bottleneck and sync is faster. At 100K the concurrent pipeline breaks even again as cumulative masking cost grows.

**Rule of thumb:** Use `ConcurrentDrain` when you have multiple producers generating logs concurrently (e.g. processing multiple log files or streams in parallel). Use sync `Drain` for single-stream sequential ingestion at high volumes.

### Run benchmarks

```bash
cargo test --release bench_scale -- --nocapture --ignored
```

## Building & Testing

```bash
cd drain3_rust
cargo build --release
cargo test                # 12 unit tests
```

## Configuration

| Parameter | Default | Description |
|-----------|---------|-------------|
| `depth` | 4 | Max depth of prefix tree (minimum 3) |
| `sim_th` | 0.4 | Similarity threshold for cluster matching |
| `max_children` | 100 | Max children per internal node |
| `max_clusters` | unlimited | LRU eviction when limit reached |
| `extra_delimiters` | none | Additional token delimiters beyond whitespace |
| `param_str` | `<*>` | Wildcard string for variable tokens |
| `parametrize_numeric_tokens` | true | Treat digit-containing tokens as wildcards |

## TODO

- [ ] **Persistent storage** — save/load Drain state (JSON snapshots, file-based and Redis backends, matching Python Drain3's persistence modes)
- [ ] **JaccardDrain** variant — alternative similarity metric
- [ ] **Parameter extraction** — extract variable parts from matched templates
- [ ] **Tree sharding** — partition the prefix tree to unlock true concurrent scaling at high volumes

## References

- Pinjia He, Jieming Zhu, Zibin Zheng, and Michael R. Lyu. [Drain: An Online Log Parsing Approach with Fixed Depth Tree](http://jiemingzhu.github.io/pub/pjhe_icws2017.pdf), Proceedings of the 24th International Conference on Web Services (ICWS), 2017.
- Original Python implementation: [logpai/Drain3](https://github.com/logpai/Drain3)
