# Public Benchmarks

This page records the reproducible deterministic parser benchmark for
`codag-drain`. It is intentionally separate from blind LLM diagnosis judging:
the benchmark below makes no model calls and can be rerun from this repo.

## Benchmark Design

Dataset: LogHub-2.0 structured CSVs under `LOGHUB_DIR`.

Systems: Apache, BGL, HDFS, HPC, Hadoop, HealthApp, Linux, Mac, OpenSSH,
OpenStack, Proxifier, Spark, Thunderbird, Zookeeper.

Sample size: 3,000 lines per system, 14 systems, 42,000 labeled log lines.

Metrics:

- `GA`: line-weighted group accuracy against oracle `EventTemplate` groups.
- `FGA`: group-level F1 requiring exact predicted/oracle member set match.
- `FTA`: FGA plus exact normalized template string match.
- `purity`: 1 - line-weighted overmerge impurity.
- `line_cx`: input lines / output template groups.
- `char_cx`: input chars / rendered template chars.

Confidence intervals are non-parametric bootstrap 95% CIs over systems
(macro-system mean). This avoids claiming significance from correlated lines
within the same system.

Run:

```bash
LOGHUB_DIR=/path/to/loghub2 \
scripts/public_benchmarks.sh
```

Equivalent direct commands:

```bash
LOGHUB_DIR=/path/to/loghub2 \
CARGO_TARGET_DIR=/private/tmp/codag-drain-target \
  cargo test -p codag-drain --test eval_loghub grouping_loghub -- --ignored --nocapture

LOGHUB_DIR=/path/to/loghub2 \
CARGO_TARGET_DIR=/private/tmp/codag-drain-target \
  cargo test -p codag-drain --test eval_loghub compression_loghub -- --ignored --nocapture

LOGHUB_DIR=/path/to/loghub2 \
CARGO_TARGET_DIR=/private/tmp/codag-drain-target \
  cargo test -p codag-drain --test eval_loghub timing_loghub_default_vs_drain3 -- --ignored --nocapture
```

## Arms

- `drain`: codag-drain default. Drain-style positional similarity with compact
  single-token lexical fallback, plus codag template rendering and slot summaries.
- `drain_stock`: Drain3-compatible tokenization/masking inside codag-drain.
- `drain_delim`: Drain with generic punctuation delimiters.
- `drain_full`: Drain similarity over all same-length clusters, bypassing
  prefix-tree routing.
- `statistical`: generic lexical co-occurrence experiment.
- `drain3`: base `drain3_rust` control.

## Grouping Results

| arm | GA mean | GA 95% CI | FGA mean | FGA 95% CI | FTA mean | FTA 95% CI | purity mean | purity 95% CI |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| drain | 0.770 | [0.596, 0.923] | 0.833 | [0.707, 0.933] | 0.297 | [0.195, 0.416] | 0.978 | [0.955, 0.994] |
| drain_stock | 0.770 | [0.596, 0.923] | 0.833 | [0.707, 0.933] | 0.297 | [0.195, 0.416] | 0.978 | [0.955, 0.994] |
| drain_delim | 0.719 | [0.546, 0.875] | 0.819 | [0.698, 0.916] | 0.290 | [0.189, 0.409] | 0.977 | [0.955, 0.994] |
| drain_full | 0.668 | [0.472, 0.850] | 0.744 | [0.600, 0.865] | 0.244 | [0.156, 0.340] | 0.926 | [0.832, 0.985] |
| statistical | 0.650 | [0.485, 0.806] | 0.782 | [0.678, 0.868] | 0.289 | [0.193, 0.407] | 0.980 | [0.964, 0.993] |
| drain3 | 0.770 | [0.596, 0.923] | 0.833 | [0.707, 0.933] | 0.186 | [0.126, 0.244] | 0.978 | [0.955, 0.994] |

## Paired Delta vs Drain3

These are paired bootstrap deltas over systems, candidate minus `drain3`.
They are stricter than comparing independent CIs because each arm sees the
same systems.

| arm | GA delta | GA 95% CI | FGA delta | FGA 95% CI | FTA delta | FTA 95% CI | purity delta | purity 95% CI |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| drain | +0.000 | [+0.000, +0.000] | +0.000 | [+0.000, +0.000] | +0.111 | [+0.027, +0.230] | +0.000 | [+0.000, +0.000] |
| drain_stock | +0.000 | [+0.000, +0.000] | +0.000 | [+0.000, +0.000] | +0.111 | [+0.027, +0.230] | +0.000 | [+0.000, +0.000] |
| drain_delim | -0.051 | [-0.122, -0.003] | -0.014 | [-0.037, +0.003] | +0.103 | [+0.021, +0.222] | -0.000 | [-0.001, +0.000] |
| drain_full | -0.102 | [-0.259, -0.002] | -0.090 | [-0.217, -0.011] | +0.058 | [+0.022, +0.108] | -0.051 | [-0.141, -0.004] |
| statistical | -0.120 | [-0.219, -0.041] | -0.051 | [-0.112, +0.010] | +0.103 | [+0.022, +0.222] | +0.003 | [-0.004, +0.011] |

