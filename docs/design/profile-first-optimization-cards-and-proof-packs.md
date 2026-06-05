# Profile-First Optimization Cards and One-Lever Proof Packs

**Bead:** `bd-db300.7.5.4` (G5.4)
**Date:** 2026-03-23
**Status:** Operational contract
**Depends on:** G5.3 (evidence ownership, CLOSED), A4.1 (metric dictionary, CLOSED)

---

## Purpose

No code-changing performance bead may begin implementation without a measured
hotspot, an EV-scored recommendation card, and a behavior-preserving proof
plan. This contract makes that discipline enforceable by defining the card
schema, the score gate, the proof-pack contents, and the gap-conversion rules
that turn missing evidence into tracked work.

---

## 1. Optimization Recommendation Card Schema

Every code-changing performance bead must carry a recommendation card before
implementation begins. The card is a structured record, not prose.

```json
{
  "schema_version": "fsqlite-e2e.optimization_card.v1",
  "bead_id": "bd-db300.4.3.1",
  "title": "Statement/program reuse with auditable invalidation",

  "_section_hotspot": "What was measured",
  "hotspot_artifact": "artifacts/MVCC-PFA-01/20260321/hot_path_profile.jsonl",
  "hotspot_rank": 1,
  "hotspot_rank_metric": "time_ns_total",
  "hotspot_rank_scope": "top_5_by_time",
  "hotspot_evidence_summary": "parse+compile lifecycle accounts for 42% of c1 wall time",
  "baseline_comparator": "sqlite3_c1_baseline",
  "baseline_artifact": "artifacts/REF-COR-01/20260321/hot_path_profile.jsonl",
  "profile_command": "realdb-e2e hot-profile --workload commutative_inserts --concurrency 1",

  "_section_scoring": "Is it worth doing",
  "ev_score": 3.2,
  "relevance_score": 0.85,
  "priority_tier": "P1",
  "score_formula": "EV = hotspot_fraction × expected_reduction × confidence",
  "score_breakdown": {
    "hotspot_fraction": 0.42,
    "expected_reduction": 0.80,
    "confidence": 0.95
  },
  "score_gate": "EV >= 2.0",
  "score_gate_passed": true,
  "override_reason": null,

  "_section_lever": "What exactly changes",
  "one_lever_scope": "Cache compiled statement programs keyed by (sql_text_hash, schema_epoch)",
  "one_lever_files": [
    "crates/fsqlite-core/src/connection.rs",
    "crates/fsqlite-vdbe/src/codegen.rs"
  ],
  "graveyard_section": "§3.2 Memoization / program-cache reuse",
  "primitive_lineage": "LRU program cache with epoch-based invalidation",

  "_section_targets": "What success looks like",
  "target_p50_delta": "-30% commit latency at c1",
  "target_p99_delta": "-20% commit latency at c1",
  "target_throughput_delta": "+40% ops/sec at c1 disjoint",
  "target_metric_primary": "c1_disjoint_throughput_ratio",
  "target_direction": "higher_is_better",

  "_section_proof": "How correctness is preserved",
  "proof_block": {
    "golden_output_check": true,
    "oracle": "rusqlite",
    "compatibility_surface": "row_level",
    "invariants_preserved": ["schema_epoch_invalidation", "concurrent_mode_default_true"],
    "proof_artifact": "artifacts/bd-db300.4.3.1/proof_pack/",
    "proof_rerun": "cargo test -p fsqlite-core test_program_cache -- --nocapture"
  },

  "_section_rollout": "How it ships safely",
  "adoption_wedge": "Enabled by default for :memory: databases; file-backed behind PRAGMA fsqlite.program_cache = on until proof pack passes at c4",
  "rollout_posture": "shadow_then_default",
  "budgeted_mode": "cache_size_bytes <= 8MB",
  "on_exhaustion_behavior": "LRU eviction, no performance cliff",
  "fallback_trigger": "If any conformance test fails with cache enabled, disable and log",
  "rollback_recipe": "Set PRAGMA fsqlite.program_cache = off; or revert commit {sha}",

  "_section_risk": "What can go wrong",
  "primary_failure_risk": "Stale cached program used after DDL changes schema",
  "countermeasure": "schema_epoch check on every cache hit; invalidate on DDL commit",
  "expected_loss_model": null,
  "calibration_trigger": null,

  "_section_repro": "How to reproduce the evidence",
  "repro_pack": {
    "env_json": "artifacts/bd-db300.4.3.1/env.json",
    "manifest_json": "artifacts/bd-db300.4.3.1/manifest.json",
    "repro_lock": null
  },
  "rerun_command": "realdb-e2e hot-profile --workload commutative_inserts --concurrency 1 --bead bd-db300.4.3.1"
}
```

