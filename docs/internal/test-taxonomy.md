# Test Taxonomy: Minimum Coverage for Epic 1 Fix Beads

Every fix bead under **bd-zywqc** (Multi-process concurrent durability) MUST
include all 9 categories below before closing. The bead's `close_reason` must
enumerate each category with a `file:test_name` reference.

## The 9 Categories

### T1: Unit Tests

Per-function correctness, edge cases, error paths for the code the fix
modifies. Delta coverage target: 90% line + 80% branch on modified code.

**Example reference:** `crates/fsqlite-mvcc/src/commit.rs:test_commit_index_monotonic`

### T2: Integration Tests

Cross-module behavior through the public API. Uses the same SQL-in / result-out
shape a rusqlite oracle test would use, exercised via `fsqlite::Connection`.

**Example reference:** `crates/fsqlite-e2e/tests/bd_073kf_swarm_harness.rs:p1_swarm_runs_with_minimal_config`

### T3: Property Tests (proptest)

At least one property per invariant the fix establishes. Uses `proptest` with
deterministic seed derivation from `FRANKEN_SEED`.

**Example reference:** `crates/fsqlite-mvcc/src/version_chain.rs:prop_version_chain_monotonic`

### T4: Crash Tests

SIGKILL at every state transition the fix introduces. Uses the cross-process
crash harness from `fsqlite-harness::cross_process_crash_harness` (bd-073kf).

**Example reference:** `crates/fsqlite-e2e/tests/bd_<slug>_crash.rs:sigkill_mid_commit_preserves_invariant`

### T5: Concurrent Tests

At least one race the multi-process swarm harness exercises. Uses the
`swarm-multiprocess` binary (bd-073kf) or in-process `std::thread` concurrency.

**Example reference:** `crates/fsqlite-e2e/tests/bd_<slug>_concurrent.rs:eight_writers_no_lost_commits`

### T6: Performance Tests

Micro-benchmark via Criterion with no >5% single-writer regression.
Benchmark result recorded in the bead close reason.

**Example reference:** `benches/mvcc_commit.rs:bench_commit_single_writer`

### T7: Structured Logging

Emissions validate against the bd-zywqc.1 tracing schema. Every operation
in the test produces a JSONL line that parses via `validate_event_line()`.

**Example reference:** `crates/fsqlite-e2e/tests/bd_<slug>_logging.rs:all_ops_emit_valid_jsonl`

### T8: E2E Test

A `tests/e2e/<fix_slug>.rs` (or `crates/fsqlite-e2e/tests/bd_<slug>_e2e.rs`)
that exercises the fix from a user perspective with structured logging. Uses
real `Connection`, real pager, real WAL — no mocks.

**Example reference:** `crates/fsqlite-e2e/tests/bd_<slug>_e2e.rs:user_workflow_with_fix`

### T9: Negative Test

A corruption of the fix's invariant IS caught by the existing test suite. This
proves the fix is required (not just additive) — removing the fix MUST cause at
least one test to fail.

**Example reference:** `crates/fsqlite-mvcc/src/commit.rs:test_without_fix_detects_stale_commit`

## Close Reason Format

When closing a fix bead, the `close_reason` must include a block like:

```
T1: crates/fsqlite-mvcc/src/foo.rs:test_bar
T2: crates/fsqlite-e2e/tests/bd_xyz_integration.rs:test_baz
T3: crates/fsqlite-mvcc/src/foo.rs:prop_invariant_holds
T4: crates/fsqlite-e2e/tests/bd_xyz_crash.rs:sigkill_test
T5: crates/fsqlite-e2e/tests/bd_xyz_concurrent.rs:race_test
T6: benches/foo.rs:bench_no_regression (result: 0.98x, within 5% gate)
T7: crates/fsqlite-e2e/tests/bd_xyz_logging.rs:jsonl_valid
T8: crates/fsqlite-e2e/tests/bd_xyz_e2e.rs:user_workflow
T9: crates/fsqlite-mvcc/src/foo.rs:test_removal_breaks_invariant
```

## CI Gate

The `scripts/ci_taxonomy_gate.sh` script parses bead close reasons for beads
under bd-zywqc and fails if any fix bead (type=task or bug) closes without all
9 T-references.

## Exemptions

Beads may be exempt from specific categories with documented rationale in the
close reason. Valid exemptions:

- **T3 exempt**: Fix is a one-line constant change with no invariant to property-test.
- **T4 exempt**: Fix does not introduce a new state transition (pure logic change).
- **T6 exempt**: Fix is in a cold path with no performance-sensitive callers.

Exemptions MUST be explicit: `T4: EXEMPT — no new state transition introduced`.

## Retroactive Sweep

Beads closed before this taxonomy was established are marked as
**grandfathered** and do not need to retroactively satisfy the gate. The
cut-off date is the commit that introduces this document.