## Compression Results

| arm | line_cx mean | line_cx 95% CI | char_cx mean | char_cx 95% CI |
|---|---:|---:|---:|---:|
| drain | 168.33x | [68.10, 320.77] | 40.91x | [16.95, 80.04] |
| drain_stock | 168.33x | [68.10, 320.77] | 40.91x | [16.95, 80.04] |
| drain_delim | 173.30x | [69.10, 325.46] | 41.89x | [17.01, 80.90] |
| drain_full | 263.24x | [78.78, 514.59] | 50.70x | [19.53, 93.89] |
| statistical | 159.62x | [61.99, 313.25] | 38.45x | [15.06, 77.75] |

## Timing Results

The grouping benchmark table times `codag-drain` arms through
`Profile::build`, while the `drain3` control only groups. The timing benchmark
separates those paths.

| mode | lines/s mean | lines/s 95% CI |
|---|---:|---:|
| drain_group | 87,989 | [74,096, 103,725] |
| drain_stock_group | 105,356 | [89,924, 122,846] |
| drain_render | 34,395 | [23,707, 48,010] |
| drain3_group | 103,808 | [88,605, 120,762] |

Speed ratios:

| ratio | mean | 95% CI |
|---|---:|---:|
| drain_group / drain3_group | 0.843 | [0.829, 0.859] |
| drain_render / drain3_group | 0.311 | [0.254, 0.380] |

## Held-Out GitHub-Shaped Sanity Check

This is not a public benchmark because the local JSONL comes from the private
codag incident corpus. It is included here as a distribution-shift check because
the old rule-heavy arms looked good on LogHub and failed here.

Data: 6,261 GitHub-shaped log lines with oracle templates, no incident
root-cause labels.

Grouping:

| arm | GA | FGA | FTA | purity |
|---|---:|---:|---:|---:|
| drain | 0.548 | 0.624 | 0.134 | 0.688 |
| drain_stock | 0.548 | 0.624 | 0.134 | 0.688 |
| drain_delim | 0.496 | 0.533 | 0.110 | 0.689 |
| drain_full | 0.511 | 0.587 | 0.131 | 0.664 |
| statistical | 0.496 | 0.420 | 0.124 | 0.837 |
| drain3 | 0.548 | 0.624 | 0.082 | 0.688 |

Compression:

| arm | templates | line_cx | char_cx |
|---|---:|---:|---:|
| drain | 266 | 23.54x | 5.91x |
| drain_stock | 266 | 23.54x | 5.91x |
| drain_delim | 294 | 21.30x | 5.48x |
| drain_full | 251 | 24.94x | 6.24x |
| statistical | 561 | 11.16x | 3.23x |

Critical read: default codag-drain matches Drain3 grouping on this held-out
shape. The measurable deterministic difference is again the rendered artifact:
FTA is higher because codag derives a more oracle-like emitted template, not
because it discovered different member groups.

## Critical Read

`drain_full` has the strongest compression, but grouping quality regresses
badly and purity drops. This is the classic overmerge failure: fewer templates
can look good on compression while harming evidence separation.

`drain` and `drain_stock` match base Drain3 on grouping. The codag-specific
advantage in this deterministic public benchmark is not grouping accuracy; it is
the output layer: codag derives templates, examples, and slot summaries in the
shape that `codag wrap` needs. The paired +0.111 FTA delta versus `drain3`
comes from codag's rendered template derivation, not from a fundamentally
different grouping algorithm.

The speed story is also an output-layer story. `drain_stock_group` and
`drain3_group` are effectively equal. Default `drain_group` is slower because it
checks for compact one-token lines before using the Drain path. `drain_render`
is much slower because it runs grouping plus template profiling, slot capture,
numeric summaries, and sample rendering.

The old structural/adaptive/fixed rule-heavy arms were removed from the current
code path. They were useful as temporary controls, but keeping them invited
LogHub overfitting and confused the thesis.

The product diagnosis result must be measured separately with the blind judge,
because deterministic grouping metrics do not directly measure whether an agent
can diagnose the incident. See
[`AGENT_SERVING_EVAL.md`](AGENT_SERVING_EVAL.md) for the downstream evidence and
its caveats. That eval is not yet a public benchmark because it depends on a
local labeled incident corpus and `gpt-5.5` access.
