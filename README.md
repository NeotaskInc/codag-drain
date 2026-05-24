# codag-drain

Deterministic log templating for `codag wrap`.

This repository is intentionally narrow. It turns log lines into template groups
with bounded raw examples and slot summaries. It does not diagnose incidents,
assign root cause, or build incident summaries; those belong to the inference
pipeline.

## Workspace

```text
drain3_rust/     Rust Drain3 implementation used as the base algorithm
codag-drain/     deterministic templating library and CLI
examples/server/ reference HTTP host for long-lived wrapping sessions
docs/            design and evaluation notes
```

## Default Algorithm

The default `GrouperKind::Drain` is Drain-style positional similarity with one
codag adaptation:

- normal logs use Drain3-compatible whitespace tokenization and default masking;
- compact punctuation-heavy one-token logs, such as compact JSON, use a generic
  character-class tokenizer so Drain still has token positions to compare;
- output rendering is codag-specific: template count, samples, and slot
  summaries.

Additional deterministic arms are available for evaluation:

- `drain-stock`
- `drain-delimited`
- `drain-fullsearch`
- `statistical`

See [docs/PUBLIC_BENCHMARKS.md](docs/PUBLIC_BENCHMARKS.md) for reproducible
LogHub parser benchmarks and
[docs/AGENT_SERVING_EVAL.md](docs/AGENT_SERVING_EVAL.md) for the downstream
blind-judge evidence.

## CLI

```bash
echo 'worker ready shard=1
worker ready shard=2' | cargo run -p codag-drain
```

JSON output:

```bash
echo 'worker ready shard=1' \
  | cargo run -p codag-drain -- --format json
```

Select a grouper:

```bash
cargo run -p codag-drain -- --grouper drain-stock
```

Print CLI compression stats on stderr:

```bash
cargo run -p codag-drain -- --stats
```

## Reference Server

The `examples/server` crate is a thin host around `TemplateIndex`. It is useful
for local integration tests and as a deployment reference; production auth,
tenancy, persistence, and routing should live in the production service layer.

```bash
cargo run -p codag-drain-server
```

Routes:

```text
GET  /health
POST /v1/template
POST /v1/session/:id/ingest
GET  /v1/session/:id/templates
```

Query parameters:

```text
grouper=drain|drain-stock|drain-delimited|drain-fullsearch|statistical
samples=N
format=text|json
body=text|ndjson
```

The hosted production instance is a separate Railway service:

```text
Railway project: codag-drain
Railway service: codag-drain
Production URL: https://codag-drain-production.up.railway.app
Backend env: CODAG_DRAIN_URL=https://codag-drain-production.up.railway.app
```

All `/v1/*` routes require `Authorization: Bearer <token>`. Configure the
same secret value on both services:

```text
codag-drain: CODAG_DRAIN_AUTH_TOKEN=<random secret>
backend:     CODAG_DRAIN_AUTH_TOKEN=<same random secret>
```

Deploy it from this repo root:

```bash
railway up --service codag-drain --environment production --detach
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Benchmark claims must be scoped and
paired against raw logs and Drain3; do not claim a win from compression alone.

## Tests

```bash
CARGO_TARGET_DIR=/private/tmp/codag-drain-target cargo test
CARGO_TARGET_DIR=/private/tmp/codag-drain-target cargo clippy --all-targets --all-features -- -D warnings
```

Held-out evals are ignored by default because they need local data:

```bash
LOGHUB_DIR=/path/to/loghub2 \
CARGO_TARGET_DIR=/private/tmp/codag-drain-target cargo test -p codag-drain --test eval_loghub grouping_loghub -- --ignored --nocapture
GITHUB_JSONL=/path/to/github.jsonl \
CARGO_TARGET_DIR=/private/tmp/codag-drain-target cargo test -p codag-drain --test eval_loghub grouping_github_lora -- --ignored --nocapture
```