---

## 2. Required Fields and Gate Rules

### 2.1 Mandatory Fields (No Exceptions)

| Field | Gate Rule |
|-------|----------|
| `bead_id` | Must be a valid bead ID |
| `hotspot_artifact` | Must point to an existing profile artifact |
| `hotspot_rank` | Must be 1–5 (top-5 by the named metric) |
| `hotspot_rank_metric` | Must be in the metric dictionary (A4.1) |
| `baseline_comparator` | Must name the reference (sqlite3, previous commit) |
| `profile_command` | Must be a runnable command |
| `ev_score` | Must be ≥ 2.0 unless `override_reason` is non-null |
| `score_formula` | Must name the factors |
| `one_lever_scope` | Must describe exactly one change |
| `one_lever_files` | Must list the files that will change |
| `proof_block.oracle` | Must name the correctness oracle |
| `proof_block.proof_rerun` | Must be a runnable command |
| `rollback_recipe` | Must describe how to undo |

### 2.2 Score Gate

```
IF ev_score < 2.0 AND override_reason IS NULL:
  → REJECT: bead cannot proceed to implementation
  → Create a bead: "Profile deeper for {bead_id} — EV too low"

IF ev_score < 2.0 AND override_reason IS NOT NULL:
  → ACCEPT with audit trail
  → Valid override reasons:
    - "correctness_blocker" (fixing a bug, not optimizing)
    - "blocking_benchmark_repair" (bd-zna34 class)
    - "architectural_prerequisite" (must land before higher-EV work)
  → Any other reason requires human approval
```

### 2.3 Hotspot Rank Gate

```
IF hotspot_rank > 5:
  → REJECT: lever is not in the top-5 hotspots
  → Valid exception: hotspot_rank_scope = "top_5_by_tail" (p99 focused)
    OR hotspot_rank_scope = "top_5_by_allocations"
  → The rank must be in the top-5 of SOME dominant metric, not just
    a plausible-sounding thing to improve
```

### 2.4 One-Lever Attribution Gate

```
IF one_lever_files touches more than 3 crates:
  → WARNING: broad change surface — verify single-lever attribution
  → Card must explain why multiple crates are needed for ONE lever

IF one_lever_scope contains "and" or lists multiple independent changes:
  → REJECT: split into separate beads with separate cards
  → Each lever gets its own card, baseline, proof, and attribution
```

---

## 3. One-Lever Proof Pack

Each optimization card references a proof pack. The proof pack is the evidence
bundle that proves the lever works without breaking correctness.

```
artifacts/{bead_id}/proof_pack/
├── baseline_profile.jsonl     ← Hot-path profile BEFORE the change
├── after_profile.jsonl        ← Hot-path profile AFTER the change
├── delta_summary.json         ← Machine-diff of baseline vs after
├── correctness_result.json    ← Oracle comparison result (pass/fail)
├── invariant_check.json       ← Specific invariants checked
├── rerun.sh                   ← Reproduce both profiles
└── rollback_test.json         ← Proof that rollback recipe works
```

### 3.1 `delta_summary.json`

```json
{
  "bead_id": "bd-db300.4.3.1",
  "lever": "program_cache_reuse",
  "baseline_commit": "abc123",
  "after_commit": "def456",
  "metric_deltas": [
    {"metric": "c1_disjoint_throughput", "baseline": 48000, "after": 67200, "delta_pct": 40.0, "direction": "improvement"},
    {"metric": "c1_disjoint_p99_ns", "baseline": 250000, "after": 200000, "delta_pct": -20.0, "direction": "improvement"},
    {"metric": "c4_mixed_throughput", "baseline": 95000, "after": 96000, "delta_pct": 1.0, "direction": "neutral"}
  ],
  "attribution_confidence": "high",
  "attribution_method": "single_commit_delta",
  "confounders_checked": ["no other commits between baseline and after", "same hardware", "same seed"]
}
```

