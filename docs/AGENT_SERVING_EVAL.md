# Agent-Serving Evaluation

This is the downstream product eval: does a bounded log artifact help an agent
diagnose incidents better than the default raw-log prompt?

It is intentionally separate from the public deterministic parser benchmark in
[`PUBLIC_BENCHMARKS.md`](PUBLIC_BENCHMARKS.md). Parser metrics measure grouping
quality. This eval measures the agent-facing artifact.

## Design

Dataset: labeled incident windows from `infra-logs/v1-hgbt/data/incidents_labeled.jsonl`.

Arms:

- `default_agent_raw`: raw log lines, truncated to the artifact budget.
- `drain3`: Drain3 template groups with counts and raw samples.
- `codag_drain`: codag-drain template groups with counts, raw samples, and slot
  summaries.

Procedure:

1. Scale each incident to a target line count.
2. Build one blinded artifact per arm.
3. Ask `gpt-5.5` to diagnose each artifact without gold labels.
4. Shuffle diagnoses and ask a separate blind judge prompt to score each one
   against gold root-cause metadata and signal lines.
5. Report paired deltas, bootstrap CIs, and one-sided paired randomization
   p-values.

Success criterion:

`codag_drain - default_agent_raw` must have positive mean score delta, bootstrap
95% CI low > 0, and permutation p < 0.05.

Artifact budget:

The raw artifact is capped at 80,000 chars, which is about 20,000 estimated
tokens in this harness. This models a practical serving budget, not an infinite
raw-log oracle.

## Results

### 300-line windows

Report: `infra-logs/reports/blind_judge_codag_drain_agent_serving_n80_scale300_v2_20260523_013617.md`

| arm | n | mean score | 95% CI | median | mean tokens |
|---|---:|---:|---:|---:|---:|
| codag_drain | 80 | 0.541 | [0.467, 0.613] | 0.700 | 1,763 |
| default_agent_raw | 80 | 0.646 | [0.583, 0.704] | 0.700 | 5,548 |
| drain3 | 80 | 0.548 | [0.477, 0.615] | 0.700 | 1,890 |

| comparison | mean delta | 95% CI | p_perm | W/L/T | token delta |
|---|---:|---:|---:|---:|---:|
| codag_drain - default_agent_raw | -0.105 | [-0.163, -0.053] | 1.0000 | 16/27/37 | -3,785 |
| codag_drain - drain3 | -0.007 | [-0.053, 0.038] | 1.0000 | 20/15/45 | -127 |
| drain3 - default_agent_raw | -0.098 | [-0.156, -0.044] | 1.0000 | 11/30/39 | -3,658 |

Interpretation: negative quality result versus raw. At this size, the raw
artifact usually preserves enough evidence and the templated arms mainly buy
token savings. Do not claim codag-drain beats the default agent in this regime.

### 3,000-line windows

Report: `infra-logs/reports/blind_judge_codag_drain_agent_serving_n80_scale3000_v1_20260523_021116.md`

| arm | n | mean score | 95% CI | median | mean tokens |
|---|---:|---:|---:|---:|---:|
| codag_drain | 80 | 0.615 | [0.540, 0.685] | 0.750 | 1,902 |
| default_agent_raw | 80 | 0.470 | [0.400, 0.538] | 0.425 | 20,000 |
| drain3 | 80 | 0.537 | [0.457, 0.611] | 0.700 | 2,046 |

| comparison | mean delta | 95% CI | p_perm | W/L/T | token delta |
|---|---:|---:|---:|---:|---:|
| codag_drain - default_agent_raw | +0.146 | [+0.053, +0.231] | 0.0007 | 49/11/20 | -18,098 |
| codag_drain - drain3 | +0.078 | [+0.018, +0.142] | 0.0076 | 34/15/31 | -143 |
| drain3 - default_agent_raw | +0.068 | [-0.019, +0.150] | 0.0661 | 39/17/24 | -17,954 |

Interpretation: positive product result under bounded-context pressure.
codag-drain materially improves diagnosis score over raw and Drain3 while using
roughly 9.5% of the raw artifact tokens.

## Honest Claim

The defensible launch claim is:

> codag-drain improves agent diagnosis on large/noisy log windows under a fixed
> artifact budget, while staying competitive with raw logs on small windows at a
> fraction of the token load.

The stronger claim "codag-drain is always better than raw logs" is false on the
300-line benchmark and should not be used.

## Known Gaps

- This eval depends on a local labeled incident corpus and `gpt-5.5` access, so
  it is not yet a public benchmark.
- Raw's 20k-token artifact cap is a serving-budget choice. Different caps should
  be swept before making claims about other deployment settings.
- Some auth/oom-style cases still favor raw. Those need case-level inspection to
  determine whether codag-drain dropped discriminating samples or whether the
  compression objective is mismatched for that incident family.
