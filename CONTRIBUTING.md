# Contributing

`codag-drain` is intentionally narrow: deterministic log templating for
agent-facing log artifacts. Keep incident diagnosis, root-cause inference, and
policy decisions out of this repo.

## Development Checks

Run these before opening a PR:

```bash
CARGO_TARGET_DIR=/private/tmp/codag-drain-target cargo fmt --check
CARGO_TARGET_DIR=/private/tmp/codag-drain-target cargo clippy --all-targets --all-features -- -D warnings
CARGO_TARGET_DIR=/private/tmp/codag-drain-target cargo test
```

Public parser benchmarks require LogHub-2.0 structured CSVs:

```bash
LOGHUB_DIR=/path/to/loghub2 \
CARGO_TARGET_DIR=/private/tmp/codag-drain-target \
scripts/public_benchmarks.sh
```

Private downstream agent-serving evals belong outside this repo unless the data
and judging setup are also made reproducible.

## Benchmark Claims

Do not claim a win from compression alone. Any benchmark claim should say:

- what dataset was used;
- whether the data is public or private;
- the sample size and sampling unit;
- the metric and confidence interval;
- the baseline, especially raw logs and Drain3;
- the regime where the claim holds.

The current honest claim is scoped to large/noisy log windows under a fixed
artifact budget. Small-window raw logs can outperform templated artifacts on
diagnosis quality.