### 3.2 `rollback_test.json`

```json
{
  "rollback_method": "pragma_disable",
  "rollback_command": "PRAGMA fsqlite.program_cache = off",
  "post_rollback_throughput": 48500,
  "post_rollback_matches_baseline": true,
  "rollback_latency_ms": 0
}
```

### 3.3 Benchmark Catastrophe Recovery Packs

Some benchmark-recovery slices are measurement-only work in `fsqlite-e2e`
rather than direct code changes in the engine. Those slices still need a typed
proof artifact instead of ad hoc stderr lines.

Current operational example: `bd-wwqen` IN-subquery catastrophe recovery.

- 10k residual probe:
  `manual_perf_probe.read_guard_shapes.in_subquery`
  Contract: pass if `p50 < 500us` and `p95 < 800us`
  Legacy anchor: `p50=3760us p95=4429us throughput=266 ops/sec`
- 100k catastrophe probe:
  `manual_hot_path_profile.in_subquery_100k`
  Contract: pass if `wall < 200ms`
  Hard fail if `wall > 5s`
  Legacy anchor: `~20s wall time`

The canonical artifact for these slices is:

```json
{
  "schema_version": "fsqlite-e2e.benchmark_recovery_report.v1",
  "bead_id": "bd-wwqen",
  "slice_id": "in_subquery_catastrophe_recovery",
  "thresholds": [
    {
      "probe_id": "in_subquery_10k_latency",
      "target_summary": "PASS if p50 < 500us and p95 < 800us"
    },
    {
      "probe_id": "in_subquery_100k_wall_time",
      "target_summary": "PASS if wall < 200ms; HARD FAIL if wall > 5s"
    }
  ]
}
```

Packaging rule:

- keep the legacy human-readable log lines because bead history may cite them
- also emit the structured recovery report JSON
- emit a markdown summary derived from the same typed report so pass/fail
  decisions are reproducible and reviewable without reinterpreting the raw logs

---

## 4. Controller/Policy-Bearing Optimization Cards

When the lever involves adaptive behavior (E4 guardrail thresholds, DRO
parameters, GC escalation tiers), additional fields are mandatory:

| Field | Description |
|-------|-------------|
| `expected_loss_model` | How expected loss is computed for the policy decision |
| `calibration_trigger` | What triggers recalibration (regime shift, drift detection) |
| `budgeted_mode` | Resource budget and what it protects |
| `on_exhaustion_behavior` | What happens when the budget is consumed |
| `fallback_trigger` | Concrete condition that reverts to safe/conservative mode |
| `shadow_lineage` | Run ID of the conservative baseline for comparison |

These fields map to the G5.3 evidence-ownership registry's `policy_id`,
`decision_id`, `budget_id`, and `shadow_lineage` columns.

---

## 5. Gap-Conversion Rules

### G1: Missing Baseline Profile → Block Implementation

```
IF hotspot_artifact does not exist or is stale (> 7 days old):
  → Implementation BLOCKED
  → Create bead: "Capture baseline profile for {bead_id}"
  → Priority: same as the implementation bead
```

### G2: Missing Proof Pack → Block Scorecard Claim

```
IF proof_pack directory is empty or correctness_result.json missing:
  → The lever's delta CANNOT appear on the scorecard
  → Create bead: "Complete proof pack for {bead_id}"
```

### G3: Multi-Lever Bundle → Split

```
IF one_lever_scope describes more than one independent change:
  → Split into N beads, each with its own card
  → The parent bead becomes an epic that coordinates them
  → Each child bead must independently pass the EV gate
```

### G4: Missing Rollback Recipe → Block Merge

```
IF rollback_recipe is null or empty:
  → The change CANNOT be merged to main
  → Every optimization must be reversible without data loss
```

---

## 6. Validation Entrypoint

```bash
scripts/verify_g5_4_profile_contract.sh
```

This script:
1. Enumerates all code-changing performance beads in the program (from the
   beads database, filtered by label `perf` or `db300-program`).
2. For each bead, checks:
   - Optimization card exists (JSON file in `artifacts/{bead_id}/`).
   - All mandatory fields are present and non-null.
   - `ev_score >= 2.0` OR `override_reason` is valid.
   - `hotspot_rank <= 5`.
   - `one_lever_files` lists ≤ 3 crates (or has justification).
   - `proof_block.proof_rerun` is a syntactically valid command.
   - `rollback_recipe` is non-empty.
3. Reports:
   - Beads with complete cards (ready for implementation).
   - Beads with incomplete cards (missing fields listed).
   - Beads with no card at all (gap → G1 rule).
4. Emits `artifacts/g5_4_profile_contract_report.json`.
5. Exits 0 if all Quick-mandatory beads have complete cards, 1 otherwise.

---

## 7. Structured Log Schema

When an optimization card is evaluated (at implementation start, proof
verification, or scorecard compilation):

```rust
tracing::info!(
    target: "fsqlite::optimize::card",
    trace_id = %trace_id,
    scenario_id = %scenario_id,
    bead_id = %bead_id,
    baseline_comparator = %baseline,
    hotspot_artifact = %hotspot_path,
    ev_score = ev,
    relevance_score = relevance,
    priority_tier = %tier,
    score_formula = %formula,
    hotspot_rank = rank,
    hotspot_rank_metric = %metric,
    one_lever_scope = %scope,
    target_metric = %target,
    fallback_trigger = fallback.as_deref().unwrap_or("none"),
    proof_artifact = %proof_path,
    rollback_recipe = %rollback,
    card_status = %status,  // "complete", "incomplete", "blocked", "override"
);
```

---

## 8. Example Cards for Active Beads

### 8.1 bd-db300.4.3.1 (Statement/Program Reuse)

- **Hotspot:** Parse+compile lifecycle = 42% of c1 wall time (rank 1 by time)
- **EV:** 3.2 (0.42 × 0.80 × 0.95)
- **Lever:** LRU program cache keyed by (sql_hash, schema_epoch)
- **Proof:** Row-level oracle against rusqlite; schema_epoch invalidation test
- **Rollback:** `PRAGMA fsqlite.program_cache = off`

### 8.2 bd-db300.4.4.1 (Copy-Kernel Elimination)

- **Hotspot:** Cell assembly + payload copy = 18% of c1 wall time (rank 3 by time)
- **EV:** 2.1 (0.18 × 0.65 × 0.80 × correction for already-optimized paths)
- **Lever:** Zero-copy cell reference for in-cache pages
- **Proof:** B-tree invariant property tests; overflow-chain round-trip
- **Rollback:** Feature flag `FSQLITE_COPY_KERNEL_V2=off`

### 8.3 bd-3t52f (DRO Abort Policy)

- **Hotspot:** Abort cascades under skew = p99 tail contributor (rank 2 by tail)
- **EV:** 2.8 (measured via adversarial schedule bd-1uguv)
- **Lever:** DRO loss matrix replacing static threshold
- **Proof:** SSI serializable oracle; adversarial schedule p99 non-regression
- **Rollback:** `PRAGMA fsqlite_ssi_risk_tolerance = 'low'` (conservative default)
- **Policy fields:** expected_loss_model = CVaR, calibration_trigger = regime_shift, fallback = conservative_matrix

### 8.4 bd-db300.11.1 (Transaction-Local DML Mutation Operator)

- **Hotspot:** Current `UPDATE/DELETE Throughput` DELETE tail remains the
  highest measured C SQLite-faster frontier. Evidence: the current full quick
  recertification
  `tests/artifacts/perf/codex-current-fullquick-1c7f5b33-20260515T002530Z/full-quick.json`,
  the matching focused DML profile
  `tests/artifacts/perf/codex-current-dml-profile-1c7f5b33-20260515T003014Z/update-delete-profile.json`,
  the matching focused DELETE compare profile
  `tests/artifacts/perf/codex-7ea5da35-dml-compare-20260513T0120Z/summary.md`,
  the prior DELETE CPU profile
  `tests/artifacts/perf/codex-current-delete-cpu-profile-20260511T1745Z/summary.md`,
  and the earlier recertification summary
  `tests/artifacts/perf/codex-dml-mutation-frontier-recert-20260511T1905Z/summary.md`.
  The latest rebuilt source commit for the full quick matrix is
  `1c7f5b33faee761fdb50cf556e97610a2f93ae4c`; the current full quick matrix
  reports `93` scenarios with FrankenSQLite faster / comparable /
  C-SQLite-faster at `78 / 2 / 13`, primary score `0.3852`, geomean F/C
  `0.2795`, and p99 F/C `3.2855`. The prepared-DML DELETE tail is still red at
  `3.2855x` slower for 5 rows, `1.9567x` slower for 50 rows, and `1.6791x`
  slower for 500 rows in that matrix. The focused DML profile at `1c7f5b33`
  keeps every DELETE on the prepared direct path (`slow=0`) and measures F/C
  ratios of `2.98x` for 5 deletes, `1.85x` for 50 deletes, and `1.76x` for
  500 deletes. Its representative 10K/500 row has 433 retained same-leaf active
  hits across 496 attempts, 63 leaf-boundary misses, 64 dirty flushes, `38.6 us`
  of delete-run materialization, `8.1 us` of page writes, `42.9 us` of execute
  body time, and `22.6 us` of commit roundtrip. The prior
  CPU profile also shows the top remaining frames are still
  `TransactionKind::get_page`, `TableLeafDeleteRun::delete_rowid_with_reason`,
  `TransactionKind::write_page_data`, and `TransactionKind::free_page`.
- **Rejected smaller cards:** retained `TableLeafDeleteRun` materializer
  tweaks, tombstone-only DELETE overlays, dense-rowid queued overlays,
  standalone freed-page lookup changes, private-memory page-1/commit shortcuts,
  cursorless/direct flush wrappers, and no-op direct-write pre-gates are fenced
  in `docs/progress/perf-negative-results.md`.
- **Lever:** one transaction-local DML mutation operator that buffers
  page-local rowid delete/update messages in logical key space, merges them at
  a proven first-read or commit boundary, and publishes the same MVCC conflict
  surface as physical row deletes. This is a B-epsilon-style message-buffer
  lever; it is not a tombstone-only shortcut and it must not key by physical
  cell index.
- **Proof:** row-level oracle against rusqlite for affected-row counts,
  duplicate and missing rowids, read-your-writes, rollback, savepoints,
  schema drift, QF/count-cache invalidation, and MVCC publication. Concurrent
  mode defaults in `Connection`, `HarnessSettings`, `FsqliteExecConfig`, and
  `benchmark_settings()` remain true.
- **Keep gate:** two focused `--quick --filter update` runs must improve the
  5-row, 50-row, and 500-row DELETE medians without regressing UPDATE, followed
  by two full quick matrix runs with primary-score neutrality or better and no
  new critical red rows.
- **Rollback:** revert the single optimization commit; no compatibility shim
  or permanent off-by-default mode is acceptable.
- **Card status:** open only for the broader mutation-operator lever above.
  Any smaller overlay or retained-leaf-run attempt is blocked until a newer
  profile invalidates the 2026-05-10 rejection artifacts. The current source
  feasibility probe
  `tests/artifacts/perf/codex-dml-operator-probe-20260513T0303Z/summary.md`
  confirms that the MVCC cell-delta primitive is not yet wired into the live
  `SharedTxnPageIo` / `TransactionHandle` path, so the next viable slice is the
  integration boundary rather than another retained DELETE micro-patch.

---

## 9. Consequences for Downstream

| Downstream | What It Gets |
|------------|-------------|
| **D1.b, D1.c** (parallel WAL) | Card schema and proof-pack requirements for WAL lever |
| **E2.2.c** (fused entry) | Card schema for fused prepared-DML entry lever |
| **E3.3.a** (metadata publication) | Card schema for immutable publication lever |
| **G5.5** (regime atlas) | EV scores and hotspot ranks for regime classification |
| **G5.6** (shadow oracle) | Shadow lineage fields in controller-bearing cards |
| **G4** (final scorecard) | Proof-pack delta_summary.json as scorecard cell evidence |
